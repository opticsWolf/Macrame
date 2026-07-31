//! T0.1: does the traversal CTE enumerate paths rather than nodes, on libSQL?
//!
//! The v0.6.0 hardening plan measured this on SQLite 3.50.4 through `node:sqlite`
//! and recorded a 1,600× gap on a 328-edge graph. This project's standard is that
//! a number is not a number until it is measured on its own harness, so this
//! example reproduces the table on libSQL 0.9.30 through the crate's own schema
//! before anything is rewritten.
//!
//! **The fixture is the whole point.** `benches/budgets.rs` seeds a chain of
//! stars, which is a *tree*: exactly one path to each node, so path count equals
//! node count and the pathological term is identically 1. D-070 investigated this
//! query's superlinearity on that fixture and concluded the growth was inherent
//! to producing a deduplicated result. That conclusion is true of trees. The
//! fixture chose the answer.
//!
//! Here the fixture is a layered graph: a root, then `layers` layers of `width`
//! nodes, every node joined to every node of the next layer. Edges are
//! `width + width² × (layers − 1)`; paths to a node in layer `i` are `width^(i−1)`.
//! Small graph, multiplicative path count — which is what a real knowledge graph
//! looks like and a tree does not.
//!
//! Run with:  cargo run --release --example traversal_diag

use std::time::Instant;

use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

/// The shipped form: `UNION ALL`, a `path` column, an `INSTR` cycle check, and a
/// trailing `SELECT DISTINCT` that collapses the duplication after the work.
const CURRENT_WALK: &str = r#"
WITH RECURSIVE walk(node_id, depth, path) AS (
    SELECT ?1, 0, '/' || CAST(?1 AS BLOB) || '/'
    UNION ALL
    SELECT l.target_id, w.depth + 1, w.path || CAST(l.target_id AS BLOB) || '/'
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
      AND INSTR(w.path, '/' || CAST(l.target_id AS BLOB) || '/') = 0
)
"#;

/// The rewrite: `UNION` dedupes on `(node_id, depth)` as rows enter the queue,
/// so `walk` is bounded by `V × (depth+1)`. No path column, no cycle check —
/// termination comes from the depth bound instead.
const REWRITE_WALK: &str = r#"
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
)
"#;

const PROJECT_IDS: &str = r#"
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
"#;

fn node(layer: usize, i: usize) -> String {
    if layer == 0 {
        "n00_0000".to_string()
    } else {
        format!("n{layer:02}_{i:04}")
    }
}

