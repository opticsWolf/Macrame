use std::path::Path;

use libsql::TransactionBehavior;

use crate::error::{Result, WriteOp};
use crate::schema::ddl::ARCHIVE_SESSION_MARKER;

/// Outcome of one archive session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    pub links_archived: usize,
    pub log_entries_archived: usize,
    /// Oldest `transaction_log.seq_id` still present in the hot file after the
    /// session, i.e. the new horizon (see glossary). `None` if the hot log is empty.
    pub horizon: Option<i64>,
}

/// Schema of the cold database. Deliberately trigger-free and FK-free: concepts
/// are never archived (D-022), so a FK from cold.links to concepts could not be
/// satisfied, and the delete guards must not exist on a file whose whole purpose
/// is to receive rows.
const COLD_SCHEMA: &[&str] = &[
    // `weight` carries the same CHECK as the hot table (T2.1, D-083). Not
    // symmetry for its own sake: the cold file is read back by `reconstruct`
    // through the same `f64` decode, so a text weight is the same panic there
    // as it is here, and a negative one is the same unsound shortest path.
    //
    // The hot table's constraint does not protect this one. Rows arrive by
    // `INSERT … SELECT` across an ATTACH, which re-checks against *this*
    // table's constraints — and a cold file may predate the hot file's rung, or
    // have been written by a version that had neither.
    //
    // `IF NOT EXISTS` means an existing cold database keeps whatever definition
    // it was created with; this constrains new cold files, and the loader guard
    // is what covers the old ones. That is the same division of labour §4.7
    // describes, and the reason the guard stays.
    r#"CREATE TABLE IF NOT EXISTS cold.links (
        source_id   TEXT NOT NULL,
        target_id   TEXT NOT NULL,
        edge_type   TEXT NOT NULL,
        valid_from  TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        valid_to    TEXT NOT NULL,
        weight      REAL NOT NULL CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
        properties  TEXT NOT NULL,
        PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)
    )"#,
    // seq_id is carried over verbatim from the hot log, so it is a plain
    // INTEGER PRIMARY KEY -- never AUTOINCREMENT, which would renumber history.
    r#"CREATE TABLE IF NOT EXISTS cold.transaction_log (
        seq_id      INTEGER PRIMARY KEY,
        table_name  TEXT NOT NULL,
        entity_id   TEXT NOT NULL,
        operation   TEXT NOT NULL,
        payload     TEXT NOT NULL,
        recorded_at TEXT NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS cold.idx_cold_txlog_entity ON transaction_log (entity_id)",
    "CREATE INDEX IF NOT EXISTS cold.idx_cold_txlog_time ON transaction_log (recorded_at)",
    r#"CREATE TABLE IF NOT EXISTS cold.archive_horizon (
        archived_at TEXT NOT NULL,
        cutoff      TEXT NOT NULL,
        horizon     INTEGER
    )"#,
];

/// A links assertion is archivable when it is older than the cutoff AND it is
/// either superseded by a later assertion for the same interval key, or it is
/// the current belief for an interval that closed before the cutoff.
///
/// This keeps every row that `links_current` still projects (Doctrine VI: the
/// materialization must stay rebuildable from `links`) while moving exactly the
/// "closed intervals, superseded history" the §2 diagram assigns to the cold file.
const LINKS_ARCHIVABLE: &str = r#"
    recorded_at < :cutoff AND (
        EXISTS (
            SELECT 1 FROM links newer
            WHERE newer.source_id   = links.source_id
              AND newer.target_id   = links.target_id
              AND newer.edge_type   = links.edge_type
              AND newer.valid_from  = links.valid_from
              AND newer.recorded_at > links.recorded_at
        )
        OR (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= :cutoff)
    )
"#;

