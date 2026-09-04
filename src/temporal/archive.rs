use std::path::Path;

use libsql::TransactionBehavior;

use crate::error::{DbError, Result, WriteOp};
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

/// Schema of the cold database. Deliberately trigger-free and FK-free.
///
/// **Corrected 2026-08-07.** This comment used to justify the FK-free part with
/// *"concepts are never archived (D-022)"*, which stopped being true in 0.9.0
/// when C2 added `cold.concepts` — the table declared a few lines below. The
/// reasons that survive are the other two, and they are the load-bearing ones:
/// a FK from `cold.links` to `concepts` still could not be satisfied, because
/// the cold file holds only the concepts that have gone cold and `cold.links`
/// may name any of them; and the delete guards must not exist on a file whose
/// whole purpose is to receive rows and, on rehydration, to give them back.
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
    //
    // **`branch_id` is in the key since v15** (0.14.15, D-232), and it had to
    // move with the hot table rather than after it. The hot key admitted the
    // pair, so archiving became the one operation that could still refuse it:
    // two lineages' rows about one edge at one `recorded_at` are legal in
    // `links` and would have collided on the way out, turning a write the crate
    // now accepts into a maintenance failure the caller cannot act on.
    // `upgrade_cold_lineage` carries existing cold files across.
    r#"CREATE TABLE IF NOT EXISTS cold.links (
        source_id   TEXT NOT NULL,
        target_id   TEXT NOT NULL,
        edge_type   TEXT NOT NULL,
        valid_from  TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        valid_to    TEXT NOT NULL,
        weight      REAL NOT NULL CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
        properties  TEXT NOT NULL,
        branch_id   TEXT NOT NULL DEFAULT 'main',
        PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at, branch_id)
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
        retired          INTEGER NOT NULL DEFAULT 0,
        branch_id        TEXT NOT NULL DEFAULT 'main'
    )"#,
    // seq_id is carried over verbatim from the hot log, so it is a plain
    // INTEGER PRIMARY KEY -- never AUTOINCREMENT, which would renumber history.
    r#"CREATE TABLE IF NOT EXISTS cold.transaction_log (
        seq_id      INTEGER PRIMARY KEY,
        table_name  TEXT NOT NULL,
        entity_id   TEXT NOT NULL,
        operation   TEXT NOT NULL,
        payload     TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        branch_id   TEXT NOT NULL DEFAULT 'main'
    )"#,
    "CREATE INDEX IF NOT EXISTS cold.idx_cold_txlog_entity ON transaction_log (entity_id)",
    "CREATE INDEX IF NOT EXISTS cold.idx_cold_txlog_time ON transaction_log (recorded_at)",
    // Lineages, as of 0.14.13 (§15.4, D-230). The one cold table that is not a
    // mirror of a hot one: `branches` carries no `archived_at` and this needs
    // one, because the hot row's `created_at` says when the lineage began and
    // nothing on it can say when the ledger stopped knowing about it.
    //
    // **This is the table `upgrade_cold_lineage` predicted.** Its note says a
    // cold row records *that* it belonged to a lineage without recording what
    // that lineage was, and that "the abandonment arm makes forgetting a branch
    // an ordinary operation, and a cold row stamped with a name nothing
    // resolves is the shape that falls out of it". This is what resolves the
    // name: the cold file carries the lineage record itself, so
    // `cold.links.branch_id` names a row in the same file rather than a string
    // whose meaning was left behind in the hot database.
    //
    // FK-free like the rest of the cold schema, `parent_id` included — the
    // parent is normally still hot, which is the whole point of the operation,
    // so a self-referencing key here would refuse every row this table exists
    // to hold.
    r#"CREATE TABLE IF NOT EXISTS cold.branches (
        branch_id   TEXT NOT NULL PRIMARY KEY,
        parent_id   TEXT,
        forked_at   TEXT,
        created_at  TEXT NOT NULL,
        archived_at TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS cold.archive_horizon (
        archived_at TEXT NOT NULL,
        cutoff      TEXT NOT NULL,
        horizon     INTEGER
    )"#,
];

