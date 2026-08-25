//! W10.2 reconnaissance: is a `PRAGMA optimize` after a bulk load worth its
//! cost, and above what run size?
//!
//! W2.2 scheduled two call sites and shipped one. `close()` runs `optimize()`
//! ([D-149]); the bulk-load one was left for "a threshold worth picking by
//! measurement rather than by taste", and [D-197] then measured something that
//! constrains the answer: `PRAGMA optimize`'s staleness test is **SQLite's own
//! ratio**, not a row count, and it is large — 2× and 5× growth left
//! `sqlite_stat1` untouched, 25× rewrote it. So a threshold on the *run* cannot
//! make the pragma do anything it was not already going to do.
//!
//! Which leaves the question that actually decides the call site: **what does
//! the caller get for it, in the run where it fires?**
//!
//! **Measured answer: nothing, and W10.2 closed as "no call site"
//! ([D-198]).** On a fresh database the `optimize()` after the load always does
//! fire — the first call is a full analysis, so the ratio never gets in the way
//! — and writes seven rows of `sqlite_stat1`. It moves **no plan and no opcode
//! count**, at 90, 500, 5,000 or 40,000 edges, across the six queries the
//! registry justifies an index with plus a join whose order the planner is free
//! to choose. `tests/statistics_effect_tests.rs` is that result as a gate.
//!
//! ```text
//! cargo run --release --example bulk_optimize
//! ```
//!
//! Three things are measured per run size, on a *fresh* database — the state
//! [D-150] insists is real, and the one a bulk load starts from most often:
//!
//! 1. Whether the statistics get written at all.
//! 2. What the `optimize()` costs, against what the import cost.
//! 3. Whether a plan the crate pins actually changes as a result.
//!
//! The third is the one that decides it. A call site that keeps `sqlite_stat1`
//! fresh and never changes a plan is an index with no reader ([D-089]).
//!
//! [D-089]: ../docs/architecture/s13-decision-register.md#d-089
//! [D-149]: ../docs/architecture/s13-decision-register.md#d-149
//! [D-150]: ../docs/architecture/s13-decision-register.md#d-150
//! [D-197]: ../docs/architecture/s13-decision-register.md#d-197
//! [D-198]: ../docs/architecture/s13-decision-register.md#d-198

