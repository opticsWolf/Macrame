//! W16.1 / C-10: flat bounded fold against fold-per-bound.
//!
//! `reconstruct_on` was first built as one `reconstruct` per **distinct
//! effective instant** — `min(ts, cutoff)` for the reader and each ancestor —
//! keeping from each fold the lineages whose instant it was. That shape reuses
//! `reconstruct` whole, snapshot composition included. It was replaced by a
//! single fold with the ancestry joined in and each lineage cut at its own
//! fork point, on the strength of the numbers this probe produces.
//!
//! # Why this is measured the way it is
//!
//! The first version of this comparison ran the two shapes in **two processes
//! against two builds**, minutes apart, and that is thin evidence for reversing
//! a design. Here both shapes run in one process against one build, alternating
//! passes, and the fold-per-bound shape is rebuilt out of `Database::ancestry`,
//! `Database::reconstruct` and `temporal::resolve_beliefs` — the three steps the
//! removed implementation took, all public — so it is the same work and not an
//! approximation of it.
//!
//! Two things are asserted rather than assumed:
//!
//! * **the two shapes agree**, edge for edge, at every point measured. A
//!   comparison where one side quietly does less work is not a comparison.
//! * **the snapshot configuration actually wrote snapshots.** The cadence is a
//!   background poll, so "snapshots on" is a request, not a fact, and the whole
//!   argument for fold-per-bound rests on composition being available.
//!
//! Three questions:
//!
//! 1. **The multiple.** What does either shape cost against a whole-ledger
//!    `reconstruct` at fork depth 1?
//! 2. **Depth.** Fold-per-bound is linear in fork depth by construction. Is the
//!    flat one flat?
//! 3. **Snapshots.** Composition is the only argument for fold-per-bound. How
//!    much does it actually buy it, and does it buy enough?
//!
//! Run with:  cargo run --release --example reconstruct_on_probe

use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use macrame::prelude::*;
use macrame::temporal::resolve_beliefs;
use macrame::BranchId;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const NODES: usize = 400;
const ROUNDS: usize = 20;
const PASSES: usize = 3;

async fn best_of<F, Fut, T>(rounds: usize, mut f: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        let out = f().await;
        best = best.min(t.elapsed());
        std::hint::black_box(out);
    }
    best
}

fn id(n: &str) -> BranchId {
    BranchId::new(n).unwrap()
}