/// A links assertion is archivable when it is older than the cutoff AND it is
/// either superseded by a later assertion **of its own lineage** for the same
/// interval key, or it is the current belief for an interval that closed before
/// the cutoff.
///
/// This keeps every row that `links_current` still projects (Doctrine VI: the
/// materialization must stay rebuildable from `links`) while moving exactly the
/// "closed intervals, superseded history" the §2 diagram assigns to the cold file.
///
/// # `newer.branch_id = links.branch_id`, added at 0.14.12 ([D-229])
///
/// Without it this predicate archived rows the ledger still believed. `links_current`
/// is keyed by `(source, target, type, valid_from, branch_id)` and the four folds in
/// `temporal::replay` partition by `(table_name, entity_id, branch_id)`, but a link's
/// `entity_id` is `source|target|type|valid_from` and carries **no lineage**
/// ([`crate::schema::ddl::CREATE_LINKS_LOG_INSERT`] says why re-keying it was
/// refused). So "a later assertion for the same interval key" matched **across**
/// lineages, and a branch asserting at an ancestor's key made the ancestor's own
/// open, current row look superseded.
///
/// Measured before the repair, on a two-row fixture: the trunk asserts `a → b`, a
/// branch forks and asserts at the same key, one `archive` runs, and the **trunk**
/// stops reaching `b`. `audit_current` reports **0**, which is why nothing caught
/// it — `links_current` is honestly re-derived from a `links` table that has been
/// wrongly pruned, so the projection is correct with respect to what survives and
/// the drift check has nothing to compare against. Doctrine VI's audit answers
/// "is the projection the image of the ledger", never "is the ledger complete".
///
/// **Exact-branch equality, not ancestry**, and for
/// [`crate::schema::ddl::CREATE_CONCEPTS_GUARD_LINEAGE`]'s reason. A descendant's
/// row shadows an ancestor's *for the descendant's reads*; the ancestor still
/// believes its own row, and Doctrine III is precisely that shadowing never
/// touches it. A predicate that let a descendant supersede an ancestor would
/// archive the parent's belief because a child disagreed.
///
/// [D-229]: ../../docs/architecture/s13-decision-register.md#d-229
///
/// # The closed-interval arm, and the row it must not take
///
/// "A closed interval is history" is true of a lineage that holds the only row
/// at its key, and false of a **shadow**. A branch retires an inherited edge by
/// writing its own closed row at the ancestor's key — the only cross-lineage
/// retirement [Doctrine III] permits, because it never touches the parent's row.
/// Archiving that row does not send history cold; it removes the branch's
/// disbelief and lets the ancestor's open row win the resolution again.
///
/// Measured before the repair: a branch retires `b → c` over `[EPOCH, T1)`, one
/// archive runs, and at `T2` the branch reaches `c` — an edge it had stopped
/// believing, restored by a maintenance operation that mints no assertions. That
/// is the resurrection [`crate::schema::ddl::CREATE_CONCEPTS_LOG_INSERT`] gates
/// the rehydration insert against, reached down the other path.
///
/// So the arm stands down whenever **another lineage holds a hot row at the same
/// interval key**. Conservative rather than exact: what strictly matters is an
/// *ancestor's* row surviving this session, and both halves of that are more than
/// this predicate can see. Ancestry would mean resolving `graph::lineage`'s chain
/// for every branch, in a whole-database operation that takes no branch
/// parameter; "surviving this session" is self-referential, since what survives
/// is the answer this predicate is computing. Leaving rows hot costs file size
/// and is never wrong, so the rule is the one that needs neither. A key held by
/// exactly one lineage — every key on a ledger that has never forked — is
/// unaffected, which the tests measure rather than argue from a column default.
///
/// [Doctrine III]: ../../docs/architecture/README.md
const LINKS_ARCHIVABLE: &str = r#"
    recorded_at < :cutoff AND (
        EXISTS (
            SELECT 1 FROM links newer
            WHERE newer.source_id   = links.source_id
              AND newer.target_id   = links.target_id
              AND newer.edge_type   = links.edge_type
              AND newer.valid_from  = links.valid_from
              AND newer.branch_id   = links.branch_id
              AND newer.recorded_at > links.recorded_at
        )
        OR (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= :cutoff
            AND NOT EXISTS (
                SELECT 1 FROM links other
                WHERE other.source_id  = links.source_id
                  AND other.target_id  = links.target_id
                  AND other.edge_type  = links.edge_type
                  AND other.valid_from = links.valid_from
                  AND other.branch_id <> links.branch_id
            ))
    )
"#;

