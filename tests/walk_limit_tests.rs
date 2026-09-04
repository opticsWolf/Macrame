//! A ceiling that bounds work rather than the answer (C-8, W13.5, D-252).
//!
//! `probe_cap` ran the whole traversal and truncated the tail. The repair is a
//! `LIMIT` pushed into the recursive CTE, and the reason it is worth a file is
//! that **the visible behaviour of the right fix and the wrong one is almost
//! identical**: a `LIMIT` on the statement's outer `SELECT` also returns `n`
//! rows, also looks like a ceiling, and also bounds nothing, because that
//! projection sorts and a sort materialises the whole walk first.
//!
//! So the tests here are chosen to separate placements rather than to check
//! that a number came back:
//!
//! * The **near end** is what a limited walk returns. Inside the CTE the cut
//!   falls on the walk's breadth-first queue; on the outer `SELECT` it falls on
//!   `ORDER BY node_id`. The fixture makes those two answers disjoint.
//! * The **walk's own row count** is what says whether the ceiling bit, and it
//!   is read from a projection anchored on the count so that it survives a walk
//!   whose every concept is retired.
//! * The **unlimited statement is unchanged**, byte for byte, which is the
//!   property W13.1 spent a release establishing.
//!
//! # What is not here
//!
//! The edge counts in [`TraversalBuilder::limit`]'s rustdoc are a measurement,
//! taken with a counting SQL function against SQLite directly
//! (`scratchpad/w135_probe4.py`, quoted in D-252). Nothing in this crate can
//! count rows a recursive CTE visited — libSQL exposes no statement counters —
//! so the claim that work falls is pinned here by the placement tests, which
//! are the thing a regression would have to break first.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{EdgeAssertion, TraversalBuilder, WalkOutcome};
use macrame::{BranchId, ConceptUpsert, Database, ReadPlan};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

fn harness() -> TestHarness {
    TestHarness::starting_at(macrame::util::parse_iso8601_utc(TS).unwrap())
}

