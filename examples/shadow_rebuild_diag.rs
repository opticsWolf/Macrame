//! T1.2: how much of the rebuild does chunking actually take off the lock?
//!
//! The shadow idea promises "the swap is microseconds". It is not, and the
//! reason is structural rather than an implementation shortfall: index names are
//! global and SQLite has no `ALTER INDEX … RENAME`, so `links_current`'s three
//! indexes can only be built once its name is free — inside the swap. What the
//! chunking moves off the lock is the *projection*, the O(E log E) window
//! function over all of `links`.
//!
//! So the number that decides whether T1.2 is worth having is not the total (it
//! will be worse — more turns, more transactions, a second table written) but
//! **the longest single hold**, which is what every other writer waits for.
//!
//! Three columns:
//!
//!   - `atomic` — `rebuild_current`, one turn, the whole repair under the lock.
//!   - `chunked max` — the longest single turn of the chunked path. In practice
//!     the swap, and the honest headline figure.
//!   - `chunked total` — every shadow turn added up, which is what the chunked
//!     path costs the machine rather than the caller.
//!
//! Run with:  cargo run --release --features metrics --example shadow_rebuild_diag

use macrame::graph::EdgeAssertion;
use macrame::metrics::CommandKind;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// `keys` interval keys asserted `generations` times, so `links` holds
/// `keys × generations` rows and `links_current` holds `keys`. The superseded
/// history is what makes the window function rank rather than degenerate.
async fn seed(db: &Database, keys: usize, generations: usize) {
    let ids: Vec<String> = (0..=keys).map(|i| format!("c{i:07}")).collect();
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
    for generation in 0..generations {
        let batch: Vec<_> = (0..keys)
            .map(|k| {
                EdgeAssertion::new(&ids[k], &ids[k + 1], "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
                    .weight(1.0 + generation as f64)
            })
            .collect();
        for chunk in batch.chunks(2_000) {
            db.bulk_import(chunk.to_vec()).await.unwrap();
        }
    }
}

struct Run {
    turns: u64,
    max_ms: f64,
    total_ms: f64,
}

fn read(db: &Database, kind: CommandKind) -> Run {
    let k = db
        .metrics()
        .kinds
        .iter()
        .find(|k| k.kind == kind)
        .unwrap()
        .clone();
    Run {
        turns: k.turns,
        max_ms: k.longest.as_secs_f64() * 1000.0,
        total_ms: k.mean.as_secs_f64() * 1000.0 * k.turns as f64,
    }
}

#[tokio::main]
async fn main() {
    println!("libSQL 0.9.30, best of 3 by `chunked max`.");
    println!("`atomic` is rebuild_current; the rest is rebuild_current_chunked.\n");
    println!(
        "{:>8} {:>8} {:>11} {:>8} {:>13} {:>15} {:>9}",
        "links", "current", "atomic ms", "turns", "chunked max ms", "chunked total ms", "hold cut"
    );

    for (keys, generations) in [(1_000usize, 4usize), (4_000, 4), (10_000, 4)] {
        let mut best: Option<(f64, f64, u64, f64, i64, i64)> = None;

        for _ in 0..3 {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("s.db");
            let db = Database::open_with_cadence(&path, None).await.unwrap();
            seed(&db, keys, generations).await;

            let count = |sql: &'static str| {
                let conn = db.read_conn().clone();
                async move {
                    conn.query(sql, ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .get::<i64>(0)
                        .unwrap()
                }
            };
            let n_links = count("SELECT COUNT(*) FROM links").await;
            let n_current = count("SELECT COUNT(*) FROM links_current").await;

            db.rebuild_current().await.unwrap();
            let atomic = read(&db, CommandKind::RebuildCurrent).max_ms;

            db.rebuild_current_chunked().await.unwrap();
            let chunked = read(&db, CommandKind::ShadowRebuild);

            db.close().await.unwrap();

            let candidate = (
                atomic,
                chunked.max_ms,
                chunked.turns,
                chunked.total_ms,
                n_links,
                n_current,
            );
            if best.as_ref().is_none_or(|b| candidate.1 < b.1) {
                best = Some(candidate);
            }
        }

        let (atomic, max, turns, total, n_links, n_current) = best.unwrap();
        println!(
            "{:>8} {:>8} {:>11.1} {:>8} {:>13.1} {:>15.1} {:>8.1}x",
            n_links,
            n_current,
            atomic,
            turns,
            max,
            total,
            atomic / max
        );
    }
}
