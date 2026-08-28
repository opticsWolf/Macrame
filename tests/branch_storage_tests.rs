//! The v12 branch storage model (§15.2, W12.2, D-214 … D-217).
//!
//! Storage only: nothing here goes through the public API, because at this
//! release there is no public API that can produce a second lineage. That is
//! not a gap in the tests, it is the shape of the release — the semantics land
//! before the surface (D-160 → D-174), and every fixture below reaches them the
//! way the schema would be reached by a caller who had `fork()`: by raw SQL.
//!
//! Which makes these tests the only thing standing between a correct storage
//! model and a plausible one until 0.14.5.

#[path = "common/harness.rs"]
mod harness;
#[path = "common/v11_schema.rs"]
mod v11_schema;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::schema::{ddl, SCHEMA_VERSION};
use macrame::{ConceptUpsert, Database};
use v11_schema::wind_back_to_v11;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const TS2: &str = "2026-02-01T00:00:00.000000Z";
const TS3: &str = "2026-03-01T00:00:00.000000Z";
const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";
/// In the future, and it has to be: `recorded_at` is crate-stamped, so a
/// past cutoff archives nothing whatever the valid-time columns say.
const CUTOFF: &str = "2099-01-01T00:00:00.000000Z";

async fn connect(harness: &TestHarness) -> libsql::Connection {
    libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn user_version(conn: &libsql::Connection) -> u32 {
    conn.query("PRAGMA user_version", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

async fn columns(conn: &libsql::Connection, schema: &str, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA {schema}.table_info({table})"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(1).unwrap());
    }
    out
}

/// The first column of the first row, as a count.
///
/// Two concrete helpers rather than one generic, because `libsql` does not
/// export the `FromValue` bound `Row::get` is written against — there is no
/// name to write in a `where` clause outside the crate.
async fn count(
    conn: &libsql::Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> i64 {
    conn.query(sql, params)
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// The first column of the first row, as text. See [`count`].
async fn text(
    conn: &libsql::Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> String {
    conn.query(sql, params)
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// Every lineage a fixture uses has to exist first: `branch_id` carries a
/// foreign key into `branches`, so an unregistered name is refused by the
/// engine (probe §15) rather than quietly stored.
async fn register(conn: &libsql::Connection, branch: &str, parent: &str, forked_at: &str) {
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES (?1, ?2, ?3, ?3)",
        libsql::params![branch, parent, forked_at],
    )
    .await
    .unwrap();
}

async fn seed_concepts(conn: &libsql::Connection) {
    for id in ["c0", "c1"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The rung
// ───────────────────────────────────────────────────────────────────────────

/// v11 → v12: the four tables gain the column, and every row already there is
/// trunk.
///
/// The rung's *purpose*, not merely that `run` reached the top — which is what
/// `a_version_bump_must_bring_its_own_rung_test` in `migration_tests` demands
/// of every version bump.
#[tokio::test]
async fn a_v11_database_climbs_to_v12_and_every_existing_row_reads_as_trunk() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    wind_back_to_v11(&conn).await;
    conn.execute("PRAGMA user_version = 11", ()).await.unwrap();

    // Rows written by v11, before lineage existed anywhere.
    seed_concepts(&conn).await;
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) VALUES ('c0','c1','A',?1,?2,1.0,'{}',?1)",
        libsql::params![TS, SENTINEL],
    )
    .await
    .unwrap();

    for table in ["concepts", "links", "transaction_log"] {
        assert!(
            !columns(&conn, "main", table)
                .await
                .contains(&"branch_id".into()),
            "the fixture is not at v11: {table} already carries branch_id"
        );
    }

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    for table in ["concepts", "links", "links_current", "transaction_log"] {
        assert!(
            columns(&conn, "main", table)
                .await
                .contains(&"branch_id".into()),
            "the rung must add branch_id to {table}"
        );
    }

    // The root exists, is the root, and every pre-rung row names it.
    let root: (String, Option<String>) = {
        let row = conn
            .query("SELECT branch_id, parent_id FROM branches", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        (row.get(0).unwrap(), row.get(1).unwrap())
    };
    assert_eq!(root, ("main".to_string(), None), "the root has no parent");

    for table in ["concepts", "links", "links_current", "transaction_log"] {
        let strays: i64 = count(
            &conn,
            &format!("SELECT COUNT(*) FROM {table} WHERE branch_id <> 'main'"),
            (),
        )
        .await;
        assert_eq!(
            strays, 0,
            "a row written before lineage existed came back as something other \
             than trunk in {table}"
        );
    }

    // The materialization was re-derived, not described: it must still agree
    // with the ledger it was rebuilt from.
    assert_eq!(macrame::integrity::audit_current(&conn).await.unwrap(), 0);
}

/// The widened key is the point of rebuilding `links_current`, so it is
/// asserted rather than assumed — and asserted against the trigger that
/// depends on it, not against a literal.
#[tokio::test]
async fn links_current_is_keyed_per_lineage_and_the_sync_trigger_agrees() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let sql: String = text(
        &conn,
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'links_current'",
        (),
    )
    .await;
    assert!(
        sql.contains("PRIMARY KEY (source_id, target_id, edge_type, valid_from, branch_id)"),
        "links_current must be keyed per lineage: {sql}"
    );

    let target = ddl::CREATE_LINKS_CURRENT_SYNC
        .split_once("ON CONFLICT(")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(cols, _)| cols)
        .expect("the sync trigger declares an ON CONFLICT target");
    assert!(
        sql.contains(&format!("PRIMARY KEY ({target})")),
        "the table's key and the trigger's conflict target have diverged: \
         key in {sql}, target {target}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Honest stamping — the finding that changed the rung
// ───────────────────────────────────────────────────────────────────────────

/// A branch's own writes are logged against the branch, not against trunk.
///
/// The defect this pins was invisible from every angle except this one. The
/// column existed, the fold partitioned on it, and every log row still said
/// `'main'` — because the log triggers' `INSERT` column lists did not name it
/// and took the default. A branch's history would have landed in the trunk's
/// fold, and 0.14.6's abandonment sweep would have found nothing to archive.
///
/// Both operations, because `concepts` permits a **same-lineage** update: the
/// guards refuse cross-lineage inserts and `branch_id` changes, and deliberately
/// leave this one alone (§15.4).
#[tokio::test]
async fn the_log_triggers_stamp_the_lineage_the_write_actually_happened_on() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;
    register(&conn, "b", "main", TS2).await;

    // Minted on `b`.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
         VALUES ('cb', 'N', ?1, ?1, 'b')",
        libsql::params![TS2],
    )
    .await
    .unwrap();

    // Corrected on `b` — same lineage, which the schema permits.
    conn.execute(
        "UPDATE concepts SET title = 'N2', recorded_at = ?1 WHERE id = 'cb'",
        libsql::params![TS3],
    )
    .await
    .unwrap();

    // An edge asserted on `b`.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at, branch_id) \
         VALUES ('c0','c1','A',?1,?2,1.0,'{}',?1,'b')",
        libsql::params![TS2, SENTINEL],
    )
    .await
    .unwrap();

    let trunk_stamped: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM transaction_log WHERE branch_id = 'main' \
         AND (entity_id = 'cb' OR entity_id LIKE 'c0|c1|A|%')",
        (),
    )
    .await;
    assert_eq!(
        trunk_stamped, 0,
        "a write that happened on branch 'b' was logged against trunk. Every \
         such row is invisible to the abandonment sweep and folds into the \
         wrong lineage's history."
    );

    let concept_rows: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'cb' AND branch_id = 'b'",
        (),
    )
    .await;
    assert_eq!(
        concept_rows, 2,
        "the mint and the same-lineage correction must both be logged on 'b'"
    );

    let edge_rows: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links' AND branch_id = 'b'",
        (),
    )
    .await;
    assert_eq!(edge_rows, 1, "the edge assertion must be logged on 'b'");
}

