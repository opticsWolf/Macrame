mod harness;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::schema::ddl;
use macrame::schema::migrations::{self, SCHEMA_VERSION};

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
            assert_eq!(to, SCHEMA_VERSION);
            reason
        }
        other => panic!("expected DbError::Migration, got {other:?}"),
    }
}

#[tokio::test]
async fn fresh_database_reaches_the_baseline_version() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;

    migrations::run(&conn).await.unwrap();

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

    migrations::run(&conn).await.unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('c1', 'T', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    migrations::run(&conn).await.unwrap();

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
    migrations::run(&conn).await.unwrap();
    conn.execute(
        &format!("PRAGMA user_version = {}", SCHEMA_VERSION + 7),
        (),
    )
    .await
    .unwrap();

    let reason = refusal_reason(migrations::run(&conn).await.unwrap_err());
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
    migrations::run(&conn).await.unwrap();
    conn.execute("PRAGMA user_version = 1", ()).await.unwrap();

    let reason = refusal_reason(migrations::run(&conn).await.unwrap_err());
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

    let reason = refusal_reason(migrations::run(&conn).await.unwrap_err());
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

    let _ = migrations::run(&conn).await.unwrap_err();

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
    migrations::run(&conn).await.unwrap();

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
    conn.execute(ddl::CREATE_CONCEPTS_TABLE, ()).await.unwrap();
    conn.execute(ddl::CREATE_LINKS_TABLE, ()).await.unwrap();
    conn.execute(ddl::CREATE_LINKS_CURRENT_TABLE, ())
        .await
        .unwrap();
    conn.execute(ddl::CREATE_TRANSACTION_LOG_TABLE, ())
        .await
        .unwrap();
    for index_ddl in ddl::CREATE_INDICES {
        // idx_annotations_label has no table yet; the rest do.
        let _ = conn.execute(index_ddl, ()).await;
    }
    for trigger_ddl in ddl::CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
    conn.execute("PRAGMA user_version = 2", ()).await.unwrap();

    migrations::run(&conn).await.unwrap();

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

    conn.query("SELECT concept_id, label, value, computed_at FROM analytics_annotations", ())
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

    migrations::run(&conn).await.unwrap();
    conn.execute("DROP INDEX idx_lc_open_interval", ())
        .await
        .unwrap();
    conn.execute("PRAGMA user_version = 5", ()).await.unwrap();

    migrations::run(&conn).await.unwrap();

    assert_eq!(user_version(&conn).await, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 6, "the ladder's top moved without this test");

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
    migrations::run(&conn).await.unwrap();

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
    migrations::run(&conn).await.unwrap();

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
    migrations::run(&conn).await.unwrap();

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
    migrations::run(&conn).await.unwrap();

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
        ("unfiltered", TraversalBuilder::new("A").max_depth(3).build_sql()),
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
    migrations::run(&conn).await.unwrap();

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
    migrations::run(&conn).await.unwrap();

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
