//! P-3 (Amendment 2 §11): a branch search at production scale — 1,000
//! versions over 100,000 trunk concepts — timed, and the fallback decided.
//!
//! The amendment priced two questions against this shape (costs 3 and 3b,
//! K7's storage fear, C9's hope that versions are a small fraction):
//!
//! - is M6-c's version-aware filter cheap enough at this scale — the branch
//!   keyword arm costing its trunk arm plus a per-hit registration drop, the
//!   branch vector arm the same — or does the FTS shape have to fall back to
//!   a per-lineage FTS rebuild ("if the filter scans");
//! - what does the version pool weigh beside the corpus it versions (the K7
//!   "roughly doubles" fear, reported per seeding leg and per version, since
//!   the probe's version share — 1,000 of 100,000 — is exactly C9's hope).
//!
//! The ledger: 100,000 trunk concepts with FTS text and 2-dimensional
//! embeddings on a cone; one fork `b0`; 1,000 supersede versions spread
//! through the corpus, whose text deliberately carries none of the trunk's
//! query words — so the stale-hit red test's healthy answer is the filter's
//! worst case here: every versioned id the trunk arm returns is a hit the
//! registration must drop, and the fetch loop escalates until it fills
//! `top_k` clean hits. "topic 41" gives the light arm: its hits are almost
//! all unversioned, so the arm costs the union plus a near-empty drop.
//!
//!   cargo run --release --example m6_p3_search_scale_probe
//!
//! Median of [`SAMPLES`] calls per arm after a warm-up; one database for the
//! whole run (the crash classes this station shows on many opens in one
//! process are the ones R15 documents).

use std::future::Future;
use std::time::{Duration, Instant};

use macrame::prelude::*;
use macrame::vector::{keyword_search_on, search_vector_on};

#[path = "../tests/common/harness.rs"]
mod harness_db;

/// A valid instant before any window the fixture writes (the searches name
/// no as-of; the arms read at current belief).
const TS: &str = "2026-01-01T00:00:00.000000Z";
/// The versions' starts.
const T_V: &str = "2026-03-01T00:00:00.000000Z";
/// Half-open at the far end.
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// Env knobs, defaults standing in for the amendment's own numbers:
/// M6_P3_NODES=100000 / M6_P3_VERSIONS=1000.
const NODES: usize = 100_000;
const VERSIONS: usize = 1_000;
const TOP_K: usize = 10;