/// A log entry is archivable when it is older than the cutoff and a later entry
/// exists **for the same entity on the same lineage**, i.e. it is superseded.
/// The newest entry per fold partition always stays hot so that
/// `reconstruct(now)` never needs the cold file.
///
/// # The lineage clause, added at 0.14.12 ([D-229])
///
/// The sentence above used to say "per entity", and the four folds in
/// `temporal::replay` have partitioned by `(table_name, entity_id, branch_id)`
/// since v12 — so the predicate stopped keeping the newest entry per *partition*
/// hot the moment lineage arrived, and nothing said so. A branch writing at an
/// ancestor's edge key made the ancestor's newest entry archivable, which is the
/// same defect [`LINKS_ARCHIVABLE`] carried, reached from the log side.
///
/// It changes nothing for `concepts` entries and that is worth stating rather
/// than leaving to be rediscovered: a concept's `entity_id` is its `id`, and
/// [`crate::schema::ddl::CREATE_CONCEPTS_GUARD_LINEAGE`] refuses a branch
/// restating an inherited one, so every log entry for one concept already
/// carries one lineage. The clause is a no-op there by construction, not by
/// accident.
///
/// [D-229]: ../../docs/architecture/s13-decision-register.md#d-229
const LOG_ARCHIVABLE: &str = r#"
    recorded_at < :cutoff AND EXISTS (
        SELECT 1 FROM transaction_log newer
        WHERE newer.entity_id = transaction_log.entity_id
          AND newer.branch_id = transaction_log.branch_id
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

/// Bring an existing cold file up to the v12 shape, inside the session's own
/// transaction (§15.2, D-217).
///
/// # Why this is not `CREATE TABLE IF NOT EXISTS`'s job
///
/// It cannot be. [`COLD_SCHEMA`] runs against a file that may already hold
/// these tables, and `IF NOT EXISTS` on an existing name **keeps the old
/// definition and reports success** — probe §10. A v11 cold file would sail
/// through the schema pass and then refuse the first insert with `table
/// cold.transaction_log has no column named branch_id` (probe §11), which is at
/// least loud; shorten the column list to avoid the error and the lineage is
/// dropped in silence instead.
///
/// # Why it is safe to do here
///
/// Probe §12–13 measured both halves on libSQL: `ALTER TABLE cold.… ADD COLUMN`
/// is accepted inside `BEGIN IMMEDIATE`, an insert in the same transaction sees
/// the new column, and **`ROLLBACK` takes the DDL with it** — columns and rows
/// both revert. So a session that fails partway leaves the cold file exactly as
/// it found it, which is the property that lets an upgrade ride along with an
/// archive instead of needing a migration of its own.
///
/// Detection is column presence. A cold file carries no version stamp worth
/// trusting: it is a file whose whole purpose is to be moved (D-026).
///
/// No foreign key on these columns, unlike their hot counterparts. `branches`
/// does not exist in the cold file, and a cold file therefore records *that* a
/// row belonged to a lineage without recording what that lineage was. Named in
/// §15.5's carry rather than left to be discovered: the abandonment arm makes
/// forgetting a branch an ordinary operation, and a cold row stamped with a
/// name nothing resolves is the shape that falls out of it.
async fn upgrade_cold_lineage(tx: &libsql::Transaction) -> Result<()> {
    for table in ["links", "concepts", "transaction_log"] {
        if !cold_has_branch(tx, table).await? {
            tx.execute(
                &format!(
                    "ALTER TABLE cold.{table} ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'"
                ),
                (),
            )
            .await?;
        }
    }

    // The column is not the whole of v15. A cold file written by 0.14.8 through
    // 0.14.14 has `branch_id` and a key that does not mention it, so it passes
    // the loop above and still refuses the pair the hot table now accepts —
    // which would make `archive` the operation that fails on a database nothing
    // else complains about.
    //
    // A rebuild rather than an `ALTER`, because SQLite has no way to add a
    // column to a primary key; the same reason the hot rung is a rebuild. It is
    // safe in this transaction for the reason above: probe §12–13 established
    // that `ROLLBACK` takes cold DDL with it, and this adds `CREATE`, `INSERT
    // … SELECT`, `DROP` and `RENAME` to the `ADD COLUMN` already covered.
    // `cold.links` carries no trigger and no index, so nothing else has to be
    // put back.
    if !cold_links_keyed_by_lineage(tx).await? {
        tx.execute(COLD_LINKS_V15, ()).await?;
        tx.execute(
            "INSERT INTO cold.links_v15 \
             (source_id, target_id, edge_type, valid_from, recorded_at, \
              valid_to, weight, properties, branch_id) \
             SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                    valid_to, weight, properties, branch_id FROM cold.links",
            (),
        )
        .await?;
        tx.execute("DROP TABLE cold.links", ()).await?;
        tx.execute("ALTER TABLE cold.links_v15 RENAME TO links", ())
            .await?;
    }

    Ok(())
}

/// The v15 cold ledger, spelled out because the rebuild needs a second name.
///
/// Not derived from [`COLD_SCHEMA`] by string surgery: the two would then be
/// one definition read two ways, and the failure mode of getting that wrong is
/// a cold file silently rebuilt into a shape the schema pass does not declare.
const COLD_LINKS_V15: &str = r#"CREATE TABLE cold.links_v15 (
        source_id   TEXT NOT NULL,
        target_id   TEXT NOT NULL,
        edge_type   TEXT NOT NULL,
        valid_from  TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        valid_to    TEXT NOT NULL,
        weight      REAL NOT NULL CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
        properties  TEXT NOT NULL,
        branch_id   TEXT NOT NULL DEFAULT 'main',
        PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at, branch_id)
    )"#;

