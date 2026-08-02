//! T3.3: is the byte budget the binding constraint, and what would interning buy?
//!
//! D-063 retired the integer-index rewrite on **CPU** grounds and the plan
//! observes that the **memory** argument was never made. It also sets the gate
//! explicitly: measure *edges per budget*, not milliseconds, and skip the work if
//! the budget is not what actually stops real callers.
//!
//! So this reports three things:
//!
//!   1. **bytes per edge** as the shipped `Subgraph` accounts for them, split
//!      into the parts interning could remove and the parts it could not;
//!   2. **edges per MiB**, now and under interning, which is the reachability
//!      number the gate is stated in;
//!   3. **the node share** of the budget, because interning edges cannot help
//!      with a graph whose bytes are mostly `content` strings — and a subgraph
//!      that hydrates attributes carries exactly that.
//!
//! (3) is the one that decides it. The edge saving is arithmetic and large; what
//! matters is whether edges are where the budget goes.
//!
//! Run with:  cargo run --release --example budget_density_diag

use macrame::graph::{EdgeRef, NodeData, Subgraph};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// What one adjacency entry cost **before** interning (0.7.0), by the loader's
/// own formula at the time: the four string payloads plus the struct.
///
/// Kept as arithmetic because the type it describes no longer exists. It is the
/// baseline column, and the point of the table is the ratio to it.
fn edge_entry_bytes_0_7_0(id_len: usize, edge_type_len: usize) -> usize {
    const EDGE_REF_0_7_0: usize = 104; // {String, String, f64, String, String}
    id_len + edge_type_len + TS.len() + OPEN.len() + EDGE_REF_0_7_0
}

/// What one costs now — **measured on the real type**, not asserted.
///
/// B2's exit gate is explicit that this table must be reproduced "on the real
/// type, not on `size_of` arithmetic", so this builds an actual `Subgraph`,
/// asks it for `estimated_bytes()`, and divides. The whole pool is included, so
/// the id table D-063 warned would "partly cancel the memory win" is paid for
/// in this number rather than argued away.
fn measured_bytes_per_edge(id_len: usize, edges: usize) -> (usize, usize) {
    let id = |i: usize| format!("{:0>width$}", i, width = id_len);
    let mut g = Subgraph::default();
    let nodes = edges / 20 + 2; // ~20 edges per node, a realistic density
    for i in 0..nodes {
        g.insert_node(id(i), NodeData::new("t", TS, OPEN));
    }
    let empty = g.estimated_bytes();
    for e in 0..edges {
        g.add_edge(
            &id(e % nodes),
            &id((e * 7 + 1) % nodes),
            "LINKS",
            1.0,
            TS,
            OPEN,
        );
    }
    let full = g.estimated_bytes();
    ((full - empty) / edges, full / edges)
}

fn main() {
    println!("Subgraph byte accounting, by the loader's own formula.\n");
    println!("size_of::<EdgeRef>()  = {}", std::mem::size_of::<EdgeRef>());
    println!(
        "size_of::<NodeData>() = {}",
        std::mem::size_of::<NodeData>()
    );
    println!(
        "timestamp width       = {} bytes, two per entry\n",
        TS.len()
    );

    println!(
        "{:>10} {:>12} {:>13} {:>14} {:>12} {:>10}",
        "id len", "B/edge now", "B/edge intern", "edges/MiB now", "intern", "ratio"
    );

    for id_len in [8usize, 26, 64] {
        // Two adjacency entries per edge (`out_adj` and `in_adj`).
        let now = 2 * edge_entry_bytes_0_7_0(id_len, "LINKS".len());
        let (marginal, all_in) = measured_bytes_per_edge(id_len, 20_000);
        let mib = 1usize << 20;
        println!(
            "{:>10} {:>12} {:>13} {:>14} {:>12} {:>9.1}x",
            id_len,
            now,
            all_in,
            mib / now,
            mib / all_in.max(1),
            now as f64 / all_in.max(1) as f64
        );
        let _ = marginal;
    }

    // ---- the part that decides it: where do the bytes actually go? ----
    //
    // A subgraph is nodes *and* edges. Interning the edges cannot touch the node
    // side, and `NodeData` carries `title` and `content` — the latter being
    // document text, which is unbounded and typically dwarfs everything else.
    println!("\nNode share of the budget, at a fixed 8-byte id and 20 edges/node:");
    println!(
        "{:>14} {:>14} {:>14} {:>16}",
        "content bytes", "node bytes", "edge bytes", "edges' share"
    );

    for content in [0usize, 200, 2_000, 20_000] {
        let mut g = Subgraph::default();
        let n = 200usize;
        for i in 0..n {
            g.insert_node(
                format!("c{i:07}"),
                NodeData::new("A title", TS, OPEN).with_content("x".repeat(content)),
            );
        }
        for i in 0..n {
            for k in 1..=20usize {
                // `add_edge` writes both adjacency entries, which is exactly
                // the thing being measured: one edge, two owned copies,
                // differing only in which endpoint `node` names. It was private
                // until 0.8.0 and this loop built the pair by hand.
                let source = format!("c{i:07}");
                let target = format!("c{:07}", (i + k) % n);
                g.add_edge(&source, &target, "LINKS", 1.0, TS, OPEN);
            }
        }

        let total = g.estimated_bytes();
        let edge_bytes = 2 * n * 20 * std::mem::size_of::<EdgeRef>();
        let node_bytes = total.saturating_sub(edge_bytes);
        println!(
            "{:>14} {:>14} {:>14} {:>15.0}%",
            content,
            node_bytes,
            edge_bytes,
            100.0 * edge_bytes as f64 / total as f64
        );
    }
}
