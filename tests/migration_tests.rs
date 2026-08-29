#[path = "common/harness.rs"]
mod harness;
#[path = "common/v7_schema.rs"]
mod v7_schema;

#[path = "common/v11_schema.rs"]
mod v11_schema;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::schema::ddl;
use macrame::schema::SCHEMA_VERSION;
use v7_schema::seeded_v7;

const TS: &str = "2026-01-01T00:00:00.000000Z";

/// Open the harness database and hand back a connection.
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

/// Assert `run` refused, and hand back the reason so the caller can check that
/// the message names the actual problem rather than being merely non-empty.
fn refusal_reason(err: DbError) -> String {
    match err {
        DbError::Migration { to, reason } => {
            // `to` is the version the failure was trying to *reach*, which for a
            // refusal partway up the ladder is that rung's target and not the
            // top. This asserted equality with SCHEMA_VERSION until 0.9.0, where
            // it held only because the last rung happened to be the top one —
            // adding v8 → v9 made the v7 → v8 orphan refusal report 8 and broke
            // a helper that was never testing the ladder in the first place.
            assert!(
                to <= SCHEMA_VERSION,
                "a refusal cannot name a version above the ladder's top: {to}"
            );
            reason
        }
        other => panic!("expected DbError::Migration, got {other:?}"),
    }
}

#[tokio::test]
async fn fresh_database_reaches_the_baseline_version() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);
    // The stamp is worth nothing on its own -- confirm the canonical-form CHECK
    // that v2 exists to deliver actually landed.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('c1', 'T', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        (),
    )
    .await
    .expect_err("second-precision timestamps must be rejected at v2");
}

/// Re-opening must be a no-op, not a re-application. The old runner re-ran every
/// `CREATE ... IF NOT EXISTS` on every open; if that behaviour returns, data
/// written between opens is what pays for it.
#[tokio::test]
async fn run_is_idempotent_and_preserves_data() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('c1', 'T', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);
    let surviving: i64 = conn
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(surviving, 1, "second run must not disturb existing rows");
}

/// Operating on a schema written by a future build is how a ledger loses
/// history: the unknown columns are invisible to every query but still there.
#[tokio::test]
async fn refuses_a_database_from_a_newer_build() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    conn.execute(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 7), ())
        .await
        .unwrap();

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains(&format!("v{}", SCHEMA_VERSION + 7)),
        "refusal should name the version found: {reason}"
    );
}

/// The legacy-free policy, enforced: a pre-0.5.4 database stamped v1 has no rung
/// leading out of it and must be refused by name, not silently accepted because
/// its tables happen to share their names with the current ones.
#[tokio::test]
async fn refuses_a_pre_canonical_v1_database() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    conn.execute("PRAGMA user_version = 1", ()).await.unwrap();

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains("v1") && reason.contains("no migration path"),
        "refusal should identify the legacy schema and say there is no path: {reason}"
    );
}

/// `user_version` defaults to 0, so an unrelated SQLite file looks fresh. Adding
/// nine triggers to somebody else's database is not a recoverable mistake.
#[tokio::test]
async fn refuses_an_unstamped_database_that_is_not_empty() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    conn.execute("CREATE TABLE somebody_elses_data (x)", ())
        .await
        .unwrap();

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains("unrelated"),
        "refusal should explain what it is protecting: {reason}"
    );
    assert_eq!(
        user_version(&conn).await,
        0,
        "a refused open must not stamp the file"
    );
}

/// The baseline either lands whole or not at all: a partial schema stamped as
/// complete is worse than no schema, because the stamp suppresses the retry.
#[tokio::test]
async fn a_refused_run_leaves_no_partial_schema() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    conn.execute("CREATE TABLE somebody_elses_data (x)", ())
        .await
        .unwrap();

    let _ = macrame::schema::run_migrations(&conn).await.unwrap_err();

    let macrame_objects: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN \
             ('concepts', 'links', 'links_current', 'transaction_log')",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(macrame_objects, 0);
}

/// Verification exists to catch DDL that no-ops instead of creating, so the
/// baseline must actually leave every declared object behind.
///
/// Checked by *name*, and the counts are derived from the DDL arrays rather
/// than written as literals. The previous version asserted `4 tables, 9
/// triggers, 4 indices` as constants and failed the moment D-041 added a fifth
/// table — which is D-038's mistake reappearing in the test that guards it: a
/// count treats any addition as breakage and tells you a number, while a
/// name check tells you which object is missing. The 0.5.4 `verify()` was
/// changed for exactly this reason and the test had not followed.
#[tokio::test]
async fn the_baseline_leaves_every_declared_object_behind() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT type, name FROM sqlite_master \
             WHERE type IN ('table','trigger','index') AND name NOT LIKE 'sqlite_%'",
            (),
        )
        .await
        .unwrap();
    let mut present: Vec<(String, String)> = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        present.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    let has = |kind: &str, name: &str| {
        present
            .iter()
            .any(|(k, n)| k == kind && n.eq_ignore_ascii_case(name))
    };

    for table in [
        "concepts",
        "links",
        "links_current",
        "transaction_log",
        "analytics_annotations",
    ] {
        assert!(has("table", table), "missing table {table}: {present:?}");
    }

    let triggers = present.iter().filter(|(k, _)| k == "trigger").count();
    assert_eq!(
        triggers,
        ddl::CREATE_TRIGGERS.len(),
        "trigger count drifted from CREATE_TRIGGERS: {present:?}"
    );

    let indices = present.iter().filter(|(k, _)| k == "index").count();
    assert_eq!(
        indices,
        ddl::CREATE_INDICES.len(),
        "index count drifted from CREATE_INDICES: {present:?}"
    );
}

