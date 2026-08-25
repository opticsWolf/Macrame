//! W10.4: what is `low_starved_run_max` on a workload shaped like use?
//!
//! [D-153] measured the `biased` `select!`'s missing floor and got a number that
//! hits the bound completely: a 39-edge chunked `bulk_import` raced by **64
//! concurrently spawned** `upsert_concept` calls gave `starved_turns=63`,
//! `run_max=63`, identically across five runs. The bulk import sat behind every
//! queued interactive write with no interleaving at all.
//!
//! What that establishes is that the mechanism is unbounded and trivially
//! reachable. What it does not establish is that a caller ever gets there — 64
//! tasks spawned at once is a burst nobody has claimed is representative, and
//! the same test asserts `run_max == 0` for a caller writing sequentially. The
//! plan's instruction for this wave is therefore **a reading first and a policy
//! only if the reading earns one**.
//!
//! ```text
//! cargo run --release --features metrics --example fairness_probe
//! ```
//!
//! # The hypothesis this was built to test, and what happened to it
//!
//! The obvious candidate for "what makes D-153's fixture synthetic" is that it
//! is **open-loop**: 64 writes fired into the channel without waiting, where
//! application code is **closed-loop** — each writer awaits its own write
//! before issuing the next. If that were the difference, the number of
//! high-priority commands queueable at once would be the number of concurrent
//! writers, and `run_max` would be small for any sane concurrency.
//!
//! **It is not the difference, and the sweep below is what says so.** Four
//! closed-loop writers in a tight loop — an entirely ordinary shape — starve
//! the low tier for essentially all 80 of their writes. The run is bounded by
//! **how long the caller keeps offering interactive work**, not by how many
//! callers there are.
//!
//! What does break the run is **think time**: 1 ms between a writer's writes
//! takes four writers from ~78 down to ~2. That lever belongs to the caller,
//! which is the finding W10.4 turns into a decision.
//!
//! The last section measures the other side of the trade — what a forced yield
//! would be admitting into an interactive write's worst case.
//!
//! [D-153]: ../docs/architecture/s13-decision-register.md#d-153

use std::sync::Arc;
use std::time::{Duration, Instant};

use macrame::metrics::CommandKind;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The low-priority work being starved: a chunked import big enough to span
/// many actor turns, so a run has room to grow.
const IMPORT_EDGES: usize = 900;
/// Interactive writes each closed-loop writer issues, one after another.
const WRITES_PER_WRITER: usize = 20;

