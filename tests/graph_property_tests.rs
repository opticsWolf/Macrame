//! Oracle tests for the native graph algorithms (§8, D-039).
//!
//! Dropping `petgraph` means dropping the years of use that stood behind
//! `dijkstra` and `tarjan_scc`. What replaces that here is differential testing:
//! each algorithm is checked against an independent brute-force definition of
//! the same thing, over generated graphs. The oracles are deliberately naive —
//! exponential in places — because an oracle that shares an optimisation with
//! the implementation shares its bugs.
//!
//! Behind the `property-tests` feature with the rest of the generated-history
//! suite (R15). These particular tests open no database, so they are not part of
//! the libSQL fault; they are gated for consistency of the test-running story,
//! and `.proptest-regressions` replays every previously found failure first.

use std::collections::{BTreeMap, BTreeSet};

use macrame::graph::{
    astar, dijkstra, k_core, louvain, modularity, scc, NodeData, Subgraph,
};
use proptest::prelude::*;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

fn node_id(i: usize) -> String {
    format!("n{i:02}")
}

/// Assemble a `Subgraph` from `(source, target, weight)` triples.
fn build(n: usize, edges: &[(usize, usize, f64)]) -> Subgraph {
    let mut g = Subgraph::default();
    for i in 0..n {
        g.insert_node(
            node_id(i),
            NodeData::new(node_id(i), T0.to_string(), OPEN.to_string()),
        );
    }
    for &(s, t, w) in edges {
        g.add_edge(&node_id(s), &node_id(t), "KNOWS", w, T0, OPEN);
    }
    g
}

/// Small graphs, so the exponential oracles stay tractable.
///
/// Weights are drawn from a small set of exact binary fractions rather than
/// arbitrary floats: the oracles compare sums of the same values in a different
/// order, and with arbitrary floats a mismatch would be association error rather
/// than a defect. Reordering these is exact.
fn arb_graph(max_nodes: usize) -> impl Strategy<Value = (usize, Vec<(usize, usize, f64)>)> {
    (2usize..=max_nodes).prop_flat_map(|n| {
        let edge = (
            0..n,
            0..n,
            prop::sample::select(vec![0.25f64, 0.5, 1.0, 2.0, 4.0]),
        );
        (Just(n), prop::collection::vec(edge, 0..(n * 2)))
    })
}

// ---------------------------------------------------------------------------
// Oracles
// ---------------------------------------------------------------------------

/// Shortest distances by exhaustive enumeration of every simple path.
///
/// Not Bellman-Ford: that shares Dijkstra's relaxation idea, and a shared idea
/// is a shared blind spot. Enumerating paths is the definition itself.
fn oracle_distances(g: &Subgraph, start: &str) -> BTreeMap<String, f64> {
    let mut best: BTreeMap<String, f64> = BTreeMap::new();
    best.insert(start.to_string(), 0.0);

    // DFS over simple paths. Bounded by the node count in `arb_graph`.
    fn walk(
        g: &Subgraph,
        at: &str,
        cost: f64,
        seen: &mut BTreeSet<String>,
        best: &mut BTreeMap<String, f64>,
    ) {
        for e in g.out_edges(at) {
            if seen.contains(e.node(g)) {
                continue;
            }
            let c = cost + e.weight();
            let entry = best.entry(e.node(g).to_string()).or_insert(f64::INFINITY);
            if c < *entry {
                *entry = c;
            }
            seen.insert(e.node(g).to_string());
            walk(g, e.node(g), c, seen, best);
            seen.remove(e.node(g));
        }
    }

    let mut seen = BTreeSet::from([start.to_string()]);
    walk(g, start, 0.0, &mut seen, &mut best);
    best
}

