//! DDL statements for the Macrame bitemporal schema as specified in §4.

/// GLOB pattern matching the canonical timestamp form `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// A macro rather than a `const` so it can be spliced into the DDL literals by
/// `concat!`, which only accepts literals. Kept byte-identical to
/// [`crate::util::timestamp::CANONICAL_TS_GLOB`] by the unit test at the bottom
/// of this file — the storage-layer guard and the Rust-layer guard must agree
/// or one of them is decorative.
macro_rules! ts_glob {
    () => {
        "[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z"
    };
}

/// Table-level CHECK asserting every temporal column is canonical (§4.1, 0.5.4).
///
/// Timestamps are compared lexicographically everywhere — in SQL predicates, in
/// `MAX(recorded_at)` when the clock recovers its floor, and in Rust `str`
/// ordering. That is sound only if every value has the same width, so mixing
/// `...T00:00:00Z` with `...T00:00:00.000000Z` makes `<=` disagree with
/// chronology and traversals return empty sets with no error. The `Z` suffix
/// alone does not achieve this; a fixed width does, and a CHECK is what makes
/// it a property of the data rather than a convention.
macro_rules! canonical_ts_check {
    ($($col:literal),+ $(,)?) => {
        concat!("CHECK (", $( $col, " GLOB '", ts_glob!(), "' AND ", )+ "1)")
    };
}

/// A macro rather than a `const` for the same reason as [`ts_glob`]: `concat!`
/// splices it into the table DDL and only accepts literals. [`WEIGHT_CHECK`] is
/// the same text as a value, and carries the reasoning.
macro_rules! weight_check {
    () => {
        "CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')"
    };
}

/// Table-level CHECK on `links.weight` (§4.7, T2.1, D-083).
///
/// Three clauses. Only the first is the one the item asked for; the other two
/// were found by probing what the first still admits.
///
/// `weight >= 0.0` is the item as written: shortest-path analytics are unsound
/// over negative weights, so Dijkstra and A\* refuse the graph at load time
/// (D-039). Until now that refusal was the *only* place the property was
/// enforced, which made it §4.7's one genuinely open gap — a database this crate
/// wrote by itself could hold a row this crate would not read back.
///
/// `typeof(weight) = 'real'` closes a hole the item does not mention and which
/// probing found. `REAL` in SQLite is an **affinity**, not a type: values that
/// can be converted are, and values that cannot are stored as they came. `'abc'`
/// cannot become a number, so it is stored as TEXT — and in SQLite's type
/// ordering every text value sorts above every numeric one, so `'abc' >= 0.0`
/// is *true* and the first clause passes it through.
///
/// That is not a wrong answer on the read side. It is a **panic**: reading a
/// text `weight` as `f64` reaches `unreachable!("invalid value type")` inside
/// libsql 0.9.30, in whatever unrelated query first touches the row. Measured,
/// not reasoned about — see `examples/weight_check_probe.rs`.
///
/// The clause costs one `typeof` per insert and refuses nothing legitimate:
/// `3`, `'5'` and `1.0` all arrive as REAL through affinity conversion and pass.
/// It is taken **now** rather than in a later rung because SQLite has no
/// `ADD CONSTRAINT` — every clause added later costs another full rebuild of the
/// largest table in the schema.
///
/// `weight < 9e999` refuses `+∞`, and the reason is not the one anybody
/// predicted. The plan expected the CHECK to admit infinity and argued the
/// loader guard would catch it; the guard tests `< 0.0` and `is_nan()`, so it
/// does not. The next guess — mine — was that this is harmless, since IEEE
/// infinity propagates through addition and stays totally ordered, leaving
/// Dijkstra terminating with "that edge is unusable": an odd answer, not a wrong
/// one.
///
/// Both were wrong, and a test found it. **An infinite weight makes the
/// transaction log unreplayable.** The log trigger serialises the row to JSON,
/// and JSON has no representation for infinity, so the payload round-trips into
/// `ReplayCorrupt { reason: "number out of range" }` — every later
/// `reconstruct()` fails, including the one `close()` performs. The ledger is
/// the source of truth under Doctrine III, so a value that cannot survive the
/// log is not an eccentric weight, it is a corrupt one.
///
/// `9e999` is the idiom because SQLite has no `isinf`: the literal overflows to
/// `+∞` on parse, and `inf < inf` is false. Finite values, including `1e308`,
/// pass.
///
/// The loader guard still **stays**, for the reason the constraint cannot cover:
/// `links_current` carries no CHECK, and neither do cold files created before
/// this rung.
pub const WEIGHT_CHECK: &str = weight_check!();

