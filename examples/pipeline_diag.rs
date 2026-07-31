//! T3.4: how much does sending bulk chunks ahead actually buy?
//!
//! The bulk paths used to `await` each chunk before building the next, so the
//! actor finished a chunk and then idled for a channel round trip before the
//! following one arrived. The item's estimate is that depth 2–4 captures most of
//! the win. This measures it rather than adopting it.
//!
//! The comparison is run against **this crate's own shipped path** at several
//! depths, by re-implementing the send loop here with the depth as a parameter.
//! That duplicates a little of `low_chunked`, and the alternative — making
//! `PIPELINE_DEPTH` runtime-configurable so a benchmark can sweep it — would put
//! a knob in the public API purely to measure it once.
//!
//! Two things are reported, because they can disagree:
//!
//!   - **total** wall time for the whole import, which is what pipelining is for;
//!   - **longest hold**, from the actor's own counters (T1.4), which must *not*
//!     move. Pipelining changes when commands are sent, never how big they are,
//!     so a rising hold would mean the change did something it should not have.
//!
//! Run with:  cargo run --release --features metrics --example pipeline_diag

use std::collections::VecDeque;
use std::time::Instant;

use macrame::connection::chunk_rows;
use macrame::graph::EdgeAssertion;
use macrame::metrics::CommandKind;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// One import at a given pipeline depth. Depth 1 is the pre-T3.4 behaviour.
async fn import_at_depth(
    db: &std::sync::Arc<Database>,
    edges: Vec<EdgeAssertion>,
    depth: usize,
) -> usize {
    let chunks: Vec<Vec<EdgeAssertion>> = edges
        .chunks(chunk_rows::EDGES)
        .map(<[_]>::to_vec)
        .collect();

    // The shipped helper is private and fixed at PIPELINE_DEPTH, so the sweep
    // drives the public per-chunk call instead: `bulk_import` on a single chunk
    // is exactly one `BulkImportChunk` command, which is the unit being paced.
    //
    // Spawned tasks over an `Arc<Database>` rather than a queue of futures held
    // by reference, because the latter needs `FuturesOrdered` and this crate
    // does not depend on `futures`. Responses are collected in send order, as
    // `low_chunked` does.
    let mut inflight: VecDeque<tokio::task::JoinHandle<usize>> = VecDeque::with_capacity(depth);
    let mut written = 0;
    let mut iter = chunks.into_iter();

    loop {
        while inflight.len() < depth {
            let Some(chunk) = iter.next() else { break };
            let db = std::sync::Arc::clone(db);
            inflight.push_back(tokio::spawn(async move { db.bulk_import(chunk).await.unwrap() }));
        }
        let Some(h) = inflight.pop_front() else { break };
        written += h.await.unwrap();
    }
    written
}

async fn seed_concepts(db: &Database, n: usize) {
    let ids: Vec<String> = (0..=n).map(|i| format!("c{i:07}")).collect();
    for chunk in ids.chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id, "n").valid_from(TS))
                .collect(),
        )
        .await
        .unwrap();
    }
}

fn edges(n: usize) -> Vec<EdgeAssertion> {
    (0..n)
        .map(|i| {
            EdgeAssertion::new(format!("c{i:07}"), format!("c{:07}", i + 1), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect()
}

#[tokio::main]
async fn main() {
    println!("libSQL 0.9.30, best of 3. Depth 1 is the pre-T3.4 behaviour.");
    println!("`hold` must not move: pipelining changes when chunks are sent,");
    println!("never how large they are.\n");
    println!(
        "{:>8} {:>7} {:>11} {:>10} {:>11}",
        "edges", "depth", "total ms", "vs depth 1", "hold ms"
    );

    for n in [20_000usize, 100_000] {
        let mut baseline = f64::NAN;
        for depth in [1usize, 2, 4, 8, 16] {
            let mut best = f64::MAX;
            let mut hold = f64::NAN;

            for _ in 0..3 {
                let dir = tempfile::TempDir::new().unwrap();
                let path = dir.path().join("p.db");
                let db =
                    std::sync::Arc::new(Database::open_with_cadence(&path, None).await.unwrap());
                seed_concepts(&db, n).await;

                let batch = edges(n);
                let start = Instant::now();
                let written = import_at_depth(&db, batch, depth).await;
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(written, n, "the import lost rows at depth {depth}");

                if ms < best {
                    best = ms;
                    hold = db
                        .metrics()
                        .kinds
                        .iter()
                        .find(|k| k.kind == CommandKind::BulkImportChunk)
                        .unwrap()
                        .longest
                        .as_secs_f64()
                        * 1000.0;
                }
                std::sync::Arc::into_inner(db).unwrap().close().await.unwrap();
            }

            if depth == 1 {
                baseline = best;
            }
            println!(
                "{:>8} {:>7} {:>11.1} {:>9.2}x {:>11.2}",
                n,
                depth,
                best,
                baseline / best,
                hold
            );
        }
        println!();
    }
}
