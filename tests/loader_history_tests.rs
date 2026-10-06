//! Historical subgraph closure: belief-at-instant hydration (0.19.1, D-289/D-290).
//!
//! The acceptance suite for the consolidated issue document
//! (`docs/macrame-0.19.1-loader-temporal-closure-consolidated.md`, §6.2):
//! a historical load must keep edges whose endpoint concept retired *after*
//! the requested instant (T1), with `execute_ids` agreeing (T2), while
//! current belief (T3), never-existed ids (T4), and pre-instant retirement
//! (T5) stay exactly as they were. T6 pins the mode contract, T7 the cold
//! refusal, and the unstated-mode test pins the §4.4 option-(a) decision.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{AttributeMode, EdgeAssertion, TraversalBuilder};
use macrame::temporal::archive;
use macrame::{ConceptUpsert, Database, DbError};

const VFROM: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const BUDGET: usize = 10_000_000;

/// `MAX(recorded_at)` over the hot log: the instant the fixture's first
/// generation of writes is fully believed.
async fn max_recorded_at(db: &Database) -> String {
    let mut rows = db
        .read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// §6.1 fixture: `A` live; `B` v1 live; `A→B CALLS` open across `t1`; then
/// `B` re-upserted retired with title v2, the edge left open.
///
/// Returns the database and `t1`. The retirement write is what makes this
/// the reproducing shape: retiring only the edge does not (C1).
async fn retired_after_fixture(harness: &TestHarness) -> (Database, String) {
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new("a::x", "a-v1")
            .content("doc-a")
            .valid_from(VFROM)
            .valid_to(OPEN)
            .retired(false),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("a::y", "b-v1")
            .content("doc-b")
            .valid_from(VFROM)
            .valid_to(OPEN)
            .retired(false),
    )
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a::x", "a::y", "CALLS")
            .valid_from(VFROM)
            .weight(1.0)
            .properties("{}"),
    )
    .await
    .unwrap();
    let t1 = max_recorded_at(&db).await;
    // THE load-bearing step: retire the ENDPOINT CONCEPT after t1.
    db.upsert_concept(
        ConceptUpsert::new("a::y", "b-v2")
            .content("doc-b")
            .valid_from(VFROM)
            .valid_to(OPEN)
            .retired(true),
    )
    .await
    .unwrap();
    (db, t1)
}

fn historical(builder: TraversalBuilder, t1: &str) -> TraversalBuilder {
    builder
        .max_depth(2)
        .as_of_recorded(t1)
        .attribute_mode(AttributeMode::AtTime)
}

#[tokio::test]
async fn t1_historical_loader_keeps_edge_whose_concept_retired_after_instant() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;

    let hist = db
        .load_subgraph_with(&historical(TraversalBuilder::new("a::x"), &t1), &t1, BUDGET)
        .await
        .unwrap();
    assert!(
        hist.is_closed(),
        "closure holds at the instant, not just live"
    );
    assert_eq!(hist.node_count(), 2, "B retired after t1 hydrates at t1");
    assert!(
        hist.out_edges("a::x")
            .iter()
            .any(|e| e.node(&hist) == "a::y"),
        "the edge live at t1 survives a historical load at t1"
    );
    assert_eq!(
        hist.node("a::y").unwrap().title(),
        "b-v1",
        "belief-at-t1 wears the at-t1 title, not today's"
    );

    // Current-belief control: the bare builder reads current belief however
    // far past `now_ts` lies (C2), so the now-retired endpoint stays absent.
    let bare = db
        .load_subgraph_with(&TraversalBuilder::new("a::x").max_depth(2), &t1, BUDGET)
        .await
        .unwrap();
    assert_eq!(bare.node_count(), 1);
    assert!(
        !bare
            .out_edges("a::x")
            .iter()
            .any(|e| e.node(&bare) == "a::y"),
        "current belief excludes the now-retired endpoint"
    );
}

#[tokio::test]
async fn t1_historical_loader_hydrates_opt_in_content_from_the_fold() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;

    // Default: content is not carried, even historically.
    let hist = db
        .load_subgraph_with(&historical(TraversalBuilder::new("a::x"), &t1), &t1, BUDGET)
        .await
        .unwrap();
    assert_eq!(hist.node("a::y").unwrap().content(), None);

    // Opted in: the at-t1 payload's content, not today's.
    let with_content = db
        .load_subgraph_with(
            &historical(TraversalBuilder::new("a::x").content(true), &t1),
            &t1,
            BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(with_content.node("a::y").unwrap().content(), Some("doc-b"));
}