/// The v2 → v3 rung must reach v3 from a database that stopped at v2 — the
/// first time the ladder has had more than one rung, so the first time `run`'s
/// loop does anything but take the baseline.
#[tokio::test]
async fn a_v2_database_climbs_to_v3_and_gains_the_annotations_table() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    // Build a v2 database: the baseline minus what v3 added, stamped v2.
    // The four ledger tables as v11 declared them, not as today's constants
    // do. Since v12 the live `CREATE`s reference a `branches` table this
    // fixture has no business owning, and carry a `branch_id` the rung under
    // test is supposed to add — `tests/common/v11_schema.rs` says why at
    // length.
    for table in v11_schema::tables_v11() {
        conn.execute(&table, ()).await.unwrap();
    }
    for index_ddl in v11_schema::indices_v11() {
        // Every remaining index has its table *and its columns* in this
        // fixture. Through v7 one did not have its table —
        // `idx_annotations_label`, which is why this loop used to swallow its
        // result — and v8 dropped it (D-118). v14 is the column version of the
        // same problem and is excluded by `indices_v11` rather than swallowed,
        // because swallowing errors is how a fixture stops testing anything.
        conn.execute(index_ddl, ()).await.unwrap();
    }
    for trigger_ddl in v11_schema::triggers_v11() {
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
    conn.execute("PRAGMA user_version = 2", ()).await.unwrap();

    macrame::schema::run_migrations(&conn).await.unwrap();

    let version: u32 = conn
        .query("PRAGMA user_version", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    conn.query(
        "SELECT concept_id, label, value, computed_at FROM analytics_annotations",
        (),
    )
    .await
    .expect("the rung must create analytics_annotations");
}

/// The v5 → v6 rung reaches v6 from a database that stopped at v5, and the
/// index it adds is actually there afterwards (D-059).
///
/// A v5 database is the baseline minus one index, so it is built by laying the
/// baseline and dropping that index rather than by reconstructing v5's DDL by
/// hand — a hand-written copy of an old schema is a second description that can
/// drift from the one the rung is written against.
#[tokio::test]
async fn a_v5_database_climbs_to_v6_and_gains_the_open_interval_index() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();
    // Wound back before the stamp is rolled, because the ladder is not
    // re-entrant: a v12 database re-stamped v5 does not replay history, it
    // meets rungs written for shapes it no longer has. `wind_back_to_v11` is
    // what makes "the baseline minus one index" true again rather than merely
    // claimed — see `tests/common/v11_schema.rs`.
    v11_schema::wind_back_to_v11(&conn).await;
    conn.execute("DROP INDEX idx_lc_open_interval", ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 5", ()).await.unwrap();

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    let found: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_lc_open_interval'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(found, 1, "the rung must create idx_lc_open_interval");
}

/// v10 → v11: the two archive indexes on `links`, and the plan they buy.
///
/// Presence is asserted, but presence alone is the weak form the gate on
/// [`a_version_bump_must_bring_its_own_rung_test`] warns against — a rung that
/// creates an index nothing seeks on passes it, and that is exactly D-089's
/// failure. So the plan is asserted too: the archiving read must go from a full
/// scan of the ledger to a seek, in the same test that stamps the version.
///
/// This is the first rung to index a **frozen** table (D-036, D-151). See
/// `add_links_archive_indices` for why that is the permitted additive case
/// rather than an exception.
#[tokio::test]
async fn a_v10_database_climbs_to_v11_and_the_archive_read_stops_scanning_links() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();
    v11_schema::wind_back_to_v11(&conn).await;
    conn.execute("DROP INDEX idx_links_recorded_at", ())
        .await
        .unwrap();
    conn.execute("DROP INDEX idx_links_target", ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 10", ()).await.unwrap();

    // The archiving SELECT, reproduced from `LINKS_ARCHIVABLE`. Bounded against
    // drift by `index_plan_tests`, which holds the same query with an
    // `include_str!` check on `archive.rs`; this copy exists because the rung
    // has to be measured on both sides of itself and the registry only sees the
    // finished schema.
    const ARCHIVING_READ: &str = "SELECT source_id FROM links WHERE recorded_at < ?1          AND (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= ?1)";

    let before = plan_string(&conn, ARCHIVING_READ).await;
    assert!(
        before.contains("SCAN links"),
        "the fixture is not starting from the v10 plan — expected a full scan          of `links`, got: {before}"
    );

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    for name in ["idx_links_recorded_at", "idx_links_target"] {
        let found: i64 = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master                  WHERE type = 'index' AND name = ?1",
                libsql::params![name],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(found, 1, "the rung must create {name}");
    }

    let after = plan_string(&conn, ARCHIVING_READ).await;
    assert!(
        after.contains("SEARCH links USING INDEX idx_links_recorded_at"),
        "the rung created the index and the planner did not take it. An index          with no reader is an index write per ledger insert, forever (D-089).          Plan: {after}"
    );
}

/// `EXPLAIN QUERY PLAN` for `sql`, joined into one line.
async fn plan_string(conn: &libsql::Connection, sql: &str) -> String {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut lines = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        lines.push(r.get::<String>(3).unwrap_or_default());
    }
    lines.join(" | ")
}

/// The ladder's top, asserted once rather than inside whichever rung test was
/// written last.
///
/// This used to live in the v5 → v6 test as `assert_eq!(SCHEMA_VERSION, 6)`,
/// where it did its job — the T2.1 rung tripped it immediately — but it made a
/// version bump look like a failure of the *v6* rung, which it is not. Hoisted
/// so the message names the actual obligation.
#[test]
fn a_version_bump_must_bring_its_own_rung_test() {
    assert_eq!(
        SCHEMA_VERSION, 15,
        "SCHEMA_VERSION moved. Add a test for the new rung — one that starts \
         from a database at the previous version and asserts what the rung is \
         *for*, not merely that `run` reached the top."
    );
}

/// The v12 body of the `branches` delete guard: unconditional, with no marker
/// probe. Pinned as text for `CONCEPTS_GUARD_DELETE_V8`'s reason — an old
/// schema described by hand is a second description that drifts, and this one
/// is three lines and will never change again.
const V12_BRANCHES_GUARD_DELETE: &str = "
    CREATE TRIGGER trg_branches_frozen_delete
    BEFORE DELETE ON branches
    BEGIN
        SELECT RAISE(ABORT, 'macrame: branch records are append-only');
    END;
";

/// Register `name` as a child of the trunk, by raw insert.
async fn register_branch(conn: &libsql::Connection, name: &str) {
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES (?1, 'main', ?2, ?2)",
        libsql::params![name, TS],
    )
    .await
    .unwrap();
}

/// Delete a lineage record inside a declared archive session, and say whether
/// the guard allowed it.
async fn delete_in_session(conn: &libsql::Connection, name: &str) -> bool {
    conn.execute("CREATE TABLE macrame_archive_session (x)", ())
        .await
        .unwrap();
    let outcome = conn
        .execute(
            "DELETE FROM branches WHERE branch_id = ?1",
            libsql::params![name],
        )
        .await;
    conn.execute("DROP TABLE macrame_archive_session", ())
        .await
        .unwrap();
    outcome.is_ok()
}