/// A two-hop graph whose **nearest** nodes are its alphabetically **last**.
///
/// `m0 → z1, z2, z3` and each `z → a1..a3`. A walk limited inside the CTE keeps
/// `m0` and the `z`s; one limited on the sorted projection keeps the `a`s. The
/// two answers share nothing, which is the whole point of the shape.
async fn near_far(h: &TestHarness) -> Database {
    let db = h.db_with_fake_clock().await;
    let mut ids = vec!["m0".to_string()];
    for z in 1..=3 {
        ids.push(format!("z{z}"));
        for a in 1..=3 {
            ids.push(format!("a{z}{a}"));
        }
    }
    for id in &ids {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
    for z in 1..=3 {
        db.assert_edge(EdgeAssertion::new("m0", format!("z{z}"), "KNOWS").valid_from(TS))
            .await
            .unwrap();
        for a in 1..=3 {
            db.assert_edge(
                EdgeAssertion::new(format!("z{z}"), format!("a{z}{a}"), "KNOWS").valid_from(TS),
            )
            .await
            .unwrap();
        }
    }
    db
}

// ---------------------------------------------------------------------------
// Where the cut falls
// ---------------------------------------------------------------------------

/// **A limited walk returns the near end, not the low end.**
///
/// This is the test that tells the two placements apart. `m0`'s neighbourhood
/// is 13 nodes; a limit of 4 leaves the walk holding `m0` and the three `z`s.
/// Had the `LIMIT` gone on the outer `SELECT` — the placement the plan's own
/// sketch named, and the one that bounds nothing — the answer would be
/// `a11, a12, a13, a21`, because that projection sorts before it truncates.
///
/// The two expected sets are disjoint, so this cannot pass by accident on a
/// fixture where they happen to overlap.
#[tokio::test]
async fn a_limited_walk_keeps_the_nodes_nearest_the_start() {
    let h = harness();
    let db = near_far(&h).await;

    let (ids, outcome) = TraversalBuilder::new("m0")
        .max_depth(3)
        .limit(4)
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();

    assert_eq!(
        ids,
        vec!["m0", "z1", "z2", "z3"],
        "a limit on the sorted projection would have answered with the `a`s"
    );
    assert_eq!(outcome, WalkOutcome::LimitReached);
    db.close().await.unwrap();
}

/// **A ceiling above the graph is not a ceiling.**
///
/// The same traversal, limited to more rows than exist: every node comes back
/// and the outcome says the walk ended on its own terms. Without this the
/// `LimitReached` above would be consistent with a builder that reports the
/// limit whenever one is set.
#[tokio::test]
async fn a_limit_the_walk_never_reaches_reports_a_complete_answer() {
    let h = harness();
    let db = near_far(&h).await;

    let unlimited = TraversalBuilder::new("m0")
        .max_depth(3)
        .execute_ids(db.read_conn(), NOW)
        .await
        .unwrap();
    let (ids, outcome) = TraversalBuilder::new("m0")
        .max_depth(3)
        .limit(1_000)
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();

    assert_eq!(ids, unlimited, "a slack ceiling must not change the answer");
    assert_eq!(outcome, WalkOutcome::Complete);
    assert!(!outcome.hit_limit());
    db.close().await.unwrap();
}

/// **An unlimited walk reports `Complete` and pays nothing to say so.**
///
/// The reporting column is emitted only under a limit, so this asserts both
/// halves at once: the outcome is right, and the statement it came from is the
/// one every release before 0.15.10 ran.
#[tokio::test]
async fn an_unlimited_walk_emits_the_statement_it_always_did() {
    let h = harness();
    let db = near_far(&h).await;

    let plain = TraversalBuilder::new("m0").max_depth(3);
    let sql = plain.build_sql();
    assert!(
        !sql.contains("LIMIT") && !sql.contains("SELECT COUNT(*) FROM walk"),
        "an unlimited traversal must not carry a ceiling or its reporting: {sql}"
    );
    assert!(
        sql.contains("SELECT DISTINCT w.node_id\nFROM walk w JOIN concepts c"),
        "the unlimited projection must be the one every plan pin asserts: {sql}"
    );

    let (_, outcome) = plain
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();
    assert_eq!(outcome, WalkOutcome::Complete);
    db.close().await.unwrap();
}

/// **The ceiling is inside the recursion, and it is bound rather than spliced.**
///
/// A text assertion, because it is the one property no answer can show: a
/// `LIMIT` after the closing parenthesis returns the same rows on every fixture
/// in this file and bounds nothing.
#[tokio::test]
async fn the_ceiling_sits_inside_the_recursive_cte() {
    let sql = TraversalBuilder::new("m0").limit(4).build_sql();
    let cte_end = sql.find("\n)").expect("the walk CTE must close");
    let limit_at = sql
        .find("LIMIT ?")
        .expect("a limited walk must carry a LIMIT");
    assert!(
        limit_at < cte_end,
        "the LIMIT must sit inside the recursion, not after it: {sql}"
    );
    assert!(
        !sql.contains("LIMIT 4"),
        "the ceiling is bound, not spliced: {sql}"
    );
}

// ---------------------------------------------------------------------------
// The placeholder layout
// ---------------------------------------------------------------------------

/// **The ceiling binds after the variadic edge types, on every shape.**
///
/// The slot is last because it is the only one whose position depends on how
/// many parameters precede it. That makes it the parameter most likely to be
/// laid out in one place and filled in another — D-030's failure mode — and the
/// way to catch it is to run a traversal that populates every optional slot at
/// once: a branch, a recorded instant, two edge types and a limit. A layout
/// that disagreed by one would bind the ceiling as an edge type and answer with
/// nothing, or refuse the statement outright.
#[tokio::test]
async fn every_optional_slot_can_be_occupied_at_once() {
    let h = harness();
    let db = near_far(&h).await;
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    let recorded = db.clock().now();

    let (ids, outcome) = TraversalBuilder::new("m0")
        .max_depth(3)
        .on_branch(alt.as_str())
        .as_of_recorded(&recorded)
        .edge_types(vec!["KNOWS".to_string(), "CITES".to_string()])
        .limit(4)
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();

    assert_eq!(ids, vec!["m0", "z1", "z2", "z3"]);
    assert_eq!(outcome, WalkOutcome::LimitReached);
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The case the anchored projection exists for
// ---------------------------------------------------------------------------

/// **A walk that reached only retired concepts still says it was cut short.**
///
/// The projection drops retired rows *after* the walk has paid for them, so
/// this traversal returns no ids at all — and the ceiling still bit. The
/// obvious way to report the count, a second column beside the id, cannot be
/// read here because there is no row to read it from; anchoring the projection
/// on the count and left-joining the ids is what makes this case answerable.
///
/// Every concept is retired rather than some, because a partial fixture would
/// pass under the scalar-column form too.
#[tokio::test]
async fn a_walk_whose_every_concept_is_retired_still_reports_the_ceiling() {
    let h = harness();
    let db = near_far(&h).await;
    for z in 1..=3 {
        for a in 1..=3 {
            db.upsert_concept(
                ConceptUpsert::new(format!("a{z}{a}"), "N")
                    .valid_from(TS)
                    .retired(true),
            )
            .await
            .unwrap();
        }
        db.upsert_concept(
            ConceptUpsert::new(format!("z{z}"), "N")
                .valid_from(TS)
                .retired(true),
        )
        .await
        .unwrap();
    }
    db.upsert_concept(ConceptUpsert::new("m0", "N").valid_from(TS).retired(true))
        .await
        .unwrap();

    let (ids, outcome) = TraversalBuilder::new("m0")
        .max_depth(3)
        .limit(4)
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();

    assert!(ids.is_empty(), "every concept is retired: {ids:?}");
    assert_eq!(
        outcome,
        WalkOutcome::LimitReached,
        "the walk spent its whole budget before the projection dropped the rows"
    );
    db.close().await.unwrap();
}

/// **A cut walk can return fewer ids than the ceiling it was cut at.**
///
/// The walk holds `(node_id, depth)` and dedupes on the pair, so a node
/// reachable at two depths spends two of the budget's rows and answers with
/// one. `p → q → r` with `p → r` as well puts `r` in at depth 1 and depth 2:
/// four walk rows, three ids, at a ceiling of four.
///
/// This is the fixture that separates the walk's row count from its answer,
/// and it is why the outcome is read from the former. Everything else in this
/// file would pass with either.
#[tokio::test]
async fn a_node_reached_twice_spends_the_ceiling_twice() {
    let h = harness();
    let db = h.db_with_fake_clock().await;
    for id in ["p", "q", "r"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
    for (s, t) in [("p", "q"), ("q", "r"), ("p", "r")] {
        db.assert_edge(EdgeAssertion::new(s, t, "KNOWS").valid_from(TS))
            .await
            .unwrap();
    }

    let (ids, outcome) = TraversalBuilder::new("p")
        .max_depth(2)
        .limit(4)
        .execute_ids_explained(db.read_conn(), NOW)
        .await
        .unwrap();

    assert_eq!(ids, vec!["p", "q", "r"]);
    assert_eq!(
        outcome,
        WalkOutcome::LimitReached,
        "four walk rows at a ceiling of four is a cut walk, and the three ids          it answered with cannot say so"
    );
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The ceiling travels with the plan
// ---------------------------------------------------------------------------

/// **A plan carries the ceiling, in both directions.**
///
/// The round trip `plan()` and `read_plan()` promise is exact, and 0.15.10 adds
/// a field to the value it is promised about. An empty plan clears it, which is
/// the same replacement rule the three instants follow.
#[tokio::test]
async fn a_ceiling_survives_a_traversal_builder_in_both_directions() {
    let plan = ReadPlan::new()
        .on(BranchId::new("alt").unwrap())
        .valid_at(TS)
        .recorded_at(NOW)
        .limit(7);

    let b = TraversalBuilder::new("m0").plan(plan.clone());
    assert_eq!(b.limit, Some(7));
    assert_eq!(b.read_plan().unwrap(), plan);

    let cleared = b.plan(ReadPlan::new());
    assert_eq!(cleared.limit, None, "an empty plan clears the ceiling too");
    assert_eq!(cleared.read_plan().unwrap(), ReadPlan::new());
}

/// **`Database::edges` spends the ceiling on its own statement.**
///
/// The whole-ledger read has no walk and no order to be near the front of, so a
/// limit there is a plain `LIMIT` and the rows kept are arbitrary. What it does
/// have is an exact truncation signal, because nothing drops rows after the
/// limit applies: `len() == n` is the answer, and the fixture holds more edges
/// than the ceiling so that means something.
#[tokio::test]
async fn a_plan_ceiling_bounds_the_whole_ledger_read() {
    let h = harness();
    let db = near_far(&h).await;

    let all = db.edges(ReadPlan::new().valid_at(NOW)).await.unwrap();
    assert_eq!(all.len(), 12, "3 hub edges and 9 leaf edges");

    let some = db
        .edges(ReadPlan::new().valid_at(NOW).limit(5))
        .await
        .unwrap();
    assert_eq!(some.len(), 5);
    assert!(
        some.iter().all(|e| all.contains(e)),
        "a limited read must be a subset of the unlimited one"
    );
    db.close().await.unwrap();
}
