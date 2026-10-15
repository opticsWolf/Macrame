//! What the 0.20.0 conversation surface costs (D-291 … D-295).
//!
//!     cargo run --release --example conversation_probe [turns]
//!
//! One session of `turns` turns on the trunk, each written as one
//! `write_batch` (turn concept, NEXT edge from the head, HEAD moved); a fork
//! after the middle turn; `turns / 5` more turns on the fork. Then per-call
//! means for the operations a chat harness repeats. A probe, not a benchmark:
//! it prints numbers for the decision register and asserts only that the
//! answers are right.

use std::time::{Duration, Instant, SystemTime};

use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::util::FakeClock;
use macrame::vector::{keyword_search, keyword_search_on};
use macrame::{BranchId, ConceptUpsert, Database, PointerSet, ReadPlan, WriteBatch};

const VF: &str = "2026-01-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

fn vf(k: usize) -> String {
    // One second per turn of valid time, from the harness's own clock.
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600 + k as u64);
    macrame::util::timestamp::format(t)
}

/// One turn as ONE call: the pointer move links from the head inside the
/// transaction (`PointerSet::link_previous`).
async fn append_linked(db: &Database, branch: Option<&BranchId>, id: &str, k: usize) -> Duration {
    let started = Instant::now();
    let mut batch = WriteBatch::new()
        .concept(
            ConceptUpsert::new(id, format!("turn {k} lorem ipsum"))
                .valid_from(VF)
                .content(format!("message {k} about lexers and parsers")),
        )
        .pointer(
            PointerSet::new("session", "HEAD", id)
                .valid_from(vf(k))
                .link_previous("NEXT"),
        );
    if let Some(b) = branch {
        batch = batch.on_branch(b.clone());
    }
    db.write_batch(batch).await.unwrap();
    started.elapsed()
}

async fn append(db: &Database, branch: Option<&BranchId>, id: &str, k: usize) -> Duration {
    let plan = match branch {
        Some(b) => ReadPlan::new().on(b.clone()),
        None => ReadPlan::new(),
    };
    let started = Instant::now();
    let head = db
        .pointer("session", "HEAD", plan)
        .await
        .unwrap()
        .unwrap_or_else(|| "session".into());
    let mut batch = WriteBatch::new()
        .concept(
            ConceptUpsert::new(id, format!("turn {k} lorem ipsum"))
                .valid_from(VF)
                .content(format!("message {k} about lexers and parsers")),
        )
        .edge(EdgeAssertion::new(&head, id, "NEXT").valid_from(VF))
        .pointer(PointerSet::new("session", "HEAD", id).valid_from(vf(k)));
    if let Some(b) = branch {
        batch = batch.on_branch(b.clone());
    }
    db.write_batch(batch).await.unwrap();
    started.elapsed()
}

fn mean_us(v: &[Duration]) -> f64 {
    v.iter().map(|d| d.as_secs_f64()).sum::<f64>() / v.len() as f64 * 1e6
}

async fn time<F, Fut, T>(reps: usize, mut f: F) -> f64
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut d = Vec::with_capacity(reps);
    for _ in 0..reps {
        let s = Instant::now();
        let _ = f().await;
        d.push(s.elapsed());
    }
    mean_us(&d)
}