/// The env override so a second configuration verifies an inference without
/// a second artifact.
fn knobs() -> (usize, usize) {
    let nodes = std::env::var("M6_P3_NODES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(NODES);
    let versions = std::env::var("M6_P3_VERSIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VERSIONS);
    (nodes, versions)
}
const SAMPLES: usize = 7;
const BRANCH: &str = "b0";

fn node_id(i: usize) -> String {
    format!("c{i:07}")
}

/// Taken-over ids: every 7th concept, the first `versions` of them.
fn taken(i: usize, versions: usize) -> bool {
    i.is_multiple_of(7) && i < 7 * versions
}

/// Unit points on the cone; the query reads at a fixed angle.
fn v(rank: f32) -> Vec<f32> {
    let theta = rank * std::f32::consts::PI / 16.0;
    vec![theta.cos(), theta.sin()]
}

fn median(mut samples: Vec<Duration>) -> Duration {
    if samples.is_empty() {
        return Duration::from_micros(0);
    }
    samples.sort();
    samples[samples.len() / 2]
}

async fn arm<F>(label: &str, mk: impl Fn() -> F) -> Duration
where
    F: Future<Output = ()>,
{
    mk().await; // warm
    let mut xs: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = Instant::now();
        mk().await;
        xs.push(t0.elapsed());
    }
    let m = median(xs);
    println!("  {label:46} median {:>8} µs", m.as_micros());
    m
}

#[tokio::main]
async fn main() {
    let (nodes, versions) = knobs();
    let harness = harness_db::TestHarness::new();
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());

    // 1. The trunk corpus, the release bench's seeding shape (bulk, 2k chunks).
    println!("seeding {nodes} trunk concepts …");
    let t0 = Instant::now();
    let mut batch: Vec<ConceptUpsert> = Vec::with_capacity(nodes);
    for i in 0..nodes {
        batch.push(
            ConceptUpsert::new(node_id(i), format!("probe item {i}"))
                .content(format!(
                    "corpus words about topic {} for probe item {i}",
                    i % 128
                ))
                .valid_from(TS)
                .valid_to(FOREVER),
        );
    }
    for chunk in batch.chunks(2_000) {
        db.write_concepts(chunk.to_vec()).await.unwrap();
    }
    let seed_done = t0.elapsed();
    let after_corpus = std::fs::metadata(&harness.db_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // 2. The trunk vector pool: one register, then the whole cone.
    println!("writing {nodes} trunk embeddings …");
    let model = ModelName::new("m6_p3").unwrap();
    db.register_model(&model, 2).await.unwrap();
    let pool: Vec<(String, Vec<f32>)> = (0..nodes).map(|i| (node_id(i), v(i as f32))).collect();
    db.upsert_embeddings(&model, pool).await.unwrap();
    let after_pool = std::fs::metadata(&harness.db_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // 3. The fork, then 1,000 versions whose text carries none of the trunk's
    //    query words — the drop filter's worst case, built in.
    println!("fork and {versions} versions …");
    db.fork(BranchId::new(BRANCH).unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(BranchId::new(BRANCH).unwrap());
    let t1 = Instant::now();
    for n in (0..nodes).filter(|&i| taken(i, versions)).take(versions) {
        view.upsert_concept(
            ConceptUpsert::new(node_id(n), format!("branch belief {n}"))
                .supersede(true)
                .valid_from(T_V)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
    }
    let versions_done = t1.elapsed();

    // 4. The version pool for the same 1,000 versions (the callee chunks).
    db.upsert_version_embeddings(
        &model,
        (0..nodes)
            .filter(|&i| taken(i, versions))
            .take(versions)
            .map(|i| {
                (
                    node_id(i),
                    BRANCH.to_string(),
                    T_V.to_string(),
                    v(i as f32 / 3.0),
                )
            })
            .collect(),
    )
    .await
    .unwrap();
    let after_all = std::fs::metadata(&harness.db_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // 5. The arms, medians only.
    let conn = db.read_conn();
    let branch = BRANCH.to_string();

    println!(
        "\n―― searches at {nodes} trunk concepts / {versions} versions (medians of {SAMPLES}) ――"
    );

    let t_light = arm("kw trunk, whole-file, light word (topic 41)", || async {
        let hits = keyword_search_on(conn, "topic 41", TOP_K, None, None, None)
            .await
            .unwrap();
        assert!(!hits.is_empty());
    })
    .await;

    let t_worst = arm("kw trunk, whole-file, widest word (corpus)", || async {
        let hits = keyword_search_on(conn, "corpus", TOP_K, None, None, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), TOP_K);
    })
    .await;

    let b_light = arm("kw branch, light arm (topic 41)", || async {
        let hits = keyword_search_on(conn, "topic 41", TOP_K, None, None, Some(&branch))
            .await
            .unwrap();
        assert!(!hits.is_empty());
        for (id, _) in &hits {
            let n: usize = id.trim_start_matches('c').parse().unwrap();
            assert!(
                !taken(n, versions),
                "stale id kept by the drop filter: {id:?}"
            );
        }
    })
    .await;

    let b_worst = arm(
        &format!("kw branch, worst arm (corpus, {versions} versions bootstrap it)"),
        || async {
            let hits = keyword_search_on(conn, "corpus", TOP_K, None, None, Some(&branch))
                .await
                .unwrap();
            assert_eq!(hits.len(), TOP_K, "escalation should still fill top_k");
            for (id, _) in &hits {
                let n: usize = id.trim_start_matches('c').parse().unwrap();
                assert!(
                    !taken(n, versions),
                    "stale id kept by the drop filter: {id:?}"
                );
            }
        },
    )
    .await;

    let t_vec = arm("vec trunk (whole-file, 100k pool)", || async {
        let hits = search_vector_on(conn, &v(7.0), &model, TOP_K, None, None, None)
            .await
            .unwrap();
        assert!(!hits.is_empty());
    })
    .await;

    let b_vec = arm("vec branch (union + version pool + drop)", || async {
        let hits = search_vector_on(conn, &v(7.0), &model, TOP_K, None, None, Some(&branch))
            .await
            .unwrap();
        assert!(!hits.is_empty());
        for hit in &hits {
            let id = &hit.concept_id;
            let n: usize = id.trim_start_matches('c').parse().unwrap();
            assert!(!taken(n, versions), "stale vector hit kept: {id:?}");
        }
    })
    .await;

    let ctl = arm("control: SELECT 1", || async {
        let mut rows = conn.query("SELECT 1", ()).await.unwrap();
        while rows.next().await.unwrap().is_some() {}
    })
    .await;

    // 6. The decision, printed beside the arms' truth.
    let delta_word = b_worst.as_micros() as i64 - t_worst.as_micros() as i64;
    let delta_light = b_light.as_micros() as i64 - t_light.as_micros() as i64;
    let delta_vec = b_vec.as_micros() as i64 - t_vec.as_micros() as i64;
    let version_pool_bytes = after_all.saturating_sub(after_pool) as f64 / versions as f64;

    println!("\n―― decision (Amendment 2, P-3 deliverables) ――");
    println!("  keyword arm: branch − trunk");
    println!("    light word   {delta_light:>7} µs  (the drop sees almost no versioned hit)");
    println!("    widest word  {delta_word:>7} µs  (the stale ids drop out; the delta barely moves 100 -> 1,000 versions:");
    println!(
        "                      the escalation's whole-corpus re-runs, not the drop, price this arm"
    );
    println!("  vector arm   {delta_vec:>7} µs  (union, version pool, same drop)");
    println!(
        "  control      {:>7} µs  (SELECT 1's own cost)",
        ctl.as_micros()
    );
    println!("  seeding: corpus {seed_done:?}, versions {versions_done:?}");
    println!("\n  storage legs (K7's roughly-doubles question, C9's share):");
    println!("    corpus only                   {after_corpus:>13} B");
    println!("    with the trunk's own pool     {after_pool:>13} B");
    println!("    with the version pool         {after_all:>13} B");
    println!(
        "    version pool adds {version_pool_bytes:.0} B per version at {versions} / {nodes} share"
    );
}