/// v12 → v13: the `branches` delete guard becomes marker-gated (0.14.13,
/// §15.4, D-230).
///
/// What the rung is *for*, not that `run` reached the top: a v12 guard refuses
/// the delete `archive_branch` has to perform, and after the rung the same
/// delete inside the same session succeeds. Both halves are measured, because
/// the failure this rung exists to prevent is the one `CREATE TRIGGER IF NOT
/// EXISTS` produces — the baseline re-issued, the old body kept, and nothing
/// anywhere saying so (D-126).
///
/// The last assertion is the one that keeps the rung honest. Gating a guard and
/// removing it look identical from inside a session; they differ only outside
/// one, which is where `branches` is append-only and must stay so.
#[tokio::test]
async fn a_v12_database_climbs_to_v13_and_its_branches_guard_learns_the_marker() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();

    // Wind the guard back to its v12 shape and the stamp with it.
    conn.execute("DROP TRIGGER trg_branches_frozen_delete", ())
        .await
        .unwrap();
    conn.execute(V12_BRANCHES_GUARD_DELETE, ()).await.unwrap();
    conn.execute("PRAGMA user_version = 12", ()).await.unwrap();

    register_branch(&conn, "before").await;
    assert!(
        !delete_in_session(&conn, "before").await,
        "the v12 guard is unconditional: it refuses inside a declared archive \
         session exactly as it does outside one. That is the state every \
         database written before 0.14.13 is in, and the reason this rung is not \
         a re-issue of the baseline"
    );

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    assert!(
        delete_in_session(&conn, "before").await,
        "after the rung the same delete inside the same session must succeed — \
         this is the capability `archive_branch` is built on"
    );

    register_branch(&conn, "after").await;
    let outside = conn
        .execute("DELETE FROM branches WHERE branch_id = 'after'", ())
        .await;
    assert!(
        outside.is_err(),
        "the rung gates the guard; it must not remove it. `branches` is still \
         append-only to every writer that has not declared a session"
    );
}

/// A v12 guard body under a v13 stamp is refused at open, by name.
///
/// The other half of D-126's repair, which is why this is a separate case: the
/// rung replaces the body, and `verify` is what makes a database that somehow
/// skipped it say so. Without the name in `DELETE_GUARDS` this file opens
/// cleanly and fails at the first abandonment with a trigger abort, which names
/// a trigger rather than the problem.
#[tokio::test]
async fn a_v13_stamp_over_a_v12_branches_guard_is_refused_at_open() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();

    conn.execute("DROP TRIGGER trg_branches_frozen_delete", ())
        .await
        .unwrap();
    conn.execute(V12_BRANCHES_GUARD_DELETE, ()).await.unwrap();
    // The stamp is left at the top: this is the database that claims to have
    // climbed the ladder and did not.

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains("trg_branches_frozen_delete"),
        "the refusal must name the guard whose body is stale: {reason}"
    );
}

/// The v6 → v7 rung rebuilds `links` with the weight constraint, keeps every
/// row, and puts the four triggers back (T2.1, D-082).
///
/// The only rung on the ladder that rewrites a ledger table, so it is the only
/// one where "did the data survive" is a real question. Three things have to
/// hold afterwards and each has failed in some version of this migration
/// somewhere: the rows are all still there, the triggers that were dropped with
/// the old table are back, and the constraint actually bites.
#[tokio::test]
async fn a_v6_database_climbs_to_v7_and_gains_the_weight_check() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    // A v6 database is the current baseline with an unconstrained `links`, so it
    // is built by laying the baseline and rebuilding that one table without the
    // CHECK — for the reason the v5 test gives: a hand-written copy of an old
    // schema is a second description that drifts.
    macrame::schema::run_migrations(&conn).await.unwrap();
    v11_schema::wind_back_to_v11(&conn).await;
    for stmt in [
        "ALTER TABLE links RENAME TO links_old",
        "CREATE TABLE links (
            source_id   TEXT NOT NULL REFERENCES concepts(id),
            target_id   TEXT NOT NULL REFERENCES concepts(id),
            edge_type   TEXT NOT NULL,
            valid_from  TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
            weight      REAL NOT NULL DEFAULT 1.0,
            properties  TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)
        )",
        "DROP TABLE links_old",
    ] {
        conn.execute(stmt, ()).await.unwrap();
    }
    for trigger_ddl in v11_schema::triggers_v11() {
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
    conn.execute("PRAGMA user_version = 6", ()).await.unwrap();

    for id in ["c0", "c1"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    for (etype, weight) in [("A", 1.0), ("B", 0.0), ("C", 2.5)] {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) \
             VALUES ('c0','c1',?1,?2,'9999-12-31T23:59:59.999999Z',?3,'{}',?2)",
            libsql::params![etype, TS, weight],
        )
        .await
        .unwrap();
    }

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    let rows: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(rows, 3, "the rebuild lost rows");

    let triggers: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'trigger' AND tbl_name = 'links'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        triggers, 4,
        "DROP TABLE took the triggers with it and the rung did not put them back"
    );

    for (label, weight) in [("negative", "-1.0"), ("text", "'abc'")] {
        let refused = conn
            .execute(
                &format!(
                    "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
                     valid_to, weight, properties, recorded_at) \
                     VALUES ('c0','c1','Z','{TS}','9999-12-31T23:59:59.999999Z',\
                     {weight},'{{}}','{TS}')"
                ),
                (),
            )
            .await;
        assert!(refused.is_err(), "a {label} weight survived the rung");
    }
}

// ---------------------------------------------------------------------------
// The v7 → v8 rung (B4, D-118, D-119)
// ---------------------------------------------------------------------------

// The v7 shape of `concepts`, its FTS index, the triggers that went with it,
// and `seeded_v7` used to live here. They moved to
// `tests/common/v7_schema.rs` in 0.8.0 so that
// `examples/v8_migration_scale_probe.rs` could measure what the rung COSTS
// against the same pinned fixture this file checks it is CORRECT against,
// rather than against a second copy that would drift (D-124).

async fn count(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// The v7 → v8 rung rebuilds `concepts` with an explicit `rowid_pk`, keeps every
/// row and every rowid, re-keys the FTS index onto the new column, and drops the
/// two indices with no reader (D-118, D-119).
///
/// The rung runs with foreign-key enforcement suspended, which is a thing worth
/// being nervous about, so what is asserted afterwards is not "it reached v8"
/// but the four things the suspension could have broken: the rows, the identity
/// of the rows, the referential integrity of what points at them, and the search
/// index that is keyed on their rowids.
#[tokio::test]
async fn a_v7_database_climbs_to_v8_and_gains_rowid_pk() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    seeded_v7(&conn, &["c0", "c1", "c2", "c3"]).await;

    // Recorded before, compared after: the rung claims to preserve identity,
    // not merely order.
    let rowids_before = ids_by_rowid(&conn, "rowid").await;
    assert_eq!(rowids_before.len(), 4);

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM concepts").await, 4);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM links").await, 3);
    assert_eq!(
        ids_by_rowid(&conn, "rowid_pk").await,
        rowids_before,
        "the rebuild renumbered the concepts; the FTS index is keyed on these"
    );

    // The suspension is the reason this has to be asserted rather than assumed:
    // with enforcement off, an orphan is exactly what the rung could have left.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0,
        "the rung committed a foreign-key violation"
    );

    // Enforcement is back on, on this very connection.
    assert!(
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) \
             VALUES ('c0','nobody','KNOWS',?1,'9999-12-31T23:59:59.999999Z',1.0,'{}',?1)",
            libsql::params![TS],
        )
        .await
        .is_err(),
        "foreign keys were not restored after the rung"
    );

    // The search index still finds every concept, which is the property the
    // whole rung exists to protect.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'findable'"
        )
        .await,
        4,
        "the FTS index did not survive the re-keying"
    );

    // (a): the two indices with no reader are gone, and nothing else went with
    // them.
    let indices = index_names(&conn).await;
    for gone in ["idx_annotations_label", "idx_lc_tgt_active"] {
        assert!(!indices.contains(&gone.to_string()), "{gone} survived v8");
    }
    assert_eq!(
        indices.len(),
        ddl::CREATE_INDICES.len(),
        "v8 left a different index set than CREATE_INDICES declares: {indices:?}"
    );
}