/// Two lineages asserting the same edge stay two beliefs at replay.
///
/// The central case, not an edge case: it is what happens the first time a
/// branch supersedes an edge it inherited. `entity_id` for a link is
/// `source|target|type|valid_from` and carries no lineage, so before v12 both
/// assertions produced the *same* key and the fold's
/// `ROW_NUMBER() … ORDER BY seq_id DESC` kept exactly one of them — silently.
#[tokio::test]
async fn two_lineages_asserting_one_edge_do_not_collapse_in_the_log() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;
    register(&conn, "b", "main", TS2).await;

    for (branch, weight, ra) in [("main", 1.0, TS), ("b", 2.0, TS2)] {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at, branch_id) \
             VALUES ('c0','c1','A',?1,?2,?3,'{}',?4,?5)",
            libsql::params![TS, SENTINEL, weight, ra, branch],
        )
        .await
        .unwrap();
    }

    // The materialization keeps them apart, which is what the widened primary
    // key buys and is the claim v12 actually shipped.
    let materialized: i64 = count(&conn, "SELECT COUNT(*) FROM links_current", ()).await;
    assert_eq!(
        materialized, 2,
        "links_current collapsed two lineages' open beliefs into one row"
    );
    assert_eq!(macrame::integrity::audit_current(&conn).await.unwrap(), 0);

    // And the log holds both rows to fold from.
    let logged: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'",
        (),
    )
    .await;
    assert_eq!(logged, 2);
}

