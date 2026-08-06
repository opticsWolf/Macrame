use std::path::Path;

use libsql::TransactionBehavior;

use crate::error::{Result, WriteOp};
use crate::schema::ddl::ARCHIVE_SESSION_MARKER;

/// Outcome of one archive session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    pub links_archived: usize,
    /// Concepts moved to `cold.concepts` (0.9.0, C2). Always `0` before v9,
    /// where no concept could leave the hot table at all.
    pub concepts_archived: usize,
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
    // Concepts, as of v9 (C2). Trigger-free and FK-free like `cold.links`, and
    // for the same reasons -- but note what it does NOT drop.
    //
    // **Every column crosses, `content` included.** Archival is a move, not a
    // rewrite (2.3), and a move that drops a column is a rewrite. The log
    // payload for a concept carries its `content` (4.3), so a cold concept
    // whose text had been dropped would contradict `cold.transaction_log` about
    // itself, and rehydration would return a concept the ledger never recorded:
    // empty text where the log says there was text. That is the unexplained
    // absence Doctrine V exists to prevent.
    //
    // The tension with D-116 is apparent rather than real. D-116 governs the
    // *in-memory* `NodeData` representation -- `content` is not loaded by
    // default because most readers do not want it. This is *on-disk* storage.
    // Disk carries the text; memory does not populate it until asked. Two
    // independent defaults, and conflating them would make rehydration lossy to
    // save a read nobody was performing.
    //
    // `rowid_pk` crosses as the record of what the rowid *was*. Restoring it is
    // C3's problem and not obviously safe: `concepts.rowid_pk` is a plain
    // INTEGER PRIMARY KEY, so SQLite may reuse a freed value, and archiving the
    // highest rowids can leave a later insert holding one a cold row still
    // claims. The column is carried because a move must not lose it.
    //
    // **The hazard has two exits and C3 must take one of them explicitly.**
    // Either reinstate the original `rowid_pk` when it is still free, or assign
    // a fresh one — and in the second case **update `concepts_fts`'s
    // `content_rowid` mapping to match**, because the FTS index is
    // external-content keyed on this column (4.6, D-119). A rehydration that
    // reassigns the rowid without re-pointing the index leaves the search index
    // silently describing the wrong row, which is the exact failure `rowid_pk`
    // was made explicit to prevent. Named here so C3 meets both exits rather
    // than rediscovering the FTS coupling.
    r#"CREATE TABLE IF NOT EXISTS cold.concepts (
        rowid_pk         INTEGER,
        id               TEXT NOT NULL PRIMARY KEY,
        title            TEXT NOT NULL,
        content          TEXT NOT NULL DEFAULT '',
        embedding_model  TEXT,
        valid_from       TEXT NOT NULL,
        valid_to         TEXT NOT NULL,
        recorded_at      TEXT NOT NULL,
        retired          INTEGER NOT NULL DEFAULT 0
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

/// A concept is archivable when it is `retired`, both its clocks are behind the
/// cutoff, **and no surviving row of hot `links` mentions it in either
/// direction** (C1, D-128).
///
/// # Why reachability, and not a closed interval
///
/// A link assertion has a closed interval, so [`LINKS_ARCHIVABLE`] can ask
/// whether the interval ended. A concept is an *entity*, and has no closed
/// state: `retired = 1` says belief in it stopped, which is not the same claim
/// as "nothing points at it any more". The two `links` foreign keys are what
/// make that difference matter — archiving a concept physically removes its row,
/// and a surviving hot link naming it would leave the key unsatisfiable.
/// `ON DELETE CASCADE` is not the way out, because the rows it would cascade
/// onto are ledger rows.
///
/// So concept archival is **strictly downstream of link archival**: a concept
/// becomes eligible only once every edge mentioning it has itself gone cold.
/// Inside a session this predicate is therefore evaluated *after* the `links`
/// delete and never before it, and the same question asked before and after one
/// session legitimately gives two different answers. That is a property of the
/// predicate, not a race.
///
/// # The other two foreign keys, and why they are not clauses here
///
/// `concepts` also has inbound keys from `analytics_annotations` and from every
/// registered `embeddings_*` table ([`crate::schema::migrations`] lists all
/// four). Neither appears above, and the distinction is the point: those hold
/// **derived** rows. Doctrine VII makes an embedding an artifact of a model
/// applied to content, and an annotation is the output of an algorithm that read
/// `concepts` in the first place. A derived row is removed and recomputed; a
/// ledger row is neither. Making archivability wait on a recomputable artifact
/// would answer "not yet" forever for any concept that had ever been embedded.
///
/// # Both clocks, because one of them is not enough
///
/// The specification for this predicate named `valid_to` alone.
/// `recorded_at < :cutoff` is here as well, mirroring [`LINKS_ARCHIVABLE`]:
/// a concept retired with its `valid_to` behind the cutoff but *recorded* at or
/// after it is a fact the session is not meant to touch yet, and archiving it
/// would send the concept cold while the log entries describing it stayed hot.
/// That is the same two-clock mismatch the `links_current` compensation carried
/// until Wave 4.5 (see [`archive_session`]), reached from the other side.
/// Doctrine II: two clocks, never mixed.
///
/// The open sentinel needs no clause of its own — `9999-12-31T23:59:59.999999Z`
/// sorts above every canonical stamp (D-029), so a concept whose validity is
/// still open fails `valid_to < :cutoff` for any cutoff a caller can pass.
const CONCEPTS_ARCHIVABLE: &str = r#"
    retired = 1
    AND recorded_at < :cutoff
    AND valid_to    < :cutoff
    AND NOT EXISTS (
        SELECT 1 FROM links
        WHERE links.source_id = concepts.id
           OR links.target_id = concepts.id
    )
