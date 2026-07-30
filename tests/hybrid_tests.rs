//! Hybrid search: the keyword arm, the fusion, and the index's fidelity to the
//! ledger (§5.9, D-051).
//!
//! The suite is organised around the claim that justifies having two arms at
//! all: **each finds documents the other cannot**. A hybrid search whose vector
//! arm is doing all the work is a vector search with overhead, and it passes any
//! test that only checks the fused list is non-empty.

mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";

fn model() -> ModelName {
    ModelName::new("hybrid_v1").unwrap()
}

/// Unit vectors by angle: `v(0)` is the query direction, larger is farther.
fn v(rank: usize) -> Vec<f32> {
    let theta = (rank as f32) * std::f32::consts::PI / 16.0;
    vec![theta.cos(), theta.sin()]
}

fn query_vec() -> Vec<f32> {
    v(0)
}

/// Concepts carrying both text and a vector, so either arm can be interrogated
/// in isolation and the two can be put in deliberate disagreement.
///
/// The shape that matters: `lexical` holds the rare term and is placed *far* in
/// vector space, while `semantic` is nearest the query and does not contain the
/// term at all. Neither arm alone returns both.
async fn fixture(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();

    let docs: [(&str, &str, &str, usize); 6] = [
        // id          title              content                              vector rank
        ("semantic", "Nearest", "a paraphrase with no rare words", 0),
        ("middle", "Middle", "ordinary filler text", 4),
        ("lexical", "Distant", "contains zygomorphic exactly once", 12),
        ("filler1", "Filler One", "ordinary filler text", 6),
        ("filler2", "Filler Two", "ordinary filler text", 8),
        ("both", "Both", "zygomorphic paraphrase", 2),
    ];

    let concepts: Vec<ConceptUpsert> = docs
        .iter()
        .map(|(id, title, content, _)| {
            ConceptUpsert::new(*id, *title).content(*content).valid_from(TS)
        })
        .collect();
    db.write_concepts(concepts).await.unwrap();

    db.register_model(&model(), 2).await.unwrap();
    let rows: Vec<(String, Vec<f32>)> = docs
        .iter()
        .map(|(id, _, _, rank)| (id.to_string(), v(*rank)))
        .collect();
    db.upsert_embeddings(&model(), rows).await.unwrap();
    db
}

fn ids(hits: &[HybridHit]) -> Vec<String> {
    hits.iter().map(|h| h.concept_id.clone()).collect()
}

// ---------------------------------------------------------------------------
// The claim that justifies two arms
// ---------------------------------------------------------------------------

/// **Each arm contributes something the other cannot.**
///
/// `lexical` holds a term no other document has, but sits far away in vector
/// space; `semantic` is the nearest vector but does not contain the term. The
/// fused result must hold both — and the test proves the point by *also*
/// checking that each arm alone misses one of them, so "the fusion works" is not
/// satisfied by a vector search that happened to rank everything.
#[tokio::test]
async fn the_fusion_returns_what_neither_arm_finds_alone() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    // Vector arm alone: nearest first, and the rare-term document is last.
    let vector_only = search_vector(db.read_conn(), &query_vec(), &model(), 3)
        .await
        .unwrap();
    let vector_ids: Vec<&str> = vector_only.iter().map(|r| r.concept_id.as_str()).collect();
    assert!(
        !vector_ids.contains(&"lexical"),
        "fixture is wrong: the vector arm was supposed to miss the rare-term \
         document in its top 3, got {vector_ids:?}"
    );

    // Keyword arm alone: only the documents containing the term.
    let keyword_only = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    let keyword_ids: Vec<&str> = keyword_only.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        !keyword_ids.contains(&"semantic"),
        "fixture is wrong: the keyword arm was supposed to miss the paraphrase, \
         got {keyword_ids:?}"
    );

    let fused = HybridSearch::new(model(), "zygomorphic", query_vec())
        .top_k(4)
        .execute(db.read_conn())
        .await
        .unwrap();
    let fused_ids = ids(&fused);

    assert!(
        fused_ids.contains(&"lexical".to_string()),
        "the keyword arm's exclusive find is missing: {fused_ids:?}"
    );
    assert!(
        fused_ids.contains(&"semantic".to_string()),
        "the vector arm's exclusive find is missing: {fused_ids:?}"
    );

    // The document both arms like should outrank either arm's exclusive find —
    // that is what RRF is for, and it is the one ordering claim worth pinning.
    assert_eq!(
        fused_ids.first().map(String::as_str),
        Some("both"),
        "the document ranked well by both arms did not come first: {fused:?}"
    );

    db.close().await.unwrap();
}

