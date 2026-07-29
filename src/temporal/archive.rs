use std::path::Path;

use libsql::TransactionBehavior;

use crate::error::{DbError, Result};
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
    r#"CREATE TABLE IF NOT EXISTS cold.links (
        source_id   TEXT NOT NULL,
        target_id   TEXT NOT NULL,
        edge_type   TEXT NOT NULL,
        valid_from  TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        valid_to    TEXT NOT NULL,
        weight      REAL NOT NULL,
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
pub async fn archive(
    conn: &libsql::Connection,
    cutoff: &str,
    archive_path: &Path,
) -> Result<ArchiveReport> {
    crate::temporal::replay::detach_stale_cold(conn).await;

    // ATTACH creates the cold file if it does not exist.
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive_path.to_string_lossy().as_ref()],
    )
    .await?;

    let result = archive_session(conn, cutoff).await;

    // Unconditional: see the DETACH note above.
    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("archive: failed to DETACH cold database: {e}");
    }

    result
}

async fn archive_session(conn: &libsql::Connection, cutoff: &str) -> Result<ArchiveReport> {
    for ddl in COLD_SCHEMA {
        conn.execute(*ddl, ()).await?;
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

    tx.execute(
        &format!("DELETE FROM links WHERE {LINKS_ARCHIVABLE}"),
        libsql::named_params! {":cutoff": cutoff},
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
    crate::integrity::rebuild::rebuild_within(&tx).await?;

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

    tx.execute(
        &format!("DELETE FROM transaction_log WHERE {LOG_ARCHIVABLE}"),
        libsql::named_params! {":cutoff": cutoff},
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
        libsql::params![cutoff, cutoff, horizon],
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

/// Report an illegal physical delete as a typed error (§7).
///
/// Delegates to [`crate::error::abort_kind`] rather than matching the message
/// itself: two independent copies of the same needle is one copy too many.
pub fn classify_archive_violation(err: &libsql::Error, table: &str) -> Option<DbError> {
    match crate::error::abort_kind(err) {
        crate::error::AbortKind::DeleteOutsideArchive => Some(DbError::ArchiveViolation {
            table: table.to_string(),
        }),
        _ => None,
    }
}
