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

#[path = "../tests/common/fixtures.rs"]
mod fixtures;

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use macrame::graph::{astar, dijkstra, k_core, louvain, scc};
use macrame::prelude::*;
use macrame::temporal::{hydrate_attributes, reconstruct, save_snapshot};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

fn scale() -> usize {
    std::env::var("MACRAME_BENCH_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

// ---------------------------------------------------------------------------
// The control row (T4.3, D-090)
// ---------------------------------------------------------------------------

/// A fixed trivial operation, and the runtime and connection it runs on.
///
/// Process-wide and built once, so the control is the same work in every group
/// and its cost is a property of the machine and the session rather than of any
/// fixture. In-memory, so no disk state accumulates across the run and the
/// hundredth measurement of it is the same operation as the first.
fn control_conn() -> &'static (tokio::runtime::Runtime, libsql::Connection) {
    static CONTROL: std::sync::OnceLock<(tokio::runtime::Runtime, libsql::Connection)> =
        std::sync::OnceLock::new();
    CONTROL.get_or_init(|| {
        let rt = runtime();
        let conn = rt.block_on(async {
            let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
            db.connect().unwrap()
        });
        (rt, conn)
    })
}

/// Open a benchmark group **with its control row already in it** (T4.3).
///
/// D-070 established that this project's absolute timings carry ~29%
/// session-to-session noise. That makes cross-run comparison meaningless, and
/// the damage is that it is not visible from a results table — a 20% "win" and
/// a 20% measurement artifact print identically. The fix is a fixed operation
/// measured in the same session, so every figure can be read as a ratio to
/// something whose true cost did not change.
///
/// **Why this is a constructor and not a convention.** T4.3 says "add a control
/// row to every bench group", and a rule of that shape is followed until the
/// next group is added in a hurry. Here the only way to obtain a
/// `BenchmarkGroup` in this file is through this function, which has already
/// added the row before it returns — so a group without a control is not
/// something a person has to remember not to write. `no_group_is_opened_without_
/// its_control` in `tests/bench_control_tests.rs` keeps the back door shut.
///
/// The control is a `SELECT 1` round trip rather than a pure-CPU loop, because
/// the noise being corrected for is not only CPU: it is thermal state, the
/// scheduler, and libSQL's own overhead, and a hashing loop tracks the first of
/// those and none of the rest.
fn controlled_group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.bench_function("control/select_1", |b| {
        let (rt, conn) = control_conn();
        b.iter(|| {
            rt.block_on(async {
                let mut rows = conn.query("SELECT 1", ()).await.unwrap();
                rows.next().await.unwrap().unwrap();
            })
        })
    });
    g
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
        db.write_concepts(chunk.iter().map(|i| concept(*i)).collect())
            .await
            .unwrap();
    }
}

