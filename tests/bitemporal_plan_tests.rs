//! What a cross-axis read costs, and why no index serves it (W10.6, F-33).
//!
//! W7.1 made bitemporal predicates expressible: a traversal can state a
//! valid-time instant and a transaction-time instant at once. F-33 is that
//! nobody had asked what that costs, and that the answer the literature is
//! usually cited for — an R\*Tree over the two axes — does not fit this schema.
//! The plan's instruction was *measure before building anything*, and the
//! measurement closed it as **no index needed**
//! ([D-196](../docs/architecture/s13-decision-register.md#d-196)).
//!
//! This file exists so that closure is a gate rather than a paragraph. A
//! decision recorded as "we measured and there was nothing to build" is exactly
//! the kind that gets re-litigated by someone who did not, and the three claims
//! it rests on are all checkable:
//!
//! 1. The transaction-time bound **already seeks**, on `idx_txlog_time`.
//! 2. Adding the valid-time instant changes the plan **not at all**, because
//!    that predicate is not applied to a table. It is applied to the walk's
//!    join against a materialised fold whose columns come out of
//!    `json_extract` — one derivation after any index could be consulted.
//! 3. A two-dimensional candidate index over `(recorded_at, json_extract(…))`
//!    **is picked and is used on its leading column only**. It is a wider
//!    `idx_txlog_time` with a dead second column, which is the concrete cost of
//!    the thing F-33 asked whether to build.
//!
//! If any of the three stops holding, the decision is due for review and this
//! file is how that gets noticed. `examples/bitemporal_index_probe.rs` is the
//! sweep these came from, including the arm on the window sort that is the
//! plans' actual cost driver.
//!
//! # Why this does not use `tests/common/plan_fixture.rs`
//!
//! That fixture inserts into `concepts` and `links` directly, so its
//! `transaction_log` is **empty** — and the transaction-time half of every
//! question here reads exactly that table. The database below is written
//! through the public API and analysed with the crate's own `analyze()`, so the
//! log is populated the way production populates it and the statistics are the
//! ones a caller gets.

#[path = "common/harness.rs"]
mod harness;
#[path = "common/plan_fixture.rs"]
mod plan_fixture;

use harness::TestHarness;
use libsql::Builder;
use macrame::prelude::*;
use plan_fixture::{counts, counts_of};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const VALID_AT: &str = "2026-06-01T00:00:00.000000Z";
const RECORDED_AT: &str = "2026-06-01T00:00:00.000000Z";

/// The same skew `plan_fixture` uses: one hub, many leaves.
const HUB_EDGES: usize = 150;
const LEAF_EDGES: usize = 60;
const CONCEPTS: usize = 260;