#[tokio::test]
async fn t2_historical_ids_agree_with_loader() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;

    let ids = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded(&t1)
        .execute_ids(db.read_conn(), &t1)
        .await
        .unwrap();
    assert_eq!(ids, vec!["a::x".to_string(), "a::y".to_string()]);
}

#[tokio::test]
async fn t3_current_belief_unchanged() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;
    let t2 = max_recorded_at(&db).await;
    assert!(t2 >= t1);

    // Bare builder at a later now: still current belief, still absent.
    let bare = db
        .load_subgraph_with(&TraversalBuilder::new("a::x").max_depth(2), &t2, BUDGET)
        .await
        .unwrap();
    assert_eq!(bare.node_count(), 1);
    assert_eq!(bare.edge_count(), 0);

    // The current-belief statement is untouched by the historical arm.
    let sql = TraversalBuilder::new("a::x").max_depth(2).build_sql();
    assert!(sql.contains("c.retired = 0"), "live join stays: {sql}");
    assert!(
        !sql.contains("transaction_log"),
        "no fold without a recorded instant: {sql}"
    );
}

#[tokio::test]
async fn t4_never_existed_stays_absent() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;

    // A start id with no concept row anywhere: empty graph, no error (C3).
    let graph = db
        .load_subgraph_with(&historical(TraversalBuilder::new("zzz"), &t1), &t1, BUDGET)
        .await
        .unwrap();
    assert_eq!(graph.node_count(), 0);
    assert_eq!(graph.edge_count(), 0);

    let ids = TraversalBuilder::new("zzz")
        .max_depth(2)
        .as_of_recorded(&t1)
        .execute_ids(db.read_conn(), &t1)
        .await
        .unwrap();
    assert!(ids.is_empty());
}

#[tokio::test]
async fn t5_pre_instant_retirement_stays_dropped() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new("a::x", "a-v1")
            .valid_from(VFROM)
            .valid_to(OPEN)
            .retired(false),
    )
    .await
    .unwrap();
    // Retired BEFORE the instant under test: the lower bound (C8).
    db.upsert_concept(
        ConceptUpsert::new("a::y", "b-v1")
            .valid_from(VFROM)
            .valid_to(OPEN)
            .retired(true),
    )
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a::x", "a::y", "CALLS")
            .valid_from(VFROM)
            .weight(1.0)
            .properties("{}"),
    )
    .await
    .unwrap();
    let t1 = max_recorded_at(&db).await;

    let hist = db
        .load_subgraph_with(&historical(TraversalBuilder::new("a::x"), &t1), &t1, BUDGET)
        .await
        .unwrap();
    assert_eq!(hist.node_count(), 1, "retired-as-of-t1 stays invisible");
    assert_eq!(hist.edge_count(), 0);

    let ids = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded(&t1)
        .execute_ids(db.read_conn(), &t1)
        .await
        .unwrap();
    assert_eq!(ids, vec!["a::x".to_string()]);
}

#[tokio::test]
async fn t6_attribute_modes_keep_their_meaning() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;
    let conn = db.read_conn();

    // AtTime (gated on §4.2: pre-fix this fails with a MISSING node, C4/C8).
    let at_time = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded(&t1)
        .attribute_mode(AttributeMode::AtTime)
        .execute(conn, &t1)
        .await
        .unwrap();
    let b = at_time
        .iter()
        .find(|n| n.id == "a::y")
        .expect("AtTime sees B");
    assert_eq!(b.title, "b-v1");

    // Current: historical topology, live text — stated explicitly, never a
    // silent AtTime. Live text means today's rows, and today B is retired:
    // absent here is the pin that Current did not quietly become AtTime.
    let current = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded(&t1)
        .attribute_mode(AttributeMode::Current)
        .execute(conn, &t1)
        .await
        .unwrap();
    assert!(current.iter().any(|n| n.id == "a::x"));
    assert!(
        current.iter().find(|n| n.id == "a::y").is_none(),
        "Current hydrates live rows, where B is retired — not the instant's"
    );

    // Omit on the loader: historical topology, no text — the nodes exist as
    // closure keys with empty titles, because the mode omits the join.
    let omitted = db
        .load_subgraph_with(
            &TraversalBuilder::new("a::x")
                .max_depth(2)
                .as_of_recorded(&t1)
                .attribute_mode(AttributeMode::Omit),
            &t1,
            BUDGET,
        )
        .await
        .unwrap();
    assert!(omitted.is_closed());
    assert_eq!(omitted.node_count(), 2);
    assert_eq!(omitted.edge_count(), 1);
    assert_eq!(omitted.node("a::y").unwrap().title(), "");
}