/// A star of `edges` edges out of `c0000000`, three hops deep.
///
/// **This is `fixtures::Shape::StarOfStars`, and it is now the matrix's
/// definition rather than a second copy of it** (T4.1). Every §9 figure in this
/// file was taken on this shape and only this shape, which is the fact D-088
/// exists to make visible — see `fixture_matrix` below for what the other three
/// do to the same measurements.
async fn seed_edges(db: &Database, edges: usize) {
    // `Shape::edges` is written in nodes; a star of `n` nodes has `n - 1` edges.
    for chunk in fixtures::Shape::StarOfStars.edges(edges + 1).chunks(2_000) {
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

    let mut group = controlled_group(c, "write_path");
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
    let mut group = controlled_group(c, "chunk_commit");
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

    // Where the chunk's time actually goes (D-056).
    //
    // Hoisting the prepared statement took 500 rows from ≈62 ms to ≈36 ms, so
    // preparation was ~40% of it and not, as first guessed, most of it. The
    // remaining ~72 µs per row is either the engine's three writes or the
    // triggers' own work — and `trg_links_log_insert` builds a JSON payload per
    // row with `json_object(…)` including `json(NEW.properties)`, which parses
    // and re-serialises on every insert.
    //
    // This isolates it by dropping the two triggers on a scratch database and
    // measuring the same 500-row commit. The difference is what Doctrine IV's
    // ledger payload costs, and it is the number §9's ≤ 3 ms has to be
    // reconciled against — a budget cannot be met by optimising work that is
    // definitionally required.
    let mut group = controlled_group(c, "chunk_commit_diagnostic");
    group.sample_size(10);
    group.bench_function("500_rows_no_triggers (trigger cost isolation)", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    seed_concepts(&fx.db, 501).await;
                    // A second connection: the actor owns the write one. Dropping
                    // the triggers is legal — the delete guards protect rows, not
                    // schema — and this database is thrown away immediately.
                    let raw = libsql::Builder::new_local(&fx.path)
                        .build()
                        .await
                        .unwrap();
                    let conn = raw.connect().unwrap();
                    for t in ["trg_links_log_insert", "trg_links_current_sync"] {
                        conn.execute(&format!("DROP TRIGGER IF EXISTS {t}"), ())
                            .await
                            .unwrap();
                    }
                    (fx, raw, conn)
                })
            },
            |(fx, _raw, conn)| {
                rt.block_on(async {
                    let tx = conn
                        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                        .await
                        .unwrap();
                    let stmt = tx.prepare(INSERT_LINK_SQL).await.unwrap();
                    for k in 0..500 {
                        stmt.reset();
                        stmt.execute(libsql::params![
                            "c0000000",
                            format!("c{:07}", k + 1),
                            "CHUNK",
                            TS,
                            OPEN,
                            1.0f64,
                            "{}",
                            TS
                        ])
                        .await
                        .unwrap();
                    }
                    drop(stmt);
                    tx.commit().await.unwrap();
                });
                fx
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// The same statement `write_edges_atomic` uses, restated for the diagnostic
/// above because `INSERT_LINK` is private to the crate.
const INSERT_LINK_SQL: &str = "INSERT INTO links \
     (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

/// Each bulk path at *its own* chunk size, against the 3 ms bound (D-058).
///
/// This is the row §9 should have had all along: one measurement per path, each
/// at the size that path actually chunks at, all answerable to one duration. The
/// old single `Chunk commit, 500 rows ≤ 3 ms` row could not be that, because 500
/// rows is 2.3 ms on one of these paths and 67 ms on another.
///
/// A bench and not a test, for [D-055](../docs/architecture/s13-decision-register.md)'s
/// reason: `assert!(elapsed <= 3ms)` on unknown CI hardware asserts something
/// about the runner. What makes the bound falsifiable is that these four numbers
/// are printed next to it.
fn chunk_budget(c: &mut Criterion) {
    let rt = runtime();

    let mut group = controlled_group(c, "chunk_budget");
    group.sample_size(20);

    group.bench_function(
        format!("edges/{} (§5.1.5 ≤ 3 ms)", chunk_rows::EDGES),
        |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, chunk_rows::EDGES + 1).await;
                        fx
                    })
                },
                |fx| {
                    let edges: Vec<EdgeAssertion> = (0..chunk_rows::EDGES)
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
        },
    );

    group.bench_function(
        format!("concepts/{} (§5.1.5 ≤ 3 ms)", chunk_rows::CONCEPTS),
        |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, chunk_rows::CONCEPTS).await;
                        fx
                    })
                },
                |fx| {
                    let rows: Vec<ConceptUpsert> = (0..chunk_rows::CONCEPTS)
                        .map(|i| {
                            ConceptUpsert::new(format!("c{i:07}"), format!("Rewritten {i}"))
                                .content(format!("new body for {i}"))
                                .valid_from(TS)
                        })
                        .collect();
                    rt.block_on(fx.db.write_concepts(rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        },
    );

    group.bench_function(
        format!("annotations/{} (§5.1.5 ≤ 3 ms)", chunk_rows::ANNOTATIONS),
        |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, chunk_rows::ANNOTATIONS).await;
                        fx
                    })
                },
                |fx| {
                    let rows: Vec<Annotation> = (0..chunk_rows::ANNOTATIONS)
                        .map(|i| Annotation {
                            concept_id: format!("c{i:07}"),
                            label: "community".into(),
                            value: format!("{}", i % 7),
                        })
                        .collect();
                    rt.block_on(fx.db.write_analytics_annotations(rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        },
    );

    group.bench_function(
        format!("embeddings/{} (§5.1.5 ≤ 3 ms)", chunk_rows::EMBEDDINGS),
        |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, chunk_rows::EMBEDDINGS).await;
                        let model = ModelName::new("bench_v1").unwrap();
                        fx.db.register_model(&model, 8).await.unwrap();
                        (fx, model)
                    })
                },
                |(fx, model)| {
                    let rows: Vec<(String, Vec<f32>)> = (0..chunk_rows::EMBEDDINGS)
                        .map(|i| {
                            let t = i as f32 / 500.0;
                            (
                                format!("c{i:07}"),
                                (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                            )
                        })
                        .collect();
                    rt.block_on(fx.db.upsert_embeddings(&model, rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        },
    );

    group.finish();
}

/// Chunk cost as a function of chunk size, for the re-derivation of §5.1.5.
///
/// §5.1.5 justifies `CHUNK_ROWS` at 500–1,000 with two quantitative claims and
/// measures neither: that per-transaction overhead "amortizes to noise" at that
/// size, and that a chunk "commits in 2–3 ms even where trigger amplification
/// applies". The second is false by a factor of twelve ([D-056](../docs/architecture/s13-decision-register.md)).
/// This group exists to test the first, and to supply the coefficient the rule
/// actually needs.
///
/// The model is `T(n) = f + c·n` — a fixed cost per transaction (BEGIN, COMMIT,
/// one `prepare`, the fsync under `synchronous = NORMAL`) plus a per-row cost.
/// Neither term can be inferred from the single 500-row measurement the previous
/// entries took: 37 ms at 500 rows is consistent with a 35 ms fixed cost and with
/// a 4 ms one, and the two imply opposite chunk sizes. Sweeping `n` separates
/// them, and the sweep also checks the linearity the model assumes rather than
/// asserting it — a superlinear term would mean chunk size trades against itself
/// and the rule needs a different shape entirely.
///
/// All four paths, because [D-057](../docs/architecture/s13-decision-register.md)
/// measured their per-row costs spanning 31× and a single row count cannot bound
/// four different durations. Every size here is ≤ `CHUNK_ROWS`, so each
/// measurement is exactly one chunk and one transaction.
fn chunk_scaling(c: &mut Criterion) {
    let rt = runtime();
    const SIZES: [usize; 6] = [1, 10, 50, 100, 500, 1_000];

    let mut group = controlled_group(c, "chunk_scaling");
    group.sample_size(10);

    for n in SIZES {
        group.bench_with_input(BenchmarkId::new("edges", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, n + 1).await;
                        fx
                    })
                },
                |fx| {
                    let edges: Vec<EdgeAssertion> = (0..n)
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

        group.bench_with_input(BenchmarkId::new("concepts", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, n).await;
                        fx
                    })
                },
                |fx| {
                    let rows: Vec<ConceptUpsert> = (0..n)
                        .map(|i| {
                            ConceptUpsert::new(format!("c{i:07}"), format!("Rewritten {i}"))
                                .content(format!("new body for {i}"))
                                .valid_from(TS)
                        })
                        .collect();
                    rt.block_on(fx.db.write_concepts(rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        });

        group.bench_with_input(BenchmarkId::new("annotations", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, n).await;
                        fx
                    })
                },
                |fx| {
                    let rows: Vec<Annotation> = (0..n)
                        .map(|i| Annotation {
                            concept_id: format!("c{i:07}"),
                            label: "community".into(),
                            value: format!("{}", i % 7),
                        })
                        .collect();
                    rt.block_on(fx.db.write_analytics_annotations(rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        });

        group.bench_with_input(BenchmarkId::new("embeddings", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, n).await;
                        let model = ModelName::new("bench_v1").unwrap();
                        fx.db.register_model(&model, 8).await.unwrap();
                        (fx, model)
                    })
                },
                |(fx, model)| {
                    let rows: Vec<(String, Vec<f32>)> = (0..n)
                        .map(|i| {
                            let t = i as f32 / 500.0;
                            (
                                format!("c{i:07}"),
                                (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                            )
                        })
                        .collect();
                    rt.block_on(fx.db.upsert_embeddings(&model, rows)).unwrap()
                },
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

/// The three bulk chunk paths that are **not** the edge chunk (D-056).
///
/// §9 budgets one chunk commit and the edge chunk is the row it names, so these
/// were unmeasured while sharing its defect: `bulk_import` was not the only path
/// calling `execute` once per row inside a transaction, merely the only one
/// anyone had timed. The statement hoist applies to each of them for the same
/// reason, and the point of this group is that the claim is checked rather than
/// asserted by analogy.
///
/// They are not equivalent to each other, and the numbers should differ:
///
/// * **concepts** — an upsert on a table with an FTS5 trigger pair, so a rewrite
///   pays a delete-then-insert into `concepts_fts` on top of the row.
/// * **annotations** — `analytics_annotations` carries no triggers at all
///   (Doctrine VI's third category: derived, outside the ledger). This is the
///   closest thing in the codebase to the cost of a bare upsert, which makes it
///   the useful control for the two above.
/// * **embeddings** — an insert into a `F32_BLOB` table with a DiskANN index,
///   whose maintenance is the dominant term and is not a trigger.
///
/// All three at 500 rows, matching §9's chunk row so the figures are comparable
/// to it, and all three via the public API so what is measured is what callers
/// actually get.
fn bulk_chunks(c: &mut Criterion) {
    let rt = runtime();

    let mut group = controlled_group(c, "bulk_chunks");
    group.sample_size(10);

    group.bench_function("concepts_500 (upsert + FTS triggers)", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    // Seeded first, so the measured chunk is the *rewrite* case —
                    // the one that pays the FTS delete as well as the insert, and
                    // the one a re-import actually performs.
                    seed_concepts(&fx.db, 500).await;
                    fx
                })
            },
            |fx| {
                let rows: Vec<ConceptUpsert> = (0..500)
                    .map(|i| {
                        ConceptUpsert::new(format!("c{i:07}"), format!("Rewritten {i}"))
                            .content(format!("new body for {i}"))
                            .valid_from(TS)
                    })
                    .collect();
                rt.block_on(fx.db.write_concepts(rows)).unwrap()
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("annotations_500 (no triggers — the control)", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    seed_concepts(&fx.db, 500).await;
                    fx
                })
            },
            |fx| {
                let rows: Vec<Annotation> = (0..500)
                    .map(|i| Annotation {
                        concept_id: format!("c{i:07}"),
                        label: "community".into(),
                        value: format!("{}", i % 7),
                    })
                    .collect();
                rt.block_on(fx.db.write_analytics_annotations(rows)).unwrap()
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("embeddings_500 (DiskANN maintenance)", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    seed_concepts(&fx.db, 500).await;
                    let model = ModelName::new("bench_v1").unwrap();
                    fx.db.register_model(&model, 8).await.unwrap();
                    (fx, model)
                })
            },
            |(fx, model)| {
                let rows: Vec<(String, Vec<f32>)> = (0..500)
                    .map(|i| {
                        let t = i as f32 / 500.0;
                        (
                            format!("c{i:07}"),
                            (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                        )
                    })
                    .collect();
                rt.block_on(fx.db.upsert_embeddings(&model, rows)).unwrap()
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

    let mut group = controlled_group(c, "traversal");
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

    let mut group = controlled_group(c, "replay");
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

    let mut group = controlled_group(c, "integrity");
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

    let mut group = controlled_group(c, "search");
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

    let mut group = controlled_group(c, "snapshot");
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

// ---------------------------------------------------------------------------
// Wave 3: the read paths §9 never budgeted and this file never measured
// ---------------------------------------------------------------------------

/// **The measurement D-047's deferral has been waiting on since it was written.**
///
/// D-047 defers the `Subgraph` integer-index rewrite until Louvain and Dijkstra
/// are measured on a budget-sized graph. Until Wave 3 nothing here measured an
/// algorithm, a subgraph load, an archive or a filtered vector search, so the
/// deferral condition could not be met and the rewrite could neither be
/// scheduled nor retired — it simply sat.
///
/// The load is separated from the algorithms on purpose, and that separation is
/// the point rather than a convenience: §8.6 predicted the dominant cost of
/// running analytics is *getting the graph into memory*, not the traversal of
/// it, and an integer-index representation does not touch the former. Reported
/// side by side, the two numbers answer D-047 directly.
fn graph_analytics(c: &mut Criterion) {
    let rt = runtime();
    let edges = 1_000 * scale();
    let fx = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, edges + 1).await;
        seed_edges(&fx.db, edges).await;
        fx
    });

    // Large enough not to refuse the fixture; the budget is not what is being
    // measured here.
    let budget = 64 << 20;
    let graph = rt.block_on(async { fx.db.load_subgraph("c0000000", 3, TS, budget).await.unwrap() });
    eprintln!(
        "graph fixture: {} nodes, {} edges",
        graph.nodes.len(),
        graph.edge_count()
    );

    let mut group = controlled_group(c, "graph_analytics");
    group.sample_size(20);

    // The load, which is where §8.6 expects the time to be.
    group.bench_function("load_subgraph_3hop", |b| {
        b.to_async(&rt)
            .iter(|| async { fx.db.load_subgraph("c0000000", 3, TS, budget).await.unwrap() })
    });

    // The five algorithms, over an already-loaded graph. Synchronous and pure,
    // so no runtime is involved and nothing here touches the database.
    group.bench_function("dijkstra", |b| b.iter(|| dijkstra(&graph, "c0000000")));
    group.bench_function("astar", |b| {
        // Zero heuristic: A* with an uninformed heuristic is Dijkstra with a
        // goal test, which is the honest comparison against the row above —
        // any other heuristic would measure the heuristic.
        b.iter(|| astar(&graph, "c0000000", "c0000001", |_, _| 0.0))
    });
    group.bench_function("scc", |b| b.iter(|| scc(&graph)));
    group.bench_function("k_core", |b| b.iter(|| k_core(&graph, 2)));
    group.bench_function("louvain", |b| b.iter(|| louvain(&graph)));

    group.finish();
}

