//! Visibility, on every surface that searches (§5.9, F-31, F-32).
//!
//! W9.3 put one predicate in one place; W9.4 gave it an instant. Both were
//! implemented so that the surfaces which *compose* another surface are fixed
//! by construction — `hybrid_search`'s vector arm **is** `search_vector`, and
//! `FilteredVectorSearch::run_post_filter` calls it too.
//!
//! Composition is exactly what this file declines to take on trust. "Fixed by
//! construction" is a statement about today's call graph, and a call graph is
//! the cheapest thing in the crate to change: an arm rewritten for speed, a
//! second access path added for a planner, and the guarantee is gone with every
//! per-surface test still green. So the requirement is stated once per surface,
//! against one fixture, and `search_filtered` is asked twice because its two
//! strategies share no query, no access path and no ordering mechanism.
//!
//! The fixture puts the two invisible concepts **first** on every surface
//! before it makes them invisible. A test that only asserts absence is
//! satisfied by a fixture that never had them within reach.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
/// When `ended` stops being true, and an instant after it.
const ENDED: &str = "2026-01-05T00:00:00.000000Z";
const AT: &str = "2026-01-06T00:00:00.000000Z";

/// One term, in every document, so the keyword arm ranks the whole corpus
/// rather than selecting part of it. Selection is the vector arm's job here.
const TERM: &str = "zygomorphic";
/// The same title on every concept, so the FTS title column cannot contribute
/// to the ordering and `content` is the only thing bm25 has to separate them.
const TITLE: &str = "N";

/// The corpus, nearest first. The first two are the ones made invisible.
const CORPUS: [&str; 6] = ["gone", "ended", "live0", "live1", "live2", "live3"];

fn model() -> ModelName {
    ModelName::new("vis_v1").unwrap()
}

/// Unit vectors by angle: rank 0 is the query direction, larger is farther.
fn v(rank: usize) -> Vec<f32> {
    let theta = (rank as f32) * std::f32::consts::PI / 16.0;
    vec![theta.cos(), theta.sin()]
}

fn query_vec() -> Vec<f32> {
    v(0)
}

/// `TERM` repeated `8 - rank` times, padded to a constant eight tokens.
///
/// Constant length is the point: bm25 reads term frequency *and* document
/// length, so padding is what makes tf the only thing that varies and the
/// keyword order therefore the same total order as the vector order. Two
/// surfaces agreeing on the ranking is what lets one expected list serve all of
/// them — and it is a property of the fixture, asserted below before anything
/// is hidden, not an assumption about bm25.
fn content(rank: usize) -> String {
    let mut tokens = vec![TERM; 8 - rank];
    tokens.extend(std::iter::repeat_n("filler", rank));
    tokens.join(" ")
}

/// Six concepts carrying both text and a vector, all reachable in one hop from
/// a `root` that is never embedded and holds no text — so the root cannot enter
/// any surface's answer and mask an ordering bug.
async fn fixture(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();

    let mut concepts = vec![ConceptUpsert::new("root", "Root").valid_from(TS)];
    concepts.extend(CORPUS.iter().enumerate().map(|(rank, id)| {
        ConceptUpsert::new(*id, TITLE)
            .content(content(rank))
            .valid_from(TS)
    }));
    db.write_concepts(concepts).await.unwrap();

    db.register_model(&model(), 2).await.unwrap();
    let rows: Vec<(String, Vec<f32>)> = CORPUS
        .iter()
        .enumerate()
        .map(|(rank, id)| (id.to_string(), v(rank)))
        .collect();
    db.upsert_embeddings(&model(), rows).await.unwrap();

    let edges: Vec<EdgeAssertion> = CORPUS
        .iter()
        .map(|id| {
            EdgeAssertion::new("root", *id, "LINKS")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect();
    db.write_bulk_atomic(edges).await.unwrap();
    db
}

/// Every surface that searches, each asked for `k`, at an optional instant.
///
/// **Five lists, not four.** `search_filtered` is asked once per strategy: the
/// predicate is spliced into two statements that share nothing, and a
/// visibility rule that holds under one plan and not the other is a wrong
/// answer selected by a byte estimate — which reproduces on one machine and not
/// the next ([D-050](../docs/architecture/s13-decision-register.md#d-050)).
///
/// `FilteredVectorSearch` is given the instant through its **traversal**,
/// because that is the only place it takes one (0.13.19, D-192).
async fn every_surface(
    db: &Database,
    at: Option<&str>,
    k: usize,
) -> Vec<(&'static str, Vec<String>)> {
    let vector = search_vector(db.read_conn(), &query_vec(), &model(), k, at, None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.concept_id)
        .collect();

    let keyword = keyword_search(db.read_conn(), &escape_fts5_query(TERM), k, at, None)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let mut hybrid = HybridSearch::new(model(), TERM, query_vec()).top_k(k);
    if let Some(t) = at {
        hybrid = hybrid.as_of_valid(t);
    }
    let hybrid = hybrid
        .execute(db.read_conn())
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.concept_id)
        .collect();

    let mut walk = TraversalBuilder::new("root").max_depth(1);
    if let Some(t) = at {
        walk = walk.as_of_valid(t);
    }
    let base = FilteredVectorSearch::new(model(), query_vec(), walk).top_k(k);
    let now = at.unwrap_or(TS);

    let mut out = vec![
        ("search_vector", vector),
        ("keyword_search", keyword),
        ("hybrid_search", hybrid),
    ];
    for (name, strategy) in [
        (
            "search_filtered/PostFilter",
            VectorFilterStrategy::PostFilter,
        ),
        (
            "search_filtered/PreFilterCTE",
            VectorFilterStrategy::PreFilterCTE,
        ),
    ] {
        let hits = base
            .clone()
            .strategy(strategy)
            .execute(db.read_conn(), now)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.concept_id)
            .collect();
        out.push((name, hits));
    }
    out
}