/// The reconstruction keeps both, and says which lineage holds which (0.14.5).
///
/// **This test found D-221 by not restating what it was testing.** Through
/// 0.14.4 the test above restated the fold's `ROW_NUMBER() … PARTITION BY
/// table_name, entity_id, branch_id` inline and asserted two winners — a test
/// of a SQL snippet in the test file, which was green while the shipped path
/// was wrong. Calling [`macrame::temporal::reconstruct`] instead returned
/// **one** edge where the ledger holds two beliefs, and the assertion below
/// pinned that wrong answer through 0.14.4 rather than leaving it to be found
/// later.
///
/// D-216 widened the partitions in `temporal::replay`'s four SQL folds and they
/// were correct; what it did not sweep was the composition immediately
/// downstream, which is Rust. `fold_delta` keyed its edge map on `entity_id`
/// alone — the edge key, shared across lineages by design — and
/// `MaterializedState::edges` was a five-tuple with nowhere to put a lineage.
/// **The widened partition was handing two rows to a container that could not
/// hold two.** 0.14.5 projects `branch_id` out of all four folds, keys the map
/// on the pair, and makes the element an
/// [`EdgeBelief`](macrame::temporal::EdgeBelief).
///
/// The assertion is on the *pairing* and not on the count, because two rows
/// both labelled `main` would satisfy a count and would have lost exactly what
/// the label is for.
#[tokio::test]
async fn a_reconstruction_keeps_both_lineages_beliefs() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;
    register(&conn, "b", "main", TS2).await;

    // The two beliefs differ in valid time as well as in lineage, so this
    // stays a test of the composition rather than of the label alone: a fold
    // that dropped one row would return one interval, not two identical ones.
    // A weight disagreement still would not show — `MaterializedState::edges`
    // does not carry a weight — and that is a different shortfall from D-221's,
    // left alone because nothing yet asks the fold about weights.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at, branch_id) \
         VALUES ('c0','c1','A',?1,?2,1.0,'{}',?1,'main')",
        libsql::params![TS, SENTINEL],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at, branch_id) \
         VALUES ('c0','c1','A',?1,?2,1.0,'{}',?2,'b')",
        libsql::params![TS, TS2],
    )
    .await
    .unwrap();

    let state = macrame::temporal::reconstruct(&conn, TS3, None, None)
        .await
        .unwrap();

    assert_eq!(
        state.edges.len(),
        2,
        "one edge key, two lineages, two beliefs: {:?}",
        state.edges
    );
    // Which lineage holds which belief, and not merely that two arrived. The
    // shape this replaced kept whichever row the fold emitted last — so a
    // composition that returned two rows both labelled `main` would satisfy a
    // length check and still have lost the thing the label is for.
    let mut got: Vec<(&str, &str)> = state
        .edges
        .iter()
        .map(|e| (e.branch_id.as_str(), e.valid_to.as_str()))
        .collect();
    got.sort_unstable();
    assert_eq!(
        got,
        vec![("b", TS2), (ddl::MAIN_BRANCH, SENTINEL)],
        "the trunk still believes the edge open and the branch has closed it"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// The guards
// ───────────────────────────────────────────────────────────────────────────

/// A branch inherits concepts; it does not restate them (D-214, Option A).
#[tokio::test]
async fn a_branch_cannot_restate_or_relabel_an_inherited_concept() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;
    register(&conn, "b", "main", TS2).await;

    let err = conn
        .execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
             VALUES ('c0', 'mine now', ?1, ?1, 'b')",
            libsql::params![TS2],
        )
        .await
        .expect_err("a second lineage restated an inherited concept");
    assert!(
        err.to_string().contains(ddl::ABORT_CROSS_LINEAGE),
        "the refusal must name the rule, not just fail a unique index: {err}"
    );

    // The same statement as an upsert, which is how the crate's own write path
    // spells it — probe §7 measured that `BEFORE INSERT` still fires ahead of
    // `ON CONFLICT`, and this is what holds that finding.
    let err = conn
        .execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
             VALUES ('c0', 'mine now', ?1, ?1, 'b') \
             ON CONFLICT(id) DO UPDATE SET title = excluded.title",
            libsql::params![TS2],
        )
        .await
        .expect_err("an upsert reached DO UPDATE across a lineage boundary");
    assert!(err.to_string().contains(ddl::ABORT_CROSS_LINEAGE), "{err}");

    // Provenance, not identity: it records where the concept was minted, and
    // minting happened once.
    let err = conn
        .execute(
            "UPDATE concepts SET branch_id = 'b', recorded_at = ?1 WHERE id = 'c0'",
            libsql::params![TS3],
        )
        .await
        .expect_err("branch_id was moved by an UPDATE");
    assert!(
        err.to_string().contains(ddl::ABORT_BRANCH_IMMUTABLE),
        "{err}"
    );
}

