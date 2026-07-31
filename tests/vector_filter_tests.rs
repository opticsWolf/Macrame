//! Filtered vector search: the strategies, the cost model, and the one property
//! that makes a planner safe to have (§5.3, D-007).
//!
//! The suite is built around a single claim — **the strategy cannot change the
//! answer**. Everything else here is about the arithmetic that picks one.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const CORPUS: usize = 60;

/// Ids per statement in `run_pre_filter`'s chunked exact scan. Mirrored here so
/// the chunk-merge test can size a corpus that actually crosses it — the first
/// version of that test ran 60 candidates against this and exercised a single
/// chunk while claiming otherwise.
const IDS_PER_STATEMENT: usize = 500;

fn node_id(i: usize) -> String {
    format!("c{i:04}")
}

/// Unit vectors fanned out by angle, so cosine distance from the query at angle
/// zero is monotone in the index: `c0000` is nearest, the last is farthest.
///
/// Monotone placement is what lets a test say "the reachable nodes are the
/// *farthest* ones" and have that mean something exact, which is what makes the
/// `PostFilter` saturation case constructible rather than hoped for.
/// `reverse` puts the *nearest* node at the highest id. Candidate ids arrive
/// from the traversal in id order, so with the default arrangement distance
/// order and chunk order coincide and the nearest rows are always in the first
/// statement — under which a merge that simply concatenated would still return
/// the right answer. Reversing is what decorrelates them.
fn embedding(i: usize, corpus: usize, reverse: bool) -> Vec<f32> {
    let rank = if reverse { corpus - 1 - i } else { i };
    let theta = (rank as f32) * std::f32::consts::PI / (corpus as f32);
    vec![theta.cos(), theta.sin()]
}

fn model() -> ModelName {
    ModelName::new("filt_v1").unwrap()
}

