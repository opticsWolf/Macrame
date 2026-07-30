//! T0.2: what does repair actually cost, and what does `archive()` pay for it?
//!
//! §5.7's published archive cost does not include the re-derivation `archive()`
//! runs inside its own write transaction, and the crate's own budget table lists
//! `rebuild_current` at "~50 s per 10M edges" with nothing behind the figure.
//! This measures the parts separately, on libSQL 0.9.30, so the number in the
//! document is a measurement rather than an estimate.
//!
//! The three costs, all under the archive's write lock when the archive is the
//! caller:
//!
//!   1. `DELETE FROM links_current`            — O(E)
//!   2. the window-function reprojection       — O(E log E)
//!   3. `audit_current`, two `EXCEPT` passes   — O(E log E) each
//!
//! (3) is what T0.2/D-077 removes from the archive path.
//!
//! Run with:  cargo run --release --example repair_diag

use std::time::Instant;

use macrame::graph::EdgeAssertion;
use macrame::integrity::audit_current;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// `edges` assertions over `edges/4` distinct interval keys, so a quarter of the
/// rows are superseded history — which is what makes the window function do work
/// rather than degenerate to one row per partition.
async fn seed(db: &Database, edges: usize) {
    let keys = (edges / 4).max(1);
    let nodes = keys + 1;

    for chunk in (0..nodes).collect::<Vec<_>>().chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|i| ConceptUpsert::new(format!("c{i:07}"), "n").valid_from(TS))
                .collect(),
        )
        .await
        .unwrap();
    }

    // Four assertions per key, each at a later recorded_at, so `links_current`
    // projects one and `links` holds four.
    let mut batch = Vec::with_capacity(edges);
    for round in 0..4 {
        for k in 0..keys {
            // Distinct valid_from per round would make these different keys, so
            // the interval is fixed and only recorded_at advances — which the
            // clock supplies, since each assert is its own write.
            batch.push(
                EdgeAssertion::new(format!("c{k:07}"), format!("c{:07}", k + 1), "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
                    .weight(1.0 + round as f64),
            );
        }
    }
    for chunk in batch.chunks(2_000) {
        // Re-assertion of the same interval key: last writer wins in
        // links_current, every row stays in links.
        let _ = db.bulk_import(chunk.to_vec()).await;
    }
}

async fn time_ms<F, T>(f: F) -> (T, f64)
where
    F: std::future::Future<Output = T>,
{
    let start = Instant::now();
    let out = f.await;
    (out, start.elapsed().as_secs_f64() * 1000.0)
}

#[tokio::main]
async fn main() {
    println!("libSQL 0.9.30, best of 5.\n");
    println!(
        "{:>10} {:>10} {:>14} {:>16} {:>14} {:>10}",
        "links", "current", "audit alone ms", "rebuild+audit ms", "rebuild only ms", "audit %"
    );

    for edges in [4_000usize, 16_000, 40_000] {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("r.db");
        let db = Database::open_with_cadence(&path, None).await.unwrap();
        seed(&db, edges).await;

        let conn = db.read_conn();
        let n_links: i64 = {
            let mut r = conn.query("SELECT COUNT(*) FROM links", ()).await.unwrap();
            r.next().await.unwrap().unwrap().get(0).unwrap()
        };
        let n_current: i64 = {
            let mut r = conn
                .query("SELECT COUNT(*) FROM links_current", ())
                .await
                .unwrap();
            r.next().await.unwrap().unwrap().get(0).unwrap()
        };

        // The audit on its own, on the read connection.
        let mut audit_ms = f64::MAX;
        for _ in 0..5 {
            let (r, ms) = time_ms(audit_current(conn)).await;
            r.unwrap();
            audit_ms = audit_ms.min(ms);
        }

        // The public repair, which verifies (Verify::Yes) — the old behaviour on
        // both paths.
        let mut with_ms = f64::MAX;
        for _ in 0..5 {
            let (r, ms) = time_ms(db.rebuild_current()).await;
            r.unwrap();
            with_ms = with_ms.min(ms);
        }

        // The archive path's cost is the rebuild without the audit. There is no
        // public entry point for it by design, so it is derived here rather than
        // measured directly — stated plainly rather than presented as a reading.
        let without_ms = with_ms - audit_ms;

        println!(
            "{:>10} {:>10} {:>14.1} {:>16.1} {:>14.1} {:>9.0}%",
            n_links,
            n_current,
            audit_ms,
            with_ms,
            without_ms,
            100.0 * audit_ms / with_ms
        );

        db.close().await.unwrap();
    }

    println!(
        "\n`rebuild only` is derived (rebuild+audit − audit alone), not measured directly.\n\
         The audit share is what D-077 removes from every archive, under its write lock."
    );
}
