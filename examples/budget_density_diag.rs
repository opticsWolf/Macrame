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

/// What one adjacency entry costs today, by the loader's own formula.
fn edge_entry_bytes(id_len: usize, edge_type_len: usize) -> usize {
    // `Subgraph::edge_bytes`: the four string payloads plus the struct.
    id_len + edge_type_len + TS.len() + OPEN.len() + std::mem::size_of::<EdgeRef>()
}

/// What it would cost with ids and edge types interned to `u32` and timestamps
/// carried as interned `u32` too.
///
/// Not a guess about layout: `{u32, u32, f64, u32, u32}` is 24 bytes with the
/// `f64`'s alignment, and there are no heap payloads left to add.
const INTERNED_EDGE_ENTRY_BYTES: usize = 24;

fn main() {
    println!("Subgraph byte accounting, by the loader's own formula.\n");
    println!("size_of::<EdgeRef>()  = {}", std::mem::size_of::<EdgeRef>());
    println!("size_of::<NodeData>() = {}", std::mem::size_of::<NodeData>());
    println!("timestamp width       = {} bytes, two per entry\n", TS.len());

    println!(
        "{:>10} {:>12} {:>13} {:>14} {:>12} {:>10}",
        "id len", "B/edge now", "B/edge intern", "edges/MiB now", "intern", "ratio"
    );

    for id_len in [8usize, 26, 64] {
        // Two adjacency entries per edge (`out_adj` and `in_adj`).
        let now = 2 * edge_entry_bytes(id_len, "LINKS".len());
        let interned = 2 * INTERNED_EDGE_ENTRY_BYTES;
        let mib = 1usize << 20;
        println!(
            "{:>10} {:>12} {:>13} {:>14} {:>12} {:>9.1}x",
            id_len,
            now,
            interned,
            mib / now,
            mib / interned,
            now as f64 / interned as f64
        );
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
            g.nodes.insert(
                format!("c{i:07}"),
                NodeData {
                    title: "A title".to_string(),
                    content: "x".repeat(content),
                    embedding_model: None,
                    valid_from: TS.to_string(),
                    valid_to: OPEN.to_string(),
                },
            );
        }
        for i in 0..n {
            for k in 1..=20usize {
                // `add_edge` is private, so the two adjacency entries it would
                // write are built here directly — which is also the clearer
                // statement of the thing being measured: one edge, two owned
                // copies, differing only in which endpoint `node` names.
                let source = format!("c{i:07}");
                let target = format!("c{:07}", (i + k) % n);
                let fwd = EdgeRef {
                    node: target.clone(),
                    edge_type: "LINKS".to_string(),
                    weight: 1.0,
                    valid_from: TS.to_string(),
                    valid_to: OPEN.to_string(),
                };
                let mut back = fwd.clone();
                back.node = source.clone();
                g.out_adj.entry(source).or_default().push(fwd);
                g.in_adj.entry(target).or_default().push(back);
            }
        }

        let total = g.estimated_bytes();
        let edge_bytes = 2 * n * 20 * edge_entry_bytes(8, 5);
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