async fn seeded(path: &std::path::Path) -> Arc<Database> {
    let db = Database::open_with_cadence(path, None).await.unwrap();
    let concepts: Vec<_> = (0..=IMPORT_EDGES + 4_000)
        .map(|i| ConceptUpsert::new(format!("c{i:06}"), "N").valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();
    Arc::new(db)
}

fn import_edges() -> Vec<EdgeAssertion> {
    (1..=IMPORT_EDGES)
        .map(|i| {
            EdgeAssertion::new("c000000", format!("c{i:06}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect()
}

#[derive(Debug)]
struct Reading {
    starved_turns: u64,
    run_max: u64,
    turns: u64,
    import_wall: Duration,
}

fn read(db: &Database, import_wall: Duration) -> Reading {
    let s = db.metrics();
    Reading {
        starved_turns: s.low_starved_turns,
        run_max: s.low_starved_run_max,
        turns: s.turns,
        import_wall,
    }
}

/// `writers` concurrent **closed-loop** writers racing one chunked import.
///
/// Each writer awaits its own `upsert_concept` before issuing the next, which
/// is the shape application code has and the shape D-153's fixture does not.
/// `think` is the pause between a writer's writes — zero is a caller in a tight
/// loop, which is already pessimistic for an interactive path.
async fn closed_loop(path: &std::path::Path, writers: usize, think: Duration) -> Reading {
    let db = seeded(path).await;

    let importer = {
        let db = Arc::clone(&db);
        tokio::spawn(async move {
            let t = Instant::now();
            db.bulk_import(import_edges()).await.unwrap();
            t.elapsed()
        })
    };

    let mut hands = Vec::with_capacity(writers);
    for w in 0..writers {
        let db = Arc::clone(&db);
        hands.push(tokio::spawn(async move {
            for i in 0..WRITES_PER_WRITER {
                db.upsert_concept(
                    ConceptUpsert::new(format!("w{w:03}_{i:03}"), "W").valid_from(TS),
                )
                .await
                .unwrap();
                if !think.is_zero() {
                    tokio::time::sleep(think).await;
                }
            }
        }));
    }
    for h in hands {
        h.await.unwrap();
    }
    let import_wall = importer.await.unwrap();

    let reading = read(&db, import_wall);
    Arc::into_inner(db).unwrap().close().await.unwrap();
    reading
}

/// D-153's fixture: `writers` interactive writes **spawned at once**, none of
/// them waiting on another. Offered load with no relationship to service rate.
async fn open_loop(path: &std::path::Path, writers: usize) -> Reading {
    let db = seeded(path).await;

    let importer = {
        let db = Arc::clone(&db);
        tokio::spawn(async move {
            let t = Instant::now();
            db.bulk_import(import_edges()).await.unwrap();
            t.elapsed()
        })
    };

    let mut hands = Vec::with_capacity(writers);
    for w in 0..writers {
        let db = Arc::clone(&db);
        hands.push(tokio::spawn(async move {
            db.upsert_concept(ConceptUpsert::new(format!("b{w:04}"), "B").valid_from(TS))
                .await
                .unwrap();
        }));
    }
    for h in hands {
        h.await.unwrap();
    }
    let import_wall = importer.await.unwrap();

    let reading = read(&db, import_wall);
    Arc::into_inner(db).unwrap().close().await.unwrap();
    reading
}

/// The longest hold each low-priority kind takes, on a database with something
/// for each of them to do.
///
/// This is what a fairness floor would add to an interactive write's worst
/// case, because "after N starved turns, take one low-priority command" cannot
/// choose *which* one: the low queue is an mpsc channel and its head is not
/// inspectable. `Archive` and `Rehydrate` are budget-exempt **by contract**
/// (`CHUNK_BUDGET`'s table) — they have no latency bound and never claimed one.
async fn longest_low_holds(path: &std::path::Path) {
    let db = seeded(path).await;
    db.bulk_import(import_edges()).await.unwrap();

    // Close every interval, so the archive has a real backlog to move rather
    // than an empty session to report on.
    let retire: Vec<EdgeAssertion> = (1..=IMPORT_EDGES)
        .map(|i| {
            EdgeAssertion::new("c000000", format!("c{i:06}"), "LINKS")
                .valid_from(TS)
                .valid_to("2026-06-01T00:00:00.000000Z")
        })
        .collect();
    db.bulk_import(retire).await.unwrap();
    db.rebuild_current().await.unwrap();

    db.archive("2027-01-01T00:00:00.000000Z").await.unwrap();
    db.rebuild_fts().await.unwrap();
    db.analyze().await.unwrap();

    let snap = db.metrics();
    for kind in [
        CommandKind::Archive,
        CommandKind::Analyze,
        CommandKind::RebuildFts,
        CommandKind::BulkImportChunk,
    ] {
        if let Some(k) = snap.kinds.iter().find(|k| k.kind == kind && k.turns > 0) {
            println!(
                "  {:<20} longest hold {:<12?} budget-exempt: {}",
                k.kind.as_str(),
                k.longest,
                kind.exempt_from_budget()
            );
        }
    }
    println!("  (archive on an 8,000-key backlog was measured at 3.3 s unwindowed, D-080.)");

    Arc::into_inner(db).unwrap().close().await.unwrap();
}

fn row(label: &str, r: &Reading) {
    println!(
        "  {label:<38} run_max={:<5} starved={:<6} turns={:<6} import={:?}",
        r.run_max, r.starved_turns, r.turns, r.import_wall
    );
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_fairness_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut n = 0usize;
    let mut next = || {
        n += 1;
        dir.join(format!("f{n}.db"))
    };

    println!("import = {IMPORT_EDGES} edges, {WRITES_PER_WRITER} writes per writer\n");

    println!("===== closed loop: each writer awaits its own write =====");
    for writers in [1usize, 2, 4, 8, 16, 64] {
        let r = closed_loop(&next(), writers, Duration::ZERO).await;
        row(&format!("{writers} writer(s), no think time"), &r);
    }

    println!("\n===== closed loop, 1 ms between a writer's writes =====");
    for writers in [4usize, 16, 64] {
        let r = closed_loop(&next(), writers, Duration::from_millis(1)).await;
        row(&format!("{writers} writer(s), 1 ms think"), &r);
    }

    println!("\n===== open loop: D-153's fixture, spawned all at once =====");
    for writers in [64usize, 256, 1024] {
        let r = open_loop(&next(), writers).await;
        row(&format!("{writers} writes fired without waiting"), &r);
    }

    println!("\n===== what a floor would insert into the interactive path =====");
    println!(
        "  The obvious floor is \"after N starved turns, take one low-priority \
         command\". These are the\n  low-tier holds that command could be; \
         `CHUNK_BUDGET` exempts some of them *by contract*."
    );
    longest_low_holds(&next()).await;

    println!(
        "\nRead the run_max column against the writer count, and then against \
         the think-time block: the\nrun is bounded by how long the caller keeps \
         offering interactive work -- not by concurrency, and not by \
         anything in the crate."
    );

    let _ = std::fs::remove_dir_all(&dir);
}
