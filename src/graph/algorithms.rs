//! In-memory graph algorithms operating on a loaded [`Subgraph`] (§5.4).
//!
//! Pure CPU, synchronous, no external dependencies (D-039).
//!
//! # Determinism
//!
//! Every function here is a deterministic function of the [`Subgraph`] value:
//! the same graph yields the same answer, byte for byte, on every run and every
//! platform. That is not automatic, and it is the reason this module reaches for
//! `BTreeMap`/`BTreeSet` in places where a `HashMap` would be the reflexive
//! choice:
//!
//! * `Subgraph`'s maps are ordered, so node iteration order is the ULID order.
//! * Returns are ordered too. A `HashSet<String>` return would push the
//!   nondeterminism onto the caller — Rust's default hasher is seeded per
//!   process, so a caller iterating the result to write it back would emit rows
//!   in a different order on every run.
//! * Ties are broken explicitly, never by iteration order. Two heap entries with
//!   equal distance are ordered by node id; two communities with equal
//!   modularity gain resolve to the lower community index.
//!
//! Without all three, `FakeClock` fixes the clock and the analytics still drift.
//!
//! # Edge weights must be non-negative
//!
//! `dijkstra` and `astar` assume `weight >= 0`; that is what makes a settled
//! node final. The schema does not enforce it (`weight REAL NOT NULL`, no
//! CHECK), so a negative weight is storable today and would yield a silently
//! wrong shortest path. Both functions therefore bound their own work and
//! [`Database::load_subgraph`](crate::Database::load_subgraph) refuses to build
//! a graph containing one, so the failure is loud at the boundary rather than
//! quiet in the result.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use super::subgraph::Subgraph;

/// A total order over `f64` so distances can live in a `BinaryHeap`.
///
/// `f64` is only `PartialOrd` because `NaN` compares false against everything,
/// which is exactly the case that would corrupt a heap's invariant silently.
/// `total_cmp` is the IEEE-754 total order: it never returns `Equal` for
/// distinct bit patterns, so the heap stays well-ordered even if a `NaN` weight
/// reaches it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrdF64(f64);

impl Eq for OrdF64 {}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dijkstra's algorithm for shortest path distances (§5.4).
///
/// Returns node id -> shortest distance from `start`, including `start` at 0.0.
/// Unreachable nodes are absent rather than present at infinity.
pub fn dijkstra(graph: &Subgraph, start: &str) -> BTreeMap<String, f64> {
    let mut dist = BTreeMap::new();
    let mut heap = BinaryHeap::new();

    if !graph.contains_node(start) {
        return dist;
    }

    dist.insert(start.to_string(), 0.0);
    heap.push(Reverse((OrdF64(0.0), start.to_string())));

    while let Some(Reverse((OrdF64(d), node))) = heap.pop() {
        // A stale entry: this node was reached again more cheaply after this
        // entry was pushed. Settle it once, at its best distance.
        if d > *dist.get(&node).unwrap_or(&f64::INFINITY) {
            continue;
        }

        for edge in graph.out_edges(&node) {
            let next = edge.node(graph);
            let new_dist = d + edge.weight();

            if new_dist < *dist.get(next).unwrap_or(&f64::INFINITY) {
                dist.insert(next.to_string(), new_dist);
                heap.push(Reverse((OrdF64(new_dist), next.to_string())));
            }
        }
    }

    dist
}