/// **AE: the batched hydrate, against the figure the review recorded for the
/// one-query-per-node version.**
///
/// The pre-Wave-1 measurement is on the record — 400 nodes, 400 round trips,
/// 13.2 ms — so this is a comparison rather than a fresh baseline. It is a
/// weaker comparison than a criterion baseline would be, because it is against a
/// number taken by a throwaway probe on the same machine rather than against a
/// saved run; the honest way to read it is as an order of magnitude, not a
/// ratio to two figures.
///
/// Parameterised by node count because the property that mattered was *linear
/// in node count*, and a single point cannot show that it no longer is.
fn hydrate_scaling(c: &mut Criterion) {
    let rt = runtime();
    let fx = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, 1_000).await;
        fx
    });

    let mut group = controlled_group(c, "hydrate_scaling");
    group.sample_size(30);

    for n in [100usize, 400, 1_000] {
        let ids: Vec<String> = (0..n).map(|i| format!("c{i:07}")).collect();
        group.bench_with_input(BenchmarkId::from_parameter(n), &ids, |b, ids| {
            b.to_async(&rt).iter(|| async {
                hydrate_attributes(fx.db.read_conn(), ids, TS, AttributeMode::Current)
                    .await
                    .unwrap()
            })
        });
    }

    group.finish();
}

