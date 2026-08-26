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
//! **Since 0.13.28 these run on integers, and that changes none of it**
//! ([D-201](../../docs/architecture/s13-decision-register.md#d-201)). Each
//! function but `astar` builds a `Dense` view of the graph — flat CSR over
//! `u32` — and translates back at the return. Dense indices are assigned in
//! `Subgraph::node_ids` order, so `u < v` **iff** `ids[u] < ids[v]`: every tie
//! listed above is broken by index instead of by string and lands on the same
//! answer. `the_interior_may_change_but_these_answers_may_not` pins that,
//! `scc`'s component order included.
//!
//! **`astar` is the exception, and since 0.13.29 it is a stated one**
//! ([D-202](../../docs/architecture/s13-decision-register.md#d-202)). It is the
//! only function here that can return before it has seen the graph, so a
//! precompute proportional to the graph is not amortised by it — it *replaces*
//! the early exit. It runs on the `String`-keyed maps, and D-202 is the
//! measurement that says why.
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

use super::dense::Dense;
use super::subgraph::Subgraph;

/// The message every entry assert carries.
///
/// [`Subgraph`]'s type-level docs state the closure invariant and say that
/// "every algorithm in [`super::algorithms`] is written assuming it and none of
/// them re-checks". [`Subgraph::is_closed`]'s own rustdoc has claimed since
/// 0.6.0 that it is "used by tests and `debug_assert`s" — and no `debug_assert`
/// existed anywhere in `src/`.
///
/// 0.10.0 (W4.8) writes them rather than softening the sentence. A live
/// assumption that no assertion covers is one refactor away from being a silent
/// wrong answer instead of a panic, and the invariant has failed once already
/// (defect Z, Wave 1: a retired neighbour left an `EdgeRef` pointing at a node
/// the loader had filtered out). `is_closed` is O(V + E) and these are
/// `debug_assert`s, so release builds pay nothing.
const CLOSURE: &str = "Subgraph closure invariant violated on entry: adjacency \
                       references a node that is not in `nodes`. Every algorithm \
                       here assumes closure and none re-checks it — see \
                       `Subgraph`'s type docs and `drop_dangling_adjacency`.";

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
    debug_assert!(graph.is_closed(), "{CLOSURE}");
    let g = Dense::of(graph);
    let Some(source) = g.index_of(start) else {
        return BTreeMap::new();
    };

    let mut dist = vec![f64::INFINITY; g.len()];
    let mut heap = BinaryHeap::new();

    dist[source] = 0.0;
    heap.push(Reverse((OrdF64(0.0), source)));

    while let Some(Reverse((OrdF64(d), node))) = heap.pop() {
        // A stale entry: this node was reached again more cheaply after this
        // entry was pushed. Settle it once, at its best distance.
        if d > dist[node] {
            continue;
        }

        for &(next, weight) in g.out(node) {
            let next = next as usize;
            let new_dist = d + weight;

            if new_dist < dist[next] {
                dist[next] = new_dist;
                heap.push(Reverse((OrdF64(new_dist), next)));
            }
        }
    }

    g.label_finite(&dist)
}

/// A* search from `start` to `goal` (§5.4).
///
/// Returns the total cost and the full path inclusive of both endpoints, or
/// `None` when `goal` is unreachable. `heuristic` must be admissible — it must
/// never overestimate the remaining cost — or the path returned is a path but
/// not necessarily the shortest one.
///
/// # This is the one algorithm here that is *not* on the dense view
///
/// Every other function in this module settles every node, so the O(V + E)
/// cost of building the integer view is charged against work that is O(V + E)
/// anyway. `astar` returns the moment the goal is popped, and that is the
/// entire reason to call it rather than [`dijkstra`]. Building a whole-graph
/// index first makes its cost **independent of how far the goal is** — which
/// is not a constant factor, it is the early exit itself.
///
/// 0.13.28 put it on the dense view without measuring it. [D-202] measured it:
/// on the 49,152-node fixture a one-hop goal cost **16.3 ms** on the dense view
/// and **0.019 ms** here, settling six nodes either way. Distant goals go the
/// other way — the dense view finishes them in about a fifth of the time — but
/// a goal that is the whole graph away is a [`dijkstra`] call written as an
/// `astar`, and the cost of serving it well is charging every near query for a
/// graph it never looks at.
///
/// `a_near_goal_does_not_pay_for_the_whole_graph` holds this, by comparing
/// against [`dijkstra`] on the same graph in the same test rather than against
/// a wall-clock threshold.
///
/// [D-202]: ../../docs/architecture/s13-decision-register.md#d-202
pub fn astar<F>(
    graph: &Subgraph,
    start: &str,
    goal: &str,
    heuristic: F,
) -> Option<(f64, Vec<String>)>
where
    F: Fn(&str, &str) -> f64,
{
    debug_assert!(graph.is_closed(), "{CLOSURE}");
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
    debug_assert!(graph.is_closed(), "{CLOSURE}");
    let g = Dense::of(graph);
    let mut visited = vec![false; g.len()];
    let mut order: Vec<usize> = Vec::with_capacity(g.len());

    // Pass 1: post-order finish times on the graph as given.
    for node in 0..g.len() {
        if visited[node] {
            continue;
        }
        let mut stack = vec![(node, false)];
        while let Some((curr, exhausted)) = stack.pop() {
            if exhausted {
                order.push(curr);
                continue;
            }
            if visited[curr] {
                continue;
            }
            visited[curr] = true;
            // Re-pushed beneath its children, so it finishes after them.
            stack.push((curr, true));

            for &(next, _) in g.out(curr) {
                if !visited[next as usize] {
                    stack.push((next as usize, false));
                }
            }
        }
    }

    // Pass 2: the transpose, in decreasing finish time.
    visited.iter_mut().for_each(|v| *v = false);
    let mut components: Vec<Vec<usize>> = Vec::new();

    for node in order.into_iter().rev() {
        if visited[node] {
            continue;
        }
        let mut comp = Vec::new();
        let mut stack = vec![node];

        while let Some(curr) = stack.pop() {
            if visited[curr] {
                continue;
            }
            visited[curr] = true;
            comp.push(curr);

            for &(prev, _) in g.inn(curr) {
                if !visited[prev as usize] {
                    stack.push(prev as usize);
                }
            }
        }
        comp.sort_unstable();
        components.push(comp);
    }

    // Index order is id order, so sorting indices is sorting ids — the
    // canonical form the doc above promises, reached without comparing strings.
    components.sort_unstable();
    components
        .into_iter()
        .map(|comp| comp.into_iter().map(|u| g.id(u).to_string()).collect())
        .collect()
}