/// A* search from `start` to `goal` (§5.4).
///
/// Returns the total cost and the full path inclusive of both endpoints, or
/// `None` when `goal` is unreachable. `heuristic` must be admissible — it must
/// never overestimate the remaining cost — or the path returned is a path but
/// not necessarily the shortest one.
pub fn astar<F>(
    graph: &Subgraph,
    start: &str,
    goal: &str,
    heuristic: F,
) -> Option<(f64, Vec<String>)>
where
    F: Fn(&str, &str) -> f64,
{
    if !graph.contains_node(start) || !graph.contains_node(goal) {
        return None;
    }

    let mut g_score: BTreeMap<String, f64> = BTreeMap::new();
    let mut came_from: BTreeMap<String, String> = BTreeMap::new();
    let mut heap = BinaryHeap::new();

    g_score.insert(start.to_string(), 0.0);
    heap.push(Reverse((OrdF64(heuristic(start, goal)), start.to_string())));

    while let Some(Reverse((OrdF64(f_score), current))) = heap.pop() {
        let current_g = g_score[&current];

        if current == goal {
            return Some((current_g, reconstruct(&came_from, goal, graph.node_count())));
        }

        // A stale entry, superseded by a cheaper route to the same node.
        if f_score > current_g + heuristic(&current, goal) {
            continue;
        }

        for edge in graph.out_edges(&current) {
            let neighbor = edge.node(graph);
            let tentative_g = current_g + edge.weight();

            if tentative_g < *g_score.get(neighbor).unwrap_or(&f64::INFINITY) {
                // `start` never gets a predecessor, so `reconstruct` cannot
                // walk into a cycle at the head of the path.
                if neighbor != start {
                    came_from.insert(neighbor.to_string(), current.clone());
                }
                g_score.insert(neighbor.to_string(), tentative_g);
                let f = tentative_g + heuristic(neighbor, goal);
                heap.push(Reverse((OrdF64(f), neighbor.to_string())));
            }
        }
    }

    None
}

/// Walk the predecessor chain back from `goal`, forwards.
///
/// `limit` bounds the walk at the node count. The chain cannot exceed that on a
/// well-formed `came_from`, so exceeding it means the map has a cycle; the walk
/// stops rather than hanging.
fn reconstruct(came_from: &BTreeMap<String, String>, goal: &str, limit: usize) -> Vec<String> {
    let mut path = vec![goal.to_string()];
    let mut curr = goal.to_string();
    while let Some(prev) = came_from.get(&curr) {
        if path.len() > limit {
            break;
        }
        path.push(prev.clone());
        curr = prev.clone();
    }
    path.reverse();
    path
}

/// Strongly connected components by Kosaraju's algorithm (§5.4).
///
/// Both passes use an explicit stack. Recursion would put the traversal depth on
/// the call stack, and a knowledge graph is deep enough for that to be a real
/// overflow rather than a theoretical one.
///
/// Components come back in a canonical form — each component sorted, and the
/// components ordered by their first element — so the result is comparable
/// across runs without the caller having to normalise it.
pub fn scc(graph: &Subgraph) -> Vec<Vec<String>> {
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();

    // Pass 1: post-order finish times on the graph as given.
    for node in graph.node_ids() {
        if visited.contains(node) {
            continue;
        }
        let mut stack = vec![(node.to_string(), false)];
        while let Some((curr, exhausted)) = stack.pop() {
            if exhausted {
                order.push(curr);
                continue;
            }
            if visited.contains(&curr) {
                continue;
            }
            visited.insert(curr.clone());
            // Re-pushed beneath its children, so it finishes after them.
            stack.push((curr.clone(), true));

            for edge in graph.out_edges(&curr) {
                if !visited.contains(edge.node(graph)) {
                    stack.push((edge.node(graph).to_string(), false));
                }
            }
        }
    }

    // Pass 2: the transpose, in decreasing finish time.
    visited.clear();
    let mut components = Vec::new();

    for node in order.into_iter().rev() {
        if visited.contains(&node) {
            continue;
        }
        let mut comp = Vec::new();
        let mut stack = vec![node];

        while let Some(curr) = stack.pop() {
            if visited.contains(&curr) {
                continue;
            }
            visited.insert(curr.clone());
            comp.push(curr.clone());

            for edge in graph.in_edges(&curr) {
                if !visited.contains(edge.node(graph)) {
                    stack.push(edge.node(graph).to_string());
                }
            }
        }
        comp.sort();
        components.push(comp);
    }

    components.sort();
    components
}

