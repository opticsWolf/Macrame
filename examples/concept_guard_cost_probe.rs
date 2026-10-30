//! 0.20.0: what the concept interval guard costs a write, as a function of
//! the concept's own history.
//!
//! [D-291] put `reject_overlapping_concept` in front of every concept write
//! and recorded its cost as *"unmeasured and waits on the plan-gate
//! re-measurement (P3)"*. P3 measured plans, not writes, so the 0.20.0
//! release bench was the first reading: `write_path/upsert_concept` at
//! **~7 ms** against a ≤ 3 ms budget and 0.18.0's 334–341 µs. That bench
//! renames one concept on every iteration, so the concept it writes carries
//! one log row per iteration so far, and the guard reads them all: it seeks
//! the concept's partition of `idx_txlog_fold_partition` and then
//! `json_extract`s and windows every row in it.
//!
//! This probe separates the two things the bench conflates — the guard's
//! fixed cost and its per-version cost — by writing to a concept that already
//! holds `k` versions, for a range of `k`, and to a fresh id as the control.
//!
//! It uses **only the public API**, so it compiles and runs unchanged against
//! a tree from before the guard (v0.19.1) and against this one.
//!
//!   cargo run --release --example concept_guard_cost_probe
//!
//! [D-291]: ../docs/architecture/s13-decision-register.md#d-291

use std::time::{Duration, Instant};

use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
/// Timed writes per history size, after one discarded warm-up write.
const SAMPLES: usize = 41;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// A fresh file with one concept rewritten `k` times at the same valid start,
/// which is the shape the bench builds by accident.
async fn with_history(dir: &std::path::Path, k: usize) -> Database {
    let path = dir.join(format!("k{k}.db"));
    let _ = std::fs::remove_file(&path);
    let db = Database::open_with_cadence(&path, None).await.unwrap();
    for i in 0..k {
        db.upsert_concept(ConceptUpsert::new("c", format!("v{i}")).valid_from(TS))
            .await
            .unwrap();
    }
    db
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("concept_guard_cost_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    println!("concept_guard_cost_probe — median of {SAMPLES} writes after a discarded warm-up\n");
    println!(
        "    {:>8} {:>16} {:>16}",
        "history", "rewrite same id", "fresh id"
    );

    for k in [1usize, 10, 100, 1_000, 3_000, 10_000] {
        let db = with_history(&dir, k).await;

        // The rewrite: one more version on the concept that holds `k`. The
        // samples add SAMPLES versions on top, which is under 5% of every
        // history from 1,000 up and is why the small rows are not flat.
        let mut same = Vec::with_capacity(SAMPLES);
        for i in 0..=SAMPLES {
            let t = Instant::now();
            db.upsert_concept(ConceptUpsert::new("c", format!("r{i}")).valid_from(TS))
                .await
                .unwrap();
            if i > 0 {
                same.push(t.elapsed());
            }
        }

        // The control: a new id on the same file, so the same ledger size
        // and a history of zero.
        let mut fresh = Vec::with_capacity(SAMPLES);
        for i in 0..=SAMPLES {
            let t = Instant::now();
            db.upsert_concept(ConceptUpsert::new(format!("f{i}"), "f").valid_from(TS))
                .await
                .unwrap();
            if i > 0 {
                fresh.push(t.elapsed());
            }
        }

        println!(
            "    {:>8} {:>14.1}µs {:>14.1}µs",
            k,
            us(median(same)),
            us(median(fresh)),
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