/// A log entry is archivable when it is older than the cutoff and a later entry
/// exists for the same entity, i.e. it is superseded. The newest entry per
/// entity always stays hot so that `reconstruct(now)` never needs the cold file.
const LOG_ARCHIVABLE: &str = r#"
    recorded_at < :cutoff AND EXISTS (
        SELECT 1 FROM transaction_log newer
        WHERE newer.entity_id = transaction_log.entity_id
          AND newer.seq_id    > transaction_log.seq_id
    )
"#;

/// Move closed edge intervals and superseded log rows older than `cutoff` into
/// the cold database at `archive_path` (§5.7, D-012, D-022).
///
/// The whole session is one `BEGIN IMMEDIATE … COMMIT` transaction (D-012):
/// copy-then-delete must be atomic, or a crash between the phases duplicates or
/// loses rows. The archive-session marker that unlocks the delete guards
/// (D-008 revised) is created as the first statement of that transaction and
/// dropped as the last, so it never exists as committed state — commit drops
/// it, rollback discards it, and there is no crash path that leaves the guards
/// disarmed.
///
/// ATTACH is issued outside the transaction and DETACH is issued unconditionally
/// on the way out, including on error: ATTACH is not transactional and survives
/// ROLLBACK, so a leaked handle would make every later archive or cold-DB
/// reconstruct fail with "database cold is already in use".
/// `archived_at` is **when the session ran**; `cutoff` is the boundary it used.
///
/// Both go into `cold.archive_horizon`, and until Wave 4.5 both columns were
/// written with the cutoff — so the table recorded that every archive had run at
/// the instant it was archiving *up to*, which is the one time it certainly did
/// not run. The two are different clocks (Doctrine II) and the row exists to
/// carry both: the cutoff says what was moved, `archived_at` says when the
/// decision was taken, and only the second can answer "how stale is this cold
/// file". The column was there, correctly named, holding the wrong value.
pub async fn archive(
    conn: &libsql::Connection,
    cutoff: &str,
    archived_at: &str,
    archive_path: &Path,
) -> Result<ArchiveReport> {
    crate::temporal::replay::detach_stale_cold(conn).await;

    // ATTACH creates the cold file if it does not exist.
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive_path.to_string_lossy().as_ref()],
    )
    .await?;

    let result = archive_session(conn, cutoff, archived_at).await;

    // Unconditional: see the DETACH note above.
    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("archive: failed to DETACH cold database: {e}");
    }

    result
}

