//! T4.1: what the four shapes actually cost, measured rather than argued.
//!
//! The fixture matrix is only worth its maintenance if the shapes *disagree* on
//! the numbers this project records. `fixture_matrix_tests` pins their
//! structure; this runs them through the database and reports the costs.
//!
//! Four columns, each chosen because a decision entry rests on it:
//!
//!   * **seed** — bulk import of the shape's edges. D-059's index note is a
//!     table of figures taken on hub inserts; the question here is whether the
//!     other shapes agree.
//!   * **load** — `load_subgraph` at depth 3, which is D-070's measurement.
//!   * **rows** — what the shipped `UNION` walk returns, beside the
//!     simple-path count the pre-T0.1 form would have materialised. On
//!     `star_of_stars` the two are equal, which is why T0.1's win was invisible
//!     on the only fixture that existed.
//!   * **louvain** — the analytics path, on the shape that has communities and
//!     on three that do not.
//!
//! Run with:  cargo run --release --example fixture_matrix_diag

#[path = "../tests/fixtures.rs"]
mod fixtures;

use std::time::Instant;

use fixtures::{depth_to_cover, facts, reach, seed, ALL_SHAPES, TS};
use macrame::graph::louvain;
use macrame::prelude::*;

const NODES: usize = 600;
const DEPTH: u32 = 3;
/// Large enough that no shape hits it, so the column measures cost rather than
/// where each shape happened to stop. `dense_small` is the one that would.
const BUDGET: usize = 512 * 1024 * 1024;

#[tokio::main]
async fn main() {
    println!("The fixture matrix, {NODES} requested nodes, depth {DEPTH}.\n");
    let mut covered: Vec<(&str, usize, usize, f64, usize)> = Vec::new();
    println!(
        "{:>14} {:>7} {:>8} {:>9} {:>9} {:>8} {:>10} {:>9}",
        "shape", "nodes", "edges", "seed ms", "load ms", "rows", "s.paths", "louvain"
    );

    for &shape in ALL_SHAPES {
        let dir = tempfile::TempDir::new().unwrap();
        // Cadence off: a background fold landing mid-measurement is noise.
        let db = Database::open_with_cadence(&dir.path().join("m.db"), None)
            .await
            .unwrap();

        let t = Instant::now();
        let edges = seed(&db, shape, NODES).await;
        let seed_ms = t.elapsed().as_secs_f64() * 1e3;

        let start = shape.start_node(NODES);
        let t = Instant::now();
        let g = db.load_subgraph(&start, DEPTH, TS, BUDGET).await.unwrap();
        let load_ms = t.elapsed().as_secs_f64() * 1e3;

        let t = Instant::now();
        let communities = louvain(&g);
        let louvain_ms = t.elapsed().as_secs_f64() * 1e3;

        let f = facts(shape, NODES, DEPTH as usize);

        println!(
            "{:>14} {:>7} {:>8} {:>9.1} {:>9.1} {:>8} {:>10} {:>9.1}",
            shape.name(),
            g.nodes.len(),
            edges,
            seed_ms,
            load_ms,
            communities.len(),
            f.simple_paths,
            louvain_ms
        );

        // ---- the same measurement, normalised on size instead of depth ----
        //
        // The row above holds *depth* fixed, and at depth 3 these shapes reach
        // 600, 25, 6 and 300 nodes. Reading it as a comparison between shapes
        // would compare a 600-node problem against a 6-node one. So the second
        // arm holds *reached nodes* fixed instead, at the depth each shape
        // needs to cover 90% of itself.
        let d = depth_to_cover(shape, NODES, 0.9, NODES);
        let t = Instant::now();
        let g = db
            .load_subgraph(&start, d as u32, TS, BUDGET)
            .await
            .unwrap();
        covered.push((
            shape.name(),
            d,
            g.nodes.len(),
            t.elapsed().as_secs_f64() * 1e3,
            g.estimated_bytes(),
        ));

        db.close().await.unwrap();
    }

    println!("\nSame load, normalised on coverage instead of depth (>=90% of nodes):");
    println!(
        "{:>14} {:>7} {:>8} {:>10} {:>12} {:>12}",
        "shape", "depth", "nodes", "load ms", "bytes", "B/node"
    );
    for (name, d, nodes, ms, bytes) in &covered {
        println!(
            "{:>14} {:>7} {:>8} {:>10.1} {:>12} {:>12}",
            name,
            d,
            nodes,
            ms,
            bytes,
            bytes.checked_div(*nodes).unwrap_or(0)
        );
    }

    println!("\nWhat each shape is the worst case for:");
    for &shape in ALL_SHAPES {
        println!("  {:>14}  {}", shape.name(), shape.worst_case_for());
    }

    println!("\nStructure at depth {DEPTH}, from the edge set alone (no database):");
    println!(
        "{:>14} {:>8} {:>10} {:>12} {:>12} {:>12}",
        "shape", "reached", "max out-deg", "s.paths", "UNION bound", "multiplier"
    );
    for &shape in ALL_SHAPES {
        let f = facts(shape, NODES, DEPTH as usize);
        println!(
            "{:>14} {:>8} {:>10} {:>12} {:>12} {:>11.1}x",
            f.shape,
            f.reached,
            f.max_out_degree,
            f.simple_paths,
            f.union_bound(DEPTH as usize),
            f.path_multiplier()
        );
    }
    println!(
        "\n`s.paths` is the pre-T0.1 walk's row count and `UNION bound` is the\n\
         shipped one's. Their ratio is what T0.1 bought — and it is 0.25x on\n\
         star_of_stars, the only fixture that existed when T0.1 was written."
    );

    println!("\nHops needed to cover the graph:");
    for &shape in ALL_SHAPES {
        let d = depth_to_cover(shape, NODES, 0.9, NODES);
        println!(
            "  {:>14}  depth {:>2} reaches {} nodes",
            shape.name(),
            d,
            reach(shape, NODES, d)
        );
    }
}