/// The per-arm ranks come back with the results, so a caller can see *why* a
/// document placed where it did rather than only that it did.
#[tokio::test]
async fn each_hit_carries_the_evidence_for_its_rank() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let fused = HybridSearch::new(model(), "zygomorphic", query_vec())
        .top_k(6)
        .execute(db.read_conn())
        .await
        .unwrap();

    let hit = |id: &str| fused.iter().find(|h| h.concept_id == id).cloned().unwrap();

    let both = hit("both");
    assert!(both.vector_rank.is_some() && both.keyword_rank.is_some());

    let semantic = hit("semantic");
    assert!(
        semantic.vector_rank.is_some() && semantic.keyword_rank.is_none(),
        "the paraphrase has no rare term, so it cannot have a keyword rank: {semantic:?}"
    );

    let lexical = hit("lexical");
    assert!(
        lexical.keyword_rank.is_some(),
        "the rare-term document must carry a keyword rank: {lexical:?}"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The index is a faithful shadow of the ledger
// ---------------------------------------------------------------------------

/// **An edit retracts the old terms.** This is the external-content trap: the
/// update trigger must issue FTS5's `'delete'` command with the *old* values
/// before inserting the new ones, or the index keeps matching text the concept
/// no longer contains — silently, and only detectably by searching for something
/// that should be gone.
#[tokio::test]
async fn rewriting_a_concept_retracts_its_old_terms() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let before = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert!(before.iter().any(|(id, _)| id == "lexical"));

    // Same concept, new text. The rare term is gone; a new one takes its place.
    db.upsert_concept(
        ConceptUpsert::new("lexical", "Distant")
            .content("now says brachiopod instead")
            .valid_from(TS),
    )
    .await
    .unwrap();

    let after = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert!(
        !after.iter().any(|(id, _)| id == "lexical"),
        "the index still matches a word the concept no longer contains: {after:?}"
    );

    let new_term = keyword_search(db.read_conn(), &escape_fts5_query("brachiopod"), 10)
        .await
        .unwrap();
    assert!(
        new_term.iter().any(|(id, _)| id == "lexical"),
        "the replacement text was never indexed: {new_term:?}"
    );

    db.close().await.unwrap();
}

/// **D-036: the derivative is rebuildable.** Corrupt the index through a raw
/// connection, rebuild through the handle, and require the same answers back.
///
/// The corruption is a real one — terms deleted from the index while the concept
/// text is untouched — so a rebuild that silently did nothing would leave the
/// search broken and this test red.
#[tokio::test]
async fn the_index_can_be_rebuilt_from_the_ledger() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let expected = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert!(!expected.is_empty(), "fixture must match something to lose");

    // Damage the index behind the crate's back, the way integrity_tests damages
    // links_current: a second connection, no triggers involved.
    {
        let outside = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let conn = outside.connect().unwrap();
        conn.execute("DELETE FROM concepts_fts", ()).await.unwrap();
    }

    let damaged = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert!(damaged.is_empty(), "the damage did not take: {damaged:?}");

    db.rebuild_fts().await.unwrap();

    let restored = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert_eq!(
        restored.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        expected.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        "rebuilding did not restore the index"
    );

    db.close().await.unwrap();
}

/// A retired concept is not a search result. The FTS table indexes only the
/// columns it was declared over, so `retired` has to be filtered on the join —
/// which means it is the kind of thing that silently stops happening.
#[tokio::test]
async fn a_retired_concept_leaves_the_results() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    db.upsert_concept(
        ConceptUpsert::new("lexical", "Distant")
            .content("contains zygomorphic exactly once")
            .valid_from(TS)
            .retired(true),
    )
    .await
    .unwrap();

    let hits = keyword_search(db.read_conn(), &escape_fts5_query("zygomorphic"), 10)
        .await
        .unwrap();
    assert!(
        !hits.iter().any(|(id, _)| id == "lexical"),
        "a retired concept came back from search: {hits:?}"
    );

    db.close().await.unwrap();
}

