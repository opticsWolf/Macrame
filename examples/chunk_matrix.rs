//! Re-derivation of the four `chunk_rows` constants against the D-088 fixture
//! matrix (0.11.0, [Appendix C](../docs/architecture/appendices.md) item 2).
//!
//! [D-059](../docs/architecture/s13-decision-register.md) left this open in
//! these words: *"the chunk constants are empty-database figures and need a
//! realistic fixture, which requires deciding what 'realistic' means"*. The
//! matrix is that decision, made in 0.6.0, and it has never been applied here.
//!
//! Not a benchmark — a set of controlled comparisons printed as a table, on the
//! `chunk_diag` pattern and for the same reason: what is wanted is one number
//! per configuration with the configuration named beside it, not a throughput
//! distribution for one arm.
//!
//! ```text
//! cargo run --release --example chunk_matrix -- edges <star|clustered|chain|dense>
//! cargo run --release --example chunk_matrix -- rest  <star|clustered|chain|dense>
//! ```
//!
//! One shape per invocation, because a populated fixture per sweep point means
//! six local database opens and R15 (`STATUS_ACCESS_VIOLATION`) scales with
//! that count.
//!
//! # What is held constant, and what is not
//!
//! Every shape is populated to the **same 8,000 edges** — the population
//! `chunk_budget`'s seeded arm and [D-142](../docs/architecture/s13-decision-register.md)
//! both use — so `links_current` and `transaction_log` hold the same row counts
//! across shapes and the only difference is how the keys are distributed. Node
//! count is *not* held constant and cannot be: `dense_small` reaches 8,000 edges
//! on a few hundred nodes and `chain` needs 8,001. That difference is a property
//! of the shapes rather than a flaw in the comparison, and it is printed.
//!
//! The measured chunk is **identical on every shape** — the same hub source, the
//! same fresh targets, the same edge type. Varying both the table and the chunk
//! would measure two things at once. D-142 found the residual to be
//! shape-independent, so what this asks is the narrower question the constants
//! actually depend on: does the *table's* shape change what a chunk costs.
//!
//! # Why the three non-edge paths sweep differently
//!
//! `write_bulk_atomic` writes any number of edges as exactly one transaction, so
//! the edge sweep can run past the constant and find where 3 ms is crossed.
//! There is no atomic variant of the other three: `write_concepts`,
//! `write_analytics_annotations` and `upsert_embeddings` chunk internally at
//! their own constant, so above it a measurement is several transactions and not
//! one chunk. Their sweeps therefore stop at the constant and report headroom
//! rather than a crossing point.
//!
//! That is a real limit on what can be re-derived from outside, and it is worth
//! stating rather than working around: a raw-connection restatement would reach
//! higher sizes, but [D-057](../docs/architecture/s13-decision-register.md)
//! rejected exactly that for these three paths — they go through the public API,
//! so the number a caller gets is the number worth having.

use std::time::{Duration, Instant};

use macrame::prelude::*;

#[path = "../tests/common/fixtures.rs"]
mod fixtures;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The population every shape is brought to. Matches `chunk_budget`'s seeded
/// arm and D-142, so these figures sit beside those rather than beneath them.
const POPULATION: usize = 8_000;

/// §5.1.5's golden rule, as a duration. The whole point of the re-derivation.
const BUDGET_MS: f64 = 3.0;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn shape_of(name: &str) -> fixtures::Shape {
    match name {
        "star" => fixtures::Shape::StarOfStars,
        "clustered" => fixtures::Shape::Clustered,
        "chain" => fixtures::Shape::Chain,
        "dense" => fixtures::Shape::DenseSmall,
        other => panic!("unknown shape {other:?}; expected star|clustered|chain|dense"),
    }
}