/// **The suspension is load-bearing, and the `links` rows are what make it so.**
///
/// Written because the previous test would pass against a rung with
/// `suspends_foreign_keys: false` if `concepts` had nothing pointing at it —
/// which is the shape D-084 originally specified and the probe refuted. This
/// pins the refutation: the same rebuild, on the same fixture, with enforcement
/// left on, must fail. If it ever stops failing, the flag has become
/// unnecessary and should be removed rather than carried.
#[tokio::test]
async fn the_v8_rung_needs_the_suspension_and_links_rows_prove_it() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    seeded_v7(&conn, &["c0", "c1"]).await;

    // The rung's central act, attempted the way a rung without the flag would
    // reach it: inside a transaction, with enforcement on.
    conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    let dropped = conn.execute("DROP TABLE concepts", ()).await;
    let _ = conn.execute("ROLLBACK", ()).await;

    assert!(
        dropped.is_err(),
        "`DROP TABLE concepts` succeeded with foreign keys enforced, so \
         `suspends_foreign_keys` is buying nothing on this engine. Either the \
         engine changed or the fixture lost its `links` rows — check the latter \
         first, because a `concepts` with no inbound rows makes this pass \
         vacuously."
    );
}

/// A v7 database that already holds an orphaned link is refused, not silently
/// migrated.
///
/// The honest cost of the suspension, pinned so it is a documented behaviour
/// rather than a surprise. `foreign_key_check` runs over the whole database, so
/// a violation that predates the rung fails the rung — and the alternative is
/// worse: enforcement is off during the rebuild, so without the check such a
/// database would migrate cleanly and carry the damage forward under a schema
/// version asserting it had been examined.
#[tokio::test]
async fn a_v7_database_with_a_pre_existing_orphan_is_refused() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    // Off for the seed, which is the only way to write the orphan at all — and
    // is how such a file would have come to exist.
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    seeded_v7(&conn, &["c0", "c1"]).await;
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) \
         VALUES ('c0','ghost','KNOWS',?1,'9999-12-31T23:59:59.999999Z',1.0,'{}',?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains("suspended foreign keys and left a violation") && reason.contains("links"),
        "the refusal should name the check and the table, not merely fail: {reason}"
    );
    assert_eq!(
        user_version(&conn).await,
        7,
        "a refused rung must leave the database honestly at its old version"
    );
}

async fn ids_by_rowid(conn: &libsql::Connection, col: &str) -> Vec<(i64, String)> {
    let mut rows = conn
        .query(
            &format!("SELECT {col}, id FROM concepts ORDER BY {col}"),
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((r.get(0).unwrap(), r.get(1).unwrap()));
    }
    out
}

async fn index_names(conn: &libsql::Connection) -> Vec<String> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get(0).unwrap());
    }
    out
}

/// A database holding a weight the v7 constraint rejects is refused, and told
/// why — it is not migrated with the offending rows altered or dropped.
///
/// Doctrine III is the whole reason: the rung copies every row verbatim, so a
/// row that cannot be represented in the new shape has no correct automatic
/// resolution. Clamping to zero and dropping the row are both edits to an
/// assertion, which is the one thing this ledger does not do. The migration
/// stops before touching anything and names a row, so the operator can decide.
#[tokio::test]
async fn a_negative_weight_already_stored_blocks_the_v7_rung_with_an_explanation() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    macrame::schema::run_migrations(&conn).await.unwrap();
    v11_schema::wind_back_to_v11(&conn).await;
    for stmt in [
        "ALTER TABLE links RENAME TO links_old",
        "CREATE TABLE links (
            source_id   TEXT NOT NULL REFERENCES concepts(id),
            target_id   TEXT NOT NULL REFERENCES concepts(id),
            edge_type   TEXT NOT NULL,
            valid_from  TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
            weight      REAL NOT NULL DEFAULT 1.0,
            properties  TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)
        )",
        "DROP TABLE links_old",
    ] {
        conn.execute(stmt, ()).await.unwrap();
    }
    conn.execute("PRAGMA user_version = 6", ()).await.unwrap();

    for id in ["c0", "c1"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) \
         VALUES ('c0','c1','NEG',?1,'9999-12-31T23:59:59.999999Z',-1.5,'{}',?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    let err = macrame::schema::run_migrations(&conn)
        .await
        .expect_err("the rung cannot represent this row and must say so");
    let msg = err.to_string();
    assert!(
        msg.contains("c0 -> c1") && msg.contains("Doctrine III"),
        "the refusal must name a row and why it will not choose: {msg}"
    );

    // Refused *before* touching anything: still at v6, row still present.
    assert_eq!(user_version(&conn).await, 6);
    let rows: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(rows, 1, "the failed rung was not clean");
}

/// **The point of the D-059 rung: the single-open-interval probe seeks on all
/// three equality columns instead of scanning a source's out-degree.**
///
/// This is the acceptance test the index exists for, and it has to inspect the
/// plan rather than time the insert. A timing assertion would need a hub large
/// enough for the difference to clear the noise — the measured spread only opens
/// up around 2,000 edges — which is a slow test that fails for machine reasons.
/// The plan is the causal claim: D-059 diagnosed the cost as `EXISTS` being
/// served by `idx_lc_traversal_cover` with only `source_id` bound, so what must
/// be asserted is which index is chosen and how much of it is bound.
///
/// The trigger body cannot be handed to `EXPLAIN QUERY PLAN` directly, so the
/// probe's `SELECT` is reproduced here. That is a second copy of the predicate
/// and the risk is real — if the trigger's `WHERE` changes and this does not,
/// the test goes on proving something about a query nobody runs. It is bounded
/// by `the_open_interval_probe_matches_the_trigger` below, which checks the
/// trigger DDL still contains the predicate this test models.
#[tokio::test]
async fn the_single_open_probe_seeks_rather_than_scans() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let probe = "SELECT 1 FROM links_current \
                 WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
                   AND valid_from <> ?4 AND valid_to = ?5";

    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {probe}"), ())
        .await
        .unwrap();
    let mut plan = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        plan.push(r.get::<String>(3).unwrap());
    }
    let step = plan.join(" | ");

    assert!(
        step.contains("idx_lc_open_interval"),
        "the probe is not using its own index: {step}"
    );
    // Three equality columns bound, not one. `(source_id=?)` alone is the
    // pre-D-059 plan and the whole defect — it makes the probe O(out-degree).
    assert!(
        step.contains("source_id=? AND target_id=? AND edge_type=?"),
        "the probe binds fewer columns than the index offers, so it still scans: {step}"
    );
}