/// The `RAISE(ABORT, …)` messages the schema's guards emit (§4.3).
///
/// Spliced into the trigger DDL *and* matched by [`crate::error::abort_kind`],
/// so the guard and its classifier cannot drift. When they drift the failure is
/// silent in the worst direction: the guard still fires, but the typed error
/// (`SingleOpenViolation`, `RecordedAtRegression`, `ArchiveViolation`) degrades
/// into an opaque `Engine` error that no caller can match on.
macro_rules! abort_single_open {
    () => {
        "macrame: edge already has an open interval; retire it first"
    };
}
macro_rules! abort_monotonic_ra {
    () => {
        "macrame: concept recorded_at must be strictly increasing"
    };
}
macro_rules! abort_delete_guard {
    () => {
        "macrame: physical delete blocked outside archive session"
    };
}

pub const ABORT_SINGLE_OPEN: &str = abort_single_open!();
pub const ABORT_MONOTONIC_RA: &str = abort_monotonic_ra!();
pub const ABORT_DELETE_GUARD: &str = abort_delete_guard!();

/// Marker table probed by the delete guards (D-008 revised).
///
/// The archive session creates this table and drops it again inside the single
/// `BEGIN IMMEDIATE … COMMIT` archive transaction, so it never exists as
/// committed state. Connection-locality — the property the original
/// `temp.sqlite_master` probe was reaching for — is preserved by two
/// independent mechanisms: uncommitted DDL is visible only to the writing
/// connection, and the archive transaction holds the write lock for its
/// duration, so no other connection can reach the guard at all.
pub const ARCHIVE_SESSION_MARKER: &str = "macrame_archive_session";

/// The `concepts` ledger table (§4.1).
///
/// # `rowid_pk` is explicit, and that is the whole point (v8, D-119)
///
/// Through v7 this table declared `id TEXT PRIMARY KEY`, which left its rowid
/// **implicit** — and `concepts_fts` is external-content keyed on that rowid.
/// `VACUUM` renumbers implicit rowids, which would silently decouple the search
/// index from the rows it indexes: no error, no integrity-check failure, just
/// results that stop matching.
///
/// [D-071](../../docs/architecture/s13-decision-register.md) proved the hazard
/// unreachable *by consequence rather than by design* — `trg_concepts_guard_delete`
/// is unconditional, so rowids are dense `1..n` and `VACUUM`'s renumbering is
/// the identity map. 0.9.0's archival makes them sparse and makes the hazard
/// real, so v8 replaces the accident with a column: an `INTEGER PRIMARY KEY` is
/// a stored value, and `VACUUM` preserves it whether the numbering is dense or
/// not (measured in `examples/concepts_rebuild_probe.rs` §5).
///
/// SQLite permits one primary key per table, so `id` becomes `NOT NULL UNIQUE`.
/// That keeps it a valid foreign-key parent for `links.source_id` /
/// `links.target_id` and keeps `ON CONFLICT(id)` working, but it **is** a
/// primary-key change — which [D-036](../../docs/architecture/s13-decision-register.md)
/// forbids outright after 1.0. Taken pre-1.0 on purpose, or never.
pub const CREATE_CONCEPTS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS concepts (
    rowid_pk         INTEGER PRIMARY KEY,
    id               TEXT NOT NULL UNIQUE,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_LINKS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS links (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at),
    "#,
    weight_check!(),
    r#",
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_LINKS_CURRENT_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS links_current (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    valid_to    TEXT NOT NULL,
    weight      REAL NOT NULL,
    properties  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id, edge_type, valid_from),
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_TRANSACTION_LOG_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS transaction_log (
    seq_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name  TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    operation   TEXT NOT NULL,
    payload     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    "#,
    canonical_ts_check!("recorded_at"),
    r#"
);
"#
);

