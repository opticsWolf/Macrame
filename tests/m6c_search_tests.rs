//! M6-c's search tests (0.22.0, [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.4 costs 3 and 3b, [Amendment 1](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%201.md)
//! §3.4; the P-3 probe's red test, [K1's](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! high gate).
//!
//! The defect cost 3 and 3b name: **the indexes are per-file, the overlay is
//! per-lineage.** M5 [D-310] made branch keyword search join the lineage's
//! visible view, and M6-b made every resolution a walk — but both arms still
//! matched on the trunk's indexes. `concepts_fts` holds one row per concept,
//! the *mint's* text, so a branch that superseded an inherited concept
//! **searched the trunk's superseded text and returned the concept**, ranked
//! by words the branch repudiated (K1's silent wrong answer, the amendment's
//! own red test). `{model}_embeddings` is keyed `concept_id PRIMARY KEY`, so a
//! branch version's text was *scored with the trunk's vector* (cost 3b) and
//! the overlay's resolution could not drop the hit it contradicted.
//!
//! M6-c's shape, per cost row:
//!
//! - `concept_versions_fts`, external content over its own shadow
//!   (`concept_version_fts_rows`, rowid-bearing — `concept_versions` is
//!   `WITHOUT ROWID`, P-1's §14 remark that P-3 settles), filled by triggers
//!   on the overlay's version rows; a branch keyword search **unions** the
//!   trunk arm and the version arm, then drops every hit whose id resolves
//!   (§5.3) to a *different* version or to absent;
//! - `{model}_version_embeddings` per registered model, filled by the caller
//!   (Doctrine VII: an embedding is derived from content, and a version is
//!   content); a branch vector search unions the same way with the same drop.
//!
//! What a read that names no lineage does is unchanged in both arms: a
//! whole-file search is D-310's file property — searched by the trunk's own
//! text, and *not* by a branch's version text, which the "unbranched" story
//! below pins.
//!
//! Stories are the amendment's §12.7 name plus the read-side gate it opens
//! the door for (the vector twin, the FTS row fidelity, and the window).

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::vector::{keyword_search_on, search_vector_on, ModelName, VectorSearchResult};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const T5: &str = "2026-01-10T00:00:00.000000Z";
const AFTER: &str = "2026-01-11T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
/// The mint's text: `trunk` is a word the branch repudiates.
const OLD: &str = "the trunk's belief";
/// The version's text: `branch` is a word the mint never asserted.
const NEW: &str = "the branch's belief";
/// A version-rewrite on the same pair start (the `DO UPDATE` arm's own text).
const NEWER: &str = "the branch's later belief";

fn model() -> ModelName {
    ModelName::new("m6c_v1").unwrap()
}

/// Unit vectors by angle, the same rule the hybrid suite ranks by.
fn v(rank: usize) -> Vec<f32> {
    let theta = (rank as f32) * std::f32::consts::PI / 16.0;
    vec![theta.cos(), theta.sin()]
}

async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

/// The trunk minted `x` (text `OLD`), a hub reaches it, and `b0` forked. With
/// `with_vectors`, a two-dimensional model is registered and the trunk's
/// vectors are written through the actor.
async fn forked(harness: &TestHarness, with_vectors: bool) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("x", OLD)
            .valid_from(TS)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("a", "Hub")
            .valid_from(TS)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.write_bulk_atomic(vec![EdgeAssertion::new("a", "x", "LINKS")
        .valid_from(TS)
        .valid_to(FOREVER)])
        .await
        .unwrap();
    if with_vectors {
        db.register_model(&model(), 2).await.unwrap();
        db.upsert_embeddings(
            &model(),
            vec![("x".to_string(), v(2)), ("a".to_string(), v(0))],
        )
        .await
        .unwrap();
    }
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    db
}