/// **The overlap guard's own query seeks on all three equality columns.**
///
/// The guard (D-060) fell into D-059's trap one wave after it was fixed. Its
/// first version carried `AND valid_from < :new_valid_to` — a provably safe
/// narrowing — and that range predicate made `idx_lc_traversal_cover` win as a
/// covering index while binding only `source_id`, so the guard scanned the
/// source's whole out-degree. Measured at **+9.8 ms** on a 90-edge chunk into a
/// 2,000-edge hub, and invisible to every correctness test because the answer
/// was right.
///
/// Pinned here because the failure mode is a *plan*, not a result: nothing about
/// the returned rows changes when this regresses.
#[tokio::test]
async fn the_overlap_guard_seeks_on_all_three_equality_columns() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    // The guard's query, as `connection::OVERLAP_CANDIDATES` states it.
    let probe = "SELECT valid_from, valid_to FROM links_current \
                 WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
                   AND valid_from <> ?4";

    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {probe}"), ())
        .await
        .unwrap();
    let mut plan = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        plan.push(r.get::<String>(3).unwrap());
    }
    let step = plan.join(" | ");

    assert!(
        step.contains("idx_lc_open_interval"),
        "the overlap guard is not using the index added for it: {step}"
    );
    assert!(
        step.contains("source_id=? AND target_id=? AND edge_type=?"),
        "the guard binds fewer columns than the index offers, so it scans the \
         source's out-degree — this is D-059's defect in D-060's guard: {step}"
    );
}

/// **The filtered subgraph walk still uses the traversal index (D-073).**
///
/// Adding `edge_types` and `min_weight` to this query lands in exactly the code
/// where D-064 found that a *narrowing* predicate can push the planner off the
/// index it was written for — a covering index is chosen for containing the
/// columns, not for discriminating between rows. That defect returned the right
/// answer throughout, so only a plan test can see it.
///
/// Both arms are checked, because the filtered one is the new shape and the
/// unfiltered one is what `load_subgraph` still compiles to: a filter that made
/// SQLite abandon `idx_lc_traversal_cover` would slow the walk without changing
/// a single returned row.
#[tokio::test]
async fn the_filtered_subgraph_walk_stays_on_the_traversal_index() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    // The recursive step as `load_subgraph_with` emits it, with and without the
    // edge-type filter.
    let step = |edge_filter: &str| {
        format!(
            "SELECT l.target_id FROM links_current l \
             WHERE l.source_id = ?1 \
               AND l.valid_from <= ?3 AND ?3 < l.valid_to \
               AND l.weight >= ?4{edge_filter}"
        )
    };

    for (label, sql) in [
        ("unfiltered", step("")),
        ("edge-type filtered", step(" AND l.edge_type IN (?5)")),
    ] {
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            plan.push(r.get::<String>(3).unwrap());
        }
        let step = plan.join(" | ");

        assert!(
            step.contains("idx_lc_traversal_cover"),
            "{label}: the filtered walk left its index: {step}"
        );
        assert!(
            step.contains("COVERING INDEX"),
            "{label}: the walk is no longer index-only: {step}"
        );
    }
}

/// The predicate the plan test models is still the predicate the trigger runs.
///
/// Guards the one weakness of testing a reproduced query: a trigger body is not
/// reachable by `EXPLAIN QUERY PLAN`, so the plan test necessarily works on a
/// copy, and a copy can outlive its original.
#[test]
fn the_open_interval_probe_matches_the_trigger() {
    let trigger = ddl::CREATE_TRIGGERS
        .iter()
        .find(|t| t.contains("trg_links_single_open"))
        .expect("trg_links_single_open must exist");

    let flat = trigger.split_whitespace().collect::<Vec<_>>().join(" ");
    for clause in [
        "source_id = NEW.source_id",
        "target_id = NEW.target_id",
        "edge_type = NEW.edge_type",
        "valid_from <> NEW.valid_from",
        "valid_to = '9999-12-31T23:59:59.999999Z'",
    ] {
        assert!(
            flat.contains(clause),
            "the trigger no longer contains {clause:?}; \
             the_single_open_probe_seeks_rather_than_scans models a stale query:\n{flat}"
        );
    }
}

/// **The test that pins the column order, not merely the index's existence.**
///
/// Two things are asserted and the second is the one with teeth. `COVERING`
/// says the recursive step never fetches a base-table row. The seek constraint
/// says how much of the index it walks to get there: with `edge_type` ahead of
/// the range columns the unfiltered traversal — the default, since `edge_types`
/// is empty unless a caller sets it — degrades to `(source_id=?)` and evaluates
/// the valid-time window as a filter across that whole source's slice, while
/// the shipped order gives `(source_id=? AND valid_from<?)` and walks only the
/// slice that can match.
///
/// Both orders are *covering* once `idx_lc_src_active` is dropped, so asserting
/// `COVERING` alone passes under either and proves nothing about the ordering
/// (verified by mutation). The seek text is what distinguishes them (D-042).
#[tokio::test]
async fn the_traversal_walks_inside_the_index_with_and_without_an_edge_type_filter() {
    use macrame::graph::TraversalBuilder;

    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let plan_of = |sql: String| {
        let conn = conn.clone();
        async move {
            let sql = sql.trim().trim_end_matches(';').to_string();
            let mut rows = conn
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
                .await
                .unwrap();
            let mut lines = Vec::new();
            while let Some(r) = rows.next().await.unwrap() {
                lines.push(r.get::<String>(3).unwrap());
            }
            lines
        }
    };

    for (label, sql) in [
        (
            "unfiltered",
            TraversalBuilder::new("A").max_depth(3).build_sql(),
        ),
        (
            "edge-type filtered",
            TraversalBuilder::new("A")
                .max_depth(3)
                .edge_types(vec!["CITES".into()])
                .build_sql(),
        ),
    ] {
        let plan = plan_of(sql).await;
        let step = plan
            .iter()
            .find(|l| l.contains(" l ") || l.ends_with(" l"))
            .unwrap_or_else(|| panic!("{label}: no plan line for links_current: {plan:?}"));
        assert!(
            step.contains("COVERING INDEX idx_lc_traversal_cover"),
            "{label}: recursive step is not index-only: {step}"
        );
        assert!(
            step.contains("valid_from<?"),
            "{label}: the valid-time window is not in the index seek, only a \
             filter over the whole source slice — check the column order: {step}"
        );
    }
}

/// The covering index has the same seek column as `idx_lc_src_active` and
/// strictly more payload, so keeping both would pay a second index write on
/// every assertion for nothing. The v3 → v4 rung drops it; if a later edit
/// reinstates it, that cost comes back silently.
#[tokio::test]
async fn the_subsumed_source_index_is_gone() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let n: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_lc_src_active'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(n, 0, "idx_lc_src_active is subsumed and must not survive");
}

