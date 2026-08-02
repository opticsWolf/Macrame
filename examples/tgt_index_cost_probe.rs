//! What did dropping `idx_lc_tgt_active` buy? (0.8.0, B4, D-118)
//!
//! D-089 found the index unread and argued the cost explicitly: an index write
//! on every insert into `links_current`, on the crate's hottest write path,
//! read by nothing. The v7 → v8 rung drops it. **That is a performance claim,
//! and D-088 says this project does not ship one unmeasured.**
//!
//! So: the same `star_of_stars` load, run against the v8 index set and against
//! the same database with `idx_lc_tgt_active` put back — which is exactly the
//! v7 shape, since it is the only difference between them on this table.
//!
//! Run with:  cargo run --release --example tgt_index_cost_probe

#[path = "../tests/common/fixtures.rs"]
mod fixtures;

use fixtures::Shape;
use macrame::prelude::*;
use std::time::{Duration, Instant};

const NODES: usize = 4_000;
const REPEATS: usize = 5;

/// Seed `nodes` concepts, then time the edge assertions alone.
///
/// The concepts are written and *not* timed: they go into `concepts`, which
/// this index never touched, so including them would dilute the difference with
/// work neither arm changes.
async fn run(dir: &std::path::Path, tag: &str, restore_index: bool) -> Duration {
    let path = dir.join(format!("{tag}.db"));
    let _ = std::fs::remove_file(&path);
    let db = Database::open(&path).await.unwrap();

    if restore_index {
        // Exactly as v7 declared it. Added through `raw()` because the crate no
        // longer has DDL for it — which is the point of the measurement.
        db.raw()
            .connect()
            .unwrap()
            .execute(
                "CREATE INDEX idx_lc_tgt_active ON links_current (target_id, valid_to)",
                (),
            )
            .await
            .unwrap();
    }

    db.write_concepts(Shape::StarOfStars.concepts(NODES))
        .await
        .unwrap();

    let edges = Shape::StarOfStars.edges(NODES);
    let started = Instant::now();
    for e in edges {
        db.assert_edge(e).await.unwrap();
    }
    let elapsed = started.elapsed();

    db.close().await.unwrap();
    elapsed
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("tgt_index_cost_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let edges = Shape::StarOfStars.edges(NODES).len();
    println!("star_of_stars, {NODES} nodes, {edges} assert_edge calls, {REPEATS} repeats\n");

    let mut with = Vec::new();
    let mut without = Vec::new();
    for i in 0..REPEATS {
        // Alternating order, so a warming disk or a drifting clock does not
        // land entirely on one arm.
        if i % 2 == 0 {
            with.push(run(&dir, "with", true).await);
            without.push(run(&dir, "without", false).await);
        } else {
            without.push(run(&dir, "without", false).await);
            with.push(run(&dir, "with", true).await);
        }
    }

    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    let a = median(with);
    let b = median(without);
    let per = |d: Duration| d.as_secs_f64() * 1e6 / edges as f64;

    println!(
        "  v7 (idx_lc_tgt_active present): {:>8.1} ms   {:>6.1} us/edge",
        a.as_secs_f64() * 1e3,
        per(a)
    );
    println!(
        "  v8 (dropped):                   {:>8.1} ms   {:>6.1} us/edge",
        b.as_secs_f64() * 1e3,
        per(b)
    );
    println!(
        "\n  delta: {:+.1}% ({:+.2} us/edge)",
        (b.as_secs_f64() / a.as_secs_f64() - 1.0) * 100.0,
        per(b) - per(a)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