/// Nothing on a `branches` row legitimately changes, so nothing may.
///
/// The engine already refuses to rename or delete a lineage any row points at
/// (the foreign key, probe §15). What it has nothing to say about is
/// `parent_id` and `forked_at`: those are the inputs to ancestry, so editing
/// either **re-derives the visibility of rows already written** with no new
/// assertion anywhere — the move Doctrine III forbids, reachable by one
/// statement.
#[tokio::test]
async fn branch_records_are_append_only_including_the_no_op_update() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    register(&conn, "b", "main", TS2).await;

    for (label, sql) in [
        (
            "re-parent",
            "UPDATE branches SET parent_id = NULL WHERE branch_id = 'b'",
        ),
        (
            "move the fork point",
            "UPDATE branches SET forked_at = '2026-06-01T00:00:00.000000Z' WHERE branch_id = 'b'",
        ),
        // A whole-row guard rather than a named subset, so this fails too — and
        // it is the case a three-column guard would have let through the day
        // someone added a fourth column.
        (
            "a no-op",
            "UPDATE branches SET branch_id = 'b' WHERE branch_id = 'b'",
        ),
    ] {
        let err = conn.execute(sql, ()).await.unwrap_err_or_else_msg(label);
        assert!(
            err.contains(ddl::ABORT_BRANCHES_FROZEN),
            "{label}: expected the append-only guard, got {err}"
        );
    }

    let err = conn
        .execute("DELETE FROM branches WHERE branch_id = 'b'", ())
        .await
        .expect_err("a lineage record was deleted");
    assert!(
        err.to_string().contains(ddl::ABORT_BRANCHES_FROZEN),
        "branches are never archived, so there is no session in which this is \
         legal: {err}"
    );
}