/// Whether `cold.links` has `branch_id` **in its primary key**.
///
/// `PRAGMA table_info`'s sixth column is the column's 1-based position in the
/// key, or 0. Asked of the pragma rather than of the stored SQL for
/// [`cold_has_branch`]'s reason — a cold file is a file this crate may not have
/// written, and matching its text would be matching someone else's formatting.
async fn cold_links_keyed_by_lineage(conn: &libsql::Connection) -> Result<bool> {
    let mut rows = conn.query("PRAGMA cold.table_info(links)", ()).await?;
    while let Some(row) = rows.next().await? {
        let named = row.get::<String>(1).is_ok_and(|name| name == "branch_id");
        if named && row.get::<i64>(5).is_ok_and(|pk| pk > 0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one cold table already carries `branch_id`.
///
/// Split out because rehydration asks the same question for the opposite
/// reason: the writer asks so it can upgrade, the reader asks so it can
/// **avoid** upgrading. A cold file may be read-only media or sit on a share,
/// and a read path that mutates it is a new failure class.
async fn cold_has_branch(conn: &libsql::Connection, table: &str) -> Result<bool> {
    let mut rows = conn
        .query(&format!("PRAGMA cold.table_info({table})"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        if row.get::<String>(1).is_ok_and(|name| name == "branch_id") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The temp table the keyed repair reads, and the statement that fills it.
///
/// Temp rather than a `WITH`: it has to be read **after** the `DELETE` that
/// makes the rows it names disappear, so the key set must be materialised
/// before then. It lives in the connection's `temp` database, which the
/// archive's own `BEGIN IMMEDIATE` covers, and is dropped before the session
/// ends so a second archive on the same connection starts from nothing.
const ARCHIVED_KEYS: &str = "archived_keys";

/// Collect the keys a `DELETE FROM links WHERE {clause}` is about to disturb.
///
/// Run before the delete, in its transaction, with the delete's own parameters:
/// the two statements must see the same rows, and the only way to be sure of
/// that is to give them the same predicate rather than a description of it.
async fn collect_archived_keys(
    tx: &libsql::Transaction,
    clause: &str,
    params: impl libsql::params::IntoParams,
) -> Result<()> {
    tx.execute(&format!("DROP TABLE IF EXISTS temp.{ARCHIVED_KEYS}"), ())
        .await?;
    tx.execute(
        &format!(
            "CREATE TEMP TABLE {ARCHIVED_KEYS} AS \
             SELECT DISTINCT {key} FROM links WHERE {clause}",
            key = crate::integrity::rebuild::PROJECTION_KEY
        ),
        params,
    )
    .await?;
    Ok(())
}

/// Re-derive the projection at the collected keys, then drop the key table.
///
/// Called only when the `DELETE` removed something, for
/// [D-080](../../docs/architecture/s13-decision-register.md#d-080)'s reason:
/// `links_current` is a function of `links`, so a delete that removed nothing
/// left nothing to repair. What changed at 0.15.3 is what "repair" costs —
/// O(keys the session archived) rather than O(every link that survived it).
async fn repair_archived_keys(tx: &libsql::Transaction) -> Result<()> {
    crate::integrity::rebuild::repair_keys_within(tx, ARCHIVED_KEYS).await?;
    tx.execute(&format!("DROP TABLE temp.{ARCHIVED_KEYS}"), ())
        .await?;
    Ok(())
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

    // Before the marker, before any insert: an existing cold file may predate
    // the lineage column, and `IF NOT EXISTS` above will not have added it.
    upgrade_cold_lineage(&tx).await?;

    // --- archive session opens: the delete guards are now satisfied ---
    tx.execute(&format!("CREATE TABLE {ARCHIVE_SESSION_MARKER} (x)"), ())
        .await?;

    let links_archived = tx
        .execute(
            &format!(
                "INSERT OR IGNORE INTO cold.links
                     (source_id, target_id, edge_type, valid_from, recorded_at,
                      valid_to, weight, properties, branch_id)
                 SELECT source_id, target_id, edge_type, valid_from, recorded_at,
                        valid_to, weight, properties, branch_id
                 FROM links WHERE {LINKS_ARCHIVABLE}"
            ),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await? as usize;

    // The keys this session is about to disturb, taken with the delete's own
    // predicate and before the delete runs. See `collect_archived_keys`.
    collect_archived_keys(
        &tx,
        LINKS_ARCHIVABLE,
        libsql::named_params! {":cutoff": cutoff},
    )
    .await?;

    let links_deleted = delete_guarded(
        &tx,
        conn,
        &format!("DELETE FROM links WHERE {LINKS_ARCHIVABLE}"),
        libsql::named_params! {":cutoff": cutoff},
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
    //
    // **And the repair is keyed since 0.15.3 (D-245).** The skip above bounded
    // *how often* the full reprojection ran; it could not bound what one costs,
    // and a session that archives ten rows from a million-row ledger still paid
    // for the million. The projection is pointwise in the key, so re-deriving
    // at the disturbed keys is the same answer — the reasoning is in
    // `repair_keys_within`, and `audit_current` is what checks it.
    if links_deleted > 0 {
        repair_archived_keys(&tx).await?;
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
                     (seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id)
                 SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id
                 FROM transaction_log WHERE {LOG_ARCHIVABLE}"
            ),
            libsql::named_params! {":cutoff": cutoff},
        )
        .await? as usize;

    delete_guarded(
        &tx,
        conn,
        &format!("DELETE FROM transaction_log WHERE {LOG_ARCHIVABLE}"),
        libsql::named_params! {":cutoff": cutoff},
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
                      valid_from, valid_to, recorded_at, retired, branch_id)
                 SELECT rowid_pk, id, title, content, embedding_model,
                        valid_from, valid_to, recorded_at, retired, branch_id
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
        libsql::named_params! {":cutoff": cutoff},
        "concepts",
    )
    .await? as usize;

    debug_assert_eq!(
        moved, deleted,
        "the predicate selected a different set for the copy than for the delete"
    );

    Ok(deleted)
}

/// Move one lineage's whole ledger to the cold file and forget the lineage
/// (0.14.13, §15.4, [D-230]).
///
/// The abandonment arm §15.4 asks for. A conversation tree discards most of what
/// it grows, and until now the only way to reclaim an abandoned branch's space
/// was [`archive`], which is indexed by *time* and therefore takes the trunk's
/// old history along with it — or leaves the branch's recent history behind,
/// which is the usual case and the reason the arm exists.
///
/// # It is all-or-nothing, and that was forced rather than chosen
///
/// The road map's justification was that "an abandoned branch's rows are a
/// contiguous archivable set by construction, which is the cheapest archive
/// predicate in the crate". `branch_id = :branch` really is the cheapest
/// predicate in the crate. **Contiguous by construction is false in both of its
/// senses**, and each refutation moved this design:
///
/// 1. *Not closed under `concepts(id)`.* `concepts` is keyed by identity
///    globally ([D-214]), so a concept minted on a branch may be named by a
///    trunk edge or a sibling's edge — measured by probe, both succeed. The set
///    is therefore not FK-closed, and the refusal below is the direct
///    expression of that: a lineage another lineage's hot edges still depend on
///    is not abandoned, whatever its author believes.
/// 2. *Not a prefix of the log.* A branch's `transaction_log` rows are
///    scattered through the sequence, exactly as `LOG_ARCHIVABLE`'s are, which
///    is what [`crate::temporal::replay`]'s reach test was rewritten for in
///    0.5.5.
///
/// What follows is a chain with no branch points. The links must go — that is
/// the operation. If the links go and the log stays, `reconstruct(now)` folds
/// the log, yields the branch's open edges, and disagrees with `links_current`
/// about present belief; so the log must go too. But `hot_log_reach`'s
/// soundness rests on **the newest row per entity is never archivable**, which
/// is true of a predicate needing a later row to exist and false of one that
/// takes a whole lineage; so the `branches` row must go as well, which is what
/// makes a hot fold that omits the lineage *correct rather than silently
/// short*. Every read and write naming the name then raises
/// [`DbError::UnknownBranch`] — a refusal, which a caller can act on, in place
/// of an answer that is quietly missing rows.
///
/// The `branches` row moving is why v13 exists: `trg_branches_frozen_delete`
/// was unconditional, on a docstring that said no session could ever legally
/// remove a lineage record. See
/// [`crate::schema::ddl::CREATE_BRANCHES_GUARD_DELETE`].
///
/// # What it refuses
///
/// * **The trunk.** Every lineage's `parent_id` chain ends there and every
///   default `branch_id` names it; there is no ledger left after it goes.
/// * **A name that is not registered** — [`DbError::UnknownBranch`], the same
///   answer every other branch-taking surface gives, rather than a silent
///   success archiving nothing.
/// * **A branch with descendants.** A child reads through its parent, so
///   archiving the parent would delete rows the child still believes — the same
///   loss [D-229] repaired in the time-indexed predicates, arrived at from the
///   other direction.
/// * **A branch whose concepts another lineage's hot link names.** This is
///   refutation 1 above, and the refusal is what makes the post-condition
///   uniform: after this returns `Ok`, nothing hot names the lineage and
///   nothing hot names anything it minted.
///
/// All four are checked **inside the session transaction**, before the marker
/// is created, so a concurrent fork cannot slip a descendant in between the
/// check and the delete.
///
/// # No `archive_horizon` row, deliberately
///
/// That table records a **cutoff** and the horizon it produced. This session
/// has no cutoff — its boundary is a lineage, not an instant — and writing
/// `archived_at` into the `cutoff` column would be the Wave 4.5 defect the
/// column's own comment describes, committed a second time on purpose. What
/// there is to record is recorded better: `cold.branches` carries the lineage
/// and when it was forgotten, and the horizon itself is still readable from the
/// hot log, which is where `archive_hint` reads it from anyway.
///
/// [D-230]: ../../docs/architecture/s13-decision-register.md#d-230
/// [D-229]: ../../docs/architecture/s13-decision-register.md#d-229
/// [D-214]: ../../docs/architecture/s13-decision-register.md#d-214
pub async fn archive_branch(
    conn: &libsql::Connection,
    branch: &str,
    archived_at: &str,
    archive_path: &Path,
) -> Result<ArchiveReport> {
    crate::temporal::replay::detach_stale_cold(conn).await;

    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive_path.to_string_lossy().as_ref()],
    )
    .await?;

    let result = archive_branch_session(conn, branch, archived_at).await;

    // Unconditional, for [`archive`]'s reason: a live `cold` handle makes every
    // later archive and cold reconstruct fail with "database cold is already in
    // use", and the refusals above are the *expected* way out of this function.
    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("archive_branch: failed to DETACH cold database: {e}");
    }

    result
}

fn not_archivable(branch: &str, reason: impl Into<String>) -> DbError {
    DbError::BranchNotArchivable {
        branch: branch.to_string(),
        reason: reason.into(),
    }
}

/// Whether `sql` — a `SELECT 1 … WHERE … = :branch` — matches anything.
async fn any_row(tx: &libsql::Transaction, sql: &str, branch: &str) -> Result<bool> {
    Ok(tx
        .query(sql, libsql::named_params! {":branch": branch})
        .await?
        .next()
        .await?
        .is_some())
}

/// The four refusals, in the order that makes the message most specific.
///
/// Order is not cosmetic. The trunk check comes first because `main` is
/// registered and childless on a ledger that has never forked, so every later
/// check would pass it. Registration comes next, because "not registered" is a
/// better answer than "has no descendants" for a typo. Descendants before
/// concepts because it is the cheaper query and the commoner mistake.
async fn refuse_unarchivable_branch(tx: &libsql::Transaction, branch: &str) -> Result<()> {
    if branch == crate::schema::ddl::MAIN_BRANCH {
        return Err(not_archivable(
            branch,
            "it is the trunk: every lineage's parent chain ends there and every \
             default branch_id names it, so there is no ledger left after it goes",
        ));
    }

    if !any_row(
        tx,
        "SELECT 1 FROM branches WHERE branch_id = :branch",
        branch,
    )
    .await?
    {
        return Err(DbError::UnknownBranch(branch.to_string()));
    }

    if any_row(
        tx,
        "SELECT 1 FROM branches WHERE parent_id = :branch",
        branch,
    )
    .await?
    {
        return Err(not_archivable(
            branch,
            "it has descendants, which read through it: archiving it would delete \
             rows they still believe. Archive the descendants first",
        ));
    }

    // The refutation of "contiguous by construction", as a query. A concept is
    // keyed by identity across the whole ledger (D-214), so an edge on any
    // lineage may name one minted here.
    let mut rows = tx
        .query(
            "SELECT c.id FROM concepts c
             WHERE c.branch_id = :branch
               AND EXISTS (
                   SELECT 1 FROM links l
                   WHERE l.branch_id <> :branch
                     AND (l.source_id = c.id OR l.target_id = c.id)
               )
             LIMIT 1",
            libsql::named_params! {":branch": branch},
        )
        .await?;
    if let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        return Err(not_archivable(
            branch,
            format!(
                "concept {id} was minted here and a hot edge on another lineage \
                 names it. A lineage other lineages still depend on is not \
                 abandoned; retire those edges first"
            ),
        ));
    }

    Ok(())
}

/// `conn` is passed alongside `tx` for [`delete_guarded`]'s sake, exactly as in
/// [`archive_session`]. Both name the same connection.
async fn archive_branch_session(
    conn: &libsql::Connection,
    branch: &str,
    archived_at: &str,
) -> Result<ArchiveReport> {
    for ddl in COLD_SCHEMA {
        conn.execute(ddl, ()).await?;
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;

    upgrade_cold_lineage(&tx).await?;

    // Before the marker: a refusal must not be able to leave the guards
    // disarmed, and these are reads, which need no session.
    refuse_unarchivable_branch(&tx, branch).await?;

    // --- archive session opens: the delete guards are now satisfied ---
    tx.execute(&format!("CREATE TABLE {ARCHIVE_SESSION_MARKER} (x)"), ())
        .await?;

    let links_archived = tx
        .execute(
            "INSERT OR IGNORE INTO cold.links
                 (source_id, target_id, edge_type, valid_from, recorded_at,
                  valid_to, weight, properties, branch_id)
             SELECT source_id, target_id, edge_type, valid_from, recorded_at,
                    valid_to, weight, properties, branch_id
             FROM links WHERE branch_id = :branch",
            libsql::named_params! {":branch": branch},
        )
        .await? as usize;

    collect_archived_keys(
        &tx,
        "branch_id = :branch",
        libsql::named_params! {":branch": branch},
    )
    .await?;

    let links_deleted = delete_guarded(
        &tx,
        conn,
        "DELETE FROM links WHERE branch_id = :branch",
        libsql::named_params! {":branch": branch},
        "links",
    )
    .await?;

    // Doctrine VI, and [`archive_session`]'s reasoning verbatim: `links_current`
    // is a function of `links`, so it is re-derived rather than described, and
    // only when `links` actually changed. It must also happen **before** the
    // `branches` row goes: `links_current.branch_id` carries the same foreign
    // key its three siblings do, so the projection has to have stopped naming
    // the lineage before the lineage can leave.
    //
    // Keyed since 0.15.3 (D-245), and here the key set is every key the lineage
    // held — which is the whole of what it wrote and *not* the whole of
    // `links`, so a branch archived out of a large trunk stops paying for the
    // trunk.
    if links_deleted > 0 {
        repair_archived_keys(&tx).await?;
    }

    // Concepts after links, for [D-128]'s reason turned around: there it was
    // that a concept is archivable only once nothing hot names it, so the edges
    // must go first. Here the same ordering is a foreign key — `links.source_id`
    // and `links.target_id` reference `concepts(id)`, and this lineage's own
    // edges are the ones that would refuse the delete.
    //
    // [D-128]: ../../docs/architecture/s13-decision-register.md#d-128
    let concepts_archived = archive_branch_concepts(&tx, conn, branch).await?;

    let log_entries_archived = tx
        .execute(
            "INSERT OR IGNORE INTO cold.transaction_log
                 (seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id)
             SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id
             FROM transaction_log WHERE branch_id = :branch",
            libsql::named_params! {":branch": branch},
        )
        .await? as usize;

    delete_guarded(
        &tx,
        conn,
        "DELETE FROM transaction_log WHERE branch_id = :branch",
        libsql::named_params! {":branch": branch},
        "transaction_log",
    )
    .await?;

    // Last, because the other three tables' `branch_id` all reference it. The
    // `archived_at` is the session's wall clock, not a ledger fact: nothing was
    // asserted or retired here, and Doctrine III would refuse it if it were.
    tx.execute(
        "INSERT OR IGNORE INTO cold.branches
             (branch_id, parent_id, forked_at, created_at, archived_at)
         SELECT branch_id, parent_id, forked_at, created_at, ?2
         FROM branches WHERE branch_id = ?1",
        libsql::params![branch, archived_at],
    )
    .await?;

    delete_guarded(
        &tx,
        conn,
        "DELETE FROM branches WHERE branch_id = :branch",
        libsql::named_params! {":branch": branch},
        "branches",
    )
    .await?;

    let horizon: Option<i64> = tx
        .query("SELECT MIN(seq_id) FROM transaction_log", ())
        .await?
        .next()
        .await?
        .and_then(|row| row.get(0).ok());

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

/// [`archive_concepts`] with the lineage predicate in place of the cutoff.
///
/// A separate function rather than a parameter on that one, because the two
/// share their *shape* and not their argument: `CONCEPTS_ARCHIVABLE` is a
/// standing predicate about retirement and reference counts, and this is a
/// lineage. The partition it encodes is the same and is the reason both exist —
/// **entity data crosses, derivative data does not** — and the derived rows are
/// deleted here for the same two reasons: Doctrine VII makes them recomputable,
/// and `concepts`' inbound foreign keys would refuse the delete otherwise.
///
/// No guard on "is anything still referencing this concept": the caller has
/// already refused the branch if another lineage's hot link names one of its
/// concepts, and this lineage's own links went cold a few statements ago.
async fn archive_branch_concepts(
    tx: &libsql::Transaction,
    conn: &libsql::Connection,
    branch: &str,
) -> Result<usize> {
    let moved = tx
        .execute(
            "INSERT OR IGNORE INTO cold.concepts
                 (rowid_pk, id, title, content, embedding_model,
                  valid_from, valid_to, recorded_at, retired, branch_id)
             SELECT rowid_pk, id, title, content, embedding_model,
                    valid_from, valid_to, recorded_at, retired, branch_id
             FROM concepts WHERE branch_id = :branch",
            libsql::named_params! {":branch": branch},
        )
        .await? as usize;

    if moved == 0 {
        return Ok(0);
    }

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
                 (SELECT id FROM concepts WHERE branch_id = :branch)"
            ),
            libsql::named_params! {":branch": branch},
        )
        .await?;
    }

    let deleted = delete_guarded(
        tx,
        conn,
        "DELETE FROM concepts WHERE branch_id = :branch",
        libsql::named_params! {":branch": branch},
        "concepts",
    )
    .await? as usize;

    debug_assert_eq!(
        moved, deleted,
        "the lineage selected a different set for the copy than for the delete"
    );

    Ok(deleted)
}

/// Outcome of one rehydration (0.9.0, C3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehydrateReport {
    /// Concepts moved back into the hot table.
    pub concepts_rehydrated: usize,
    /// Of those, how many could **not** keep their original `rowid_pk` because
    /// something else had claimed it while they were cold, and were reassigned
    /// with the FTS index re-pointed to match.
    ///
    /// Reported rather than hidden because it is the one way a rehydrated
    /// concept differs from the row that was archived, and a caller comparing
    /// rowids across the boundary should be able to see that it happened.
    pub rowids_reassigned: usize,
}

