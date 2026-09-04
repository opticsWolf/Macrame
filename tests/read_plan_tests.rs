//! One value says what a read asks for (§16, F-34, W13.4, D-251).
//!
//! `ReadPlan` is three `Option`s and no behaviour, so almost nothing here is
//! about the struct. What is worth testing is the two claims made *around* it:
//! that a plan and the loose arguments it replaces name the same read, on every
//! lineage shape; and that `Database::edges` can be given a question no read
//! surface in this crate could previously be given at all — a lineage, a
//! valid-time instant and a transaction-time instant at once.
//!
//! # Why these fixtures fork through the public API
//!
//! `tests/branch_read_tests.rs` cuts its lineages by raw SQL so the reader is
//! tested against shapes `fork()` refuses to write, and that is right for a
//! file about the resolution. This one is about a surface a caller reaches,
//! and the surface has to agree with the other surfaces a caller reaches, so
//! every fixture below is built the way a caller would build it.
//!
//! # What is not here
//!
//! The transaction-time **refusal** — a recorded instant below what the hot log
//! still covers — is in `tests/temporal_tests.rs` beside the traversal's, on
//! the archived-ledger fixture that file already owns. Rebuilding that fixture
//! here to assert the same guard from a second entry point would be a copy of a
//! fixture rather than a second test.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::temporal::EdgeBelief;
use macrame::{BranchId, ConceptUpsert, Database, DbError, ReadPlan};
use std::time::Duration;

/// When every fixture edge starts, and where the fake clock is set: a read with
/// no valid instant asks the *handle's* clock, so a harness left at the epoch
/// would make "now" 1970 and every fixture edge invisible for a reason that has
/// nothing to do with what is under test.
const TS: &str = "2026-01-01T00:00:00.000000Z";
/// Inside every open interval, and inside the one closed interval.
const TS2: &str = "2026-02-01T00:00:00.000000Z";
/// Where `b → c` closes.
const TS3: &str = "2026-03-01T00:00:00.000000Z";
/// After every fixture write, and after the one closed interval ends.
const NOW: &str = "2026-06-01T00:00:00.000000Z";

fn edge(source: &str, target: &str, to: Option<&str>) -> EdgeAssertion {
    let e = EdgeAssertion::new(source, target, "KNOWS").valid_from(TS);
    match to {
        Some(t) => e.valid_to(t),
        None => e,
    }
}

fn harness() -> TestHarness {
    TestHarness::starting_at(macrame::util::parse_iso8601_utc(TS).unwrap())
}

/// `(source, target, branch)` from a plan read, sorted.
async fn via_plan(db: &Database, plan: ReadPlan) -> Vec<(String, String, String)> {
    let mut out: Vec<_> = db
        .edges(plan)
        .await
        .unwrap()
        .into_iter()
        .map(|e: EdgeBelief| (e.source_id, e.target_id, e.branch_id))
        .collect();
    out.sort();
    out
}

/// The same read through the free function, which does not return the lineage.
async fn via_free_fn(db: &Database, ts: &str, branch: Option<&str>) -> Vec<(String, String)> {
    let mut out: Vec<_> = macrame::temporal::query_as_of_edges_on(db.read_conn(), ts, branch)
        .await
        .unwrap()
        .into_iter()
        .map(|(s, t, _, _, _)| (s, t))
        .collect();
    out.sort();
    out
}