/// An unregistered lineage is refused by the engine, not by a convention.
///
/// This is probe §15 held as a test. libSQL accepts `ADD COLUMN … NOT NULL
/// DEFAULT 'main' REFERENCES …`, which SQLite documents as illegal, and
/// **enforces** the resulting key. The whole design of the `branches` guard
/// rests on that being true, and a silent upstream alignment with SQLite would
/// otherwise turn lineage referential integrity off with nothing going red.
#[tokio::test]
async fn a_row_cannot_name_a_lineage_that_was_never_registered() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;

    let err = conn
        .execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
             VALUES ('ghost', 'N', ?1, ?1, 'never-registered')",
            libsql::params![TS2],
        )
        .await
        .expect_err("a concept named a lineage that does not exist");
    assert!(
        err.to_string().to_lowercase().contains("foreign key"),
        "the lineage column must carry a real key, not a convention: {err}"
    );

    // And the migrated shape enforces it as well as the fresh one — the ALTER
    // is where SQLite's documented rule says the key should have been dropped.
    let strays: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM concepts WHERE id = 'ghost'",
        (),
    )
    .await;
    assert_eq!(strays, 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Visibility, pinned as it stands today
// ───────────────────────────────────────────────────────────────────────────

/// Concept reads are lineage-blind, and at 0.14.4 that stopped being temporary.
///
/// **This pin was written to go red at 0.14.4 and it does not, which is the
/// finding rather than a stale comment.** It said the visibility predicate
/// would land in `visible_concept` and a scoped read on `main` would return one
/// row instead of two. That was written before Option A settled: concepts do
/// not branch. `branch_id` on `concepts` is *provenance* — where the row was
/// minted — and every lineage sees every concept, which is why the guards refuse
/// a branch restating one at all. There is no predicate to add here, and adding
/// one would split a namespace the design deliberately keeps whole.
///
/// What did change at 0.14.4 is the *edge* read, which is where lineage lives:
/// `branch_read_tests` holds it. Kept and renamed rather than deleted, because
/// a schedule that turned out to be wrong about which read would move is worth
/// more written down than removed — the same reason D-219 records the probe's
/// first draft instead of quietly correcting it.
#[tokio::test]
async fn concepts_are_shared_across_lineages_and_the_read_does_not_filter() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    seed_concepts(&conn).await;
    register(&conn, "b", "main", TS2).await;

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
         VALUES ('cb', 'minted on b', ?1, ?1, 'b')",
        libsql::params![TS2],
    )
    .await
    .unwrap();

    let visible: i64 = count(&conn, "SELECT COUNT(*) FROM concepts WHERE retired = 0", ()).await;
    assert_eq!(
        visible, 3,
        "every lineage sees every concept under Option A. If this is red, a \
         visibility predicate has landed on concepts — which is a change to \
         what a branch *is*, not an optimisation, and belongs in §15.2 before \
         it belongs in the schema."
    );

    // The lineage is still recorded, because provenance is what the column is
    // for: `fork()`'s abandonment sweep at §15.5 needs to know which rows a
    // discarded branch minted, and that question is unanswerable from a
    // namespace with no provenance in it.
    let minted_on_b: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM concepts WHERE branch_id = 'b'",
        (),
    )
    .await;
    assert_eq!(minted_on_b, 1, "shared visibility is not shared provenance");
}

// ───────────────────────────────────────────────────────────────────────────
// The cold side
// ───────────────────────────────────────────────────────────────────────────
//
// Every assertion below is about the *boundary*. The hot side is checkable by
// reading a column; the cold side is where lineage goes missing quietly,
// because a cold file has no version stamp worth trusting (D-026), no
// `branches` table to point at, and a schema pass written `CREATE TABLE IF NOT
// EXISTS` that reports success on a shape it did not create (probe §10).