/// The **shipped** traversal CTE keeps D-042's covering index, filtered or not.
///
/// The test above approximates the recursive step by hand, which is one hazard
/// away from testing a query nobody runs. This one explains the exact string
/// `TraversalBuilder::build_sql` emits, so T0.1's rewrite cannot have moved the
/// planner off `idx_lc_traversal_cover` while every returned row stayed correct
/// — D-064's failure mode, and the reason plan shape is a test category here.
#[tokio::test]
async fn the_shipped_traversal_cte_stays_on_the_covering_index() {
    use macrame::graph::TraversalBuilder;

    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    for (label, builder) in [
        ("unfiltered", TraversalBuilder::new("a").max_depth(3)),
        (
            "edge-type filtered",
            TraversalBuilder::new("a")
                .max_depth(3)
                .edge_types(vec!["CITES".to_string()]),
        ),
    ] {
        let sql = builder.build_sql();
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            plan.push(r.get::<String>(3).unwrap());
        }
        let plan = plan.join(" | ");

        assert!(
            plan.contains("idx_lc_traversal_cover"),
            "{label}: the walk left its index: {plan}"
        );
        assert!(
            plan.contains("COVERING INDEX"),
            "{label}: the walk is no longer index-only: {plan}"
        );
    }
}

// ---------------------------------------------------------------------------
// v13 → v14 — the lineage read gets an index to seek on (0.14.14, D-231)
// ---------------------------------------------------------------------------

/// The rung is index-only, so *what it is for* is a plan and not a row count.
///
/// Two assertions, and the second is the one that makes this rung the shape it
/// is rather than the one §15.4 asked for. Before the rung the branched read
/// has no persistent index leading on `branch_id` and SQLite builds one per
/// execution; after it, the two base scans over `links_current` seek
/// `idx_lc_lineage_cut`. And the **trunk** walk is unchanged across the rung —
/// which every single-index shape D-231 measured could not manage, because
/// leading on `branch_id` evicts the trunk walk from `idx_lc_traversal_cover`
/// altogether.
///
/// The fixture winds a real database back rather than building a v13 one:
/// `DROP INDEX` plus a stamp is exactly what a v13 database is, because this
/// rung changes nothing else.
#[tokio::test]
async fn a_v13_database_climbs_to_v14_and_the_lineage_read_stops_building_its_own_index() {
    use macrame::graph::TraversalBuilder;

    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    // A second lineage, so `build_sql` emits the resolved shape below and the
    // plan is the one a forked database actually runs.
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES ('b1', 'main', ?1, ?1)",
        libsql::params!["2026-01-01T00:00:00.000000Z"],
    )
    .await
    .unwrap();

    let branched = TraversalBuilder::new("a")
        .max_depth(3)
        .on_branch("b1")
        .build_sql();
    let trunk = TraversalBuilder::new("a").max_depth(3).build_sql();

    conn.execute("DROP INDEX idx_lc_lineage_cut", ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 13", ()).await.unwrap();

    let before = plan_string(&conn, &branched).await;
    assert!(
        before.contains("AUTOMATIC"),
        "the fixture is not starting from the v13 plan — expected SQLite to be \
         building the index itself, got: {before}"
    );
    assert!(
        !before.contains("idx_lc_lineage_cut"),
        "the index survived the drop: {before}"
    );

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    let after = plan_string(&conn, &branched).await;
    assert!(
        after.contains("COVERING INDEX idx_lc_lineage_cut"),
        "the rung created the index and the branched read did not take it — an \
         index nothing seeks on is D-089's failure, not a schema change: {after}"
    );

    // The half the plan's own rung could not have kept. Asserted after the
    // rung, on the same connection, so it is a statement about the schema this
    // release ships rather than about the one it started from.
    let trunk_plan = plan_string(&conn, &trunk).await;
    assert!(
        trunk_plan.contains("COVERING INDEX idx_lc_traversal_cover"),
        "the new index displaced the trunk walk from its own — which is what \
         D-231 measured every `branch_id`-leading shape doing: {trunk_plan}"
    );
}

/// A v14 stamp over a database that never ran the rung is refused at open.
///
/// The index is in `CREATE_INDICES`, and `verify` compares the declared index
/// names against what the file holds — so this needs no new list to be added
/// to, which is the difference between an index rung and the trigger rung
/// below it. Pinned anyway: the guarantee is that a mis-stamped database is a
/// sentence at open time rather than a branched read that is quietly three
/// times slower, and nothing else in this file would notice that.
#[tokio::test]
async fn a_v14_stamp_over_a_v13_index_set_is_refused_at_open() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    conn.execute("DROP INDEX idx_lc_lineage_cut", ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 14", ()).await.unwrap();

    let err = macrame::schema::run_migrations(&conn)
        .await
        .expect_err("a v14 stamp over a v13 index set");
    let msg = err.to_string();
    assert!(
        msg.contains("idx_lc_lineage_cut"),
        "the refusal does not name what is missing: {msg}"
    );
}

// ---------------------------------------------------------------------------
// v14 → v15 — the ledger is keyed by lineage (0.14.15, D-232)
// ---------------------------------------------------------------------------

/// `links` **as v14 declared it**, pinned as text.
///
/// Pinned for `v11_schema`'s reason and for one more that is specific to this
/// rung: the fixture cannot be built by omitting something. Every v15 object
/// exists at v14 too — same tables, same triggers, same indices — and the only
/// difference is a clause inside one `CREATE TABLE`. So the wind-back has to
/// *rebuild the table backwards*, which means it needs the shape it is winding
/// back to, written out.
const LINKS_V14: &str = r#"
CREATE TABLE links_v14 (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    branch_id   TEXT NOT NULL DEFAULT 'main' REFERENCES branches(branch_id),
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at),
    CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
    CHECK (valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND valid_to GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND 1)
)
"#;

/// Rebuild `links` under the v14 key, rows and all, and put back what the drop
/// took.
///
/// The rung's own recipe run in reverse, which is the only honest fixture here:
/// a database that merely *claims* v14 would be one this rung repairs by
/// accident.
async fn wind_links_back_to_v14(conn: &libsql::Connection) {
    conn.execute(LINKS_V14, ()).await.unwrap();
    conn.execute(
        "INSERT INTO links_v14 (source_id, target_id, edge_type, valid_from, \
         recorded_at, valid_to, weight, properties, branch_id) \
         SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                valid_to, weight, properties, branch_id FROM links",
        (),
    )
    .await
    .unwrap();
    conn.execute("DROP TABLE links", ()).await.unwrap();
    conn.execute("ALTER TABLE links_v14 RENAME TO links", ())
        .await
        .unwrap();

    for ddl in [
        ddl::CREATE_LINKS_CURRENT_SYNC,
        ddl::CREATE_LINKS_SINGLE_OPEN,
        ddl::CREATE_LINKS_LOG_INSERT,
        ddl::CREATE_LINKS_GUARD_DELETE,
    ] {
        conn.execute(ddl, ()).await.unwrap();
    }
    for sql in ddl::CREATE_INDICES {
        if sql.contains("idx_links_recorded_at") || sql.contains("idx_links_target") {
            conn.execute(sql, ()).await.unwrap();
        }
    }
}