/// Strongly connected components from the transitive closure of reachability.
///
/// u and v share a component exactly when each reaches the other. Computed as a
/// naive O(V^3) closure, with no reference to any SCC algorithm.
fn oracle_scc(g: &Subgraph) -> Vec<Vec<String>> {
    let ids: Vec<String> = g.node_ids().map(str::to_string).collect();
    let n = ids.len();
    let idx: BTreeMap<&String, usize> = ids.iter().enumerate().map(|(i, s)| (s, i)).collect();

    let mut reach = vec![vec![false; n]; n];
    for (i, id) in ids.iter().enumerate() {
        reach[i][i] = true;
        for e in g.out_edges(id) {
            if let Some(&j) = idx.get(&e.node(g).to_string()) {
                reach[i][j] = true;
            }
        }
    }
    // Warshall.
    for k in 0..n {
        for i in 0..n {
            for j in 0..n {
                if reach[i][k] && reach[k][j] {
                    reach[i][j] = true;
                }
            }
        }
    }

    let mut assigned = vec![false; n];
    let mut comps = Vec::new();
    for i in 0..n {
        if assigned[i] {
            continue;
        }
        let mut comp = Vec::new();
        for j in 0..n {
            if !assigned[j] && reach[i][j] && reach[j][i] {
                assigned[j] = true;
                comp.push(ids[j].clone());
            }
        }
        comp.sort();
        comps.push(comp);
    }
    comps.sort();
    comps
}

/// k-core by repeatedly deleting every under-degree node until nothing changes.
///
/// The definition, restated: recompute all degrees from scratch each round
/// rather than maintaining them incrementally, which is exactly the step
/// `k_core` optimises and therefore the step worth checking.
fn oracle_k_core(g: &Subgraph, k: usize) -> BTreeSet<String> {
    let mut alive: BTreeSet<String> = g.node_ids().map(str::to_string).collect();
    loop {
        let mut doomed = BTreeSet::new();
        for node in &alive {
            let deg = g
                .out_edges(node)
                .iter()
                .chain(g.in_edges(node))
                .filter(|e| alive.contains(e.node(g)))
                .count();
            if deg < k {
                doomed.insert(node.clone());
            }
        }
        if doomed.is_empty() {
            return alive;
        }
        for d in doomed {
            alive.remove(&d);
        }
    }
}