/// Build a database, write two lineages' worth of rows, and archive.
///
/// `archive` is reached through the public `Database` rather than by calling
/// `temporal::archive` directly, because the cold path's failure modes are in
/// the *session* — the marker table, the transaction that carries the DDL
/// upgrade, the guards that fire when it is absent — and a test that bypassed
/// the session would exercise none of them.
async fn archived_pair(harness: &TestHarness) -> std::path::PathBuf {
    let db = Database::open(&harness.db_path).await.unwrap();
    // Retired with a closed valid time from the start, because `recorded_at` is
    // crate-stamped and cannot be moved afterwards: the monotonicity guard
    // refuses a stamp that goes backwards and `FutureRecordedAt` refuses one
    // that goes forwards. Valid time is the caller's, so archivability has to
    // be expressed there — which is why `CUTOFF` is in the future rather than
    // the rows being in the past.
    for id in ["a", "b"] {
        db.upsert_concept(
            ConceptUpsert::new(id, "T")
                .valid_from(TS)
                .valid_to(TS2)
                .retired(true),
        )
        .await
        .unwrap();
    }
    // The edge closes too, or it keeps both endpoints reachable and the concept
    // side of the archive moves nothing.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(TS)
            .valid_to(TS2),
    )
    .await
    .unwrap();

    db.archive(CUTOFF).await.unwrap();
    db.close().await.unwrap();

    let mut cold = harness.db_path.clone();
    let stem = harness.db_path.file_stem().unwrap().to_str().unwrap();
    cold.set_file_name(format!("{stem}_archive.db"));
    cold
}

async fn attach_cold(conn: &libsql::Connection, cold: &std::path::Path) {
    conn.execute(&format!("ATTACH DATABASE '{}' AS cold", cold.display()), ())
        .await
        .unwrap();
}

/// An archive writes the lineage the row was actually on, and the cold file
/// grows the column to hold it.
#[tokio::test]
async fn an_archive_carries_the_lineage_across_the_boundary() {
    let harness = TestHarness::new();
    let cold = archived_pair(&harness).await;
    assert!(cold.exists(), "the archive must have produced a cold file");

    let conn = connect(&harness).await;
    attach_cold(&conn, &cold).await;

    for table in ["links", "transaction_log"] {
        assert!(
            columns(&conn, "cold", table)
                .await
                .contains(&"branch_id".into()),
            "cold.{table} did not gain branch_id, so the lineage of every \
             archived row is gone the moment it crosses"
        );
    }

    let moved: i64 = count(&conn, "SELECT COUNT(*) FROM cold.links", ()).await;
    assert!(
        moved > 0,
        "the fixture archived nothing, so this proves nothing"
    );
    let trunk: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM cold.links WHERE branch_id = ?1",
        libsql::params![ddl::MAIN_BRANCH],
    )
    .await;
    assert_eq!(
        trunk, moved,
        "every row the fixture wrote was on the trunk, so every archived row \
         must read as trunk — a different count means the write carried a \
         literal rather than the row's own column"
    );
}

/// A cold file written before v12 is upgraded in place by the next archive,
/// and its existing rows read as trunk.
///
/// The wind-back is `DROP COLUMN` rather than a hand-built v11 cold file,
/// because what has to be exercised is the *detection* — `cold_has_branch`
/// asking `PRAGMA cold.table_info` — and detection on a file this crate did not
/// write is the same question as detection on one it did.
#[tokio::test]
async fn a_pre_v12_cold_file_is_upgraded_by_the_archive_that_meets_it() {
    let harness = TestHarness::new();
    let cold = archived_pair(&harness).await;

    {
        let conn = connect(&harness).await;
        attach_cold(&conn, &cold).await;
        for table in ["links", "concepts", "transaction_log"] {
            conn.execute(
                &format!("ALTER TABLE cold.{table} DROP COLUMN branch_id"),
                (),
            )
            .await
            .unwrap();
        }
        assert!(
            !columns(&conn, "cold", "links")
                .await
                .contains(&"branch_id".into()),
            "the wind-back did not take"
        );
    }

    // A second archive with nothing to move still runs the session, and the
    // upgrade rides along with it rather than needing a migration of its own.
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("c", "C").valid_from(TS))
        .await
        .unwrap();
    db.archive(CUTOFF).await.unwrap();
    db.close().await.unwrap();

    let conn = connect(&harness).await;
    attach_cold(&conn, &cold).await;
    for table in ["links", "concepts", "transaction_log"] {
        assert!(
            columns(&conn, "cold", table)
                .await
                .contains(&"branch_id".into()),
            "cold.{table} was not upgraded, so the next insert either fails \
             loudly or drops the lineage silently — probe §10 and §11"
        );
    }
    let orphaned: i64 = count(
        &conn,
        "SELECT COUNT(*) FROM cold.links WHERE branch_id <> ?1",
        libsql::params![ddl::MAIN_BRANCH],
    )
    .await;
    assert_eq!(
        orphaned, 0,
        "a row that predates the column must take the default, not a null or \
         a blank"
    );
}