/// `corpus` concepts, all embedded; `root` links to exactly `reachable`.
///
/// Reachability is one hop from a root that is itself excluded from the answer
/// by never being embedded — otherwise the root is a candidate at distance zero
/// and every top-k begins with it, which would mask an ordering bug.
async fn fixture_sized(
    harness: &TestHarness,
    reachable: &[usize],
    corpus: usize,
    reverse: bool,
) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = model();

    let mut concepts = vec![ConceptUpsert::new("root", "Root").valid_from(TS)];
    for i in 0..corpus {
        concepts.push(ConceptUpsert::new(node_id(i), "N").valid_from(TS));
    }
    db.write_concepts(concepts).await.unwrap();

    db.register_model(&m, 2).await.unwrap();
    let rows: Vec<(String, Vec<f32>)> = (0..corpus)
        .map(|i| (node_id(i), embedding(i, corpus, reverse)))
        .collect();
    db.upsert_embeddings(&m, rows).await.unwrap();

    let edges: Vec<EdgeAssertion> = reachable
        .iter()
        .map(|&i| {
            EdgeAssertion::new("root", node_id(i), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect();
    db.write_bulk_atomic(edges).await.unwrap();
    db
}

async fn fixture(harness: &TestHarness, reachable: &[usize]) -> Database {
    fixture_sized(harness, reachable, CORPUS, false).await
}

fn walk() -> TraversalBuilder {
    TraversalBuilder::new("root").max_depth(1)
}

fn search(reachable_query: Vec<f32>) -> FilteredVectorSearch {
    FilteredVectorSearch::new(model(), reachable_query, walk())
}

fn ids(results: &[VectorSearchResult]) -> Vec<String> {
    results.iter().map(|r| r.concept_id.clone()).collect()
}

/// The query vector: angle zero, so `c000` is the nearest concept in the corpus.
fn query() -> Vec<f32> {
    vec![1.0, 0.0]
}

// ---------------------------------------------------------------------------
// The acceptance gate
// ---------------------------------------------------------------------------

/// **The two strategies agree, always.**
///
/// `PostFilter` and `PreFilterCTE` share no query, no access path and no
/// ordering mechanism: one asks DiskANN for a generous k′ and discards the
/// rejects, the other scores every candidate by brute force. They exist so the
/// planner can trade cost against cost — which is only sound if the trade is
/// invisible in the result.
///
/// Swept across filter tightness and k, because the interesting failures live at
/// the ends: a filter so tight that `PostFilter` runs out of k′, and a candidate
/// set large enough to cross `run_pre_filter`'s statement-chunking boundary,
/// where a per-chunk `LIMIT` merged wrongly would return a plausible list that
/// is not the top-k.
///
/// This is the D-049 shape — two mechanisms for one question, held together by
/// requiring them to agree — and it is the reason a planner is safe to have.
#[tokio::test]
async fn the_strategy_never_changes_the_answer() {
    // Tight, mid, loose, and one crossing the 500-id chunking boundary is not
    // reachable at CORPUS = 60 — noted rather than silently omitted; the chunk
    // merge gets its own test below.
    for reachable in [
        vec![55, 56, 57, 58, 59],                 // tight, and far from the query
        vec![0, 1, 2, 30, 31, 59],                // spans the whole distance range
        (0..CORPUS).step_by(2).collect::<Vec<_>>(), // loose: half the corpus
        (0..CORPUS).collect::<Vec<_>>(),          // everything
    ] {
        let harness = TestHarness::new();
        let db = fixture(&harness, &reachable).await;

        for k in [1usize, 3, 10, 25] {
            let base = search(query()).top_k(k);

            let post = base
                .clone()
                .strategy(VectorFilterStrategy::PostFilter)
                .execute(db.read_conn(), TS)
                .await
                .unwrap();
            let pre = base
                .clone()
                .strategy(VectorFilterStrategy::PreFilterCTE)
                .execute(db.read_conn(), TS)
                .await
                .unwrap();
            let planned = base.execute(db.read_conn(), TS).await.unwrap();

            assert_eq!(
                ids(&post), ids(&pre),
                "strategies disagree at k={k} over {} reachable nodes; \
                 the planner is choosing between different answers",
                reachable.len()
            );
            assert_eq!(
                ids(&planned), ids(&pre),
                "the planned strategy disagreed with the exact one at k={k}"
            );

            // And the answer is the right one: the nearest `k` reachable nodes,
            // which the monotone embedding makes computable in the test.
            let mut expected: Vec<String> = reachable.iter().map(|&i| node_id(i)).collect();
            expected.sort();
            expected.truncate(k);
            assert_eq!(ids(&pre), expected, "wrong top-{k} over {reachable:?}");
        }
        db.close().await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// PostFilter's failure mode, and the escalation that removes it
// ---------------------------------------------------------------------------

/// **A saturated post-filter escalates rather than under-returning.**
///
/// The construction is deliberate: the five reachable nodes are the five
/// *farthest* from the query, so an index scan for k′ neighbours reaches them
/// only after returning all 55 nearer ones. `PostFilter` therefore filters its
/// whole k′ away and has nothing to return — while the correct answer is three
/// rows that plainly exist.
///
/// Silently returning zero is the §5.3 failure this module exists to prevent,
/// and it is the exact shape Doctrine II names: not an error, just a wrong
/// answer that looks like an empty result set. The escalation is what converts
/// it into the right answer, and forcing the strategy is what proves the
/// escalation runs rather than the planner routing around the problem.
#[tokio::test]
async fn a_saturated_post_filter_escalates_instead_of_under_returning() {
    let reachable = vec![55, 56, 57, 58, 59];
    let harness = TestHarness::new();
    let db = fixture(&harness, &reachable).await;

    let forced = search(query())
        .top_k(3)
        .strategy(VectorFilterStrategy::PostFilter)
        .execute(db.read_conn(), TS)
        .await
        .unwrap();

    assert_eq!(
        ids(&forced),
        vec![node_id(55), node_id(56), node_id(57)],
        "a forced PostFilter under-returned: the k' scan never reached the \
         reachable nodes, and without escalation this is an empty result that \
         reports success"
    );
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The cost model — D-007's mechanism, not only its interface
// ---------------------------------------------------------------------------

/// The estimator prices both strategies and picks the cheaper, and the choice
/// moves with the filter rather than with a hard-coded threshold.
///
/// The 0.4.5–0.5.4 estimator branched on `candidate_count` against 500 and 5000
/// with a `byte_budget` field it never read. At `CORPUS = 60` every case here
/// sits below the first of those thresholds, so the old selector would answer
/// `PostFilter` for all of them — which is what makes this a test of the
/// mechanism and not of the interface.
#[tokio::test]
async fn the_planner_follows_the_arithmetic_not_a_threshold() {
    let tight = vec![55, 56, 57, 58, 59];
    let loose: Vec<usize> = (0..CORPUS).collect();

    for (reachable, expected) in [
        (&tight, VectorFilterStrategy::PreFilterCTE),
        (&loose, VectorFilterStrategy::PostFilter),
    ] {
        let harness = TestHarness::new();
        let db = fixture(&harness, reachable).await;

        let (_, plan) = search(query())
            .top_k(3)
            .execute_explained(db.read_conn(), TS)
            .await
            .unwrap();

        assert_eq!(
            plan.strategy, expected,
            "with {} of {CORPUS} reachable the planner chose {:?} \
             (post={}, pre={}, k'={})",
            reachable.len(), plan.strategy, plan.post_filter_bytes,
            plan.pre_filter_bytes, plan.k_prime
        );
        db.close().await.unwrap();
    }
}

/// k′ inflates by the filter's selectivity, which is what makes `PostFilter`
/// viable at all — and it is clamped, so a near-empty candidate set cannot ask
/// the index for an unbounded scan.
#[test]
fn k_prime_inflates_by_selectivity_and_stays_clamped() {
    let est = CostEstimator::new(usize::MAX, 1000, 8);

    assert_eq!(est.k_prime(10, 1000), 10, "no filter, no inflation");
    assert_eq!(est.k_prime(10, 100), 100, "a tenth reachable, ten times the k");
    assert_eq!(
        est.k_prime(10, 1), 1000,
        "one candidate would want k'=10000; the corpus is the ceiling"
    );
    assert_eq!(est.k_prime(10, 0), 10, "an empty candidate set must not divide by zero");
}

/// **`byte_budget` is read.** The candidate set is checked against the ceiling
/// before either strategy runs, and exceeding it is a typed refusal rather than
/// a degraded answer (§5.3, D-007).
#[tokio::test]
async fn a_candidate_set_over_the_budget_is_refused() {
    let harness = TestHarness::new();
    let db = fixture(&harness, &(0..CORPUS).collect::<Vec<_>>()).await;

    let err = search(query())
        .top_k(3)
        .byte_budget(64) // smaller than one candidate id
        .execute(db.read_conn(), TS)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DbError::SubgraphTooLarge { .. }),
        "expected the budget ceiling to refuse; got {err:?}"
    );
    db.close().await.unwrap();
}

/// A probe that hits its cap reports a lower bound, not a count — and the
/// planner still answers, because "more than the cap" is enough to decide.
#[tokio::test]
async fn a_capped_probe_reports_a_lower_bound() {
    let harness = TestHarness::new();
    let db = fixture(&harness, &(0..CORPUS).collect::<Vec<_>>()).await;

    let (results, plan) = search(query())
        .top_k(3)
        .probe_cap(10)
        .execute_explained(db.read_conn(), TS)
        .await
        .unwrap();

    assert!(plan.candidates.is_capped(), "the cap was not reported: {:?}", plan.candidates);
    assert_eq!(plan.candidates.lower_bound(), 10);
    assert_eq!(results.len(), 3, "a capped probe must still answer");
    db.close().await.unwrap();
}

/// The chunked exact scan merges correctly.
///
/// `run_pre_filter` splits the candidate ids across statements to stay under
/// SQLITE_MAX_VARIABLE_NUMBER and applies `LIMIT k` to each, so the merge must
/// re-sort globally. A merge that concatenated would return the first chunk's
/// top-k — a plausible list that is not the answer.
///
/// **The corpus is sized to cross the boundary**, which the first version of
/// this test did not do: at 60 candidates against a 500-id chunk there is one
/// chunk, the per-chunk `LIMIT` is the global one, and deleting the merge sort
/// changes nothing. The nearest nodes and the farthest must land in *different*
/// statements for the merge to be under test at all.
#[tokio::test]
async fn the_chunked_exact_scan_returns_a_global_top_k() {
    let corpus = IDS_PER_STATEMENT * 2 + 40; // three statements
    let harness = TestHarness::new();
    // Reversed, so the nearest nodes carry the highest ids and land in the
    // *last* statement. Concatenating the per-statement results would return the
    // first chunk's five, which are the farthest in the corpus.
    let db = fixture_sized(&harness, &(0..corpus).collect::<Vec<_>>(), corpus, true).await;

    let got = search(query())
        .top_k(5)
        .strategy(VectorFilterStrategy::PreFilterCTE)
        .execute(db.read_conn(), TS)
        .await
        .unwrap();

    let expected: Vec<String> = (1..=5).map(|n| node_id(corpus - n)).collect();
    assert_eq!(
        ids(&got), expected,
        "the global nearest five were not returned — the per-statement LIMIT \
         was merged without re-sorting"
    );
    // Scores ascend: the merge sorted, rather than trusting per-chunk order.
    for w in got.windows(2) {
        assert!(w[0].score <= w[1].score, "results are not ordered by distance");
    }
    db.close().await.unwrap();
}

/// An empty candidate set is an empty answer, not an error and not the
/// unfiltered top-k — which is what a filter that silently fails open returns.
#[tokio::test]
async fn an_unreachable_filter_returns_nothing_rather_than_everything() {
    let harness = TestHarness::new();
    let db = fixture(&harness, &[]).await;

    let got = search(query()).top_k(5).execute(db.read_conn(), TS).await.unwrap();
    assert!(got.is_empty(), "a filter matching nothing returned {got:?}");
    db.close().await.unwrap();
}