/// `NODES` concepts, a chain of edges, and a fork chain with churn between the
/// forks so that every fork point is a distinct instant.
async fn seeded(
    dir: &std::path::Path,
    depth: usize,
    snapshots: Option<i64>,
) -> (Database, std::path::PathBuf) {
    let name = format!(
        "d{depth}_{}.db",
        if snapshots.is_some() { "snap" } else { "raw" }
    );
    let cadence = snapshots.map(|n| {
        SnapshotCadence::default()
            .every_entries(n)
            .poll_interval(std::time::Duration::from_millis(20))
    });
    let path = dir.join(name);
    let db = Database::open_with_cadence(&path, cadence).await.unwrap();

    let names: Vec<String> = (0..NODES).map(|i| format!("n{i:04}")).collect();
    db.write_concepts(
        names
            .iter()
            .map(|n| ConceptUpsert::new(n.as_str(), "t").valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();
    db.bulk_import(
        names
            .windows(2)
            .map(|w| {
                EdgeAssertion::new(w[0].as_str(), w[1].as_str(), "CITES")
                    .valid_from(T0)
                    .valid_to(FOREVER)
            })
            .collect(),
    )
    .await
    .unwrap();

    let mut parent = BranchId::main();
    for i in 1..=depth {
        let child = id(&format!("b{i}"));
        db.fork(child.clone(), parent).await.unwrap();
        parent = child;
        // A write between forks, so no two fork points share an instant and the
        // bounds cannot collapse into one fold by accident.
        db.assert_edge(
            EdgeAssertion::new(names[i].as_str(), names[NODES - 1 - i].as_str(), "CITES")
                .valid_from(T0)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
    }

    // The cadence is a background poll; give it a chance to fire before the
    // measurement asks whether it did.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    (db, path)
}

/// The shape that was replaced, rebuilt from the public API.
///
/// One `reconstruct` per distinct effective instant, keeping from each fold only
/// the lineages whose instant it is, then the same nearest-lineage resolution
/// the shipped path applies. Step for step what `reconstruct_on` did before it
/// grew its own fold.
async fn fold_per_bound(db: &Database, ts: &str, branch: &str) -> Vec<EdgeBelief> {
    let ancestry = db.ancestry(branch).await.unwrap();
    let mut by_bound: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for a in &ancestry {
        let bound = match a.cutoff.as_deref() {
            Some(c) if c < ts => c,
            _ => ts,
        };
        by_bound.entry(bound).or_default().push(&a.branch_id);
    }

    let mut visible: Vec<EdgeBelief> = Vec::new();
    for (bound, names) in &by_bound {
        let state = db.reconstruct(bound).await.unwrap();
        visible.extend(
            state
                .edges
                .into_iter()
                .filter(|e| names.iter().any(|n| *n == e.branch_id)),
        );
    }
    resolve_beliefs(&visible, &ancestry)
}

/// How many snapshot files the handle's directory holds.
///
/// "Snapshots on" is a request to a background poll, not a fact, and every
/// number in the lower half of this report is meaningless if the answer is 0.
fn snapshot_count(db_path: &std::path::Path) -> usize {
    let mut dir = db_path.to_path_buf();
    let stem = db_path.file_stem().and_then(|s| s.to_str()).unwrap_or("x");
    dir.set_file_name(format!("{stem}_snapshots"));
    std::fs::read_dir(dir)
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_recon_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    println!(
        "reconstruct_on_probe — crate {}\n{NODES} concepts, best of {ROUNDS}, {PASSES} \
         alternating passes\n",
        env!("CARGO_PKG_VERSION")
    );

    for snapshots in [None, Some(200_i64)] {
        println!(
            "snapshots: {}",
            match snapshots {
                Some(n) => format!("every {n} entries"),
                None => "off".to_string(),
            }
        );
        for depth in [1_usize, 8] {
            let (db, path) = seeded(&dir, depth, snapshots).await;
            let leaf = format!("b{depth}");

            // The comparison is void unless the two shapes answer the same.
            let flat = db.reconstruct_on(FOREVER, &leaf).await.unwrap().edges;
            let per_bound = fold_per_bound(&db, FOREVER, &leaf).await;
            assert_eq!(
                flat, per_bound,
                "depth {depth}: the two shapes disagree, so the timings below                  are not comparing one thing"
            );

            let files = snapshot_count(&path);
            match snapshots {
                Some(_) => assert!(files > 0, "asked for snapshots and got none written"),
                None => assert_eq!(files, 0, "snapshots off, but files appeared"),
            }

            // Alternating passes: whatever the machine is doing drifts across
            // both shapes rather than into one of them.
            let mut whole = Duration::MAX;
            let mut one = Duration::MAX;
            let mut multi = Duration::MAX;
            for _ in 0..PASSES {
                whole = whole.min(
                    best_of(ROUNDS, || async { db.reconstruct(FOREVER).await.unwrap() }).await,
                );
                one = one.min(
                    best_of(ROUNDS, || async {
                        db.reconstruct_on(FOREVER, &leaf).await.unwrap()
                    })
                    .await,
                );
                multi = multi.min(
                    best_of(ROUNDS, || async {
                        fold_per_bound(&db, FOREVER, &leaf).await
                    })
                    .await,
                );
            }

            let us = |d: Duration| d.as_secs_f64() * 1e6;
            println!(
                "  depth {depth}  ({files} snapshot file(s), {} edges)\n                 \x20   reconstruct (whole ledger) {:>9.1} µs\n                 \x20   flat bounded fold          {:>9.1} µs   ({:.2}x)\n                 \x20   fold per bound             {:>9.1} µs   ({:.2}x)",
                flat.len(),
                us(whole),
                us(one),
                us(one) / us(whole),
                us(multi),
                us(multi) / us(whole),
            );
            db.close().await.unwrap();
        }
        println!();
    }

    let _ = std::fs::remove_dir_all(&dir);
}
