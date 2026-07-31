//! T1.3: what does `write_bulk_atomic` actually cost as a function of batch size?
//!
//! The item asks for a warning computed as "rows × measured per-row cost". That
//! model assumes the cost is linear, and it is not: `write_edges_atomic` opens
//! with `reject_overlaps_within`, which compares every pair in the batch — O(n²)
//! before a single row is written. So there are two terms, and which one
//! dominates depends on the size, which is exactly what a caller trying to
//! predict their stall needs to know.
//!
//! Three quantities per size:
//!
//!   - **hold** — the actor turn, from its own counters (D-079). This is what a
//!     concurrent interactive assertion waits for, and the only number the
//!     warning has any business quoting.
//!   - **µs/row** — hold ÷ rows. If the cost were linear this would be flat.
//!   - **guard** — `reject_overlaps_within` alone, called directly. Not a
//!     public entry point, so this is measured on an equivalent local copy and
//!     labelled as such rather than presented as an instrumented reading.
//!
//! The batch is deliberately *worst case for the guard*: every edge shares a
//! source, so the early `continue` on mismatched endpoints never fires and all
//! n(n−1)/2 pairs are compared in full. A batch of unrelated edges is much
//! cheaper, which is itself worth knowing — the ceiling a caller can predict
//! from the signature has to be the worst case, not the typical one.
//!
//! Run with:  cargo run --release --features metrics --example bulk_atomic_diag

use macrame::graph::EdgeAssertion;
use macrame::metrics::CommandKind;
use macrame::{ConceptUpsert, Database};

const EPOCH: &str = "2026-01-01T00:00:00.000000Z";

/// The two batch shapes, which cost very different amounts in the guard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `n` distinct targets off one source. Every pair mismatches on
    /// `target_id`, so the guard's early `continue` fires and the inner loop is
    /// three string comparisons. This is what a normal bulk write looks like.
    Fanout,
    /// `n` assertions about the **same** `(source, target, type)` at distinct
    /// non-overlapping closed intervals — legal, since only one interval may be
    /// *open*. Every pair reaches `Interval::new` and `overlaps`, so the guard
    /// runs its expensive path n(n−1)/2 times. A batch of corrections to one
    /// relationship's history has exactly this shape.
    History,
}

fn batch(shape: Shape, n: usize) -> Vec<EdgeAssertion> {
    (0..n)
        .map(|i| {
            // Distinct, adjacent, non-overlapping microseconds. `valid_to` is
            // closed, so `defer_to_single_open` never short-circuits either.
            let from = format!("2026-01-01T00:00:00.{:06}Z", i);
            let to = format!("2026-01-01T00:00:00.{:06}Z", i + 1);
            let target = match shape {
                Shape::Fanout => format!("t{i:07}"),
                Shape::History => "t0000000".to_string(),
            };
            EdgeAssertion::new("src", target, "LINKS")
                .valid_from(from)
                .valid_to(to)
        })
        .collect()
}

async fn hold_ms(shape: Shape, n: usize) -> f64 {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("b.db");
    let db = Database::open_with_cadence(&path, None).await.unwrap();

    let mut ids: Vec<String> = (0..n).map(|i| format!("t{i:07}")).collect();
    ids.push("src".to_string());
    for chunk in ids.chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id, "n").valid_from(EPOCH))
                .collect(),
        )
        .await
        .unwrap();
    }

    db.write_bulk_atomic(batch(shape, n)).await.unwrap();

    let k = db
        .metrics()
        .kinds
        .iter()
        .find(|k| k.kind == CommandKind::WriteBulkAtomic)
        .unwrap()
        .clone();
    assert_eq!(k.turns, 1, "the batch did not reach the actor as one turn");

    db.close().await.unwrap();
    k.longest.as_secs_f64() * 1000.0
}

#[tokio::main]
async fn main() {
    println!("libSQL 0.9.30, best of 3. `hold` is the actor turn (T1.4).");
    println!("`predicted` is macrame::connection::estimated_bulk_hold.\n");
    println!(
        "{:>8} {:>10} {:>11} {:>11} {:>11} {:>11}",
        "rows", "shape", "hold ms", "us/row", "predicted", "ratio"
    );

    for shape in [Shape::Fanout, Shape::History] {
        let label = match shape {
            Shape::Fanout => "fanout",
            Shape::History => "history",
        };
        for n in [100usize, 500, 2_000, 5_000, 10_000, 20_000] {
            let mut best = f64::MAX;
            for _ in 0..3 {
                best = best.min(hold_ms(shape, n).await);
            }
            let predicted =
                macrame::connection::estimated_bulk_hold(&batch(shape, n)).as_secs_f64() * 1000.0;
            println!(
                "{:>8} {:>10} {:>11.1} {:>11.1} {:>11.1} {:>10.2}x",
                n,
                label,
                best,
                best * 1000.0 / n as f64,
                predicted,
                predicted / best
            );
        }
        println!();
    }
}