// ---------------------------------------------------------------------------
// The acceptance gate
// ---------------------------------------------------------------------------

/// **What the ledger says is invisible is invisible on every surface, and
/// `top_k` is still a count on every surface** (0.13.21, W9.6, F-31, F-32).
///
/// One retirement and one ended validity, on the two concepts every surface
/// ranks first. Three claims, in order:
///
/// 1. **The fixture can return them.** Every surface leads with `gone` and
///    `ended` while both are believed and current. Skipping this makes the rest
///    of the test passable by a corpus that was never in reach.
/// 2. **After one `retired` and one `valid_to`, neither appears anywhere.** Not
///    "not in the vector arm" — nowhere, including both filtered strategies.
/// 3. **`top_k` is still a count.** Three asked for, three live concepts back,
///    rather than three minus however many were hidden. That is what W9.3's
///    k′ escalation exists for, and it is the part a filter bolted on after the
///    index has already chosen its rows quietly gets wrong.
///
/// The two hidden concepts are hidden by *different mechanisms* on purpose:
/// `retired` is a flag and an ended validity is an interval, they are separate
/// terms of the shared predicate, and a surface can splice one and forget the
/// other.
#[tokio::test]
async fn every_search_surface_reads_the_same_visibility() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let leading: Vec<String> = CORPUS[..3].iter().map(|s| s.to_string()).collect();
    for (surface, got) in every_surface(&db, None, 3).await {
        assert_eq!(
            got, leading,
            "fixture is wrong: {surface} was supposed to lead with the two \
             concepts this test then hides, and rank them the way every other \
             surface does"
        );
    }

    // One flag and one interval. Nothing else about either concept changes,
    // and nothing about the other four changes at all.
    db.write_concepts(vec![
        ConceptUpsert::new("gone", TITLE)
            .content(content(0))
            .valid_from(TS)
            .retired(true),
        ConceptUpsert::new("ended", TITLE)
            .content(content(1))
            .valid_from(TS)
            .valid_to(ENDED),
    ])
    .await
    .unwrap();

    let expected: Vec<String> = CORPUS[2..5].iter().map(|s| s.to_string()).collect();
    for (surface, got) in every_surface(&db, Some(AT), 3).await {
        assert_eq!(
            got, expected,
            "{surface} disagrees with the ledger about what is visible, or \
             stopped treating top_k as a count"
        );
    }

    db.close().await.unwrap();
}

/// **The instant is what changes the answer, not the passage of time**
/// (0.13.19, D-192, D-155).
///
/// The same corpus, the same `now`, and the retirement still applied — but with
/// no instant stated, every surface reads the current corpus and `ended` is
/// back. An absent knob leaves the mechanism alone; without this arm the gate
/// above is also passed by a build that valid-time-bounds every search whether
/// or not one was asked for.
#[tokio::test]
async fn an_unstated_instant_reads_the_corpus_on_every_surface() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    db.write_concepts(vec![
        ConceptUpsert::new("gone", TITLE)
            .content(content(0))
            .valid_from(TS)
            .retired(true),
        ConceptUpsert::new("ended", TITLE)
            .content(content(1))
            .valid_from(TS)
            .valid_to(ENDED),
    ])
    .await
    .unwrap();

    // `gone` is absent because retirement is not a question about time;
    // `ended` is present because nobody asked about a time it had stopped.
    let expected: Vec<String> = CORPUS[1..4].iter().map(|s| s.to_string()).collect();
    for (surface, got) in every_surface(&db, None, 3).await {
        assert_eq!(
            got, expected,
            "{surface} bounded valid time without being asked to, or lost a \
             retirement that has nothing to do with the instant"
        );
    }

    db.close().await.unwrap();
}