/// Move concepts back from the cold file into the hot tables (§2.3, C3).
///
/// # Rehydration is a move back, not a write
///
/// It mints no transaction-time facts and is invisible to both clocks. The
/// concept's log entries were never removed, so the ledger already says
/// everything true about it; writing a fresh `'I'` would assert the concept was
/// *learned* at rehydration time, and — because the fold takes the highest
/// `seq_id` per entity — would additionally outrank any later `'U'` that retired
/// it. See [`crate::schema::ddl::CREATE_CONCEPTS_LOG_INSERT`], which is
/// marker-gated at v10 for exactly this reason. The whole operation therefore
/// runs inside a declared archive session, which is what suppresses the trigger.
///
/// # `rowid_pk`: reinstate, or reassign and re-point the index
///
/// The common case has no collision — the rowid was freed by archival and
/// nothing has claimed it since — and reinstating is the clean move-back with no
/// side effects at all. When something *has* taken it, the fallback is a fresh
/// `rowid_pk` plus an FTS correction: `concepts_fts` is external-content keyed
/// on `rowid_pk` ([D-119]), so a reassignment without re-pointing leaves the
/// index describing the wrong row, silently. Both exits are taken here rather
/// than one being assumed, and [`RehydrateReport::rowids_reassigned`] reports
/// which was used.
pub async fn rehydrate(
    conn: &libsql::Connection,
    ids: &[&str],
    archive_path: &Path,
) -> Result<RehydrateReport> {
    if ids.is_empty() {
        return Ok(RehydrateReport {
            concepts_rehydrated: 0,
            rowids_reassigned: 0,
        });
    }

    crate::temporal::replay::detach_stale_cold(conn).await;
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive_path.to_string_lossy().as_ref()],
    )
    .await?;

    let result = rehydrate_session(conn, ids).await;

    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("rehydrate: failed to DETACH cold database: {e}");
    }
    result
}