/// k-core decomposition: the maximal induced subgraph in which every node has
/// degree at least `k` (§5.4).
///
/// Treats the graph as undirected, summing in- and out-degree. Parallel edges
/// count once each — a node held in by three edges to one neighbour has degree
/// three, which is what makes this a multigraph core.
pub fn k_core(graph: &Subgraph, k: usize) -> BTreeSet<String> {
    debug_assert!(graph.is_closed(), "{CLOSURE}");
    let g = Dense::of(graph);
    let mut degree: Vec<usize> = (0..g.len()).map(|u| g.degree(u)).collect();

    let mut queue: VecDeque<usize> = (0..g.len()).filter(|&u| degree[u] < k).collect();

    let mut removed = vec![false; g.len()];

    while let Some(node) = queue.pop_front() {
        if removed[node] {
            continue;
        }
        removed[node] = true;

        let neighbours = g.out(node).iter().chain(g.inn(node).iter());

        for &(other, _) in neighbours {
            let other = other as usize;
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
            degree[other] -= 1;
            if degree[other] < k && !removed[other] {
                queue.push_back(other);
            }
        }
    }

    (0..g.len())
        .filter(|&u| !removed[u])
        .map(|u| g.id(u).to_string())
        .collect()
}

/// Newman-Girvan modularity of a partition, treating the graph as undirected.
///
/// Exists so `louvain` can be tested against what it claims to maximise rather
/// than against its own output. A community detector that returns one node per
/// community satisfies "modularity did not decrease from the singleton
/// partition" by being that partition; measuring Q is what tells the two apart.
pub fn modularity(graph: &Subgraph, communities: &BTreeMap<String, usize>) -> f64 {
    let g = Dense::of(graph);
    let m = g.total_weight();
    if m == 0.0 {
        return 0.0;
    }

    // The caller's partition, resolved once per node rather than once per edge.
    let comm: Vec<Option<usize>> = g
        .ids()
        .iter()
        .map(|id| communities.get(*id).copied())
        .collect();
    let weighted = g.weighted_degrees();

    // Sum of weights of edges inside each community, and of degrees within it.
    let mut internal: BTreeMap<usize, f64> = BTreeMap::new();
    let mut total_deg: BTreeMap<usize, f64> = BTreeMap::new();

    for node in 0..g.len() {
        let Some(c) = comm[node] else {
            continue;
        };
        *total_deg.entry(c).or_insert(0.0) += weighted[node];

        for &(other, weight) in g.out(node) {
            if comm[other as usize] == Some(c) {
                *internal.entry(c).or_insert(0.0) += weight;
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
    debug_assert!(graph.is_closed(), "{CLOSURE}");
    let g = Dense::of(graph);
    let m = g.total_weight();

    // Every node its own community: the only sensible answer with no edges, and
    // the baseline the modularity gain is measured against.
    let mut comm: Vec<usize> = (0..g.len()).collect();

    if m == 0.0 {
        return g.label(&comm);
    }

    let weighted = g.weighted_degrees();
    let mut sigma_tot: BTreeMap<usize, f64> = BTreeMap::new();
    for (node, &c) in comm.iter().enumerate() {
        *sigma_tot.entry(c).or_insert(0.0) += weighted[node];
    }

    for _ in 0..LOUVAIN_MAX_SWEEPS {
        let mut moved = false;

        for node in 0..g.len() {
            let curr_comm = comm[node];
            let k_i = weighted[node];

            // Withdraw the node before scoring, so staying put is scored on the
            // same footing as moving.
            *sigma_tot.get_mut(&curr_comm).unwrap() -= k_i;

            // Weight from this node into each neighbouring community.
            let mut k_i_c: BTreeMap<usize, f64> = BTreeMap::new();
            for &(other, weight) in g.out(node).iter().chain(g.inn(node)) {
                let other = other as usize;
                if other == node {
                    continue; // a self-loop joins no community
                }
                *k_i_c.entry(comm[other]).or_insert(0.0) += weight;
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
                comm[node] = best_comm;
                moved = true;
            }
        }

        if !moved {
            break;
        }
    }

    g.label(&renumber(&comm))
}

/// Compact community indices to `0..n` in order of first appearance.
fn renumber(comm: &[usize]) -> Vec<usize> {
    let mut dense: BTreeMap<usize, usize> = BTreeMap::new();
    let mut next = 0;
    comm.iter()
        .map(|&c| {
            *dense.entry(c).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            })
        })
        .collect()
}
