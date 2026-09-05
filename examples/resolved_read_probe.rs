//! W16.1, step N: what the bound ancestry does to a **whole read**.
//!
//! `ancestry_resolve_probe` measured the ancestry in isolation and in one join
//! shape, which is what a *design* question needs. It is not what a release
//! note needs. −52% on a fragment that is 4 µs of a 400 µs read is a rounding
//! error a caller will never see, and +12% at depth 16 on the same fragment is
//! equally invisible — but neither of those sentences can be written without
//! measuring the read the caller actually issues.
//!
//! So this probe uses **only the public API**, deliberately: it compiles and
//! runs unchanged against the 0.15.16 tree and against this one, so the
//! before/after is the same source measuring two builds rather than two
//! programs measuring themselves.
//!
//!   git stash && cargo run --release --example resolved_read_probe   # before
//!   git stash pop && cargo run --release --example resolved_read_probe  # after
//!
//! Four reads, chosen because they are the ones whose SQL changed:
//!
//! 1. **`edges(plan)` on a fork** — the flat projection read, current belief.
//!    The one with the least other work around the ancestry, so the largest
//!    proportional effect will be here.
//! 2. **`edges(plan)` at a recorded instant** — the same read with the fold in
//!    it, which is the expensive shape.
//! 3. **a traversal on a fork** — the recursive walk, where the ancestry is a
//!    small part of a large statement.
//! 4. **`edges(plan)` on the trunk** — the shape that binds no ancestry at all,
//!    included as the control: if this moves, the measurement is noise.
//!
//! Each at fork depth 1 and depth 8, because the isolated probe found the two
//! forms cross over near depth 13 and the question is whether that crossover is
//! reachable from a read.
//!
//! Run with:  cargo run --release --example resolved_read_probe

use std::time::{Duration, Instant};

use macrame::prelude::*;
use macrame::BranchId;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const NODES: usize = 400;
const ROUNDS: usize = 40;

/// Best-of, because the question is what the work costs and not what the
/// machine was doing at the time.
async fn best_of<F, Fut, T>(rounds: usize, mut f: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        // Held, not dropped: a discarded result in a timing loop is a timing
        // loop that measures the error path.
        let out = f().await;
        best = best.min(t.elapsed());
        std::hint::black_box(out);
    }
    best
}

fn id(n: &str) -> BranchId {
    BranchId::new(n).unwrap()
}

/// A ledger with `NODES` concepts, a chain of edges through them, and a fork
/// chain `b1 → … → b{depth}` with post-fork churn on the trunk.
///
/// The churn matters: without it every ancestor's rows are pre-cutoff, the
/// `churned` set is empty and `links_cut` degenerates to the projection —
/// which is the *cheap* shape and would understate what a resolved read costs.
async fn seeded(dir: &std::path::Path, depth: usize) -> Database {
    let db = Database::open(dir.join(format!("d{depth}.db")))
        .await
        .unwrap();

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
    }

    // The trunk moves after every fork, so each ancestor has post-cutoff rows
    // and the fold arm of `links_cut` is not empty.
    db.bulk_import(
        names
            .windows(3)
            .step_by(7)
            .map(|w| {
                EdgeAssertion::new(w[0].as_str(), w[2].as_str(), "CITES")
                    .valid_from(T0)
                    .valid_to(FOREVER)
            })
            .collect(),
    )
    .await
    .unwrap();

    db
}

fn line(label: &str, d: Duration, rows: usize) {
    println!(
        "  {label:<44} {:>9.1} µs   {rows} rows",
        d.as_secs_f64() * 1e6
    );
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_read_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    println!(
        "resolved_read_probe — crate {}\n{} concepts, best of {ROUNDS}\n",
        env!("CARGO_PKG_VERSION"),
        NODES
    );

    for depth in [1_usize, 8] {
        let db = seeded(&dir, depth).await;
        let leaf = format!("b{depth}");
        println!("fork depth {depth} (reading `{leaf}`)");

        // 1. the flat projection read, current belief
        let plan = ReadPlan::new().on(id(&leaf)).valid_at(T0);
        let rows = db.edges(plan.clone()).await.unwrap().len();
        let t = best_of(ROUNDS, || {
            let p = plan.clone();
            async { db.edges(p).await.unwrap() }
        })
        .await;
        line("edges(plan), current belief", t, rows);

        // 2. the same read with the transaction-time fold in it
        let folded = ReadPlan::new()
            .on(id(&leaf))
            .valid_at(T0)
            .recorded_at(FOREVER);
        let rows = db.edges(folded.clone()).await.unwrap().len();
        let t = best_of(ROUNDS, || {
            let p = folded.clone();
            async { db.edges(p).await.unwrap() }
        })
        .await;
        line("edges(plan), at a recorded instant", t, rows);

        // 3. the recursive walk
        let walk = TraversalBuilder::new("n0000")
            .max_depth(6)
            .on_branch(id(&leaf));
        let rows = walk.execute_ids(db.read_conn(), T0).await.unwrap().len();
        let t = best_of(ROUNDS, || async {
            walk.clone().execute_ids(db.read_conn(), T0).await.unwrap()
        })
        .await;
        line("traverse, depth 6", t, rows);

        // 4. the control: no ancestry is bound on a root
        let trunk = ReadPlan::new().valid_at(T0);
        let rows = db.edges(trunk.clone()).await.unwrap().len();
        let t = best_of(ROUNDS, || {
            let p = trunk.clone();
            async { db.edges(p).await.unwrap() }
        })
        .await;
        line("edges(plan) on the trunk  [control]", t, rows);

        db.close().await.unwrap();
        println!();
    }

    let _ = std::fs::remove_dir_all(&dir);
}