/// k-core decomposition: the maximal induced subgraph in which every node has
/// degree at least `k` (§5.4).
///
/// Treats the graph as undirected, summing in- and out-degree. Parallel edges
/// count once each — a node held in by three edges to one neighbour has degree
/// three, which is what makes this a multigraph core.
pub fn k_core(graph: &Subgraph, k: usize) -> BTreeSet<String> {
    let mut degree: BTreeMap<String, usize> = graph
        .node_ids()
        .map(|n| (n.to_string(), graph.degree(n)))
        .collect();

    let mut queue: VecDeque<String> = degree
        .iter()
        .filter(|(_, &d)| d < k)
        .map(|(n, _)| n.clone())
        .collect();

    let mut removed = BTreeSet::new();

    while let Some(node) = queue.pop_front() {
        if removed.contains(&node) {
            continue;
        }
        removed.insert(node.clone());

        let neighbours = graph
            .out_edges(&node)
            .iter()
            .chain(graph.in_edges(&node).iter());

        for edge in neighbours {
            // `-=` rather than `saturating_sub`, deliberately.
            //
            // The arithmetic is exact: an edge (u,v) appears once in `out_adj[u]`
            // and once in `in_adj[v]`, and `degree` counts both, so removing
            // every neighbour decrements a node exactly to zero and never past
            // it. That holds for self-loops and parallel edges too. Since the
            // subtraction cannot underflow on a well-formed `Subgraph`, letting
            // it panic turns the invariant into an assertion — an `in_adj` that
            // has drifted out of step with `out_adj` fails here loudly instead
            // of being absorbed into a plausible wrong core.
            if let Some(d) = degree.get_mut(edge.node(graph)) {
                *d -= 1;
                if *d < k && !removed.contains(edge.node(graph)) {
                    queue.push_back(edge.node(graph).to_string());
                }
            }
        }
    }

    graph
        .node_ids()
        .filter(|n| !removed.contains(*n))
        .map(str::to_string)
        .collect()
}

/// Newman-Girvan modularity of a partition, treating the graph as undirected.
///
/// Exists so `louvain` can be tested against what it claims to maximise rather
/// than against its own output. A community detector that returns one node per
/// community satisfies "modularity did not decrease from the singleton
/// partition" by being that partition; measuring Q is what tells the two apart.
pub fn modularity(graph: &Subgraph, communities: &BTreeMap<String, usize>) -> f64 {
    let m = graph.total_weight();
    if m == 0.0 {
        return 0.0;
    }

    // Sum of weights of edges inside each community, and of degrees within it.
    let mut internal: BTreeMap<usize, f64> = BTreeMap::new();
    let mut total_deg: BTreeMap<usize, f64> = BTreeMap::new();

    for node in graph.node_ids() {
        let Some(&c) = communities.get(node) else {
            continue;
        };
        *total_deg.entry(c).or_insert(0.0) += graph.weighted_degree(node);

        for edge in graph.out_edges(node) {
            if communities.get(edge.node(graph)) == Some(&c) {
                *internal.entry(c).or_insert(0.0) += edge.weight();
            }
        }
    }

    total_deg
        .iter()
        .map(|(c, deg)| {
            let inside = internal.get(c).copied().unwrap_or(0.0);
            (inside / m) - (deg / (2.0 * m)).powi(2)
        })
        .sum()
}

/// Maximum sweeps before `louvain` gives up moving nodes.
///
/// Greedy modularity ascent terminates in exact arithmetic because every
/// accepted move strictly increases Q. In floating point a move worth `+1e-17`
/// can be undone next sweep by one worth `+1e-17`, and the loop oscillates. The
/// epsilon below makes that rare and this cap makes it bounded.
const LOUVAIN_MAX_SWEEPS: usize = 100;

/// A move must beat this to be taken, so float noise cannot drive a sweep.
const LOUVAIN_MIN_GAIN: f64 = 1e-12;