#[tokio::main]
async fn main() {
    let turns: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let dir = tempfile::tempdir().unwrap();
    let keep = std::env::var("PROBE_KEEP").ok();
    let clock = std::sync::Arc::new(FakeClock::new(
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600),
    ));
    let path = keep
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.path().join("probe.db"));
    let db = Database::open_with_clock(path, None, clock).await.unwrap();
    db.register_edge_kind("HEAD", true).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("session", "s").valid_from(VF))
        .await
        .unwrap();

    let mut trunk_appends = Vec::new();
    for k in 0..turns {
        trunk_appends.push(append(&db, None, &format!("t{k}"), k).await);
    }
    // Where the fork is cut, as a fraction of the trunk (default: the middle).
    let frac: f64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.5);
    let cut = ((turns as f64 * frac) as usize).min(turns - 1);
    let mid = format!("t{cut}");
    let alt = BranchId::new("alt").unwrap();
    let fork_started = Instant::now();
    db.fork_after(alt.clone(), BranchId::main(), &mid)
        .await
        .unwrap();
    let fork_us = fork_started.elapsed().as_secs_f64() * 1e6;

    let mut alt_appends = Vec::new();
    let mut alt_linked = Vec::new();
    for k in 0..turns / 5 {
        // Alternate the two spellings so both see the same lineage size.
        let id = format!("a{k}");
        if k % 2 == 0 {
            alt_appends.push(append(&db, Some(&alt), &id, turns + k).await);
        } else {
            alt_linked.push(append_linked(&db, Some(&alt), &id, turns + k).await);
        }
    }
    assert_eq!(
        db.pointer("session", "HEAD", ReadPlan::new().on(alt.clone()))
            .await
            .unwrap()
            .as_deref(),
        Some(format!("a{}", turns / 5 - 1).as_str())
    );

    let depth = turns * 2;
    let walk_main = TraversalBuilder::new("session")
        .max_depth(depth)
        .edge_types(vec!["NEXT".into()]);
    let walk_alt = walk_main.clone().on_branch("alt");
    let path_main = time(5, || walk_main.execute_ids(db.read_conn(), NOW)).await;
    let path_alt = time(5, || walk_alt.execute_ids(db.read_conn(), NOW)).await;
    let n_alt = TraversalBuilder::new("session")
        .max_depth(depth)
        .edge_types(vec!["NEXT".into()])
        .on_branch("alt")
        .execute_ids(db.read_conn(), NOW)
        .await
        .unwrap()
        .len();
    assert_eq!(n_alt, cut + 1 + turns / 5 + 1, "session + t0..t_cut + a*");

    let ptr_main = time(50, || db.pointer("session", "HEAD", ReadPlan::new())).await;
    let ptr_alt = time(50, || {
        db.pointer("session", "HEAD", ReadPlan::new().on(alt.clone()))
    })
    .await;
    let kw_all = time(20, || {
        keyword_search(db.read_conn(), "lexers", 10, None, None)
    })
    .await;
    let kw_alt = time(20, || {
        keyword_search_on(db.read_conn(), "lexers", 10, None, None, Some("alt"))
    })
    .await;
    let ids: Vec<String> = (0..50).map(|k| format!("t{k}")).collect();
    let cbi_alt = time(20, || {
        db.concepts_by_id(&ids, ReadPlan::new().on(alt.clone()))
    })
    .await;
    let cbi_main = time(20, || db.concepts_by_id(&ids, ReadPlan::new())).await;
    let cursor = db.head_seq().await.unwrap() - 300;
    let feed = time(20, || db.changes(cursor, None, &[], 1000)).await;

    println!(
        "turns on trunk: {turns}, fork after t{cut}, turns on fork: {}",
        turns / 5
    );
    println!(
        "append (pointer read + write_batch), trunk mean: {:>9.1} us",
        mean_us(&trunk_appends)
    );
    println!(
        "append (pointer read + write_batch), fork mean:  {:>9.1} us",
        mean_us(&alt_appends)
    );
    println!(
        "append as one call (link_previous), fork mean:   {:>9.1} us",
        mean_us(&alt_linked)
    );
    println!(
        "fork_after:                                      {:>9.1} us",
        fork_us
    );
    println!(
        "path walk (NEXT, depth {depth}), trunk:          {:>9.1} us",
        path_main
    );
    println!(
        "path walk (NEXT, depth {depth}), fork:           {:>9.1} us",
        path_alt
    );
    println!(
        "pointer read, trunk:                             {:>9.1} us",
        ptr_main
    );
    println!(
        "pointer read, fork:                              {:>9.1} us",
        ptr_alt
    );
    println!(
        "keyword_search, whole file:                      {:>9.1} us",
        kw_all
    );
    println!(
        "keyword_search_on, fork:                         {:>9.1} us",
        kw_alt
    );
    println!(
        "concepts_by_id x50, trunk:                       {:>9.1} us",
        cbi_main
    );
    println!(
        "concepts_by_id x50, fork:                        {:>9.1} us",
        cbi_alt
    );
    println!(
        "changes (last 300 rows):                         {:>9.1} us",
        feed
    );
}