#[tokio::test]
async fn t7_cold_instant_still_refuses() {
    let harness = TestHarness::new();
    // Raw SQL throughout: deterministic `recorded_at` stamps, and a writable
    // handle for `archive` (`Database::read_conn` is read-only). Column lists
    // mirror `temporal_tests`: the log triggers do the rest.
    let raw = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let w = raw.connect().unwrap();
    macrame::schema::run_migrations(&w).await.unwrap();
    for (id, title) in [("a::x", "a-v1"), ("a::y", "b-v1")] {
        w.execute(
            "INSERT INTO concepts (id, title, content, valid_from, recorded_at) VALUES (?1, ?2, 'doc', '2026-01-01T00:00:00.000000Z', '2026-02-01T00:00:00.000000Z')",
            [id, title],
        )
        .await
        .unwrap();
    }
    w.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('a::x', 'a::y', 'CALLS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-02-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();
    let t1 = "2026-02-01T00:00:00.000000Z";
    // Retire B after t1, then move the superseded v1 row cold.
    w.execute(
        "UPDATE concepts SET title = 'b-v2', retired = 1, recorded_at = '2026-03-01T00:00:00.000000Z' WHERE id = 'a::y'",
        (),
    )
    .await
    .unwrap();
    let archive_path = harness.temp_dir.path().join("t7_archive.db");
    let report = archive(
        &w,
        "2026-02-15T00:00:00.000000Z",
        "2026-07-30T12:00:00.000000Z",
        &archive_path,
    )
    .await
    .expect("archive session should succeed");
    assert_eq!(
        report.log_entries_archived, 1,
        "the superseded v1 entry moves; newest-per-partition stays hot"
    );

    let db = Database::open(&harness.db_path).await.unwrap();
    let err = db
        .load_subgraph_with(&historical(TraversalBuilder::new("a::x"), t1), t1, BUDGET)
        .await
        .expect_err("a cold instant is a named refusal, not a short graph");
    assert!(
        matches!(err, DbError::RecordedInstantUnreachable { .. }),
        "wrong error: {err:?}"
    );

    // Current belief is unaffected by the cold move.
    let bare = db
        .load_subgraph_with(&TraversalBuilder::new("a::x").max_depth(2), t1, BUDGET)
        .await
        .unwrap();
    assert_eq!(bare.node_count(), 1);
}

#[tokio::test]
async fn unstated_mode_with_recorded_instant_refuses_on_loader() {
    let harness = TestHarness::new();
    let (db, t1) = retired_after_fixture(&harness).await;

    // §4.4 option (a): with a recorded instant the mode must be stated. The
    // loader reuses `resolved_mode` (D-085) rather than guessing.
    let err = db
        .load_subgraph_with(
            &TraversalBuilder::new("a::x")
                .max_depth(2)
                .as_of_recorded(&t1),
            &t1,
            BUDGET,
        )
        .await
        .expect_err("unstated mode + recorded instant must refuse");
    assert!(
        matches!(err, DbError::AttributeModeUnstated { .. }),
        "wrong error: {err:?}"
    );
}

#[test]
fn historical_sql_folds_concepts_at_the_recorded_slot() {
    // §6.3: new pins for the historical shapes only. Trunk shape binds the
    // recorded instant at ?5 (BRANCH_SLOT, unbranched), which `bind_params`
    // already fills — the fold reuses the slot rather than moving layout.
    let sql = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded("2026-06-01T00:00:00.000000Z")
        .build_sql();
    assert!(sql.contains("FROM walk w JOIN ("), "folded join: {sql}");
    assert!(
        sql.contains("recorded_at <= ?5"),
        "reuses the recorded slot: {sql}"
    );
    assert!(
        sql.contains("COALESCE(json_extract(payload, '$.retired'), 0) = 0"),
        "retirement read at the instant: {sql}"
    );
    assert!(!sql.contains("c.retired = 0"), "no live filter: {sql}");

    let limited = TraversalBuilder::new("a::x")
        .max_depth(2)
        .as_of_recorded("2026-06-01T00:00:00.000000Z")
        .limit(10)
        .build_sql();
    assert!(
        limited.contains("COALESCE(json_extract(payload, '$.retired'), 0) = 0"),
        "limited projection folds too: {limited}"
    );
    assert!(
        limited.contains("SELECT COUNT(*) AS n FROM walk"),
        "count anchor survives: {limited}"
    );
}