/// Root + `layers` layers of `width`, each layer fully joined to the next.
async fn seed_layered(db: &Database, layers: usize, width: usize) -> usize {
    let mut ids = vec![node(0, 0)];
    for layer in 1..=layers {
        for i in 0..width {
            ids.push(node(layer, i));
        }
    }
    for chunk in ids.chunks(500) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id.clone(), id.clone()).valid_from(TS))
                .collect(),
        )
        .await
        .unwrap();
    }

    let mut edges = Vec::new();
    for i in 0..width {
        edges.push(
            EdgeAssertion::new(node(0, 0), node(1, i), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for layer in 1..layers {
        for a in 0..width {
            for b in 0..width {
                edges.push(
                    EdgeAssertion::new(node(layer, a), node(layer + 1, b), "LINKS")
                        .valid_from(TS)
                        .valid_to(OPEN),
                );
            }
        }
    }
    let total = edges.len();
    for chunk in edges.chunks(500) {
        db.bulk_import(chunk.to_vec()).await.unwrap();
    }
    total
}

async fn count_walk_rows(conn: &libsql::Connection, walk: &str, depth: usize) -> i64 {
    let sql = format!("{walk} SELECT COUNT(*) FROM walk;");
    let mut rows = conn
        .query(
            &sql,
            libsql::params![node(0, 0), depth as i64, NOW, f64::NEG_INFINITY],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn ids(conn: &libsql::Connection, walk: &str, depth: usize) -> (Vec<String>, f64) {
    let sql = format!("{walk}{PROJECT_IDS}");
    let start = Instant::now();
    let mut rows = conn
        .query(
            &sql,
            libsql::params![node(0, 0), depth as i64, NOW, f64::NEG_INFINITY],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get(0).unwrap());
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    (out, ms)
}

/// Best of `n`, because one timing on a warm cache is a sample and not a figure.
async fn best_of(
    conn: &libsql::Connection,
    walk: &str,
    depth: usize,
    n: usize,
) -> (Vec<String>, f64) {
    let mut best = f64::MAX;
    let mut set = Vec::new();
    for _ in 0..n {
        let (v, ms) = ids(conn, walk, depth).await;
        if ms < best {
            best = ms;
        }
        set = v;
    }
    (set, best)
}

/// The shipped read path, not a literal: `TraversalBuilder::execute_ids`.
async fn shipped(conn: &libsql::Connection, depth: usize, n: usize) -> (Vec<String>, f64) {
    let t = macrame::graph::TraversalBuilder::new(node(0, 0))
        .max_depth(depth)
        .min_weight(f64::NEG_INFINITY);
    let mut best = f64::MAX;
    let mut set = Vec::new();
    for _ in 0..n {
        let start = Instant::now();
        set = t.execute_ids(conn, NOW).await.unwrap();
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        if ms < best {
            best = ms;
        }
    }
    (set, best)
}

/// A chain of stars — the shape `benches/budgets.rs` seeds, and a tree.
async fn seed_star(db: &Database, edges: usize) -> usize {
    let ids: Vec<String> = (0..=edges).map(|i| format!("n{i:07}")).collect();
    for chunk in ids.chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id.clone(), id.clone()).valid_from(TS))
                .collect(),
        )
        .await
        .unwrap();
    }
    let mut batch = Vec::with_capacity(edges);
    for i in 1..=edges {
        let src = if i <= edges / 3 {
            0
        } else if i <= 2 * (edges / 3) {
            i - edges / 3
        } else {
            i - 2 * (edges / 3)
        };
        batch.push(
            EdgeAssertion::new(format!("n{src:07}"), format!("n{i:07}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for chunk in batch.chunks(2_000) {
        db.bulk_import(chunk.to_vec()).await.unwrap();
    }
    edges
}

#[tokio::main]
async fn main() {
    let depth: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    println!(
        "libSQL 0.9.30, best of 5. `old` is the simple-path CTE shipped through\n\
         0.5.6, kept here as the baseline; `shipped` is TraversalBuilder::execute_ids.\n"
    );
    println!(
        "== layered fixture, depth = {depth} — paths to a layer-i node grow as width^(i-1) ==\n"
    );
    println!(
        "{:<16} {:>7} {:>14} {:>14} {:>11} {:>11}",
        "fixture", "edges", "walk rows old", "walk rows new", "ms old", "ms shipped"
    );

    for (layers, width) in [(4, 6), (5, 6), (5, 8), (6, 8)] {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.db");
        let db = Database::open_with_cadence(&path, None).await.unwrap();
        let edges = seed_layered(&db, layers, width).await;
        let conn = db.read_conn();

        let cur_rows = count_walk_rows(conn, CURRENT_WALK, depth).await;
        let new_rows = count_walk_rows(conn, REWRITE_WALK, depth).await;
        let (cur_ids, cur_ms) = best_of(conn, CURRENT_WALK, depth, 5).await;
        let (new_ids, new_ms) = shipped(conn, depth, 5).await;

        assert_eq!(
            cur_ids, new_ids,
            "the shipped traversal must return what the simple-path form returned ({layers} x {width})"
        );

        println!(
            "{:<16} {:>7} {:>14} {:>14} {:>11.1} {:>11.1}",
            format!("{layers} layers x {width}"),
            edges,
            cur_rows,
            new_rows,
            cur_ms,
            new_ms
        );

        db.close().await.unwrap();
    }

    // The other half of the claim: free where the old form was already optimal.
    // This is the fixture that hid the defect, so it is also the one that has to
    // show the rewrite costs nothing.
    println!("\n== star-of-stars (a tree — the benches/ fixture), depth 3 ==\n");
    println!(
        "{:<16} {:>7} {:>14} {:>14} {:>11} {:>11}",
        "fixture", "edges", "walk rows old", "walk rows new", "ms old", "ms shipped"
    );
    for edges in [1_010usize, 5_050, 10_100] {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("s.db");
        let db = Database::open_with_cadence(&path, None).await.unwrap();
        seed_star(&db, edges).await;
        let conn = db.read_conn();

        // The star's root is n0000000, which `node(0, 0)` does not name — so run
        // these through explicit builders rather than the layered helpers.
        let old_sql = format!("{CURRENT_WALK} SELECT COUNT(*) FROM walk;");
        let new_sql = format!("{REWRITE_WALK} SELECT COUNT(*) FROM walk;");
        let count = |sql: String| async move {
            let mut rows = conn
                .query(
                    &sql,
                    libsql::params!["n0000000", 3i64, NOW, f64::NEG_INFINITY],
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
        };
        let cur_rows = count(old_sql).await;
        let new_rows = count(new_sql).await;

        // Collect the ids, exactly as `execute_ids` does. Discarding rows here
        // instead made the old arm skip ~10,000 String allocations and read as a
        // 18% regression that was entirely in the harness.
        let mut old_ms = f64::MAX;
        for _ in 0..15 {
            let sql = format!("{CURRENT_WALK}{PROJECT_IDS}");
            let start = Instant::now();
            let mut rows = conn
                .query(
                    &sql,
                    libsql::params!["n0000000", 3i64, NOW, f64::NEG_INFINITY],
                )
                .await
                .unwrap();
            let mut out: Vec<String> = Vec::new();
            while let Some(r) = rows.next().await.unwrap() {
                out.push(r.get(0).unwrap());
            }
            std::hint::black_box(&out);
            old_ms = old_ms.min(start.elapsed().as_secs_f64() * 1000.0);
        }

        let t = macrame::graph::TraversalBuilder::new("n0000000")
            .max_depth(3)
            .min_weight(f64::NEG_INFINITY);
        let mut new_ms = f64::MAX;
        for _ in 0..15 {
            let start = Instant::now();
            t.execute_ids(conn, NOW).await.unwrap();
            new_ms = new_ms.min(start.elapsed().as_secs_f64() * 1000.0);
        }

        println!(
            "{:<16} {:>7} {:>14} {:>14} {:>11.1} {:>11.1}",
            format!("{} nodes", edges + 1),
            edges,
            cur_rows,
            new_rows,
            old_ms,
            new_ms
        );

        db.close().await.unwrap();
    }

    println!("\nNode sets agreed at every layered size (asserted, not eyeballed).");
}