"#;

/// The ids of every concept that `CONCEPTS_ARCHIVABLE` admits at `cutoff`, in
/// `id` order.
///
/// **Read-only, and deliberately available before anything can act on it.**
/// Concept archival is the one operation in this crate a caller cannot undo
/// without a cold file to hand, so the predicate that decides it is observable
/// on its own rather than only as a count in a report after the fact.
///
/// The answer is a function of the hot state *now*. Archiving links first will
/// generally enlarge it — that is the downstream relationship
/// `CONCEPTS_ARCHIVABLE` describes, not an inconsistency — so a caller
/// planning a session should ask after the link archive, not before it.
pub async fn archivable_concepts(conn: &libsql::Connection, cutoff: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            &format!("SELECT id FROM concepts WHERE {CONCEPTS_ARCHIVABLE} ORDER BY id"),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await?;

    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}

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

    // Concepts, and **only now** — after the `links` delete, never before it
    // ([D-128](../../docs/architecture/s13-decision-register.md)). A concept is
    // archivable when nothing in hot `links` names it, so evaluating the
    // predicate before the edges have gone cold archives strictly less than the
    // session is entitled to. This ordering is the whole content of "concept
    // archival is downstream of link archival".
    let concepts_archived = archive_concepts(&tx, conn, cutoff).await?;

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
        concepts_archived,
        log_entries_archived,
        horizon,
    })
}

/// Move every concept [`CONCEPTS_ARCHIVABLE`] admits into `cold.concepts`, and
/// dispose of its derived rows (C2).
///
/// # The partition, which is the decision this function encodes
///
/// **Entity data crosses; derivative data does not.** The concept row itself is
/// moved column for column — a move that drops a column is a rewrite, and
/// [Doctrine V] does not permit an absence the ledger cannot explain. Its
/// `analytics_annotations` and `embeddings_*` rows are *deleted* rather than
/// moved, because [Doctrine VII] makes both recomputable from the content that
/// did cross. Carrying them would also be unimplementable for the vectors:
/// `F32_BLOB` and DiskANN are libSQL-specific and the cold file is a plain
/// database opened through `ATTACH`.
///
/// The disposal is not incidental to the move — it is what makes the move
/// legal. `concepts` has four inbound foreign keys, and the two derived ones
/// would refuse the `DELETE` outright.
///
/// # Why the deletes are not logged, and why that is right
///
/// `concepts` carries log triggers on insert and update but **not** on delete —
/// there was no delete path to log while the guard was unconditional, and there
/// deliberately still is not. Archival mints no transaction-time facts: the
/// concept is in the cold file, the log entries describing it are either still
/// hot or in `cold.transaction_log`, and nothing about what was believed, or
/// when, has changed. A log entry here would assert that something happened to
/// the concept at archive time, which is exactly the lie [Doctrine III] forbids.
async fn archive_concepts(
    tx: &libsql::Transaction,
    conn: &libsql::Connection,
    cutoff: &str,
) -> Result<usize> {
    let moved = tx
        .execute(
            &format!(
                "INSERT OR IGNORE INTO cold.concepts
                     (rowid_pk, id, title, content, embedding_model,
                      valid_from, valid_to, recorded_at, retired)
                 SELECT rowid_pk, id, title, content, embedding_model,
                        valid_from, valid_to, recorded_at, retired
                 FROM concepts WHERE {CONCEPTS_ARCHIVABLE}"
            ),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await? as usize;

    if moved == 0 {
        return Ok(0);
    }

    // The derived rows, before the concept they hang off. `embeddings_*` is
    // enumerated from the catalogue rather than from a list, because the set is
    // whatever `register_model` has created on *this* database and a hard-coded
    // list would silently miss a model the caller added.
    let mut derived: Vec<String> = vec!["analytics_annotations".to_string()];
    let mut rows = tx
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name LIKE 'embeddings\\_%' ESCAPE '\\'",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        derived.push(row.get::<String>(0)?);
    }
    drop(rows);

    for table in &derived {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE concept_id IN \
                 (SELECT id FROM cold.concepts)"
            ),
            (),
        )
        .await?;
    }

    // `trg_concepts_fts_delete` fires on this and keeps the search index
    // correct — the capability v8 installed inert and this rung made reachable.
    let deleted = delete_guarded(
        tx,
        conn,
        &format!("DELETE FROM concepts WHERE {CONCEPTS_ARCHIVABLE}"),
        cutoff,
        "concepts",
    )
    .await? as usize;

    debug_assert_eq!(
        moved, deleted,
        "the predicate selected a different set for the copy than for the delete"
    );

    Ok(deleted)
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