/// The per-model embedding table (§4.1, D-005), for a validated model name.
///
/// A function rather than a `const` because the table's identity *and its
/// column type* both depend on the model: `F32_BLOB(dim)` carries the declared
/// dimension in the schema, which is what [`crate::vector::declared_dimension`]
/// reads back so the crate never keeps a second copy of it.
///
/// Deliberately not part of the baseline migration. Which models exist is an
/// application's choice made over time, not a property of the schema version,
/// and D-036 classifies these tables as disposable periphery: a migration may
/// drop one and re-embed. `IF NOT EXISTS` makes registration idempotent.
///
/// No temporal columns, on purpose. Doctrine VII makes an embedding a derived
/// artifact of a model applied to content — it has no valid time of its own, and
/// giving it a `recorded_at` would put a third clock next to the two §2 permits
/// and invite queries that mix them.
pub fn create_embeddings_table(model: &crate::vector::ModelName, dim: usize) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
    concept_id  TEXT PRIMARY KEY REFERENCES concepts(id),
    embedding   F32_BLOB({dim}) NOT NULL
);",
        table = model.table(),
    )
}

/// The DiskANN index over a model's vectors.
///
/// **Load-bearing for correctness, not only for speed.** Measured against
/// libSQL 0.9.30: a blob of the wrong length inserted into an `F32_BLOB(4)`
/// column is *accepted* while no vector index exists, and rejected — with the
/// row not landing — once one does. §4.1 previously claimed the column type
/// enforced its own dimension at insert time; it does not. So this index is
/// created together with the table it indexes and is never optional, and
/// dropping it to speed up a bulk load would silently disarm the only
/// storage-layer check on dimension.
pub fn create_embeddings_index(model: &crate::vector::ModelName) -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS {index} ON {table} (libsql_vector_idx(embedding));",
        index = model.index(),
        table = model.table(),
    )
}

/// Derived analytics output, keyed by concept and label (§5.4, D-041).
///
/// Deliberately outside the ledger. Three properties are load-bearing and each
/// is the opposite of what the four normative tables above do.
///
/// **No log trigger.** Nothing in [`CREATE_TRIGGERS`] fires on this table, so an
/// annotation never reaches `transaction_log`. That is Doctrine VII's reasoning
/// about embeddings applied to the other derived artifact: a community label is
/// a function of an algorithm, a version of that algorithm, and a graph — not a
/// statement about the world, and a ledger that records it is recording the
/// analytics schedule as though it were history. A reconstruction that wants
/// labels recomputes them, which is the only honest way to ask what a past
/// graph's communities *were*.
///
/// **No delete guard.** Doctrine V protects the hot ledger tables; this table is
/// derivative state in Doctrine VI's second category, so wiping it must stay a
/// legal, ordinary operation — a rerun replaces the previous pass, and dropping
/// the whole table costs nothing but the recomputation.
///
/// **Upsert on `(concept_id, label)`.** One current value per label per concept.
/// Storing a history of successive runs here would be the ledger again, by
/// another name.
///
/// The foreign key is safe in a way `links_current`'s omitted ones are not:
/// concepts are never physically deleted (D-022), and this table is rebuilt by
/// re-running an algorithm that read `concepts` in the first place, so there is
/// no insertion-order problem to solve.
pub const CREATE_ANALYTICS_ANNOTATIONS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS analytics_annotations (
    concept_id  TEXT NOT NULL REFERENCES concepts(id),
    label       TEXT NOT NULL,
    value       TEXT NOT NULL,
    computed_at TEXT NOT NULL,
    PRIMARY KEY (concept_id, label),
    "#,
    canonical_ts_check!("computed_at"),
    r#"
);
"#
);

