//! Criterion benchmarks for the §9 performance budgets (D-055).
//!
//! §9 has carried a table of targets since 0.4.0 and, until this file, nothing
//! measured any of them — the table said "these budgets are CI gates" and it was
//! not true. §8 specifies the harness; this is it.
//!
//! # What these do and do not do
//!
//! They **measure**. They are deliberately **not** absolute CI gates, and that is
//! a departure from §9's own wording, argued in D-055 rather than assumed:
//!
//! * §9's numbers are stated for named reference hardware (Windows 11, NVMe SSD,
//!   32 GB RAM, release build). CI does not run on that machine, so a gate on
//!   `≤ 5 ms` is a gate on whichever runner picked up the job. The project's own
//!   position on that is already on record — a suite that fails for reasons
//!   unrelated to the code under test trains people to ignore red (the R15 note
//!   in `Cargo.toml`), and `the_loader_stays_linear_in_edge_count` asserts a
//!   *ratio* precisely because "absolute timings are a property of the machine,
//!   the growth rate is a property of the algorithm" (D-047).
//! * The regression mechanism that *is* meaningful compares a machine against
//!   itself, which is what criterion baselines are for:
//!
//!   ```text
//!   cargo bench --bench budgets -- --save-baseline before
//!   # …change something…
//!   cargo bench --bench budgets -- --baseline before
//!   ```
//!
//! So a §9 row is now checked by running this and reading the number, and the
//! budgets have stopped being unfalsifiable. Where a hardware-independent gate is
//! possible it belongs in `tests/` as an assertion about shape, next to the two
//! that already exist (D-042's plan shape, D-047's growth rate).
//!
//! # Scale
//!
//! §9's larger rows — 100K concepts, 1M log entries, 10M edges — cost minutes of
//! fixture construction each and are not built by default. `MACRAME_BENCH_SCALE`
//! multiplies every fixture size, so `MACRAME_BENCH_SCALE=100` approaches the
//! table's stated sizes. The default is small enough to run on a laptop while
//! still exercising the same code paths, and every group below names the §9 row
//! it corresponds to so the shortfall is visible rather than implied.
//!
//! The cadence (D-053) is **disabled** in every fixture: a background task
//! folding state and writing files mid-measurement is noise that would land in
//! whichever sample happened to overlap it.

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use macrame::prelude::*;
use macrame::temporal::{reconstruct, save_snapshot};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