/// The fold spans a hot database that knows about lineage and a cold file that
/// does not, and answers per lineage on both sides.
///
/// This is the case the two-shape projection exists for. `cold_lineage` probes
/// the attached file once and picks `branch_id` or `'main' AS branch_id`, so a
/// reconstruction across the boundary has one column list whatever the cold
/// file's vintage. Without it the `UNION ALL` fails on column count — loudly,
/// which is the good outcome — or the cold arm is written to omit the column
/// and the fold silently partitions half its input on nothing.
///
/// # Reaching the cold arm at all is half of this test
///
/// `hot_log_reach` decides whether the archive is consulted, and with an
/// archive present the rule is `MAX(recorded_at) <= ts`: a question asked
/// *after* every hot row is answered from the hot file alone, archive path or
/// not. The first draft of this test asked at 2099 with every row stamped
/// today, took that branch, and passed without the cold file ever being
/// attached — green, and measuring nothing. Two pieces of the fixture exist to
/// stop that recurring:
///
/// * **`LATER`**, a hot row recorded *after* the instant asked about, which is
///   the only thing that puts the fold on the cold path at all.
/// * **`only_in_cold`**, a log row planted in the cold file and held nowhere
///   else, which is the only evidence available that the cold arm contributed.
///   The archived concepts cannot serve as that evidence: `archived_pair`
///   retires them to make them archivable, and a retired concept is
///   deliberately absent from a composed state (see `Delta::concepts_gone`).
#[tokio::test]
async fn a_reconstruction_spans_a_v11_cold_file_and_a_v12_hot_one() {
    /// The instant the reconstruction asks about.
    const AS_OF: &str = "2099-06-01T00:00:00.000000Z";
    /// Before it, so what carries this stamp is inside the question.
    const LATE: &str = "2099-03-01T00:00:00.000000Z";
    /// After it, so the hot log does not cover `AS_OF` — see the note above.
    const LATER: &str = "2099-09-01T00:00:00.000000Z";

    let harness = TestHarness::new();
    let cold = archived_pair(&harness).await;

    {
        let conn = connect(&harness).await;
        attach_cold(&conn, &cold).await;
        conn.execute("ALTER TABLE cold.transaction_log DROP COLUMN branch_id", ())
            .await
            .unwrap();
        // Written after the wind-back and by column name, so it is a row of the
        // v11 shape rather than a v12 row with a column dropped out from under
        // it — the projection has to read it either way, but only one of those
        // is the vintage being claimed.
        conn.execute(
            "INSERT INTO cold.transaction_log \
             (table_name, entity_id, operation, payload, recorded_at) \
             VALUES ('concepts', 'only_in_cold', 'I', \
                     json_object('v', 2, 'title', 'Cold', 'content', '', \
                                 'valid_from', ?1, 'valid_to', NULL, \
                                 'retired', 0, 'embedding_model', NULL), ?2)",
            libsql::params![TS, LATE],
        )
        .await
        .unwrap();
        conn.execute("DETACH DATABASE cold", ()).await.unwrap();
    }

    // A second lineage on the hot side, written the way a caller with `fork()`
    // would reach it — by raw SQL, because at this release there is no other
    // way (see this file's header).
    {
        let conn = connect(&harness).await;
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('b', ?1, ?2, ?2)",
            libsql::params![ddl::MAIN_BRANCH, TS2],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
             VALUES ('only_on_b', 'B', ?1, ?2, 'b')",
            libsql::params![TS3, LATE],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
             VALUES ('after_the_question', 'Later', ?1, ?2, ?3)",
            libsql::params![TS3, LATER, ddl::MAIN_BRANCH],
        )
        .await
        .unwrap();
    }

    // A plain connection, not the `Database` handle: `reconstruct` attaches the
    // cold file itself, and what is under test is the projection it picks after
    // probing that file.
    let conn = connect(&harness).await;
    let state = macrame::temporal::reconstruct(&conn, AS_OF, Some(&cold), None)
        .await
        .expect(
            "the fold must span a cold file without branch_id and a hot one \
             with it; a column-count failure here means the two-shape \
             projection is not being selected",
        );

    assert!(
        state.concepts.contains_key("only_in_cold"),
        "the row held only by the pre-v12 cold file is missing, so the cold arm \
         contributed nothing and whatever this test proved, it was not about \
         the projection"
    );
    assert!(
        state.concepts.contains_key("only_on_b"),
        "the hot lineage's own concept is missing from the reconstruction"
    );
    assert!(
        !state.concepts.contains_key("after_the_question"),
        "a row recorded after the instant asked about is in the answer, so the \
         fold is not bounded by recorded_at and the two assertions above prove \
         less than they appear to"
    );
}