/// The keyword half of hybrid search: an FTS5 index over concept text (§5.9).
///
/// **External content.** The table declares `content='concepts'`, so the tokens
/// are indexed but the text itself is not duplicated — FTS5 reads it back from
/// `concepts` by rowid when it needs a column value. Two reasons beyond the
/// storage saving, and the second is the one that decided it:
///
/// * There is exactly one copy of the text, so the index cannot disagree with
///   the concept about what the concept says. A standalone FTS table would be a
///   second description of data the ledger already holds, which is the failure
///   class D-030 and D-035 exist to prevent.
/// * `INSERT INTO concepts_fts(concepts_fts) VALUES('rebuild')` reconstructs the
///   whole index from the content table in one statement. D-036 requires every
///   derivative table to be rebuildable from the ledger, and here that is the
///   engine's own operation rather than code of ours that has to be kept honest.
///
/// The cost is that external-content tables do not maintain themselves: an
/// `UPDATE` must retract the *old* terms before adding the new ones, using the
/// old column values. That is what `trg_concepts_fts_update` does, and getting
/// it wrong leaves an index that still matches text no concept contains.
///
/// **`content_rowid` names `rowid_pk`, not `rowid` (v8, D-119).** They are the
/// same value — an `INTEGER PRIMARY KEY` *is* the rowid — but naming the column
/// is what makes the key a declared one rather than an implicit one `VACUUM` is
/// free to renumber. See [`CREATE_CONCEPTS_TABLE`].
pub const CREATE_CONCEPTS_FTS: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS concepts_fts USING fts5(
    title,
    content,
    content='concepts',
    content_rowid='rowid_pk'
);
"#;

/// FTS5's own consistency check — **and it cannot see the failure that matters**
/// (§5.9, D-071).
///
/// Kept as a named constant so the finding has somewhere to live, and used by
/// `an_emptied_fts_index_still_passes_integrity_check`, which is a tripwire
/// rather than a guarantee.
///
/// On this libSQL build (0.9.30), `'integrity-check'` verifies the index's
/// *internal* consistency and not its agreement with the content table. Measured:
/// after `'delete-all'` the index answers zero matches where it answered ten, and
/// both `'integrity-check'` and `'integrity-check', 0` still report success. So a
/// `verify_fts()` built on this would report a healthy index for an empty one —
/// which is why there is no `verify_fts()`. See D-071.
pub const VERIFY_CONCEPTS_FTS: &str =
    "INSERT INTO concepts_fts (concepts_fts) VALUES ('integrity-check');";

/// Reconstruct the FTS index from `concepts` (§5.9, D-036).
///
/// The engine's own operation, so the rebuild path is not a second
/// implementation of the triggers that could drift from them.
pub const REBUILD_CONCEPTS_FTS: &str =
    "INSERT INTO concepts_fts (concepts_fts) VALUES ('rebuild');";