fn scale() -> usize {
    std::env::var("MACRAME_BENCH_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A fixture database plus the temp dir keeping it alive.
struct Fixture {
    db: Database,
    _dir: tempfile::TempDir,
    path: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.db");
    // Cadence off: see the module note.
    let db = Database::open_with_cadence(&path, None).await.unwrap();
    Fixture {
        db,
        _dir: dir,
        path,
    }
}

fn concept(i: usize) -> ConceptUpsert {
    ConceptUpsert::new(format!("c{i:07}"), format!("Concept {i}"))
        .content(format!("body text for concept number {i}"))
        .valid_from(TS)
}

/// `n` concepts, written in bulk rather than one round trip each.
async fn seed_concepts(db: &Database, n: usize) {
    for chunk in (0..n).collect::<Vec<_>>().chunks(2_000) {
        db.write_annotations(chunk.iter().map(|i| concept(*i)).collect())
            .await
            .unwrap();
    }
}

/// A star of `edges` edges out of `c0000000`, three hops deep.
async fn seed_edges(db: &Database, edges: usize) {
    let mut batch = Vec::with_capacity(edges);
    for i in 1..=edges {
        // Depth is i's position in a simple chain-of-stars, so a 3-hop walk from
        // the root reaches a bounded, predictable slice rather than everything.
        let src = if i <= edges / 3 {
            0
        } else if i <= 2 * (edges / 3) {
            i - edges / 3
        } else {
            i - 2 * (edges / 3)
        };
        batch.push(
            EdgeAssertion::new(format!("c{src:07}"), format!("c{i:07}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for chunk in batch.chunks(2_000) {
        db.bulk_import(chunk.to_vec()).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// §9: the write path
// ---------------------------------------------------------------------------

/// `Single edge assertion ≤ 5 ms`, `Single concept upsert ≤ 3 ms`,
/// `Chunk commit, 500 rows ≤ 3 ms`.
fn write_path(c: &mut Criterion) {
    let rt = runtime();
    let fx = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, 2_000 * scale()).await;
        fx
    });

    let mut group = c.benchmark_group("write_path");
    group.sample_size(50);

    // Bound as a reference: `async move` inside an `FnMut` would otherwise try to
    // move the handle out of the fixture on every iteration.
    let db = &fx.db;

    let counter = std::cell::Cell::new(0usize);
    group.bench_function("assert_edge (§9 ≤ 5 ms)", |b| {
        b.to_async(&rt).iter(|| {
            let i = counter.get();
            counter.set(i + 1);
            async move {
                // Distinct interval per iteration: re-asserting the same open
                // interval is what trg_links_single_open exists to refuse, and
                // benchmarking a rejection measures the guard, not the write.
                db
                    .assert_edge(
                        EdgeAssertion::new("c0000000", "c0000001", format!("B{i}"))
                            .valid_from(TS)
                            .valid_to(OPEN),
                    )
                    .await
                    .unwrap()
            }
        })
    });

    let ucount = std::cell::Cell::new(0usize);
    group.bench_function("upsert_concept (§9 ≤ 3 ms)", |b| {
        b.to_async(&rt).iter(|| {
            let i = ucount.get();
            ucount.set(i + 1);
            async move {
                db
                    .upsert_concept(
                        ConceptUpsert::new("c0000000", format!("Rename {i}")).valid_from(TS),
                    )
                    .await
                    .unwrap()
            }
        })
    });

    group.finish();

    // Its own group, because it needs a fresh database per iteration and
    // therefore a different shape of harness.
    //
    // **The first version of this measured its own fixture growing.** It reused
    // one database and appended 500 edges per iteration under a fresh edge type,
    // so by the end of a run the table held tens of thousands of rows that the
    // first iteration had not. The tell was the spread: criterion reported
    // `[296 ms … 780 ms]`, a 2.6× range where every other row in this file is
    // flat to a few percent, and a stable operation does not do that. A
    // benchmark whose fixture changes while it is being sampled is measuring the
    // fixture.
    //
    // `iter_batched` fixes it properly: setup is **not timed**, so each iteration
    // commits its 500 rows into a database in the same state as every other one.
    let mut group = c.benchmark_group("chunk_commit");
    group.sample_size(10);
    group.bench_function("500_rows_trigger_amplified (§9 ≤ 3 ms)", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    seed_concepts(&fx.db, 501).await;
                    fx
                })
            },
            |fx| {
                let edges: Vec<EdgeAssertion> = (0..500)
                    .map(|k| {
                        EdgeAssertion::new("c0000000", format!("c{:07}", k + 1), "CHUNK")
                            .valid_from(TS)
                            .valid_to(OPEN)
                    })
                    .collect();
                rt.block_on(fx.db.write_bulk_atomic(edges)).unwrap()
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// §9: traversal
// ---------------------------------------------------------------------------

/// `3-hop traversal, warm cache (1K edges) ≤ 10 ms` and the `as_of` row at
/// `≤ 15 ms`. The cold-cache variant is not here: flushing the OS page cache is
/// not something a benchmark can do portably, and pretending otherwise would
/// produce a number labelled "cold" that is warm.
fn traversal(c: &mut Criterion) {
    let rt = runtime();
    let edges = 1_000 * scale();
    let fx = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, edges + 1).await;
        seed_edges(&fx.db, edges).await;
        fx
    });

    let mut group = c.benchmark_group("traversal");
    group.sample_size(50);

    group.bench_function("three_hop_warm (§9 ≤ 10 ms)", |b| {
        b.to_async(&rt).iter(|| async {
            TraversalBuilder::new("c0000000")
                .max_depth(3)
                .execute_ids(fx.db.read_conn(), TS)
                .await
                .unwrap()
        })
    });

    group.bench_function("as_of_edges (§9 ≤ 15 ms)", |b| {
        b.to_async(&rt)
            .iter(|| async { query_as_of_edges(fx.db.read_conn(), TS).await.unwrap() })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// §9: reconstruction
// ---------------------------------------------------------------------------

/// `reconstruct(ts), 10K log entries, no snapshot ≤ 100 ms` and the composed
/// row, `with snapshot ≤ 200 ms` at 1M — the two paths D-049 requires to agree,
/// measured side by side so the point of having a snapshot is visible as a
/// number rather than asserted.
fn replay(c: &mut Criterion) {
    let rt = runtime();
    let n = 5_000 * scale();
    let (fx, now, snaps) = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, n).await;
        let now: String = fx
            .db
            .read_conn()
            .query("SELECT MAX(recorded_at) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();

        // An anchor taken most of the way through, so the composed path folds a
        // small delta and the difference between the two rows is the whole log.
        let snaps = fx.path.parent().unwrap().join("bench_snaps");
        let base = reconstruct(fx.db.read_conn(), &now, None, None).await.unwrap();
        save_snapshot(&snaps, &base).unwrap();
        for i in n..n + 50 {
            fx.db.upsert_concept(concept(i)).await.unwrap();
        }
        let now: String = fx
            .db
            .read_conn()
            .query("SELECT MAX(recorded_at) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        (fx, now, snaps)
    });

    let mut group = c.benchmark_group("replay");
    group.sample_size(20);

    group.bench_function("reconstruct_full_fold (§9 ≤ 100 ms @ 10K)", |b| {
        b.to_async(&rt)
            .iter(|| async { reconstruct(fx.db.read_conn(), &now, None, None).await.unwrap() })
    });

    group.bench_function("reconstruct_composed (§9 ≤ 200 ms @ 1M)", |b| {
        b.to_async(&rt).iter(|| async {
            reconstruct(fx.db.read_conn(), &now, None, Some(&snaps))
                .await
                .unwrap()
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// §9: integrity
// ---------------------------------------------------------------------------

/// `audit_current() (100K edges) ≤ 200 ms` and `rebuild_current() ≤ 500 ms`.
///
/// `rebuild_current` writes, so it goes through the actor and each iteration
/// re-does the whole delete-and-reinsert. That is the operation §9 prices, and
/// it is why this group's sample size is small.
fn integrity(c: &mut Criterion) {
    let rt = runtime();
    let edges = 2_000 * scale();
    let fx = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, edges + 1).await;
        seed_edges(&fx.db, edges).await;
        fx
    });

    let mut group = c.benchmark_group("integrity");
    group.sample_size(10);

    group.bench_function("audit_current (§9 ≤ 200 ms @ 100K)", |b| {
        b.to_async(&rt)
            .iter(|| async { audit_current(fx.db.read_conn()).await.unwrap() })
    });

    group.bench_function("rebuild_current (§9 ≤ 500 ms @ 100K)", |b| {
        b.to_async(&rt)
            .iter(|| async { fx.db.rebuild_current().await.unwrap() })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// §9: search
// ---------------------------------------------------------------------------

/// `Vector top-10 search (100K concepts) ≤ 20 ms` and
/// `Hybrid search, top-10 ≤ 50 ms` — the latter unreachable before D-051 and
/// therefore never measured at all.
fn search(c: &mut Criterion) {
    let rt = runtime();
    let n = 2_000 * scale();
    let (fx, model) = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, n).await;
        let model = ModelName::new("bench_v1").unwrap();
        fx.db.register_model(&model, 8).await.unwrap();
        let rows: Vec<(String, Vec<f32>)> = (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                (
                    format!("c{i:07}"),
                    (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                )
            })
            .collect();
        fx.db.upsert_embeddings(&model, rows).await.unwrap();
        (fx, model)
    });

    let query: Vec<f32> = (0..8).map(|k| ((k as f32) * 0.37).sin()).collect();

    let mut group = c.benchmark_group("search");
    group.sample_size(50);

    group.bench_function("vector_top10 (§9 ≤ 20 ms @ 100K)", |b| {
        b.to_async(&rt).iter(|| async {
            search_vector(fx.db.read_conn(), &query, &model, 10)
                .await
                .unwrap()
        })
    });

    group.bench_function("hybrid_top10 (§9 ≤ 50 ms @ 100K)", |b| {
        b.to_async(&rt).iter(|| async {
            HybridSearch::new(model.clone(), "body text concept", query.clone())
                .top_k(10)
                .execute(fx.db.read_conn())
                .await
                .unwrap()
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// §9: snapshot write
// ---------------------------------------------------------------------------

/// `Snapshot write (100K-edge state) ≤ 2 s` — the read-fold, bincode and zstd,
/// which is what the cadence pays every `every_entries` log rows (D-053).
fn snapshot(c: &mut Criterion) {
    let rt = runtime();
    let edges = 2_000 * scale();
    let (fx, state) = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, edges + 1).await;
        seed_edges(&fx.db, edges).await;
        let now: String = fx
            .db
            .read_conn()
            .query("SELECT MAX(recorded_at) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let state = reconstruct(fx.db.read_conn(), &now, None, None).await.unwrap();
        (fx, state)
    });

    let mut group = c.benchmark_group("snapshot");
    group.sample_size(20);

    // A fresh directory per iteration, or retention starts deleting mid-measure
    // and the timing includes a variable amount of unlinking.
    group.bench_function("save_snapshot (§9 ≤ 2 s @ 100K edges)", |b| {
        b.iter_batched(
            || tempfile::TempDir::new().unwrap(),
            |dir| save_snapshot(dir.path(), &state).unwrap(),
            BatchSize::SmallInput,
        )
    });

    let _ = &fx;
    group.finish();
}

criterion_group!(
    budgets,
    write_path,
    traversal,
    replay,
    integrity,
    search,
    snapshot
);
criterion_main!(budgets);