/// Louvain community detection, local-moving phase (§5.4).
///
/// Returns node id -> community index. Communities are renumbered densely from
/// zero in order of first appearance, so the result is stable and comparable.
///
/// This is phase one of the two-phase Louvain method: nodes are moved greedily
/// to whichever neighbouring community most increases modularity, repeatedly,
/// until no move helps. It does *not* then aggregate each community into a
/// single node and recurse, which is what the full method does to find coarser
/// structure.
///
/// # Why the aggregation phase is absent, and it is not the reason given before
///
/// Through 0.7.0 this note said the aggregation phase *"would matter on graphs
/// far larger than the byte budget admits"*. [D-115] raised what the budget
/// admits by 5.8×–6.8×, so the claim was re-measured against the new ceiling —
/// and it is **false**. `examples/louvain_aggregation_probe.rs` finds two-phase
/// returning a different partition from 6,144 nodes upward, well inside the
/// budget, with the gap widening as the graph grows.
///
/// What the difference *is* settles it. On `clustered` — cliques joined by one
/// bridge each, where the right answer is known — phase-one recovers the ground
/// truth **exactly** at every size up to the ceiling, and two-phase scores a
/// higher Q by **merging whole cliques**: two per community at 512 cliques,
/// four at 4,096, never splitting one. Its Q also exceeds the ground truth's.
/// That is the modularity resolution limit (Fortunato & Barthélemy): on a large
/// graph the objective prefers a partition coarser than the true one, so
/// optimising it harder moves away from the answer rather than towards it.
///
/// So the aggregation phase is declined because at the sizes this crate serves
/// it changes a correct answer into a merged one — not because it would make no
/// difference. `modularity_prefers_a_merged_partition_over_the_true_one_at_scale`
/// pins the fact underneath that without needing a two-phase implementation
/// here: the merged partition outscores the truth, so a Q comparison cannot be
/// the criterion.
///
/// [D-115]: ../../docs/architecture/s13-decision-register.md
pub fn louvain(graph: &Subgraph) -> BTreeMap<String, usize> {
    let m = graph.total_weight();

    // Every node its own community: the only sensible answer with no edges, and
    // the baseline the modularity gain is measured against.
    let mut comm: BTreeMap<String, usize> = graph
        .node_ids()
        .enumerate()
        .map(|(i, n)| (n.to_string(), i))
        .collect();

    if m == 0.0 {
        return comm;
    }

    let mut sigma_tot: BTreeMap<usize, f64> = BTreeMap::new();
    for node in graph.node_ids() {
        *sigma_tot.entry(comm[node]).or_insert(0.0) += graph.weighted_degree(node);
    }

    for _ in 0..LOUVAIN_MAX_SWEEPS {
        let mut moved = false;

        for node in graph.node_ids() {
            let curr_comm = comm[node];
            let k_i = graph.weighted_degree(node);

            // Withdraw the node before scoring, so staying put is scored on the
            // same footing as moving.
            *sigma_tot.get_mut(&curr_comm).unwrap() -= k_i;

            // Weight from this node into each neighbouring community.
            let mut k_i_c: BTreeMap<usize, f64> = BTreeMap::new();
            for edge in graph.out_edges(node).iter().chain(graph.in_edges(node)) {
                if edge.node(graph) == node {
                    continue; // a self-loop joins no community
                }
                *k_i_c.entry(comm[edge.node(graph)]).or_insert(0.0) += edge.weight();
            }

            // dQ = k_i_in/m - (sigma_tot * k_i)/(2m^2), the standard reduced
            // form. Iterating a BTreeMap makes the scan order the community
            // index, so a tie resolves to the lowest index rather than to
            // whatever the hasher seeded this process with.
            let mut best_comm = curr_comm;
            let mut best_gain = LOUVAIN_MIN_GAIN;

            for (&c, k_i_in) in &k_i_c {
                let tot = sigma_tot.get(&c).copied().unwrap_or(0.0);
                let gain = (k_i_in / m) - (tot * k_i / (2.0 * m * m));
                if gain > best_gain {
                    best_gain = gain;
                    best_comm = c;
                }
            }

            *sigma_tot.entry(best_comm).or_insert(0.0) += k_i;

            if best_comm != curr_comm {
                comm.insert(node.to_string(), best_comm);
                moved = true;
            }
        }

        if !moved {
            break;
        }
    }

    renumber(comm)
}

/// Compact community indices to `0..n` in order of first appearance.
fn renumber(comm: BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    let mut dense: BTreeMap<usize, usize> = BTreeMap::new();
    let mut next = 0;
    comm.into_iter()
        .map(|(node, c)| {
            let id = *dense.entry(c).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            });
            (node, id)
        })
        .collect()
}