/// One `SELECT COUNT(*)`-shaped read, since this test asks for three.
async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// One batch, one edge key, two lineages — refused at v14, written at v15.
///
/// **This is what the rung is for**, and it is a row count rather than a plan
/// because the rung changes what the ledger will accept, not how it is read.
/// The `INSERT`s are issued directly and share a `recorded_at` on purpose: that
/// is not a contrivance, it is precisely what `write_bulk_atomic` and
/// `bulk_import` do — one stamp for the whole batch, by contract (D-014) —
/// which is why §15.4's "unreachable through the crate" stopped being true at
/// 0.14.8 without anything noticing.
///
/// Three further assertions, each covering a way a table rebuild goes wrong
/// quietly:
///
/// * **Every row survives.** A rung that rebuilds the ledger and loses a row
///   has violated Doctrine III whatever its key says.
/// * **The four triggers come back.** `DROP TABLE` takes them, and a database
///   missing `trg_links_current_sync` writes to `links` and never updates
///   `links_current` — reads go stale with no error at all.
/// * **The two indices come back.** They were not the v6 → v7 rung's problem
///   (there was no index on `links` at v7) and they are this one's.
#[tokio::test]
async fn a_v14_database_climbs_to_v15_and_two_lineages_may_assert_one_edge_at_one_instant() {
    const TS: &str = "2026-01-01T00:00:00.000000Z";
    const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    for id in ["a", "b"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 't', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES ('b1', 'main', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    wind_links_back_to_v14(&conn).await;
    conn.execute("PRAGMA user_version = 14", ()).await.unwrap();

    let insert = |branch: &'static str| {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
             valid_to, weight, properties, recorded_at, branch_id) \
             VALUES ('a', 'b', 'LINKS', ?1, ?2, 1.0, '{}', ?1, ?3)",
            libsql::params![TS, FOREVER, branch],
        )
    };

    insert("main").await.unwrap();
    let err = insert("b1")
        .await
        .expect_err("the fixture is not starting from the v14 key");
    assert!(
        err.to_string().contains("UNIQUE constraint failed: links."),
        "the fixture failed for some other reason: {err}"
    );

    let before = scalar(&conn, "SELECT COUNT(*) FROM links").await;

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    insert("b1")
        .await
        .expect("v15 still refuses a second lineage's belief about one edge");

    let after = scalar(&conn, "SELECT COUNT(*) FROM links").await;
    assert_eq!(
        after,
        before + 1,
        "the rebuild did not carry every row across: {before} before, {after} \
         after one insert"
    );

    for name in [
        "trg_links_current_sync",
        "trg_links_single_open",
        "trg_links_log_insert",
        "trg_links_guard_delete",
    ] {
        let found: i64 = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'trigger' AND name = ?1",
                libsql::params![name],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(found, 1, "`DROP TABLE links` took {name} and left it off");
    }

    for name in ["idx_links_recorded_at", "idx_links_target"] {
        let found: i64 = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = ?1",
                libsql::params![name],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            found, 1,
            "the rebuild dropped {name} and did not put it back"
        );
    }

    // The sync trigger is back *and wired*: the row just written reached the
    // materialization on its own lineage. A trigger present but pointed at the
    // old table would satisfy the name check above and fail this.
    let current = scalar(
        &conn,
        "SELECT COUNT(*) FROM links_current WHERE branch_id = 'b1'",
    )
    .await;
    assert_eq!(current, 1, "the restored sync trigger did not fire");
}

/// A v15 stamp over a v14 key is refused at open.
///
/// **The one guarantee this release could not get for free.** Every previous
/// rung added an object with a name, and `verify` finds a missing name without
/// being told to look. A primary key has no name — a v15 stamp over a v14
/// `links` opens cleanly, reads correctly, and then refuses one legal batch in
/// a hundred with raw engine text. So `verify` gained a check on the key
/// itself, and this is what pins it.
#[tokio::test]
async fn a_v15_stamp_over_a_v14_links_key_is_refused_at_open() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    wind_links_back_to_v14(&conn).await;
    conn.execute("PRAGMA user_version = 15", ()).await.unwrap();

    let err = macrame::schema::run_migrations(&conn)
        .await
        .expect_err("a v15 stamp over a v14 key");
    let msg = err.to_string();
    assert!(
        msg.contains("keyed by lineage") && msg.contains("branch_id"),
        "the refusal does not say what is wrong with the table: {msg}"
    );
}

// ---------------------------------------------------------------------------
// v8 → v9 — the concepts delete guard becomes marker-gated (C2, D-126)
// ---------------------------------------------------------------------------

/// The v8 guard body, reproduced here rather than imported.
///
/// A test that asked the crate what v8 looked like would be asking the thing
/// under test, and would pass no matter what the rung did. This is the same
/// discipline `v7_schema.rs` follows and the same trap the plan flagged for this
/// item: a fixture built from today's `ddl::` constants already has the change.
const CONCEPTS_GUARD_V8: &str = "
    CREATE TRIGGER trg_concepts_guard_delete
    BEFORE DELETE ON concepts
    BEGIN
        SELECT RAISE(ABORT, 'macrame: concepts are never physically archived (D-022)');
    END;
";

/// Put a v9 database back into the v8 state this rung exists to leave behind:
/// the unconditional guard, and the version stamp to match.
async fn downgrade_guard_to_v8(conn: &libsql::Connection) {
    v11_schema::wind_back_to_v11(conn).await;
    conn.execute("DROP TRIGGER trg_concepts_guard_delete", ())
        .await
        .unwrap();
    conn.execute(CONCEPTS_GUARD_V8, ()).await.unwrap();
    conn.execute("PRAGMA user_version = 8", ()).await.unwrap();
}

async fn guard_sql(conn: &libsql::Connection) -> String {
    conn.query(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' \
         AND name = 'trg_concepts_guard_delete'",
        (),
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .expect("the concepts delete guard should exist")
    .get(0)
    .unwrap()
}

#[tokio::test]
async fn a_v8_database_climbs_past_v9_and_the_concepts_guard_becomes_conditional() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    downgrade_guard_to_v8(&conn).await;

    // Precondition, asserted rather than assumed: the fixture really is v8.
    assert!(
        guard_sql(&conn).await.contains("never physically archived"),
        "the fixture did not start from the v8 guard"
    );

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);
    let sql = guard_sql(&conn).await;
    assert!(
        sql.contains(ddl::ARCHIVE_SESSION_MARKER),
        "the guard is not marker-gated after the rung: {sql}"
    );
    assert!(
        !sql.contains("never physically archived"),
        "the v8 body survived the rung: {sql}"
    );
}