/// Best achievable modularity, by enumerating every partition of the nodes.
///
/// Restricted growth strings: `a[i]` is the community of node i, constrained so
/// that `a[i] <= 1 + max(a[0..i])`. That enumerates each set partition exactly
/// once. Bell numbers grow fast, so callers keep n small.
fn oracle_best_modularity(g: &Subgraph) -> f64 {
    let ids: Vec<String> = g.node_ids().map(str::to_string).collect();
    let n = ids.len();
    let mut assign = vec![0usize; n];
    let mut best = f64::NEG_INFINITY;

    fn enumerate(
        g: &Subgraph,
        ids: &[String],
        assign: &mut Vec<usize>,
        i: usize,
        max_used: usize,
        best: &mut f64,
    ) {
        if i == ids.len() {
            let part: BTreeMap<String, usize> =
                ids.iter().cloned().zip(assign.iter().copied()).collect();
            let q = modularity(g, &part);
            if q > *best {
                *best = q;
            }
            return;
        }
        for c in 0..=max_used {
            assign[i] = c;
            enumerate(g, ids, assign, i + 1, max_used.max(c + 1), best);
        }
    }

    enumerate(g, &ids, &mut assign, 0, 0, &mut best);
    best
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Dijkstra agrees with exhaustive path enumeration.
    #[test]
    fn dijkstra_matches_brute_force_path_enumeration((n, edges) in arb_graph(7)) {
        let g = build(n, &edges);
        let start = node_id(0);

        let got = dijkstra(&g, &start);
        let want: BTreeMap<String, f64> = oracle_distances(&g, &start)
            .into_iter()
            .filter(|(_, d)| d.is_finite())
            .collect();

        prop_assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>());
        for (node, d) in &got {
            prop_assert!(
                (d - want[node]).abs() < 1e-9,
                "node {}: dijkstra {} vs oracle {}", node, d, want[node]
            );
        }
    }

    /// A* with a zero heuristic must agree with Dijkstra on cost, and the path
    /// it returns must be a real path whose weights sum to that cost.
    ///
    /// Checking the cost alone would miss a reconstruction that returns the
    /// right number attached to the wrong route.
    #[test]
    fn astar_agrees_with_dijkstra_and_returns_a_walkable_path((n, edges) in arb_graph(7)) {
        let g = build(n, &edges);
        let start = node_id(0);
        let dist = dijkstra(&g, &start);

        for target in g.node_ids() {
            let found = astar(&g, &start, target, |_, _| 0.0);
            match (dist.get(target), found) {
                (None, f) => prop_assert!(f.is_none(), "unreachable {} reported reachable", target),
                (Some(_), None) => prop_assert!(false, "reachable {} reported unreachable", target),
                (Some(&want), Some((cost, path))) => {
                    prop_assert!((cost - want).abs() < 1e-9);
                    prop_assert_eq!(path.first().map(String::as_str), Some(start.as_str()));
                    prop_assert_eq!(path.last().map(String::as_str), Some(target));

                    // Every consecutive pair must be a real edge, and the
                    // cheapest such edge sums to the reported cost.
                    let mut sum = 0.0;
                    for pair in path.windows(2) {
                        let step = g.out_edges(&pair[0])
                            .iter()
                            .filter(|e| e.node(&g) == pair[1].as_str())
                            .map(|e| e.weight())
                            .fold(f64::INFINITY, f64::min);
                        prop_assert!(step.is_finite(), "{} -> {} is not an edge", pair[0], pair[1]);
                        sum += step;
                    }
                    prop_assert!((sum - cost).abs() < 1e-9, "path sums to {} not {}", sum, cost);
                }
            }
        }
    }

    /// Kosaraju agrees with the reachability closure.
    #[test]
    fn scc_matches_the_reachability_closure((n, edges) in arb_graph(8)) {
        let g = build(n, &edges);
        prop_assert_eq!(scc(&g), oracle_scc(&g));
    }

    /// Incremental peeling agrees with recompute-from-scratch peeling.
    #[test]
    fn k_core_matches_fixpoint_filtering((n, edges) in arb_graph(8), k in 0usize..6) {
        let g = build(n, &edges);
        prop_assert_eq!(k_core(&g, k), oracle_k_core(&g, k));
    }

    /// Louvain never returns a partition worse than the singletons it starts
    /// from, and never claims more than the true optimum.
    ///
    /// The upper bound is the load-bearing half. "At least as good as
    /// singletons" is satisfied by returning the singletons — which is what the
    /// previous implementation did — so on its own it certifies nothing.
    #[test]
    fn louvain_improves_on_singletons_without_exceeding_the_optimum((n, edges) in arb_graph(6)) {
        let g = build(n, &edges);
        let comms = louvain(&g);

        prop_assert_eq!(comms.len(), g.node_count(), "every node must be assigned");

        let singletons: BTreeMap<String, usize> =
            g.node_ids().enumerate().map(|(i, x)| (x.to_string(), i)).collect();

        let q = modularity(&g, &comms);
        prop_assert!(q >= modularity(&g, &singletons) - 1e-9);
        prop_assert!(
            q <= oracle_best_modularity(&g) + 1e-9,
            "reported modularity {} exceeds the enumerated optimum", q
        );
    }

    /// Communities are numbered densely from zero, so callers can size an array
    /// by the community count without a sparse-index surprise.
    #[test]
    fn louvain_community_indices_are_dense((n, edges) in arb_graph(6)) {
        let g = build(n, &edges);
        let comms = louvain(&g);
        let used: BTreeSet<usize> = comms.values().copied().collect();
        prop_assert_eq!(used.iter().copied().collect::<Vec<_>>(), (0..used.len()).collect::<Vec<_>>());
    }

    /// The determinism the `BTreeMap` choice exists to buy (§5.4).
    #[test]
    fn every_algorithm_is_a_function_of_the_graph_alone((n, edges) in arb_graph(8)) {
        let g = build(n, &edges);
        let start = node_id(0);
        for _ in 0..3 {
            prop_assert_eq!(dijkstra(&g, &start), dijkstra(&g, &start));
            prop_assert_eq!(scc(&g), scc(&g));
            prop_assert_eq!(k_core(&g, 2), k_core(&g, 2));
            prop_assert_eq!(louvain(&g), louvain(&g));
        }
    }
}