fn keys(rows: &[(String, String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(s, t, _)| (s.clone(), t.clone()))
        .collect()
}

/// The trunk, before anything forks: `a → b` open, `b → c` closed at `TS3`.
async fn unforked(h: &TestHarness) -> Database {
    let db = h.db_with_fake_clock().await;
    for id in ["a", "b", "c", "d"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
    db.assert_edge(edge("a", "b", None)).await.unwrap();
    db.assert_edge(edge("b", "c", Some(TS3))).await.unwrap();
    db
}

// ---------------------------------------------------------------------------
// A plan and the arguments it replaces name the same read
// ---------------------------------------------------------------------------

/// **An empty plan is the ordinary read**, on the handle's clock.
///
/// This is the claim that makes `plan()` additive rather than a fourth way to
/// configure a read: if `ReadPlan::default()` were not exactly "trunk, now,
/// current belief", then applying a partially-filled plan would silently move
/// the qualifiers the caller did not mention.
///
/// The closed interval is here because a read at "now" has to be able to see an
/// interval end at all. It does **not** pin the comparison operators — the
/// clock is months past `TS3`, so `?1 <= valid_to` answers the same. That is
/// [`the_window_is_half_open_at_both_ends`]'s job, and this comment claimed
/// otherwise until a mutation of `<` to `<=` walked past every test in the file.
#[tokio::test]
async fn an_empty_plan_reads_the_trunk_now_under_current_belief() {
    let h = harness();
    let db = unforked(&h).await;
    h.advance(Duration::from_secs(180 * 86_400)); // past TS3

    let now = db.clock().now();
    assert_eq!(
        keys(&via_plan(&db, ReadPlan::new()).await),
        via_free_fn(&db, &now, None).await,
        "an empty plan must be the read every other surface takes by default"
    );
    assert_eq!(
        keys(&via_plan(&db, ReadPlan::new()).await),
        vec![("a".to_string(), "b".to_string())],
        "`b -> c` closed at TS3 and the clock is past it"
    );

    // And the same plan with the instant stated is the same answer, so `None`
    // really is the clock rather than a second default hiding behind it.
    assert_eq!(
        via_plan(&db, ReadPlan::new()).await,
        via_plan(&db, ReadPlan::new().valid_at(&now)).await
    );

    db.close().await.unwrap();
}

/// **The window is half-open, asserted at the two instants where that shows.**
///
/// `valid_from <= ?1 AND ?1 < valid_to` has two neighbours that are wrong and
/// invisible everywhere except on the boundary itself: `?1 <= valid_to` admits
/// an interval at the instant it ends, and `valid_from < ?1` drops one at the
/// instant it begins. Every other fixture in this file reads at an instant
/// comfortably inside or outside its intervals, so every other fixture answers
/// identically under all four spellings.
///
/// Both shapes, because the predicate is one format string but the relation it
/// filters is not: under `Resolved` it is the tail of a four-CTE chain, and a
/// bound that a CTE had already applied would not show here.
#[tokio::test]
async fn the_window_is_half_open_at_both_ends() {
    let h = harness();
    let db = unforked(&h).await;
    h.advance(Duration::from_secs(86_400));
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();

    for branch in [None, Some(alt.clone())] {
        let plan = |ts: &str| {
            let p = ReadPlan::new().valid_at(ts);
            match &branch {
                Some(b) => p.on(b.clone()),
                None => p,
            }
        };
        let at = |ts: &'static str| {
            let p = plan(ts);
            let db = &db;
            async move { keys(&via_plan(db, p).await) }
        };

        // `valid_from` is included: at the instant it begins, an edge is there.
        assert_eq!(
            at(TS).await,
            vec![
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "c".to_string()),
            ],
            "{branch:?}: `valid_from < ?1` would drop an interval at its own start"
        );

        // `valid_to` is excluded: at the instant it ends, it is gone.
        assert_eq!(
            at(TS3).await,
            vec![("a".to_string(), "b".to_string())],
            "{branch:?}: `?1 <= valid_to` would admit `b -> c` at the instant \
             it closed, and a half-open interval is the crate's whole \
             definition of one"
        );
    }

    db.close().await.unwrap();
}

/// **The plan reader and the free function are one statement, on all three
/// shapes.**
///
/// `query_as_of_edges_on` has been the crate's whole-ledger valid-time read
/// since 0.5.0 and grew its lineage resolution over four releases (D-220,
/// D-223, D-227). `Database::edges` is not a second reader beside it: the free
/// function is this one with `recorded` unset and the lineage dropped from each
/// row, so the two cannot disagree. This asserts that on the shape a database
/// has before it forks, and on both shapes it has after.
#[tokio::test]
async fn a_plan_and_the_free_function_answer_alike_on_every_shape() {
    let h = harness();
    let db = unforked(&h).await;

    // `Trunk`: no fork, nothing to resolve.
    let plan = ReadPlan::new().valid_at(TS2);
    assert_eq!(
        keys(&via_plan(&db, plan.clone()).await),
        via_free_fn(&db, TS2, None).await
    );

    h.advance(Duration::from_secs(86_400));
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(86_400));
    db.assert_edge(edge("a", "b", None).weight(4.0).on_branch(alt.clone()))
        .await
        .unwrap();
    db.assert_edge(edge("c", "d", None)).await.unwrap();

    // `TrunkOnForked`: a root on a forked ledger, which is a two-predicate
    // lookup rather than the resolved form (D-244, D-250).
    assert_eq!(
        keys(&via_plan(&db, plan.clone()).await),
        via_free_fn(&db, TS2, None).await
    );
    // `Resolved`: four CTEs and an ancestry.
    let on_alt = plan.on(alt.clone());
    assert_eq!(
        keys(&via_plan(&db, on_alt.clone()).await),
        via_free_fn(&db, TS2, Some("alt")).await
    );
    // The fixture has to actually separate the lineages, or the equality above
    // is between two copies of the same trivial answer.
    assert_ne!(
        keys(&via_plan(&db, on_alt).await),
        keys(&via_plan(&db, ReadPlan::new().valid_at(TS2)).await),
        "the trunk wrote `c -> d` after the fork; `alt` must not have it"
    );

    db.close().await.unwrap();
}