/// Every index the schema declares.
///
/// # Two entries left in v8, and why the list is now allowed to be short
///
/// `idx_annotations_label` and `idx_lc_tgt_active` were dropped by the v7 → v8
/// rung ([D-089](../../docs/architecture/s13-decision-register.md), completed by
/// D-118). Neither had a reader anywhere in the crate — `analytics_annotations`
/// is never selected from here at all, and no query seeks on
/// `links_current.target_id` as a leading column — so each was an index write
/// per insert, forever, buying nothing. One of them was on the crate's hottest
/// write path.
///
/// `tests/index_plan_tests.rs` now requires the unread set to be **empty**,
/// which turns "these two are known bad" into "an index with no reader is a red
/// test". That is the guarantee this list is kept short by.
pub const CREATE_INDICES: &[&str] = &[
    // Covering index for the traversal CTE (§5.2, D-042).
    //
    // Column order is load-bearing and was measured with EXPLAIN QUERY PLAN.
    // The seek column is `source_id`; everything after it is there so the
    // recursive step never touches the base table. The two range columns come
    // next and `edge_type` comes *after* them, because `edge_types` is empty
    // unless a caller sets it: with `edge_type` in second position SQLite
    // declines the index for the unfiltered traversal — the default one — and
    // silently falls back to a non-covering plan.
    //
    //   (source_id, edge_type, valid_from, ...)   filtered: COVERING
    //                                             unfiltered: NOT covering
    //   (source_id, valid_from, valid_to, weight, edge_type, target_id)
    //                                             both: COVERING
    //
    // This subsumes the former idx_lc_src_active (source_id, valid_to): same
    // prefix column, strictly more payload. Keeping both would pay two index
    // writes per assertion on a table that already takes three writes.
    "CREATE INDEX IF NOT EXISTS idx_lc_traversal_cover ON links_current \
     (source_id, valid_from, valid_to, weight, edge_type, target_id);",
    // The single-open-interval probe's own index (D-059, shipped v5 -> v6).
    //
    // `trg_links_single_open` runs an `EXISTS` on every edge insert, keyed on
    // (source_id, target_id, edge_type, valid_to) with valid_from as an
    // inequality. Before this index the planner served that probe from
    // `idx_lc_traversal_cover` with only `source_id` bound — it wins as a
    // covering index over the primary-key autoindex, which lacks `valid_to` —
    // so **every insert scanned its source's entire out-degree**. Measured on a
    // fixed 90-row chunk: 4.4 ms into an empty table, 18.4 ms into a
    // 2,000-edge hub, 47.7 ms into an 8,000-edge one, and 1.06 s into 90,000.
    // Growth in the table, not in the chunk.
    //
    // With this index the same 90 rows into the 8,000-edge hub take 8.0 ms and
    // stay flat. It matters beyond bulk import: the probe is on the insert path,
    // so an interactive `assert_edge` against a high-degree node paid the same
    // scan, and that is the path CHUNK_BUDGET's 3 ms exists to protect.
    //
    // Column order follows the trigger's WHERE exactly — the three equalities
    // first, then `valid_to` which is compared to the sentinel, then
    // `valid_from` which is the `<>` and cannot be a seek column. This does not
    // subsume `idx_lc_traversal_cover` and is not subsumed by it: that one leads
    // on `source_id` alone for the recursive walk, this one needs all three
    // equality columns bound. Both are kept, which is a fourth index write per
    // assertion buying a scan's removal from the same operation.
    "CREATE INDEX IF NOT EXISTS idx_lc_open_interval ON links_current \
     (source_id, target_id, edge_type, valid_to, valid_from);",
    "CREATE INDEX IF NOT EXISTS idx_txlog_time ON transaction_log (recorded_at);",
    "CREATE INDEX IF NOT EXISTS idx_txlog_entity ON transaction_log (entity_id);",
];