/// **D-060: what the overlap guard costs on the interactive write path.**
///
/// One indexed probe per assertion, added in Wave 2 to a path `CHUNK_BUDGET`'s
/// 3 ms exists to protect. It rides `idx_lc_open_interval`, shipped in the same
/// wave, so the expectation is that it is cheap — and an expectation is what
/// this wave exists to replace.
///
/// Measured against a **high-degree source**, because that is where the guard
/// could plausibly be expensive: the probe binds all three equality columns, so
/// out-degree should not matter, and if it does the index is not being used the
/// way `the_single_open_probe_seeks_rather_than_scans` says it is.
fn overlap_guard(c: &mut Criterion) {
    let rt = runtime();
    let hub = 2_000 * scale();

    let mut group = controlled_group(c, "overlap_guard");
    group.sample_size(20);

    for degree in [0usize, hub] {
        group.bench_with_input(
            BenchmarkId::from_parameter(degree),
            &degree,
            |b, &degree| {
                // Synchronous `iter_batched` with `block_on` on both halves,
                // matching `chunk_budget` above: `to_async` drives the setup
                // closure on the runtime's own thread, so a `block_on` inside it
                // panics with "cannot start a runtime from within a runtime".
                b.iter_batched(
                    || {
                        rt.block_on(async {
                            let fx = fixture().await;
                            seed_concepts(&fx.db, degree + 2).await;
                            if degree > 0 {
                                seed_edges(&fx.db, degree).await;
                            }
                            fx
                        })
                    },
                    |fx| {
                        // A closed interval on a fresh edge type: the guard
                        // runs, finds nothing, and the insert proceeds. That is
                        // the common case and the one on the latency path.
                        rt.block_on(fx.db.assert_edge(
                            EdgeAssertion::new("c0000000", "c0000001", "PROBED")
                                .valid_from(TS)
                                .valid_to("2027-01-01T00:00:00.000000Z"),
                        ))
                        .unwrap()
                    },
                    BatchSize::PerIteration,
                )
            },
        );
    }

    group.finish();
}