/// The smallest node count at which this shape emits at least `edges` edges.
///
/// Searched rather than derived, because each shape's edge count is its own
/// function of node count — linear for `chain`, quadratic for `dense_small` —
/// and a closed form per shape would be a second copy of the generator, kept in
/// step by hand.
fn nodes_for(shape: fixtures::Shape, edges: usize) -> usize {
    let mut lo = 2usize;
    let mut hi = edges + 2;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if shape.edges(mid).len() >= edges {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// A database holding exactly `POPULATION` edges of this shape, plus `spare`
/// unlinked concepts for a measured chunk to point at.
async fn populated(
    dir: &tempfile::TempDir,
    name: &str,
    shape: fixtures::Shape,
    spare: usize,
) -> (Database, usize) {
    let nodes = nodes_for(shape, POPULATION);
    let db = Database::open_with_cadence(dir.path().join(name), None)
        .await
        .unwrap();

    // Concepts first: every edge has two foreign keys.
    let mut concepts = shape.concepts(nodes);
    concepts.extend((nodes..nodes + spare).map(fixtures::concept));
    for c in concepts.chunks(600) {
        db.write_concepts(c.to_vec()).await.unwrap();
    }

    // Truncated to the target so the population is identical across shapes.
    // Truncation keeps each shape's character — a prefix of `dense_small` is
    // still dense, a prefix of `chain` is still a chain — while removing the
    // row-count difference that would otherwise dominate the comparison.
    let mut edges = shape.edges(nodes);
    edges.truncate(POPULATION);
    for chunk in edges.chunks(2_000) {
        db.bulk_import(chunk.to_vec()).await.unwrap();
    }
    (db, nodes)
}

/// The chunk under test: `n` fresh edges out of the hub, on their own edge type.
fn measured_chunk(n: usize, first_target: usize) -> Vec<EdgeAssertion> {
    (0..n)
        .map(|k| {
            EdgeAssertion::new(
                fixtures::node_id(0),
                fixtures::node_id(first_target + k),
                "MEASURED",
            )
            .valid_from(TS)
            .valid_to(OPEN)
        })
        .collect()
}

fn verdict(rows: &[(usize, f64)]) -> String {
    let largest = rows
        .iter()
        .filter(|(_, t)| *t <= BUDGET_MS)
        .map(|(n, _)| *n)
        .max();
    match largest {
        Some(n) => format!("largest size within {BUDGET_MS} ms: {n}"),
        None => format!(
            "no swept size meets {BUDGET_MS} ms — smallest measured is {} rows at {:.2} ms",
            rows[0].0, rows[0].1
        ),
    }
}

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "edges".into());
    let shape_name = std::env::args().nth(2).unwrap_or_else(|| "star".into());
    let shape = shape_of(&shape_name);
    let dir = tempfile::TempDir::new().unwrap();

    println!(
        "== {mode}: chunk cost into a {POPULATION}-edge {} table ==",
        shape.name()
    );
    println!("   worst case for: {}", shape.worst_case_for());

    match mode.as_str() {
        // Sweeps past the constant, because `write_bulk_atomic` is one
        // transaction at any size.
        "edges" => {
            // Centred below the current constant rather than above it. The
            // first sweep ran 30..500 and every point was over budget — 30 rows
            // already cost 3.11 ms — so the question is not where the cost
            // crosses 3 ms going up, but whether it crosses at all going down.
            const SIZES: [usize; 5] = [5, 10, 20, 45, 90];
            let mut rows = Vec::new();
            for n in SIZES {
                // A fresh database per point. Measuring the sweep into one
                // growing database would make every later point land in a
                // bigger table than the one before it, and D-142 established
                // that table size is exactly what this path is sensitive to —
                // so the sweep would confound chunk size with population.
                let (db, nodes) = populated(&dir, &format!("e{n}.db"), shape, SIZES.len() + n).await;
                let batch = measured_chunk(n, nodes);
                let t = Instant::now();
                db.write_bulk_atomic(batch).await.unwrap();
                let e = ms(t.elapsed());
                println!(
                    "  {n:>5} edges : {e:>8.2} ms  ({:>6.1} µs/row){}",
                    e * 1e3 / n as f64,
                    if e <= BUDGET_MS { "" } else { "   over budget" }
                );
                rows.push((n, e));
                db.close().await.unwrap();
            }
            println!("\n  nodes in fixture: {}", nodes_for(shape, POPULATION));
            println!("  current constant: {}", chunk_rows::EDGES);
            println!("  {}", verdict(&rows));
        }

        // Stops at each constant: above it the public API chunks, and the
        // measurement stops being one transaction.
        "rest" => {
            let (db, nodes) = populated(&dir, "r.db", shape, 4_000).await;
            println!("  nodes in fixture: {nodes}\n");

            let mut rows = Vec::new();
            for n in [10usize, 30, 50, 70] {
                let batch: Vec<ConceptUpsert> = (0..n)
                    .map(|i| {
                        ConceptUpsert::new(fixtures::node_id(i), format!("Rewritten {i}"))
                            .content(format!("new body for {i}"))
                            .valid_from(TS)
                    })
                    .collect();
                let t = Instant::now();
                db.write_concepts(batch).await.unwrap();
                rows.push((n, ms(t.elapsed())));
                println!("  concepts   {n:>5} : {:>8.2} ms", rows.last().unwrap().1);
            }
            println!(
                "  -> constant {}, {}\n",
                chunk_rows::CONCEPTS,
                verdict(&rows)
            );

            let mut rows = Vec::new();
            for n in [100usize, 300, 600] {
                let batch: Vec<Annotation> = (0..n)
                    .map(|i| Annotation {
                        concept_id: fixtures::node_id(i),
                        label: "community".into(),
                        value: format!("{}", i % 7),
                    })
                    .collect();
                let t = Instant::now();
                db.write_analytics_annotations(batch).await.unwrap();
                rows.push((n, ms(t.elapsed())));
                println!("  annotations{n:>5} : {:>8.2} ms", rows.last().unwrap().1);
            }
            println!(
                "  -> constant {}, {}\n",
                chunk_rows::ANNOTATIONS,
                verdict(&rows)
            );

            let model = ModelName::new("matrix_v1").unwrap();
            db.register_model(&model, 8).await.unwrap();
            let mut rows = Vec::new();
            for n in [10usize, 20, 30] {
                let batch: Vec<(String, Vec<f32>)> = (0..n)
                    .map(|i| {
                        let t = i as f32 / 500.0;
                        (
                            fixtures::node_id(i),
                            (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                        )
                    })
                    .collect();
                let t = Instant::now();
                db.upsert_embeddings(&model, batch).await.unwrap();
                rows.push((n, ms(t.elapsed())));
                println!("  embeddings {n:>5} : {:>8.2} ms", rows.last().unwrap().1);
            }
            println!(
                "  -> constant {}, {}",
                chunk_rows::EMBEDDINGS,
                verdict(&rows)
            );

            db.close().await.unwrap();
        }

        other => println!("unknown mode: {other}"),
    }
}
