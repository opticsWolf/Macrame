//! Does Louvain's aggregation phase change the answer at the post-interning
//! ceiling? (0.8.0, B6)
//!
//! `louvain` is deliberately phase-one-only: nodes move greedily to whichever
//! neighbouring community most increases modularity, and communities are never
//! aggregated into super-nodes and re-run. The rustdoc justifies that by graph
//! size — *"the aggregation phase would matter on graphs far larger than the
//! byte budget admits"* — and [D-115] raised what the byte budget admits by
//! 5.8×–6.8×. The scope limit was argued against the old ceiling and has to be
//! re-argued against the new one, which is what this measures.
//!
//! **The comparison is Q, not runtime.** Aggregation is not a speed
//! optimisation; it finds coarser structure that local moving cannot reach. If
//! it does not find any here, it is not worth having, and *that* is the result
//! rather than an absence of one.
//!
//! # Two controls, because the arms are easy to confound
//!
//! The two-phase implementation below is local moving plus aggregation, and its
//! local-moving step is a reimplementation — the crate's lives inside `louvain`
//! and is not separately callable. So a Q difference could be aggregation, or it
//! could be that the reimplementation simply differs. **Control 1** asserts that
//! at level 0 this file's `local_moving` produces exactly the partition the
//! crate's `louvain` does. Without it the headline number means nothing.
//!
//! **Control 2** is the fixture. `clustered` is a chain of `CLUSTER_SIZE`
//! cliques joined by single bridges, so the ground truth is known and the
//! probe reports how each arm scores against it. An arm that beats the ground
//! truth on Q is not wrong — it is the resolution limit, which is precisely the
//! phenomenon aggregation is supposed to exhibit.
//!
//! Built in memory rather than through the database: the question is about the
//! algorithm at a graph size, not about the loader, and `insert_node` /
//! `add_edge` are public since [D-114]. That is what makes reaching the real
//! ceiling affordable.
//!
//! Run:  cargo run --release --example louvain_aggregation_probe

use std::collections::BTreeMap;

use macrame::graph::{louvain, modularity, NodeData, Subgraph};

/// Matches `tests/common/fixtures.rs`, which is where the shape is defined.
const CLUSTER_SIZE: usize = 12;
const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The budget `benches/budgets.rs` loads its graph fixtures under.
const BUDGET: usize = 64 << 20;

fn node_id(i: usize) -> String {
    format!("c{i:07}")
}

/// A chain of cliques, built directly into a `Subgraph`.
///
/// Intra-community links go both ways, as the fixture's note requires: a
/// one-directional clique is a DAG, which is a different question.
fn clustered(communities: usize) -> (Subgraph, BTreeMap<String, usize>) {
    let mut g = Subgraph::default();
    let mut truth = BTreeMap::new();

    let nodes = communities * CLUSTER_SIZE;
    for i in 0..nodes {
        g.insert_node(node_id(i), NodeData::new("N", TS, OPEN));
        truth.insert(node_id(i), i / CLUSTER_SIZE);
    }

    for c in 0..communities {
        let base = c * CLUSTER_SIZE;
        for i in 0..CLUSTER_SIZE {
            for j in 0..CLUSTER_SIZE {
                if i != j {
                    g.add_edge(
                        &node_id(base + i),
                        &node_id(base + j),
                        "KNOWS",
                        1.0,
                        TS,
                        OPEN,
                    );
                }
            }
        }
        if c + 1 < communities {
            g.add_edge(
                &node_id(base + CLUSTER_SIZE - 2),
                &node_id(base + CLUSTER_SIZE),
                "KNOWS",
                1.0,
                TS,
                OPEN,
            );
        }
    }
    (g, truth)
}

/// An undirected weighted graph as integer-indexed adjacency.
///
/// The crate's `louvain` reads both directions off the `Subgraph` and sums
/// them, so a reciprocal pair contributes twice — reproduced here rather than
/// normalised away, because control 1 requires the two to agree exactly.
struct Flat {
    /// `adj[u] = [(v, w)]`, both directions present.
    adj: Vec<Vec<(usize, f64)>>,
    /// Weighted degree, summed the same way `Subgraph::weighted_degree` does.
    deg: Vec<f64>,
    /// `total_weight()`: the sum over stored (directed) edges.
    m: f64,
}

impl Flat {
    fn from_subgraph(g: &Subgraph) -> (Self, Vec<String>) {
        let ids: Vec<String> = g.node_ids().map(str::to_string).collect();
        let index: BTreeMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();

        let mut adj = vec![Vec::new(); ids.len()];
        for (u, id) in ids.iter().enumerate() {
            for e in g.out_edges(id).iter().chain(g.in_edges(id)) {
                adj[u].push((index[e.node(g)], e.weight()));
            }
        }
        let deg = ids.iter().map(|id| g.weighted_degree(id)).collect();
        let m = g.total_weight();
        (Flat { adj, deg, m }, ids)
    }
}