/// Rehydration brings the lineage back with the row, and reads a pre-v12 cold
/// file without writing to it.
///
/// The read path asks `cold_has_branch` for the opposite reason the writer
/// does: a cold file may be read-only media or on a share, so a reader that
/// upgraded what it read would be a new failure class rather than a
/// convenience.
#[tokio::test]
async fn rehydration_restores_the_lineage_and_does_not_upgrade_what_it_reads() {
    let harness = TestHarness::new();
    let cold = archived_pair(&harness).await;

    {
        let conn = connect(&harness).await;
        attach_cold(&conn, &cold).await;
        conn.execute("ALTER TABLE cold.concepts DROP COLUMN branch_id", ())
            .await
            .unwrap();
    }

    let archived: Vec<String> = {
        let conn = connect(&harness).await;
        attach_cold(&conn, &cold).await;
        let mut rows = conn
            .query("SELECT id FROM cold.concepts", ())
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(r.get::<String>(0).unwrap());
        }
        out
    };
    if archived.is_empty() {
        // Nothing archived means nothing to rehydrate, and an assertion about
        // an empty set is not an assertion. Say so rather than passing.
        panic!("the fixture archived no concepts, so this test proves nothing");
    }

    let db = Database::open(&harness.db_path).await.unwrap();
    let ids: Vec<&str> = archived.iter().map(String::as_str).collect();
    db.rehydrate(&ids).await.unwrap();
    db.close().await.unwrap();

    let conn = connect(&harness).await;
    for id in &archived {
        let branch: String = text(
            &conn,
            "SELECT branch_id FROM concepts WHERE id = ?1",
            libsql::params![id.as_str()],
        )
        .await;
        assert_eq!(
            branch,
            ddl::MAIN_BRANCH,
            "a concept rehydrated from a cold file that predates the column \
             must come back on the trunk, not with a null the NOT NULL would \
             have refused"
        );
    }

    attach_cold(&conn, &cold).await;
    assert!(
        !columns(&conn, "cold", "concepts")
            .await
            .contains(&"branch_id".into()),
        "rehydration wrote to the cold file it was only supposed to read"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// A small ergonomic helper
// ───────────────────────────────────────────────────────────────────────────

trait ExpectErrMsg {
    fn unwrap_err_or_else_msg(self, label: &str) -> String;
}

impl<T> ExpectErrMsg for Result<T, libsql::Error> {
    fn unwrap_err_or_else_msg(self, label: &str) -> String {
        match self {
            Ok(_) => panic!("{label}: the statement was accepted and should not have been"),
            Err(e) => e.to_string(),
        }
    }
}