/// **Doctrine VII's reasoning, applied to the other derivative.** The FTS
/// triggers fire on every concept write; none of that may reach the ledger.
#[tokio::test]
async fn maintaining_the_index_never_touches_the_ledger() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    async fn log_len(db: &Database) -> i64 {
        db.read_conn()
            .query("SELECT COUNT(*) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }

    let before = log_len(&db).await;
    db.rebuild_fts().await.unwrap();
    assert_eq!(
        log_len(&db).await,
        before,
        "rebuilding the search index wrote to transaction_log"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The query language does not leak
// ---------------------------------------------------------------------------

/// Arbitrary text from a search box must not become FTS5 syntax — neither an
/// error nor, worse, a different question.
///
/// `NOT` is the case that matters. Passed raw, `zygomorphic NOT paraphrase`
/// *excludes* documents and returns a plausible, wrong answer. Escaped, it is
/// three literal terms and simply matches nothing.
#[tokio::test]
async fn a_search_box_cannot_become_a_query_expression() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    for hostile in [
        r#"unbalanced " quote"#,
        "trailing AND",
        "title:Distant",
        "zygomorphic NOT paraphrase",
        "*",
        "!!!",
    ] {
        let out = HybridSearch::new(model(), hostile, query_vec())
            .top_k(3)
            .execute(db.read_conn())
            .await;
        assert!(
            out.is_ok(),
            "hostile query {hostile:?} reached FTS5 as syntax: {:?}",
            out.err()
        );
    }

    // And the opt-in path really does hand the expression through, or the
    // escaping above would be guarding nothing.
    let raw_ok = HybridSearch::new(model(), "zygomorphic OR brachiopod", query_vec())
        .raw_match(true)
        .top_k(3)
        .execute(db.read_conn())
        .await;
    assert!(raw_ok.is_ok(), "a valid raw expression was refused: {:?}", raw_ok.err());

    let raw_bad = HybridSearch::new(model(), r#"unbalanced " quote"#, query_vec())
        .raw_match(true)
        .top_k(3)
        .execute(db.read_conn())
        .await;
    assert!(
        raw_bad.is_err(),
        "raw_match must not silently escape; if this passes, the flag does nothing"
    );

    db.close().await.unwrap();
}

/// Fusion is deterministic. Ties are the common case — two documents at the same
/// pair of ranks score identically — and `HashMap` iteration order used to
/// decide them, so the same query could answer in a different order twice.
#[tokio::test]
async fn the_same_query_answers_the_same_way_twice() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let once = HybridSearch::new(model(), "ordinary filler text", query_vec())
        .top_k(6)
        .execute(db.read_conn())
        .await
        .unwrap();
    for _ in 0..8 {
        let again = HybridSearch::new(model(), "ordinary filler text", query_vec())
            .top_k(6)
            .execute(db.read_conn())
            .await
            .unwrap();
        assert_eq!(ids(&once), ids(&again), "fused order is not stable across runs");
    }

    db.close().await.unwrap();
}

/// Depth is why fusion is not just "merge two top-k lists": a document ranked
/// modestly by *both* arms should be reachable, and it is invisible if neither
/// list is read past `top_k`.
#[tokio::test]
async fn reading_each_arm_deeper_than_top_k_is_what_lets_agreement_win() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let shallow = HybridSearch::new(model(), "ordinary filler text", query_vec())
        .top_k(2)
        .depth(2)
        .execute(db.read_conn())
        .await
        .unwrap();
    let deep = HybridSearch::new(model(), "ordinary filler text", query_vec())
        .top_k(2)
        .depth(10)
        .execute(db.read_conn())
        .await
        .unwrap();

    assert_eq!(shallow.len(), 2);
    assert_eq!(deep.len(), 2);
    // Not an ordering claim about which is "better" — only that depth is read
    // and changes what can be considered. A depth parameter nothing consults
    // would leave these identical.
    let deep_has_both_arms = deep
        .iter()
        .any(|h| h.vector_rank.is_some() && h.keyword_rank.is_some());
    assert!(
        deep_has_both_arms,
        "at depth 10 a document agreed on by both arms should surface: {deep:?}"
    );

    db.close().await.unwrap();
}

/// `top_k = 0` is an empty answer, not an FTS5 error and not everything.
#[tokio::test]
async fn asking_for_nothing_returns_nothing() {
    let harness = TestHarness::new();
    let db = fixture(&harness).await;

    let hits = HybridSearch::new(model(), "zygomorphic", query_vec())
        .top_k(0)
        .execute(db.read_conn())
        .await
        .unwrap();
    assert!(hits.is_empty());

    db.close().await.unwrap();
}