const MAX_SWEEPS: usize = 100;
const MIN_GAIN: f64 = 1e-12;

/// One local-moving pass to convergence — the crate's `louvain`, on `Flat`.
///
/// Deliberately a transcription and not an improvement: same `dQ`, same
/// epsilon, same withdraw-before-scoring, same lowest-index tie-break by
/// scanning a `BTreeMap`. Any divergence here would show up as a Q difference
/// and be attributed to aggregation, which is the confound control 1 exists to
/// rule out.
fn local_moving(f: &Flat) -> Vec<usize> {
    let n = f.adj.len();
    let mut comm: Vec<usize> = (0..n).collect();
    if f.m == 0.0 {
        return comm;
    }

    let mut sigma_tot: BTreeMap<usize, f64> = BTreeMap::new();
    for (u, &c) in comm.iter().enumerate() {
        *sigma_tot.entry(c).or_insert(0.0) += f.deg[u];
    }

    for _ in 0..MAX_SWEEPS {
        let mut moved = false;
        for u in 0..n {
            let curr = comm[u];
            let k_i = f.deg[u];
            *sigma_tot.get_mut(&curr).unwrap() -= k_i;

            let mut k_i_c: BTreeMap<usize, f64> = BTreeMap::new();
            for &(v, w) in &f.adj[u] {
                if v == u {
                    continue;
                }
                *k_i_c.entry(comm[v]).or_insert(0.0) += w;
            }

            let mut best = curr;
            let mut best_gain = MIN_GAIN;
            for (&c, k_i_in) in &k_i_c {
                let tot = sigma_tot.get(&c).copied().unwrap_or(0.0);
                let gain = (k_i_in / f.m) - (tot * k_i / (2.0 * f.m * f.m));
                if gain > best_gain {
                    best_gain = gain;
                    best = c;
                }
            }

            *sigma_tot.entry(best).or_insert(0.0) += k_i;
            if best != curr {
                comm[u] = best;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    comm
}

/// Collapse each community to one node, preserving weights.
///
/// Intra-community weight becomes a self-loop, which is what keeps `deg` and
/// `m` invariant across levels — the property that makes the next level's `dQ`
/// mean the same thing as this level's.
fn aggregate(f: &Flat, comm: &[usize]) -> (Flat, Vec<usize>) {
    let mut dense: BTreeMap<usize, usize> = BTreeMap::new();
    for &c in comm {
        let next = dense.len();
        dense.entry(c).or_insert(next);
    }
    let mapping: Vec<usize> = comm.iter().map(|c| dense[c]).collect();
    let k = dense.len();

    let mut adj = vec![Vec::new(); k];
    for (u, edges) in f.adj.iter().enumerate() {
        for &(v, w) in edges {
            adj[mapping[u]].push((mapping[v], w));
        }
    }
    let mut deg = vec![0.0; k];
    for (u, d) in f.deg.iter().enumerate() {
        deg[mapping[u]] += d;
    }
    (Flat { adj, deg, m: f.m }, mapping)
}

/// Full two-phase Louvain: local moving, aggregate, repeat until stable.
fn two_phase(f: &Flat) -> Vec<usize> {
    let mut level = Flat {
        adj: f.adj.clone(),
        deg: f.deg.clone(),
        m: f.m,
    };
    // Where each *original* node currently sits.
    let mut assignment: Vec<usize> = (0..f.adj.len()).collect();

    for _ in 0..32 {
        let comm = local_moving(&level);
        let distinct = comm.iter().collect::<std::collections::BTreeSet<_>>().len();
        let (next, mapping) = aggregate(&level, &comm);
        assignment = assignment.iter().map(|&a| mapping[a]).collect();
        if distinct == level.adj.len() {
            // Nothing merged: this level is already as coarse as it gets.
            break;
        }
        level = next;
    }
    assignment
}

fn labelled(ids: &[String], comm: &[usize]) -> BTreeMap<String, usize> {
    ids.iter().cloned().zip(comm.iter().copied()).collect()
}

/// Two partitions agree iff they induce the same grouping — labels are
/// arbitrary, so comparing them directly would report differences that are not.
fn same_partition(a: &BTreeMap<String, usize>, b: &BTreeMap<String, usize>) -> bool {
    let mut fwd: BTreeMap<usize, usize> = BTreeMap::new();
    let mut rev: BTreeMap<usize, usize> = BTreeMap::new();
    for (k, &ca) in a {
        let Some(&cb) = b.get(k) else { return false };
        if *fwd.entry(ca).or_insert(cb) != cb {
            return false;
        }
        if *rev.entry(cb).or_insert(ca) != ca {
            return false;
        }
    }
    a.len() == b.len()
}

fn distinct(c: &BTreeMap<String, usize>) -> usize {
    c.values().collect::<std::collections::BTreeSet<_>>().len()
}

/// How an arm's partition relates to the ground truth.
///
/// The distinction the headline Q number cannot make. A higher Q is only
/// evidence of *found structure* if the extra communities are real; if instead
/// every returned community is a union of whole ground-truth cliques, the arm
/// has not discovered anything, it has **merged** things that are genuinely
/// separate — the modularity resolution limit, which is a known property of the
/// objective rather than of the algorithm.
struct VersusTruth {
    /// Every returned community is a union of complete cliques.
    coarsens: bool,
    /// The most cliques any one returned community swallowed.
    max_cliques_merged: usize,
    /// A clique was split across returned communities — the other failure.
    splits: bool,
}

fn versus_truth(arm: &BTreeMap<String, usize>, truth: &BTreeMap<String, usize>) -> VersusTruth {
    // Which returned communities each true clique's members landed in.
    let mut clique_to_arms: BTreeMap<usize, std::collections::BTreeSet<usize>> = BTreeMap::new();
    let mut arm_to_cliques: BTreeMap<usize, std::collections::BTreeSet<usize>> = BTreeMap::new();
    for (node, &t) in truth {
        let a = arm[node];
        clique_to_arms.entry(t).or_default().insert(a);
        arm_to_cliques.entry(a).or_default().insert(t);
    }
    VersusTruth {
        coarsens: clique_to_arms.values().all(|s| s.len() == 1),
        max_cliques_merged: arm_to_cliques.values().map(|s| s.len()).max().unwrap_or(0),
        splits: clique_to_arms.values().any(|s| s.len() > 1),
    }
}

fn main() {
    // Find the ceiling: the largest `clustered` graph inside the budget.
    let mut communities = 64;
    let mut ceiling = communities;
    loop {
        let (g, _) = clustered(communities);
        if g.estimated_bytes() > BUDGET {
            break;
        }
        ceiling = communities;
        communities *= 2;
        if communities > 1 << 20 {
            break;
        }
    }
    let (g, _) = clustered(ceiling);
    println!(
        "post-interning ceiling on `clustered` under a {} MiB budget:\n  \
         {} communities, {} nodes, {} edges, {:.1} MiB ({} B/edge)\n",
        BUDGET >> 20,
        ceiling,
        g.node_count(),
        g.edge_count(),
        g.estimated_bytes() as f64 / (1 << 20) as f64,
        g.estimated_bytes() / g.edge_count().max(1),
    );

    // Sizes below the ceiling too: the resolution limit is a function of graph
    // size, so one point cannot show a trend and a trend is what decides this.
    let mut sizes: Vec<usize> = Vec::new();
    let mut s = 8;
    while s <= ceiling {
        sizes.push(s);
        s *= 4;
    }
    if sizes.last() != Some(&ceiling) {
        sizes.push(ceiling);
    }

    println!(
        "{:>7} {:>7} {:>9} {:>9} {:>9} {:>8} {:>6} {:>6}  {:<28} two-phase vs truth",
        "comms",
        "nodes",
        "Q(truth)",
        "Q(1phase)",
        "Q(2phase)",
        "dQ",
        "k(p1)",
        "k(2p)",
        "phase-one vs truth",
    );

    for &communities in &sizes {
        let (g, truth) = clustered(communities);
        let (flat, ids) = Flat::from_subgraph(&g);

        let crate_answer = louvain(&g);
        let mine = labelled(&ids, &local_moving(&flat));

        // **Control 1.** If these disagree, the comparison below is measuring
        // two implementations rather than one feature.
        assert!(
            same_partition(&crate_answer, &mine),
            "the transcribed local-moving step diverged from the crate's \
             `louvain` at {communities} communities, so any Q difference below \
             would be unattributable"
        );

        let full = labelled(&ids, &two_phase(&flat));

        let q_truth = modularity(&g, &truth);
        let q1 = modularity(&g, &crate_answer);
        let q2 = modularity(&g, &full);

        let describe = |v: VersusTruth, k: usize| -> String {
            if k == communities && !v.splits && v.max_cliques_merged == 1 {
                "exact".to_string()
            } else if v.splits {
                format!("splits cliques (max merge {})", v.max_cliques_merged)
            } else if v.coarsens {
                format!("merges {} cliques/community", v.max_cliques_merged)
            } else {
                "neither".to_string()
            }
        };

        println!(
            "{:>7} {:>7} {:>9.5} {:>9.5} {:>9.5} {:>+8.5} {:>6} {:>6}  {:<28} {}",
            communities,
            g.node_count(),
            q_truth,
            q1,
            q2,
            q2 - q1,
            distinct(&crate_answer),
            distinct(&full),
            describe(versus_truth(&crate_answer, &truth), distinct(&crate_answer)),
            describe(versus_truth(&full, &truth), distinct(&full)),
        );
    }

    println!(
        "\ndQ is Q(two-phase) - Q(phase-one). The last two columns are what dQ \
         alone cannot\ntell you: whether a higher Q came from *finding* structure \
         or from merging communities\nthat are genuinely separate. `clustered`'s \
         ground truth is one community per clique,\nand cliques are joined only \
         by a single bridge edge."
    );
}