/// Every trigger the schema declares.
///
/// **`IF NOT EXISTS` means a changed body does not reach an existing file.**
/// `migrations::verify` checks trigger *presence by name*, which is deliberate
/// (a count refuses healthy databases) but does not and cannot notice that a
/// trigger present under the right name carries an older body. A database
/// stamped v5 by an earlier build therefore keeps whatever trigger text it was
/// created with until a rung drops and recreates it.
///
/// This is why the payload carries a version. Changing a log trigger's payload
/// splits the database population in two — files created after the change write
/// the new shape, files created before keep writing the old one — and the only
/// thing that makes that survivable is that every reader accepts both. A
/// payload change that did *not* bump `v` would be indistinguishable at read
/// time from corruption, which is the case `DbError::PayloadVersion` exists for.
///
/// The v1 → v2 concept payload (defect V) is deliberately left to ride along on
/// the next rung that has to move `user_version` anyway rather than claiming one
/// of its own: an old file loses `embedding_model` from its temporal reads, which
/// is exactly the behaviour it had before, and gains it the moment it is
/// migrated. Nothing regresses in the meantime.
pub const CREATE_TRIGGERS: &[&str] = &[
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_current_sync
    AFTER INSERT ON links
    BEGIN
        INSERT INTO links_current
            (source_id, target_id, edge_type, valid_from, valid_to,
             weight, properties, recorded_at)
        VALUES
            (NEW.source_id, NEW.target_id, NEW.edge_type, NEW.valid_from,
             NEW.valid_to, NEW.weight, NEW.properties, NEW.recorded_at)
        ON CONFLICT(source_id, target_id, edge_type, valid_from) DO UPDATE SET
            valid_to    = excluded.valid_to,
            weight      = excluded.weight,
            properties  = excluded.properties,
            recorded_at = excluded.recorded_at
        WHERE excluded.recorded_at > links_current.recorded_at;
    END;
    "#,
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_single_open
    BEFORE INSERT ON links
    WHEN NEW.valid_to = '9999-12-31T23:59:59.999999Z'
         AND EXISTS (
             SELECT 1 FROM links_current
             WHERE source_id  = NEW.source_id
               AND target_id  = NEW.target_id
               AND edge_type  = NEW.edge_type
               AND valid_from <> NEW.valid_from
               AND valid_to   = '9999-12-31T23:59:59.999999Z'
         )
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_single_open!(),
        r#"');
    END;
    "#
    ),
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_monotonic_ra
    BEFORE UPDATE ON concepts
    WHEN NEW.recorded_at <= OLD.recorded_at
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_monotonic_ra!(),
        r#"');
    END;
    "#
    ),
    // Payload v2 adds `embedding_model` (defect V). Before it, the field was
    // written by nobody and read by two — `replay::fold_delta` and
    // `as_of::hydrate_attributes` both asked the payload for it and both always
    // saw null, so `AttributeMode::AtTime`, the faithful mode Doctrine VIII
    // exists to offer, returned a *less* complete record than `Current`.
    //
    // The version number moves because the shape is a compat surface: readers
    // must be able to tell "this build wrote no model" from "this payload
    // predates the field". v1 is still accepted and folds with the field absent,
    // which is what makes this safe without a migration rung — see the note on
    // [`CREATE_TRIGGERS`].
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_insert
    AFTER INSERT ON concepts
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('concepts', NEW.id, 'I',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at);
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_update
    AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('concepts', NEW.id, 'U',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at);
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_log_insert
    AFTER INSERT ON links
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('links',
                NEW.source_id || '|' || NEW.target_id || '|' || NEW.edge_type || '|' || NEW.valid_from,
                'I',
                json_object('v', 1, 'source_id', NEW.source_id, 'target_id', NEW.target_id,
                            'edge_type', NEW.edge_type, 'valid_from', NEW.valid_from,
                            'valid_to', NEW.valid_to, 'weight', NEW.weight,
                            'properties', json(NEW.properties)),
                NEW.recorded_at);
    END;
    "#,
    // Concepts are NEVER physically archived (D-022), so this guard is
    // unconditional -- there is no session in which the delete becomes legal.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_guard_delete
    BEFORE DELETE ON concepts
    BEGIN
        SELECT RAISE(ABORT, 'macrame: concepts are never physically archived (D-022)');
    END;
    "#,
    // D-008 (revised): probe main.sqlite_master for the archive-session marker.
    // SQLite forbids a trigger in `main` from referencing objects in another
    // database, temp included, so the original temp.sqlite_master probe fails
    // at CREATE TRIGGER time and is unimplementable.
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_guard_delete
    BEFORE DELETE ON links
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = 'macrame_archive_session'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_delete_guard!(),
        r#"');
    END;
    "#
    ),
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_txlog_guard_delete
    BEFORE DELETE ON transaction_log
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = 'macrame_archive_session'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_delete_guard!(),
        r#"');
    END;
    "#
    ),
    // --- FTS sync (§5.9) ------------------------------------------------
    //
    // These write to `concepts_fts` and to nothing else. In particular they do
    // not touch `transaction_log`: an FTS index is derived from concept text
    // the ledger already records, so logging it would record the same fact
    // twice — the reasoning Doctrine VII applies to embeddings, and the reason
    // `doctrine_static_tests` scans this array.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_insert
    AFTER INSERT ON concepts
    BEGIN
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid_pk, NEW.title, NEW.content);
    END;
    "#,
    // The retraction is not optional and not symmetric with the insert. An
    // external-content FTS5 index stores terms, not text, so replacing a row
    // means telling it which terms to *remove* — and it needs the old column
    // values to work that out. Omit this and the index keeps matching words the
    // concept no longer contains, with no error and no way to notice except by
    // searching for something that is no longer there.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_update
    AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO concepts_fts (concepts_fts, rowid, title, content)
        VALUES ('delete', OLD.rowid_pk, OLD.title, OLD.content);
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid_pk, NEW.title, NEW.content);
    END;
    "#,
    // The third trigger, installed **inert** by v8 (§4.6, D-119).
    //
    // Through v7 this array had no delete trigger, and the stated reason was
    // that `trg_concepts_guard_delete` is unconditional (D-022) so no delete
    // path exists to keep in sync. That was true and it was the wrong shape:
    // the index's correctness depended on a *different* trigger staying
    // unconditional, and nothing connected the two except a comment.
    //
    // It cannot fire today — the guard is a `BEFORE DELETE` that always aborts,
    // so the statement never reaches `AFTER DELETE`. It is here because 0.9.0's
    // archive session is what makes the guard conditional, and the moment that
    // lands the index would go silently stale without this. Installing the
    // capability in the rung that is already rebuilding the table costs nothing.
    //
    // It does **not** mean 0.9.0 needs no migration of its own — that claim was
    // written here and it is wrong (D-126, corrected 0.8.0 pre-tag). This trigger
    // is C2's step 3; step 2 is making `trg_concepts_guard_delete` conditional,
    // and that is a `v8 → v9` rung, because `CREATE TRIGGER IF NOT EXISTS` on an
    // existing name keeps the **old body** and `verify()` compares names only, so
    // a re-issued baseline would leave the unconditional guard in place and pass.
    // Deliberately not fixed here: the archive-session marker exists during
    // *links* archival too, so a conditional concepts guard shipped in 0.8.0
    // would leave concepts deletable during those sessions.
    //
    // `the_fts_delete_trigger_is_installed_and_inert` (wave1_regression_tests)
    // pins both halves rather than assuming either.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_delete
    AFTER DELETE ON concepts
    BEGIN
        INSERT INTO concepts_fts (concepts_fts, rowid, title, content)
        VALUES ('delete', OLD.rowid_pk, OLD.title, OLD.content);
    END;
    "#,
];