/// `archive()` — one `BEGIN IMMEDIATE` holding the write lock for its whole
/// duration, including a full `rebuild_within` (§8.6).
///
/// The reason this is worth a number: it is one of the three paths that sit
/// *outside* `CHUNK_BUDGET`, and the exemption has only ever been justified by
/// argument. D-012 says the archive must be atomic, which is why it cannot be
/// chunked; it does not say how long it takes.
fn archive_cost(c: &mut Criterion) {
    let rt = runtime();
    let edges = 2_000 * scale();

    let mut group = controlled_group(c, "archive");
    group.sample_size(10);

    group.bench_function("archive_superseded", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let fx = fixture().await;
                    seed_concepts(&fx.db, edges + 1).await;
                    // Closed intervals whose valid_to precedes the cutoff, so
                    // LINKS_ARCHIVABLE's second branch matches and the archive
                    // has real work rather than measuring an empty transaction.
                    let mut batch = Vec::with_capacity(edges);
                    for i in 1..=edges {
                        batch.push(
                            EdgeAssertion::new("c0000000", format!("c{i:07}"), "LINKS")
                                .valid_from(TS)
                                .valid_to("2026-06-01T00:00:00.000000Z"),
                        );
                    }
                    for chunk in batch.chunks(2_000) {
                        fx.db.bulk_import(chunk.to_vec()).await.unwrap();
                    }
                    fx
                })
            },
            |fx| {
                let report = rt
                    .block_on(fx.db.archive("2099-01-01T00:00:00.000000Z"))
                    .unwrap();
                assert!(report.links_archived > 0, "the fixture archived nothing");
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

/// **AF: `corpus_size` runs `COUNT(*)` over the whole model table per query.**
///
/// D-007's argument is that strategy choice should be arithmetic rather than a
/// rule of thumb. The arithmetic is currently O(corpus) per query and the thing
/// it selects is not, so the planner's input can cost more than the plan. This
/// measures the search with the planner in the loop; the fix and its effect are
/// Wave 3.2.
fn filtered_vector(c: &mut Criterion) {
    let rt = runtime();
    let n = 2_000 * scale();
    let (fx, model) = rt.block_on(async {
        let fx = fixture().await;
        seed_concepts(&fx.db, n).await;
        seed_edges(&fx.db, n.min(1_000)).await;
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

    let mut group = controlled_group(c, "filtered_vector");
    group.sample_size(30);

    // AF's claim, isolated: "the planner's input costs more than the plan".
    // The two reads `execute` makes before it can price anything, measured
    // against the whole search they inform.
    group.bench_function("planner_input/corpus_size", |b| {
        let sql = format!("SELECT COUNT(*) FROM {}", model.table());
        b.to_async(&rt).iter(|| async {
            let n: i64 = fx
                .db
                .read_conn()
                .query(&sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();
            n
        })
    });

    group.bench_function("planner_input/declared_dimension", |b| {
        b.to_async(&rt)
            .iter(|| async { declared_dimension(fx.db.read_conn(), &model).await.unwrap() })
    });

    group.bench_function("filtered_top10", |b| {
        b.to_async(&rt).iter(|| async {
            FilteredVectorSearch::new(
                model.clone(),
                query.clone(),
                TraversalBuilder::new("c0000000").max_depth(3),
            )
            .top_k(10)
            .execute(fx.db.read_conn(), TS)
            .await
            .unwrap()
        })
    });

    group.finish();
}

/// **3.1c: what D-059's index costs the write path, and what it buys.**
///
/// D-058's four constants were measured before `idx_lc_open_interval` existed,
/// and an index is paid for on every insert. This measures the edge chunk with
/// the index present and absent, into an empty table and into a populated hub —
/// the two-variable separation D-059 established as necessary, since measuring
/// chunk size into a fresh database confounds chunk size with table size.
///
/// The hub arm is the case the index was added for; the empty arm is the case it
/// can only cost. Both are needed, because a constant chosen from one alone is
/// what produced the confound D-059 had to correct.
fn chunk_index_cost(c: &mut Criterion) {
    let rt = runtime();

    let mut group = controlled_group(c, "chunk_index_cost");
    group.sample_size(20);

    for (label, hub, with_index) in [
        ("empty/with_index", 0usize, true),
        ("empty/no_index", 0, false),
        ("hub2000/with_index", 2_000, true),
        ("hub2000/no_index", 2_000, false),
    ] {
        group.bench_function(label, |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let fx = fixture().await;
                        seed_concepts(&fx.db, hub + chunk_rows::EDGES + 1).await;
                        if hub > 0 {
                            // A real hub: every edge out of one source, which is
                            // what makes the single-open probe's out-degree scan
                            // expensive when it happens.
                            let batch: Vec<EdgeAssertion> = (1..=hub)
                                .map(|i| {
                                    EdgeAssertion::new(
                                        "c0000000",
                                        format!("c{i:07}"),
                                        "LINKS",
                                    )
                                    .valid_from(TS)
                                    .valid_to(OPEN)
                                })
                                .collect();
                            for chunk in batch.chunks(2_000) {
                                fx.db.bulk_import(chunk.to_vec()).await.unwrap();
                            }
                        }
                        if !with_index {
                            // Dropping it leaves the schema stamped v6 without
                            // the object the rung created — legitimate only
                            // inside a benchmark, and the reason this arm exists
                            // is that it is the only way to attribute the cost.
                            fx.db
                                .raw()
                                .connect()
                                .unwrap()
                                .execute("DROP INDEX IF EXISTS idx_lc_open_interval", ())
                                .await
                                .unwrap();
                        }
                        fx
                    })
                },
                |fx| {
                    let base = hub + 1;
                    let edges: Vec<EdgeAssertion> = (0..chunk_rows::EDGES)
                        .map(|k| {
                            EdgeAssertion::new(
                                "c0000000",
                                format!("c{:07}", base + k),
                                "CHUNK",
                            )
                            .valid_from(TS)
                            .valid_to(OPEN)
                        })
                        .collect();
                    rt.block_on(fx.db.write_bulk_atomic(edges)).unwrap()
                },
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// T4.1: the same measurement on all four shapes
// ---------------------------------------------------------------------------

/// `load_subgraph` across the fixture matrix, at fixed coverage.
///
/// Every other group in this file runs on `star_of_stars`. This one runs the
/// load on all four shapes so the figure above it can be read as what it is —
/// a measurement of one shape — rather than as a property of the loader.
///
/// **Coverage, not depth, is what is held fixed**, and that is the correction
/// the matrix forced. At a fixed depth of 3 these shapes reach 600, 25, 6 and
/// 300 nodes; a table indexed by depth would be comparing a 600-node problem
/// against a 6-node one and reporting the difference as a property of the
/// shape. Each shape here runs at the depth it needs to cover 90% of itself,
/// so the rows are comparable and the *depth* is what varies — which is
/// `chain`'s cost, stated rather than hidden.
fn fixture_matrix(c: &mut Criterion) {
    use fixtures::{depth_to_cover, seed, ALL_SHAPES};

    let rt = runtime();
    let nodes = 600 * scale();
    let budget = 512 << 20;

    let mut group = controlled_group(c, "fixture_matrix");
    group.sample_size(10);

    for &shape in ALL_SHAPES {
        let depth = depth_to_cover(shape, nodes, 0.9, nodes) as u32;
        let start = shape.start_node(nodes);
        let fx = rt.block_on(async {
            let fx = fixture().await;
            seed(&fx.db, shape, nodes).await;
            fx
        });

        group.bench_with_input(
            BenchmarkId::new("load_subgraph_90pct", shape.name()),
            &depth,
            |b, &depth| {
                b.to_async(&rt).iter(|| async {
                    fx.db
                        .load_subgraph(&start, depth, TS, budget)
                        .await
                        .unwrap()
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    budgets,
    write_path,
    bulk_chunks,
    chunk_budget,
    chunk_scaling,
    traversal,
    replay,
    integrity,
    search,
    snapshot,
    // Wave 3.
    graph_analytics,
    hydrate_scaling,
    overlap_guard,
    archive_cost,
    filtered_vector,
    chunk_index_cost,
    // T4.1.
    fixture_matrix
);
criterion_main!(budgets);