/// A written-through, analysed database, handed back as a plain connection.
///
/// The handle is closed before the connection is opened, deliberately: every
/// question here is about a plan, the plan needs a writable connection for the
/// candidate-index arm, and a second writer alongside a live actor is a
/// complication none of these tests are about.
async fn written_and_analysed(harness: &TestHarness) -> libsql::Connection {
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();

    let concepts: Vec<ConceptUpsert> = (0..CONCEPTS)
        .map(|i| ConceptUpsert::new(format!("c{i:04}"), "N").valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();

    let mut edges = Vec::new();
    for i in 1..=HUB_EDGES {
        edges.push(EdgeAssertion::new("c0000", format!("c{i:04}"), "LINKS").valid_from(TS));
    }
    for i in (HUB_EDGES + 1)..=(HUB_EDGES + LEAF_EDGES) {
        edges.push(
            EdgeAssertion::new(format!("c{i:04}"), format!("c{:04}", i + 1), "LINKS")
                .valid_from(TS),
        );
    }
    db.write_bulk_atomic(edges).await.unwrap();
    db.analyze().await.unwrap();
    db.close().await.unwrap();

    Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn plan_of(conn: &libsql::Connection, sql: &str) -> String {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut lines = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        lines.push(r.get::<String>(3).unwrap());
    }
    lines.join(" | ")
}

fn walk() -> TraversalBuilder {
    TraversalBuilder::new("c0000").max_depth(2)
}

// ---------------------------------------------------------------------------
// The three claims F-33 closes on
// ---------------------------------------------------------------------------

/// **The transaction-time bound already seeks** (0.13.23, W10.6).
///
/// The first of the plan's options is "a second one-dimensional index, per
/// domain". For the transaction-time domain there already is one, it is chosen,
/// and the guard on the fixture is that the log is not empty — an empty table
/// is seekable in a way that proves nothing.
#[tokio::test]
async fn a_transaction_time_read_seeks_rather_than_scans() {
    let harness = TestHarness::new();
    let conn = written_and_analysed(&harness).await;

    let mut rows = conn
        .query("SELECT COUNT(*) FROM transaction_log", ())
        .await
        .unwrap();
    let logged: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert!(
        logged > 100,
        "the fixture logged {logged} rows, which is too few for a plan on this \
         table to mean anything"
    );

    let plan = plan_of(&conn, &walk().as_of_recorded(RECORDED_AT).build_sql()).await;
    assert!(
        plan.contains("SEARCH transaction_log USING INDEX idx_txlog_time"),
        "the transaction-time bound stopped seeking, which is the one arm of \
         F-33 that was already answered: {plan}"
    );
}

/// **Stating a valid instant as well changes the plan not at all, and that is
/// the structural reason F-33 closes** (0.13.23, W10.6).
///
/// The two axes never meet on a table. `recorded_at` is a column of
/// `transaction_log`; the valid-time bound is applied to the walk's join
/// against `links_at_tx`, a materialised fold whose `valid_from` and `valid_to`
/// are produced by `json_extract` — one derivation past anything an index could
/// be consulted for. So there is no cross-axis access path to index, and the
/// question F-33 asked has no object rather than a difficult answer.
///
/// Asserted as **plan equality**, which is stronger than "both use the index"
/// and is the form that would notice a change in either direction.
#[tokio::test]
async fn adding_the_valid_instant_does_not_reach_the_plan() {
    let harness = TestHarness::new();
    let conn = written_and_analysed(&harness).await;

    let recorded_only = plan_of(&conn, &walk().as_of_recorded(RECORDED_AT).build_sql()).await;
    let both = plan_of(
        &conn,
        &walk()
            .as_of_valid(VALID_AT)
            .as_of_recorded(RECORDED_AT)
            .build_sql(),
    )
    .await;

    assert_eq!(
        both, recorded_only,
        "the valid-time instant now reaches the plan. That is not a regression \
         — it means the two axes have started meeting somewhere indexable, \
         and F-33's decision (D-196: nothing to build) is due for review"
    );

    // And the fixture is capable of showing a difference: the valid-time
    // instant alone gets an entirely different plan, so the equality above is
    // about where the predicate lands and not about the builder ignoring it.
    let valid_only = plan_of(&conn, &walk().as_of_valid(VALID_AT).build_sql()).await;
    assert_ne!(
        valid_only, recorded_only,
        "the premise has stopped holding: all three arms now plan alike, so \
         this test could not tell an ignored instant from a relocated one"
    );
}

/// **A two-dimensional index is picked and used on its leading column only**
/// (0.13.23, W10.6, D-196).
///
/// This is the option F-33 names — a covering composite over the two temporal
/// bounds — built here rather than argued about. The planner does take it, in
/// preference to `idx_txlog_time`, and the plan says `(recorded_at<?)`: the
/// valid-time column is never consulted, because the predicate that would use
/// it is evaluated against a derived relation. So the composite is a wider
/// `idx_txlog_time` that costs an index write per log row, forever, and returns
/// nothing.
///
/// The R\*Tree option needs no test to reject: `rtree` coordinates are float32
/// and `rtree_i32` is int32, so neither holds a microsecond epoch, and the
/// recheck it would need is against columns that do not exist on the table.
#[tokio::test]
async fn a_two_dimensional_candidate_is_used_as_a_one_dimensional_one() {
    let harness = TestHarness::new();
    let conn = written_and_analysed(&harness).await;

    let sql = walk()
        .as_of_valid(VALID_AT)
        .as_of_recorded(RECORDED_AT)
        .build_sql();
    let before = plan_of(&conn, &sql).await;

    conn.execute(
        "CREATE INDEX probe_txlog_two_d ON transaction_log \
         (recorded_at, json_extract(payload, '$.valid_from'))",
        (),
    )
    .await
    .unwrap();
    let _ = conn.query("ANALYZE", ()).await.unwrap();

    let after = plan_of(&conn, &sql).await;
    assert!(
        after.contains("USING INDEX probe_txlog_two_d (recorded_at<?)"),
        "the candidate index is either unused or used on more than its leading \
         column. Either way D-196's arithmetic changes and F-33 wants \
         re-opening: {after}"
    );
    assert_eq!(
        after.replace("probe_txlog_two_d", "idx_txlog_time"),
        before,
        "the two-dimensional candidate changed the shape of the plan and not \
         just the name of the index it seeks on, which is the outcome D-196 \
         measured as not happening"
    );
}

/// **The cross-axis read costs what the transaction-time read costs, to the
/// cursor** (§14 item 11).
///
/// The three tests above are about the *plan* — what the planner says it will
/// do. This is about the program it compiles to, and it is the stronger form of
/// the same claim: plan equality is an assertion about text SQLite chooses to
/// print, and `(4, 2, 5)` on both arms is an assertion about work the VDBE
/// actually does. Adding the valid instant opens no cursor, issues no seek and
/// rewinds nothing extra.
///
/// **Why the valid-time-only arm is here too.** Without it the pinned pair is
/// passed by any build where both numbers happen to be equal, including one
/// where the transaction-time bound has quietly stopped being applied. The
/// valid-only arm is a different shape — `(3, 2, 1)` — so the fixture is shown
/// to be capable of producing a different answer before the two that matter are
/// asserted equal.
///
/// **These integers were measured before they were written down**, by
/// `examples/bitemporal_index_probe.rs`, which is the sweep [D-196] closed F-33
/// on and prints these same three triples. When this goes red on a dependency
/// bump it is a plan review and not a bug: re-run the probe, read the sweep, and
/// change the numbers deliberately ([D-195]).
///
/// [D-195]: ../docs/architecture/s13-decision-register.md
/// [D-196]: ../docs/architecture/s13-decision-register.md
#[tokio::test]
async fn a_cross_axis_read_costs_what_the_transaction_time_read_costs() {
    let harness = TestHarness::new();
    let conn = written_and_analysed(&harness).await;

    let valid_only = counts_of(&conn, &walk().as_of_valid(VALID_AT).build_sql()).await;
    let recorded_only = counts_of(&conn, &walk().as_of_recorded(RECORDED_AT).build_sql()).await;
    let both = counts_of(
        &conn,
        &walk()
            .as_of_valid(VALID_AT)
            .as_of_recorded(RECORDED_AT)
            .build_sql(),
    )
    .await;

    assert_eq!(
        valid_only,
        counts(3, 2, 1),
        "the valid-time-only arm moved. It is the control here: if it stops \
         differing from the transaction-time arm, the equality this test \
         asserts below stops meaning anything"
    );
    assert_eq!(
        recorded_only,
        counts(4, 2, 5),
        "the transaction-time read's cost moved. Re-run \
         `cargo run --example bitemporal_index_probe` and read the sweep \
         before changing this number (D-195)"
    );
    assert_eq!(
        both, recorded_only,
        "stating a valid instant as well as a transaction instant now costs \
         something. F-33 closed as *no index* because the two axes never meet \
         on a table and the second bound reaches no access path; a difference \
         here means that is no longer true and D-196 is due for review"
    );
}