async fn rehydrate_session(conn: &libsql::Connection, ids: &[&str]) -> Result<RehydrateReport> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;

    // The session opens for the same reason the archive's does, plus one more:
    // it is what stops `trg_concepts_log_insert` from firing (v10).
    tx.execute(&format!("CREATE TABLE {ARCHIVE_SESSION_MARKER} (x)"), ())
        .await?;

    // Asked once, and never acted on. A cold file that predates v12 is read
    // through a literal — `'main'` is what those rows *were*, since they were
    // written when only the trunk existed — and left exactly as it was found.
    // The archive writer upgrades cold files; the reader must not, because a
    // cold file can be read-only media or sit on a share, and a read path that
    // mutates one is a new failure class (D-026, §15.2).
    let lineage = if cold_has_branch(&tx, "concepts").await? {
        "branch_id"
    } else {
        "'main' AS branch_id"
    };

    let mut rehydrated = 0usize;
    let mut reassigned = 0usize;

    for id in ids {
        let Some(row) = tx
            .query(
                &format!(
                    "SELECT rowid_pk, id, title, content, embedding_model, \
                     valid_from, valid_to, recorded_at, retired, {lineage} \
                     FROM cold.concepts WHERE id = ?1"
                ),
                libsql::params![*id],
            )
            .await?
            .next()
            .await?
        else {
            continue;
        };

        let old_rowid: i64 = row.get(0)?;
        let title: String = row.get(2)?;
        let content: String = row.get(3)?;
        let model: Option<String> = row.get(4)?;
        let valid_from: String = row.get(5)?;
        let valid_to: String = row.get(6)?;
        let recorded_at: String = row.get(7)?;
        let retired: i64 = row.get(8)?;
        let branch_id: String = row.get(9)?;

        let taken: i64 = tx
            .query(
                "SELECT COUNT(*) FROM concepts WHERE rowid_pk = ?1",
                libsql::params![old_rowid],
            )
            .await?
            .next()
            .await?
            .expect("COUNT(*) always returns a row")
            .get(0)?;

        if taken == 0 {
            // The clean move back: same row, same rowid, no side effects.
            tx.execute(
                "INSERT INTO concepts (rowid_pk, id, title, content, embedding_model, \
                 valid_from, valid_to, recorded_at, retired, branch_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                libsql::params![
                    old_rowid,
                    *id,
                    title.clone(),
                    content.clone(),
                    model,
                    valid_from,
                    valid_to,
                    recorded_at,
                    retired,
                    branch_id
                ],
            )
            .await?;
        } else {
            // Something claimed the rowid while this concept was cold. Take a
            // fresh one, then correct the index: `concepts_fts` is
            // external-content keyed on `rowid_pk`, and its insert trigger will
            // have written an entry at the *new* rowid — what has to be undone
            // is the stale entry still sitting at the old one, which the archive
            // could not remove because the row it described had already gone.
            tx.execute(
                "INSERT INTO concepts (id, title, content, embedding_model, \
                 valid_from, valid_to, recorded_at, retired, branch_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                libsql::params![
                    *id,
                    title.clone(),
                    content.clone(),
                    model,
                    valid_from,
                    valid_to,
                    recorded_at,
                    retired,
                    branch_id
                ],
            )
            .await?;
            tx.execute(
                "INSERT INTO concepts_fts (concepts_fts, rowid, title, content) \
                 VALUES ('delete', ?1, ?2, ?3)",
                libsql::params![old_rowid, title, content],
            )
            .await?;
            reassigned += 1;
        }

        tx.execute(
            "DELETE FROM cold.concepts WHERE id = ?1",
            libsql::params![*id],
        )
        .await?;
        rehydrated += 1;
    }

    tx.execute(&format!("DROP TABLE {ARCHIVE_SESSION_MARKER}"), ())
        .await?;
    tx.commit().await?;

    Ok(RehydrateReport {
        concepts_rehydrated: rehydrated,
        rowids_reassigned: reassigned,
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
    params: impl libsql::params::IntoParams,
    table: &str,
) -> Result<u64> {
    match tx.execute(sql, params).await {
        Ok(n) => Ok(n),
        Err(e) => Err(crate::error::classify(conn, e, WriteOp::Delete { table }).await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
    const OPEN: &str = "9999-12-31T23:59:59.999999Z";
    const CLOSED: &str = "1970-01-01T00:30:00.000000Z";
    const CUTOFF: &str = "1970-01-01T02:00:00.000000Z";

    async fn seeded() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        for id in ["a", "b", "c", "e"] {
            conn.execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) \
                 VALUES (?1, 'n', ?2, ?2)",
                libsql::params![id, EPOCH],
            )
            .await
            .unwrap();
        }
        // `a → b` twice, so the older row is superseded and archivable;
        // `a → c` closed before the cutoff, so the second arm takes it;
        // `a → e` open and never superseded, so nothing can touch it.
        for (target, valid_to, recorded_at) in [
            ("b", OPEN, EPOCH),
            ("b", OPEN, "1970-01-01T01:00:00.000000Z"),
            ("c", CLOSED, EPOCH),
            ("e", OPEN, EPOCH),
        ] {
            conn.execute(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
                 weight, properties, recorded_at) VALUES ('a', ?1, 'LINKS', ?2, ?3, 1.0, '{}', ?4)",
                libsql::params![target, EPOCH, valid_to, recorded_at],
            )
            .await
            .unwrap();
        }
        conn
    }

    /// **The key set is the keys the delete will disturb, and no others**
    /// (0.15.3, [D-245](../../docs/architecture/s13-decision-register.md#d-245)).
    ///
    /// A key set that is too wide leaves the projection *correct* — it
    /// re-derives untouched partitions to the rows they already held — so
    /// every equality test in `archive_projection_tests` passes with it, and
    /// the whole point of the release does not. This is the assertion those
    /// tests cannot make: what the repair is allowed to look at. `a → e` is
    /// the row that must not appear, and the count is pinned as well, because
    /// a key set that is too *narrow* is a correctness bug the equality tests
    /// would catch but this one names.
    #[tokio::test]
    async fn the_collected_keys_are_only_the_ones_the_delete_disturbs() {
        let conn = seeded().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        collect_archived_keys(
            &tx,
            LINKS_ARCHIVABLE,
            libsql::named_params! {":cutoff": CUTOFF},
        )
        .await
        .unwrap();

        let mut rows = tx
            .query(
                &format!("SELECT target_id FROM {ARCHIVED_KEYS} ORDER BY target_id"),
                (),
            )
            .await
            .unwrap();
        let mut targets = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            targets.push(row.get::<String>(0).unwrap());
        }

        assert_eq!(
            targets,
            vec!["b".to_string(), "c".to_string()],
            "a → e is untouched by this cutoff and the repair has no business \
             re-deriving it; a key set this wide is the full rebuild wearing \
             the keyed repair's name"
        );
    }

    /// Two rows at one key are one key, and the repair is per key.
    ///
    /// `DISTINCT` rather than a bare `SELECT`: `a → b` has two archivable-or-
    /// not rows and the repair re-derives its partition once. Without it the
    /// `IN` subquery still gives the right answer and the key table grows with
    /// the *rows* archived rather than the keys, which is the same cost defect
    /// one level down.
    #[tokio::test]
    async fn a_key_asserted_twice_is_collected_once() {
        let conn = seeded().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        collect_archived_keys(&tx, "1 = 1", ()).await.unwrap();

        let n: i64 = tx
            .query(&format!("SELECT COUNT(*) FROM {ARCHIVED_KEYS}"), ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(n, 3, "four rows at three keys collected as three keys");
    }
}