/// The M6-a takeover: b0 supersedes the trunk's open `x` at `T5` with its own
/// text. One version row on `(x, b0)`; the projection keeps the mint's row.
async fn take_over(db: &std::sync::Arc<Database>) {
    tick().await;
    db.view(BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("x", NEW)
                .supersede(true)
                .valid_from(T5)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
}

/// The branch's version of `x` ends its window at `AFTER` — the windowed
/// takeover the amendment's own sentence reads.
async fn take_over_windowed(db: &std::sync::Arc<Database>) {
    tick().await;
    db.view(BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("x", NEW)
                .supersede(true)
                .valid_from(T5)
                .valid_to(AFTER),
        )
        .await
        .unwrap();
}

/// Branch keyword hits, ids in answer order.
async fn kw(
    conn: &libsql::Connection,
    q: &str,
    valid: Option<&str>,
    branch: Option<&str>,
) -> Vec<String> {
    keyword_search_on(conn, q, 10, valid, None, branch)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Branch vector hits.
async fn vec_hits(
    conn: &libsql::Connection,
    query: &[f32],
    valid: Option<&str>,
    branch: Option<&str>,
) -> Vec<VectorSearchResult> {
    search_vector_on(conn, query, &model(), 10, valid, None, branch)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// §12.7 — the stale-hit red test and what answers on the branch instead
// ---------------------------------------------------------------------------

/// **The trunk's superseded text is not a branch result.** The branch
/// superseded `x` at `T5`; the mint's row (still saying `trunk`) stays on the
/// blue projection unchanged, and `concepts_fts`'s single row per concept
/// therefore MATCHes words the branch repudiated. The branch search drops
/// every hit whose id resolves to a different version — here, to the branch's
/// own — so the answer is exactly the id the branch's text hides under.
///
/// [K1](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
/// priced this a high silent wrong answer; §12.7 is the red test the
/// amendment wrote into the story list.
#[tokio::test]
async fn a_branch_search_for_the_trunks_superseded_text_does_not_return_the_concept() {
    let harness = TestHarness::new();
    let db = forked(&harness, false).await;
    take_over(&db).await;

    let conn = db.read_conn();
    // The mint's words are gone from the branch's answer...
    assert_eq!(
        kw(conn, "trunk belief", None, Some("b0")).await,
        Vec::<String>::new(),
        "the trunk's superseded text is not a branch hit"
    );
    // ...and the branch's own words are, exactly once — the union's two arms
    // cannot both survive for one id, because they would contradict the
    // resolution they are dropped by.
    assert_eq!(
        kw(conn, "branch belief", None, Some("b0")).await,
        vec!["x".to_string()],
        "the branch's own text is the hit that answers"
    );
}

/// **Where the overlay says nothing, the trunk's text is the result.** The
/// forked branch with no version rows of its own searches its trunk-mint text
/// exactly as M5's reader did — this is the switch's disengaged half, pinned
/// here because the version arm only ever carries rows when the overlay has
/// something in it.
#[tokio::test]
async fn an_unversioned_branch_searches_the_mint_s_text() {
    let harness = TestHarness::new();
    let db = forked(&harness, false).await;
    let conn = db.read_conn();

    assert_eq!(
        kw(conn, "trunk belief", None, Some("b0")).await,
        vec!["x".to_string()],
        "no version rows: the mint's text is the hit"
    );
}

/// **A search that names no lineage reads no branch's version text.** The
/// whole-file keyword search answers through `concepts_fts` only — D-310's
/// asymmetry ("the index is a property of the file"). The version's text was
/// written by `b0` after the fork; searching the trunk for it finds nothing,
/// and searching the trunk for the mint's own words still finds the mint's
/// row.
#[tokio::test]
async fn an_unbranched_search_reads_the_trunk_s_corpus_only() {
    let harness = TestHarness::new();
    let db = forked(&harness, false).await;
    take_over(&db).await;
    let conn = db.read_conn();

    assert_eq!(
        kw(conn, "branch belief", None, None).await,
        Vec::<String>::new(),
        "the trunk's index does not hold a branch's version text"
    );
    assert_eq!(
        kw(conn, "trunk belief", None, None).await,
        vec!["x".to_string()],
        "the trunk's own text stands"
    );
    // And the mint-holder's own search reads the mint, not the branch: the
    // branch's rows live on the branch's line, per the fold law.
    assert_eq!(
        kw(conn, "branch belief", None, Some("main")).await,
        Vec::<String>::new(),
        "the trunk's own lineage does not answer the branch's text"
    );
}

/// **The version FTS row is the shadow's, keyed by the pair, and it tracks a
/// rewrite of the pair's text.** The overlay version row is written by the
/// takeover; the import trigger fills the shadow and the FTS; and a
/// re-upsert of the same start (the `DO UPDATE` arm) changes what MATCHes,
/// which is D-303's write-last rule on the overlay, now visible in the index.
#[tokio::test]
async fn the_version_fts_row_tracks_the_pair_s_text() {
    let harness = TestHarness::new();
    let db = forked(&harness, false).await;
    take_over(&db).await;
    let conn = db.read_conn();

    // The shadow row exists, keyed by the trio the fold's channels key by.
    assert_eq!(
        let_one(
            &db,
            "SELECT COUNT(*) FROM concept_version_fts_rows \
                       WHERE id = 'x' AND branch_id = 'b0'"
        )
        .await,
        1,
        "the takeover's version row imported into the shadow"
    );
    // The FTS matches the row it is content for.
    assert_eq!(
        fts_ids(conn, "branch belief").await,
        vec!["x".to_string()],
        "the version's text is in the version FTS"
    );
    assert_eq!(
        fts_ids(conn, "trunk belief").await,
        Vec::<String>::new(),
        "the version FTS does not hold the mint's text"
    );

    // The pair's text moves by the write law's own arms: D-303's
    // write-last rule lets a later stamp rewrite the same start (the
    // takeover's `DO UPDATE` arm reaches it through the API), and the
    // import trigger fires on the `UPDATE`, so the shadow and the index
    // move with it. A plain upsert would refuse (the pair's own open
    // interval is single), so this drives the update arm at the SQL
    // surface the write path's own rewrite reaches - the same statement
    // shape `UPSERT_CONCEPT_VERSION` issues on that arm, the stamp
    // advanced, the texts replaced.
    tick().await;
    // The rewrite's stamp is the row's own, one microsecond later - the
    // law's direction, read from the ledger so the check is against what
    // is actually there.
    let row_stamp = {
        let mut r = db
            .read_conn()
            .query(
                "SELECT recorded_at FROM concept_versions WHERE id = 'x'",
                (),
            )
            .await
            .unwrap();
        r.next().await.unwrap().unwrap().get::<String>(0).unwrap()
    };
    let conn2 = db.raw().connect().unwrap();
    conn2
        .execute(
            "UPDATE concept_versions SET title = ?1, recorded_at = ?2 \
         WHERE id = 'x' AND branch_id = 'b0'",
            libsql::params![NEWER, a_micro_after(&row_stamp)],
        )
        .await
        .expect("a same-start rewrite under a later stamp");
    assert_eq!(
        fts_ids(conn, "later belief").await,
        vec!["x".to_string()],
        "the rewritten text is what the index answers"
    );
    // The retraction: a word only the old text held. The new text keeps
    // `branch`, which is why the first assertion above is the discriminator
    // for the rewrite and this one is for the retraction.
    assert_eq!(
        fts_ids(conn, "trunk").await,
        Vec::<String>::new(),
        "the old text was retracted, not left standing beside the rewrite"
    );
}

/// An external-content FTS row check straight against the index.
async fn fts_ids(conn: &libsql::Connection, q: &str) -> Vec<String> {
    let mut rows = conn
        .query(
            "SELECT s.id FROM concept_versions_fts \
             JOIN concept_version_fts_rows s ON s.seq_id = concept_versions_fts.rowid \
             WHERE concept_versions_fts MATCH ?1 ORDER BY s.id",
            libsql::params![q],
        )
        .await
        .expect("the version FTS exists");
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.expect("row") {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

/// One microsecond after `s` - the direction D-303's law allows a rewrite
/// to stamp with: the takeover's own stamp precedes the rewrite's.
fn a_micro_after(s: &str) -> String {
    let (body, micro) = s.split_once('.').expect("canonical stamp");
    let digits = micro.trim_end_matches('Z');
    let n: u64 = digits.parse().expect("microseconds");
    format!("{body}.{:0>6}Z", n + 1)
}

/// A leveled scalar helper for the raw facts (the same shape every fixture
/// uses), with the stamp the takeover wrote.
async fn let_one(db: &std::sync::Arc<Database>, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    v
}

/// **The windowed takeover: the version's text hits within its window and the
/// trunk's text before it, and nothing past the window.** The same story the
/// content readers tell, on the search surface: before `T5` the walk answers
/// the mint (the version's window has not opened), so the trunk's words hit;
/// inside the window the branch's words hit; past it the cut covers
/// (the far end of the axis) and the id is absent from both arms.
#[tokio::test]
async fn a_windowed_version_hits_within_the_window_and_not_past_it() {
    let harness = TestHarness::new();
    let db = forked(&harness, false).await;
    take_over_windowed(&db).await;
    let conn = db.read_conn();

    // Before the window: the mint's row answers.
    assert_eq!(
        kw(conn, "trunk belief", Some(TS), Some("b0")).await,
        vec!["x".to_string()],
        "before the window the trunk's text is the hit"
    );
    // Inside it: the version's.
    assert_eq!(
        kw(conn, "branch belief", Some(T5), Some("b0")).await,
        vec!["x".to_string()],
        "the window's own text hits within it"
    );
    // Past it: absent on both arms — the walk's cut covers, and no reader
    // revives a cut id.
    assert_eq!(
        kw(conn, "branch belief", Some(AFTER), Some("b0")).await,
        Vec::<String>::new(),
        "past the window the branch's text is not a hit"
    );
    assert_eq!(
        kw(conn, "trunk belief", Some(AFTER), Some("b0")).await,
        Vec::<String>::new(),
        "past the window the trunk's text is not a hit either"
    );
}

// ---------------------------------------------------------------------------
// Cost 3b — the vector twin, and the drop the walk prices into the pool
// ---------------------------------------------------------------------------

/// **A branch vector search scores the version's own vector.** The mint's
/// embedding is the trunk's (it was written for the trunk's text); the
/// version's text is *this* text, so the version embedding the caller writes
/// is what the branch search must score. The mint's row in the index stays
/// where it is — the trunk's reading of `x` does not move — but on `b0` the
/// hit's distance is the version's.
#[tokio::test]
async fn a_branch_vector_search_scores_the_version_s_own_vector() {
    let harness = TestHarness::new();
    let db = forked(&harness, true).await;
    take_over(&db).await;
    // The caller embeds the version's own text: the vector `v(0)` is the
    // query direction, nearer than anything the trunk index holds for `x`
    // (v(2)).
    db.upsert_version_embeddings(
        &model(),
        vec![("x".to_string(), "b0".to_string(), T5.to_string(), v(0))],
    )
    .await
    .unwrap();

    let conn = db.read_conn();
    let hits = vec_hits(conn, &v(0), None, Some("b0")).await;
    let x = hits.iter().find(|h| h.concept_id == "x").expect("x hit");
    assert!(
        x.score < 0.02,
        "the branch hit is the version's vector, not the trunk's: {}",
        x.score
    );
    // And the trunk's reading never moved: an unbranched search scores `x`
    // by the trunk's vector, at the trunk's own distance.
    let trunk = search_vector_on(conn, &v(0), &model(), 10, None, None, None)
        .await
        .unwrap();
    let tx = trunk.iter().find(|h| h.concept_id == "x").expect("x trunk");
    assert!(
        tx.score > 0.02,
        "the trunk hit is scored by its own vector: {}",
        tx.score
    );
}

/// **A hit in the trunk's index drops when the id resolves to a version
/// without one.** The walk answers `x` with the branch's version; the version
/// has no vector yet (it will be embedded in its own time — Doctrine VII), so
/// the trunk's row for `x` is a hit whose id resolves to a *different
/// version*: dropped. `a` (whose resolution is the mint) survives untouched.
/// This is cost 3b's second half: not "search the version vector where one
/// exists" alone, but *no hit survives a resolution it contradicts*.
#[tokio::test]
async fn a_branch_vector_search_drops_a_hit_whose_id_resolves_to_the_version() {
    let harness = TestHarness::new();
    let db = forked(&harness, true).await;
    take_over(&db).await;
    // No version embedding written: the only `x` vector in the file is the
    // trunk's (v(2)), and `a`'s is the query direction (v(0)).
    let conn = db.read_conn();
    let ids: Vec<String> = vec_hits(conn, &v(0), None, Some("b0"))
        .await
        .into_iter()
        .map(|h| h.concept_id)
        .collect();
    assert_eq!(
        ids,
        vec!["a".to_string()],
        "the trunk-pool hit for `x` resolves to a version; dropped"
    );
}

/// **The vector arm obeys the window the way the keyword arm does** — the
/// same three-instant story on a version whose text has a vector.
#[tokio::test]
async fn a_windowed_version_vector_hits_within_the_window_and_not_past_it() {
    let harness = TestHarness::new();
    let db = forked(&harness, true).await;
    take_over_windowed(&db).await;
    db.upsert_version_embeddings(
        &model(),
        vec![("x".to_string(), "b0".to_string(), T5.to_string(), v(0))],
    )
    .await
    .unwrap();
    let conn = db.read_conn();

    // Inside the window: the version's vector answers.
    let ids: Vec<String> = vec_hits(conn, &v(0), Some(T5), Some("b0"))
        .await
        .into_iter()
        .map(|h| h.concept_id)
        .collect();
    assert!(ids.contains(&"x".to_string()), "the window's vector hits");

    // Past it: the cut covers, the id is absent — no drop into the trunk's
    // row, whose id now resolves elsewhere-or-absent.
    let past: Vec<String> = vec_hits(conn, &v(0), Some(AFTER), Some("b0"))
        .await
        .into_iter()
        .map(|h| h.concept_id)
        .collect();
    assert!(
        !past.contains(&"x".to_string()),
        "past the window the version's vector is not a hit"
    );
}