#[cfg(test)]
mod tests {
    use crate::util::timestamp::{CANONICAL_TS_GLOB, OPEN_SENTINEL};

    /// The DDL's CHECK pattern and the Rust-side pattern must be the same
    /// pattern. If they drift, one layer accepts what the other rejects and the
    /// canonical-form invariant is enforced in name only.
    #[test]
    fn ddl_glob_matches_the_rust_canonical_pattern() {
        assert_eq!(format!("'{}'", ts_glob!()), CANONICAL_TS_GLOB);
    }

    /// Every DDL statement that declares a temporal default must use the
    /// canonical sentinel; a second-precision default would be rejected by the
    /// very CHECK sitting next to it.
    #[test]
    fn ddl_defaults_use_the_canonical_sentinel() {
        for ddl in [
            super::CREATE_CONCEPTS_TABLE,
            super::CREATE_LINKS_TABLE,
            super::CREATE_LINKS_CURRENT_TABLE,
            super::CREATE_TRANSACTION_LOG_TABLE,
        ] {
            assert!(
                !ddl.contains("9999-12-31T23:59:59Z"),
                "DDL still carries the pre-0.5.4 second-precision sentinel: {ddl}"
            );
        }
        for trigger in super::CREATE_TRIGGERS {
            assert!(
                !trigger.contains("9999-12-31T23:59:59Z"),
                "trigger still carries the pre-0.5.4 sentinel: {trigger}"
            );
        }
        assert!(super::CREATE_LINKS_TABLE.contains(OPEN_SENTINEL));
    }

    /// Every abort message the classifier matches on must actually appear in the
    /// DDL that emits it. `concat!` makes this true by construction today; the
    /// test is what keeps it true if someone re-inlines a literal.
    #[test]
    fn every_abort_message_appears_in_a_trigger() {
        for msg in [
            super::ABORT_SINGLE_OPEN,
            super::ABORT_MONOTONIC_RA,
            super::ABORT_DELETE_GUARD,
        ] {
            assert!(
                super::CREATE_TRIGGERS.iter().any(|t| t.contains(msg)),
                "no trigger emits {msg:?}, so its typed error is unreachable"
            );
        }
    }
}