/// **The rung cannot be replaced by re-issuing the baseline, and this is the
/// measurement that says so** (D-126).
///
/// `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the old body. That
/// is the whole reason C2 needs a schema rung rather than a baseline re-issue,
/// and it is a claim about libSQL rather than about this crate — so it is
/// verified against the engine here, not argued in a comment.
#[tokio::test]
async fn re_issuing_the_baseline_guard_keeps_the_v8_body() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    downgrade_guard_to_v8(&conn).await;

    conn.execute(ddl::CREATE_CONCEPTS_GUARD_DELETE, ())
        .await
        .unwrap();

    let sql = guard_sql(&conn).await;
    assert!(
        sql.contains("never physically archived"),
        "IF NOT EXISTS replaced the body — D-126's premise no longer holds and \
         the v8 → v9 rung may be unnecessary: {sql}"
    );
}

/// **The other half of D-126's hole: `verify` used to compare names only.**
///
/// A guard with the right name and a pre-v9 body passed verification in
/// silence, which is what made the stale-guard failure mode invisible. A
/// database stamped v9 whose guard is not marker-gated must now be refused —
/// otherwise the rung above is the only thing standing between a user and a
/// concept archival that aborts at the trigger.
#[tokio::test]
async fn a_v9_stamp_over_an_ungated_guard_is_refused() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    // The stamp says v9; the guard says v8. Nothing in the ladder runs.
    conn.execute("DROP TRIGGER trg_concepts_guard_delete", ())
        .await
        .unwrap();
    conn.execute(CONCEPTS_GUARD_V8, ()).await.unwrap();

    let reason = refusal_reason(macrame::schema::run_migrations(&conn).await.unwrap_err());
    assert!(
        reason.contains("trg_concepts_guard_delete") && reason.contains("archive-session"),
        "the refusal should name the guard and what it lacks: {reason}"
    );
}

/// The climb from v7 passes *through* v8's guard, not around it.
///
/// `add_concepts_rowid_pk` rebuilds `concepts` and restores its triggers from
/// `CREATE_TRIGGERS`, which is today's DDL — so without the pinned
/// `CONCEPTS_GUARD_DELETE_V8` the v7 → v8 rung would install the v9 body and
/// this whole ladder would reach v9 without the v8 → v9 rung ever doing
/// anything. There is no way to observe that from the outside once the climb
/// finishes, so what is asserted is the end state plus the fact that the guard
/// is genuinely functional: an ad-hoc delete is refused, and the same delete
/// inside a declared session is not.
#[tokio::test]
async fn a_v7_database_climbs_all_the_way_to_the_top_with_a_working_guard() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seeded_v7(&conn, &["c1", "c2"]).await;
    conn.execute("PRAGMA user_version = 7", ()).await.unwrap();

    macrame::schema::run_migrations(&conn).await.unwrap();
    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);

    let res = conn
        .execute("DELETE FROM concepts WHERE id = 'c1'", ())
        .await;
    assert!(
        res.is_err(),
        "an ad-hoc concept delete must still be refused"
    );

    // Inside a declared archive session the same delete is legal — which is the
    // capability the rung exists to grant, and the thing v8 could not do.
    conn.execute(
        &format!("CREATE TABLE {} (x)", ddl::ARCHIVE_SESSION_MARKER),
        (),
    )
    .await
    .unwrap();
    // Links first. `seeded_v7` gives c1 an outbound edge, so deleting the
    // concept while that edge is hot fails on the foreign key rather than on the
    // guard — which is precisely the downstream relationship C1's predicate
    // describes (D-128), reached here from the schema side.
    conn.execute(
        "DELETE FROM links WHERE source_id = 'c1' OR target_id = 'c1'",
        (),
    )
    .await
    .unwrap();
    conn.execute("DELETE FROM concepts WHERE id = 'c1'", ())
        .await
        .expect("a concept delete inside an archive session must be permitted at v9");
    conn.execute(&format!("DROP TABLE {}", ddl::ARCHIVE_SESSION_MARKER), ())
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// v9 → v10 — the concepts insert log trigger becomes marker-gated (C3)
// ---------------------------------------------------------------------------

/// The v9 body, reproduced rather than imported, for the reason
/// [`CONCEPTS_GUARD_V8`] gives.
const CONCEPTS_LOG_INSERT_V9_FIXTURE: &str = "
    CREATE TRIGGER trg_concepts_log_insert
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
";

async fn log_insert_sql(conn: &libsql::Connection) -> String {
    conn.query(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' \
         AND name = 'trg_concepts_log_insert'",
        (),
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .expect("the concepts insert log trigger should exist")
    .get(0)
    .unwrap()
}

#[tokio::test]
async fn a_v9_database_climbs_to_v10_and_the_insert_log_becomes_marker_gated() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    v11_schema::wind_back_to_v11(&conn).await;

    conn.execute("DROP TRIGGER trg_concepts_log_insert", ())
        .await
        .unwrap();
    conn.execute(CONCEPTS_LOG_INSERT_V9_FIXTURE, ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 9", ()).await.unwrap();

    assert!(
        !log_insert_sql(&conn)
            .await
            .contains(ddl::ARCHIVE_SESSION_MARKER),
        "the fixture did not start from the v9 trigger"
    );

    macrame::schema::run_migrations(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);
    assert!(
        log_insert_sql(&conn)
            .await
            .contains(ddl::ARCHIVE_SESSION_MARKER),
        "the insert log trigger is not marker-gated after the rung"
    );
}

/// **What the rung is *for*, asserted as behaviour rather than as DDL text.**
///
/// Outside a session a concept insert logs, as it always has. Inside one it does
/// not — which is what makes rehydration a physical move rather than a write,
/// and what stops a rehydrated concept from outranking its own retirement in the
/// fold (C3).
#[tokio::test]
async fn an_insert_inside_a_session_writes_no_log_row_and_outside_one_still_does() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    let log_rows = |conn: libsql::Connection| async move {
        conn.query("SELECT COUNT(*) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap()
    };

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('outside', 'T', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();
    assert_eq!(
        log_rows(conn.clone()).await,
        1,
        "an ordinary concept insert must still be logged"
    );

    conn.execute(
        &format!("CREATE TABLE {} (x)", ddl::ARCHIVE_SESSION_MARKER),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('inside', 'T', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();
    conn.execute(&format!("DROP TABLE {}", ddl::ARCHIVE_SESSION_MARKER), ())
        .await
        .unwrap();

    assert_eq!(
        log_rows(conn.clone()).await,
        1,
        "an insert inside an archive session wrote a transaction_log row; \
         rehydration is a move back and mints no transaction-time facts"
    );
}