use macrame::metrics::CommandKind;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Skewed out-degree, for the reason `analyze_hold` and `plan_fixture` both
/// give: uniform data is exactly where measured statistics and SQLite's
/// defaults agree, so a uniform fixture cannot show the difference (D-088).
fn edges(n: usize) -> Vec<EdgeAssertion> {
    let hub = n / 4;
    let mut out = Vec::with_capacity(n);
    for i in 1..=hub {
        out.push(
            EdgeAssertion::new("c000000", format!("c{i:06}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for i in hub + 1..n {
        out.push(
            EdgeAssertion::new(format!("c{i:06}"), format!("c{:06}", i + 1), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    out
}

async fn stat1_rows(path: &std::path::Path) -> usize {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let Ok(mut rows) = conn.query("SELECT count(*) FROM sqlite_stat1", ()).await else {
        return 0;
    };
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<i64>(0).unwrap_or(0) as usize)
        .unwrap_or(0)
}

/// The queries `tests/operation_count_tests.rs` pins, plus the traversal the
/// builder actually emits.
///
/// Asking only about the traversal would be asking the one question a covering
/// index answers the same way regardless of statistics. These are the six the
/// registry justifies an index with, which is the set a change in `sqlite_stat1`
/// has any business moving.
const QUERIES: &[(&str, &str)] = &[
    (
        "traversal recursive step",
        "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1          AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
    ),
    (
        "overlap guard",
        "SELECT valid_from, valid_to FROM links_current          WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3            AND valid_from <> ?4",
    ),
    (
        "fold window",
        "SELECT seq_id, table_name, entity_id, operation, payload          FROM transaction_log WHERE recorded_at <= ?1",
    ),
    (
        "archive supersession",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS (            SELECT 1 FROM transaction_log newer            WHERE newer.entity_id = transaction_log.entity_id              AND newer.seq_id > transaction_log.seq_id)",
    ),
    (
        "links archive cutoff",
        "SELECT source_id, target_id FROM links WHERE recorded_at < ?1 AND (            EXISTS (              SELECT 1 FROM links newer              WHERE newer.source_id = links.source_id                AND newer.target_id = links.target_id                AND newer.edge_type = links.edge_type                AND newer.valid_from = links.valid_from                AND newer.recorded_at > links.recorded_at)            OR (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= ?1))",
    ),
    (
        "join with a free order",
        "SELECT c.id, l.target_id FROM concepts c JOIN links_current l          ON l.source_id = c.id WHERE c.retired = 0 AND l.weight >= ?1",
    ),
    (
        "concept reverse-reachability",
        "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1          AND valid_to < ?1 AND NOT EXISTS (            SELECT 1 FROM links WHERE links.source_id = concepts.id               OR links.target_id = concepts.id)",
    ),
];

/// Every pinned plan plus the emitted traversal, read off a plain connection.
async fn plans(path: &std::path::Path) -> Vec<(String, String)> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let traversal = TraversalBuilder::new("c000000").max_depth(2).build_sql();

    let mut out = Vec::new();
    for (label, sql) in QUERIES
        .iter()
        .map(|(l, s)| ((*l).to_string(), (*s).to_string()))
        .chain(std::iter::once((
            "emitted traversal".to_string(),
            traversal,
        )))
    {
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut lines = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            lines.push(r.get::<String>(3).unwrap());
        }

        // W10.1's instrument, because a plan can read identically while the
        // program does not: `EXPLAIN QUERY PLAN` prints a summary and `EXPLAIN`
        // prints what will run (D-195).
        let mut prog = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
        let (mut opens, mut seeks, mut rewinds, mut sorts) = (0, 0, 0, 0);
        while let Some(r) = prog.next().await.unwrap() {
            let op: String = r.get(1).unwrap();
            if op == "OpenRead" || op == "OpenEphemeral" || op == "OpenAutoindex" {
                opens += 1;
            } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
                seeks += 1;
            } else if op == "Rewind" || op == "Last" {
                rewinds += 1;
            } else if op == "SorterOpen" || op == "SorterSort" {
                sorts += 1;
            }
        }
        out.push((
            label,
            format!("({opens},{seeks},{rewinds},{sorts}) {}", lines.join(" | ")),
        ));
    }
    out
}

fn optimize_hold(db: &Database) -> std::time::Duration {
    db.metrics()
        .kinds
        .iter()
        .find(|k| k.kind == CommandKind::Optimize)
        .map(|k| k.longest)
        .unwrap_or_default()
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_bulk_optimize_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    println!(
        "budget = {:?}, chunk ceiling = {} edges",
        macrame::CHUNK_BUDGET,
        chunk_rows::EDGES
    );

    for n in [90usize, 500, 5_000, 40_000] {
        let path = dir.join(format!("b{n}.db"));
        let db = Database::open_with_cadence(&path, None).await.unwrap();

        let concepts: Vec<_> = (0..=n)
            .map(|i| ConceptUpsert::new(format!("c{i:06}"), format!("C{i}")).valid_from(TS))
            .collect();
        db.write_concepts(concepts).await.unwrap();

        let t = std::time::Instant::now();
        db.bulk_import(edges(n)).await.unwrap();
        let import = t.elapsed();

        let before_rows = stat1_rows(&path).await;
        let before_plans = plans(&path).await;

        let t = std::time::Instant::now();
        db.optimize().await.unwrap();
        let wall = t.elapsed();
        let hold = optimize_hold(&db);

        let after_rows = stat1_rows(&path).await;
        let after_plans = plans(&path).await;

        println!(
            "\n{n:>6} edges: import {import:?}, optimize wall {wall:?} (hold {hold:?}, \
             {:.2}% of the import)",
            100.0 * wall.as_secs_f64() / import.as_secs_f64()
        );
        println!("        sqlite_stat1 rows {before_rows} -> {after_rows}");
        let moved: Vec<_> = before_plans
            .iter()
            .zip(&after_plans)
            .filter(|((_, b), (_, a))| b != a)
            .collect();
        if moved.is_empty() {
            println!("        plans: all {} unchanged", before_plans.len());
        } else {
            println!(
                "        plans: {} of {} CHANGED",
                moved.len(),
                before_plans.len()
            );
            for ((label, b), (_, a)) in moved {
                println!("          {label}");
                println!("            before: {b}");
                println!("            after:  {a}");
            }
        }

        db.close().await.unwrap();
    }

    // And the case the threshold is meant to keep the probe *off*: a run so
    // small that nothing about it is a bulk load.
    println!("\n===== the small-run case the threshold exists for =====");
    let path = dir.join("small.db");
    let db = Database::open_with_cadence(&path, None).await.unwrap();
    db.write_concepts(
        (0..=40)
            .map(|i| ConceptUpsert::new(format!("c{i:06}"), format!("C{i}")).valid_from(TS))
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    let t = std::time::Instant::now();
    db.bulk_import(edges(40)).await.unwrap();
    let import = t.elapsed();
    let t = std::time::Instant::now();
    db.optimize().await.unwrap();
    let wall = t.elapsed();
    println!(
        "    40 edges (under one chunk): import {import:?}, optimize {wall:?} \
         ({:.0}% of the import)",
        100.0 * wall.as_secs_f64() / import.as_secs_f64()
    );
    db.close().await.unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}