/// `conn` is passed alongside `tx` only so [`delete_guarded`] can hand it to
/// `classify`, which queries on the error path. Both name the same connection.
async fn archive_session(
    conn: &libsql::Connection,
    cutoff: &str,
    archived_at: &str,
) -> Result<ArchiveReport> {
    for ddl in COLD_SCHEMA {
        conn.execute(ddl, ()).await?;
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;

    // --- archive session opens: the delete guards are now satisfied ---
    tx.execute(&format!("CREATE TABLE {ARCHIVE_SESSION_MARKER} (x)"), ())
        .await?;

    let links_archived = tx
        .execute(
            &format!(
                "INSERT OR IGNORE INTO cold.links
                     (source_id, target_id, edge_type, valid_from, recorded_at,
                      valid_to, weight, properties)
                 SELECT source_id, target_id, edge_type, valid_from, recorded_at,
                        valid_to, weight, properties
                 FROM links WHERE {LINKS_ARCHIVABLE}"
            ),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await? as usize;

    let links_deleted = delete_guarded(
        &tx,
        conn,
        &format!("DELETE FROM links WHERE {LINKS_ARCHIVABLE}"),
        cutoff,
        "links",
    )
    .await?;

    // links_current is derivative (Doctrine VI) and must equal the latest-belief
    // projection of whatever remains in links, or audit_current() reports drift
    // the moment an archive runs. Re-derive it rather than trying to describe
    // the deletion's shadow: this used to be a hand-written
    // `DELETE FROM links_current WHERE valid_to <= :cutoff`, which filters on
    // *valid* time while LINKS_ARCHIVABLE also requires `recorded_at < :cutoff`.
    // A row closed at the cutoff but recorded at or after it therefore survived
    // in links and was deleted from links_current — permanent drift no later
    // audit could explain, from a compensation that had quietly stopped being
    // the image of the thing it compensated for. Doctrine II: two clocks, never
    // mixed. Deriving from the definition cannot drift from the definition.
    //
    // **Skipped when the DELETE removed nothing (T1.1, D-080).** `links_current`
    // is a function of `links`, so if `links` did not change its projection did
    // not either, and there is no drift for a rebuild to repair. This was
    // harmless while `archive()` was called once against a whole backlog,
    // because the one session always had work. It stops being harmless the
    // moment the caller windows: `rebuild_within` costs O(surviving `links`)
    // regardless of how much the session archived (D-077), so without this a run
    // of twenty windows over a quiet stretch of history pays twenty full
    // reprojections to delete nothing — and windowing makes the archive slower
    // in total than not windowing. `log_entries_archived` deliberately does not
    // enter into it: archiving the transaction log cannot change `links`.
    if links_deleted > 0 {
        crate::integrity::rebuild::rebuild_within(&tx, crate::integrity::rebuild::Verify::No)
            .await?;
    }

    let log_entries_archived = tx
        .execute(
            &format!(
                "INSERT OR IGNORE INTO cold.transaction_log
                     (seq_id, table_name, entity_id, operation, payload, recorded_at)
                 SELECT seq_id, table_name, entity_id, operation, payload, recorded_at
                 FROM transaction_log WHERE {LOG_ARCHIVABLE}"
            ),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await? as usize;

    delete_guarded(
        &tx,
        conn,
        &format!("DELETE FROM transaction_log WHERE {LOG_ARCHIVABLE}"),
        cutoff,
        "transaction_log",
    )
    .await?;

    // Record the new horizon in the cold file so a pre-horizon reconstruct can
    // tell "archived" from "never existed" (glossary; R14).
    let horizon: Option<i64> = tx
        .query("SELECT MIN(seq_id) FROM transaction_log", ())
        .await?
        .next()
        .await?
        .and_then(|row| row.get(0).ok());

    tx.execute(
        "INSERT INTO cold.archive_horizon (archived_at, cutoff, horizon) VALUES (?1, ?2, ?3)",
        libsql::params![archived_at, cutoff, horizon],
    )
    .await?;

    // --- archive session closes: the guards re-arm before COMMIT ---
    tx.execute(&format!("DROP TABLE {ARCHIVE_SESSION_MARKER}"), ())
        .await?;

    tx.commit().await?;

    Ok(ArchiveReport {
        links_archived,
        log_entries_archived,
        horizon,
    })
}

/// Run one of the archive's `DELETE`s, naming the table if a guard refuses it.
///
/// **This is what closes defect AC, and the shape of the fix is the point.**
/// There used to be a second classifier here — `classify_archive_violation` —
/// which was defined, delegated correctly to [`crate::error::abort_kind`], and
/// called from nowhere, so `DbError::ArchiveViolation` was unreachable by any
/// code path in the crate. It was recorded as defect H, marked Fixed by a commit
/// that made the *body* delegate rather than making the function *called*, and
/// so survived its own repair. It is deleted rather than wired up, because
/// [`crate::error::classify`] with [`WriteOp::Delete`] already did exactly what
/// it did: the defect was one classifier too many, not one too few.
///
/// A guard firing here means the marker table is absent or was dropped early —
/// the session's invariant broken from inside. That is worth a typed error
/// naming the table rather than a raw engine message naming a trigger.
async fn delete_guarded(
    tx: &libsql::Transaction,
    conn: &libsql::Connection,
    sql: &str,
    cutoff: &str,
    table: &str,
) -> Result<u64> {
    match tx
        .execute(sql, libsql::named_params! {":cutoff": cutoff})
        .await
    {
        Ok(n) => Ok(n),
        Err(e) => Err(crate::error::classify(conn, e, WriteOp::Delete { table }).await),
    }
}