/// **A row says which lineage holds it**, which the tuple reader cannot.
///
/// `query_as_of_edges_on` returns five strings and the branch is not one of
/// them, so on a forked ledger it can tell a caller *that* `a → b` is visible
/// and not *whose* `a → b` it is. That distinction is the entire content of
/// D-220's resolution, and until this release it was observable only through
/// the traversal's weights or by reading `links_current` directly.
#[tokio::test]
async fn a_resolved_row_carries_the_lineage_that_holds_it() {
    let h = harness();
    let db = unforked(&h).await;
    h.advance(Duration::from_secs(86_400));
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(86_400));

    // `alt` corrects one inherited edge and leaves the other alone.
    db.assert_edge(edge("a", "b", None).weight(4.0).on_branch(alt.clone()))
        .await
        .unwrap();

    assert_eq!(
        via_plan(&db, ReadPlan::new().valid_at(TS2).on(alt)).await,
        vec![
            ("a".to_string(), "b".to_string(), "alt".to_string()),
            ("b".to_string(), "c".to_string(), "main".to_string()),
        ],
        "the corrected edge is the branch's own row and the other is inherited"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The question that had no reader
// ---------------------------------------------------------------------------

/// **A lineage, a valid instant and a recorded instant, in one read.**
///
/// This is what W13.4 adds beyond a tidier signature. `query_as_of_edges_on`
/// takes no transaction-time instant; the traversal takes all three but needs a
/// start node the question does not have; `reconstruct` takes one and folds the
/// entire log to get it. The fold here is the traversal's own — `links_at_tx`
/// bounded by the ancestry's cutoffs (D-223) — so the bitemporal cell costs
/// what reading it costs.
///
/// The fixture separates the two axes on purpose. `c → d` is written to the
/// trunk *after* the recorded instant, so it is absent under the old belief and
/// present under the new one at the same valid instant; `b → c` closes at `TS3`
/// on the valid axis and is absent at `NOW` under *both* beliefs. A read that
/// confused the two would show one of them in the wrong place.
#[tokio::test]
async fn a_recorded_instant_names_a_belief_no_other_edge_read_could_ask_for() {
    let h = harness();
    let db = unforked(&h).await;
    h.advance(Duration::from_secs(86_400));
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(86_400));
    db.assert_edge(edge("a", "b", None).weight(4.0).on_branch(alt.clone()))
        .await
        .unwrap();

    // What we believed before the trunk's next write.
    let believed = db.clock().now();
    h.advance(Duration::from_secs(86_400));
    db.assert_edge(edge("c", "d", None)).await.unwrap();
    db.assert_edge(edge("d", "a", None).on_branch(alt.clone()))
        .await
        .unwrap();

    let at_ts2 = ReadPlan::new().valid_at(TS2);

    // The trunk, then and now: `c -> d` is the difference and `b -> c` is not.
    assert_eq!(
        keys(&via_plan(&db, at_ts2.clone().recorded_at(&believed)).await),
        vec![
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
        ]
    );
    assert_eq!(
        keys(&via_plan(&db, at_ts2.clone()).await),
        vec![
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
            ("c".to_string(), "d".to_string()),
        ]
    );

    // The branch, then and now, at the same two instants: its own later write
    // is the difference, and the trunk's is not — `c -> d` was recorded after
    // the fork and no belief makes it `alt`'s.
    assert_eq!(
        keys(&via_plan(&db, at_ts2.clone().on(alt.clone()).recorded_at(&believed)).await),
        vec![
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
        ]
    );
    assert_eq!(
        keys(&via_plan(&db, at_ts2.clone().on(alt.clone())).await),
        vec![
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
            ("d".to_string(), "a".to_string()),
        ]
    );

    // The valid axis is untouched by either belief: `b -> c` closed at TS3.
    for plan in [at_ts2.valid_at(NOW), ReadPlan::new().valid_at(NOW)] {
        for p in [plan.clone(), plan.recorded_at(&believed)] {
            assert!(
                !keys(&via_plan(&db, p.clone()).await)
                    .contains(&("b".to_string(), "c".to_string())),
                "a closed interval is closed under every belief: {p:?}"
            );
        }
    }

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The builder round trip
// ---------------------------------------------------------------------------

/// **A plan survives a traversal builder in both directions.**
///
/// The pair is what makes a plan worth having over three setters: a caller can
/// take the qualifiers off a traversal they were handed and give the same read
/// to `Database::edges`, or to a second traversal from another start node.
/// A round trip that dropped a field would make that silently a different read.
#[tokio::test]
async fn a_plan_survives_a_traversal_builder_in_both_directions() {
    let full = ReadPlan::new()
        .on(BranchId::new("alt").unwrap())
        .valid_at(TS2)
        .recorded_at(TS3);

    for plan in [
        ReadPlan::new(),
        ReadPlan::new().valid_at(TS2),
        ReadPlan::new().on(BranchId::new("alt").unwrap()),
        full.clone(),
    ] {
        let back = TraversalBuilder::new("a")
            .plan(plan.clone())
            .read_plan()
            .unwrap();
        assert_eq!(back, plan, "a plan changed shape going through a builder");
    }

    // And the other direction: reading a builder's plan and applying it back
    // must leave the builder alone.
    let b = TraversalBuilder::new("a")
        .max_depth(7)
        .on_branch("alt")
        .as_of_valid(TS2);
    let same = b.clone().plan(b.read_plan().unwrap());
    assert_eq!(same.branch, b.branch);
    assert_eq!(same.as_of_valid, b.as_of_valid);
    assert_eq!(same.as_of_recorded, b.as_of_recorded);
    assert_eq!(
        same.max_depth, 7,
        "a plan carries no depth and must not set one"
    );

    // **An unset field unsets.** A plan is the read, not an amendment to it,
    // and this is the half a caller is most likely to get wrong if it were the
    // other way: applying `ReadPlan::new()` must not leave last week's
    // `as_of_recorded` in place.
    let cleared = TraversalBuilder::new("a").plan(full).plan(ReadPlan::new());
    assert_eq!(cleared.branch, None);
    assert_eq!(cleared.as_of_valid, None);
    assert_eq!(cleared.as_of_recorded, None);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// **An unregistered lineage is refused by name**, not answered for the trunk.
///
/// The same refusal the traversal and the free function give, for
/// `graph::lineage::lineage_shape`'s reason: on a database that has never
/// forked, the trunk's answer is the one a caller is least able to detect as
/// wrong, because it is the answer they expected.
#[tokio::test]
async fn an_unregistered_lineage_is_refused_by_name() {
    let h = harness();
    let db = unforked(&h).await;

    let err = db
        .edges(ReadPlan::new().on(BranchId::new("ghost").unwrap()))
        .await
        .expect_err("a lineage that was never registered must not read as the trunk");
    match &err {
        DbError::UnknownBranch(branch) => assert_eq!(branch, "ghost"),
        other => panic!("got {other:?}"),
    }

    db.close().await.unwrap();
}

/// **A malformed instant is refused where every other instant is.**
///
/// A plan validates nothing — it is three `Option`s — so the normaliser that
/// every other read runs is what a caller meets, and it meets it on both axes.
#[tokio::test]
async fn a_malformed_instant_is_refused_on_either_axis() {
    let h = harness();
    let db = unforked(&h).await;

    for plan in [
        ReadPlan::new().valid_at("last Tuesday"),
        ReadPlan::new().recorded_at("last Tuesday"),
    ] {
        match db.edges(plan.clone()).await {
            Err(DbError::InvalidTimestamp { .. }) => {}
            other => panic!("{plan:?} gave {other:?}"),
        }
    }

    db.close().await.unwrap();
}
