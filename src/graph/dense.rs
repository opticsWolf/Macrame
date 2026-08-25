//! The integer-indexed view the algorithms actually run on (0.13.28, W10.3b,
//! [D-201]).
//!
//! [`Subgraph`] is keyed by `String` and stays that way: the maps, their order
//! and the `BTreeMap<String, _>` return types are the public surface, and
//! [D-063] makes node order a structural property rather than a sort applied at
//! the end. What changes here is what happens *between* those two boundaries.
//!
//! # Why this is in the crate and not in the caller
//!
//! §2.5 of the 0.12.0 review proposed exactly this — "build a dense
//! `Vec`-indexed view once, run on integers, and translate back at the
//! boundary" — and [D-200] measured it built at the boundary, where it is
//! 1.8×–2.1× on `louvain` and **a loss** on `dijkstra`. The reason is the
//! build: from outside, mapping an edge's far end to an index costs a
//! `BTreeMap<&str, _>` lookup **per edge endpoint**, which is the very cost
//! being removed, paid once for the graph instead of once per sweep. One pass
//! never earns that back.
//!
//! In here it is not that. [D-115] already interned every string an
//! [`EdgeRef`](super::EdgeRef) carries, so an edge's far end is *already* a
//! `u32` — and mapping the pool onto dense indices costs one string lookup
//! **per node**, not per endpoint. The build is therefore O(V log V) in string
//! comparisons and O(V + E) in integer work, and the per-edge term has no
//! strings in it at all.
//!
//! # Why CSR and not `Vec<Vec<_>>`
//!
//! The first version of this was a `Vec<Vec<(u32, f64)>>` per direction, which
//! is the obvious shape and was measured to be the wrong one: two heap
//! allocations per node, 98,304 of them at the byte budget's ceiling, and the
//! build became the dominant term for every algorithm that makes a single pass.
//! `k_core` came out **1.8× slower than the string-keyed version it replaced**
//! — its own work is one degree count per node, so it was paying an O(V)
//! allocation storm to avoid an O(V log V) lookup.
//!
//! Flat arrays with an offset table fix that: two allocations per direction for
//! the whole graph. §2.5 said "CSR adjacency" and meant it.
//!
//! # What it does not change
//!
//! Dense indices are assigned in [`Subgraph::node_ids`] order, which is
//! `nodes`' `BTreeMap` order. So `u < v` **iff** `ids[u] < ids[v]`, and every
//! tie the algorithms break by node id can be broken by index instead without
//! moving an answer: heap entries at equal distance, `scc`'s sorted components
//! and their sorted order, Louvain's lowest-community-index rule. That
//! equivalence is why the rewrite is behaviour-preserving by construction
//! rather than only by test — and
//! `the_interior_may_change_but_these_answers_may_not` is the test that holds
//! it anyway.
//!
//! [D-063]: ../../docs/architecture/s13-decision-register.md#d-063
//! [D-115]: ../../docs/architecture/s13-decision-register.md#d-115
//! [D-200]: ../../docs/architecture/s13-decision-register.md#d-200
//! [D-201]: ../../docs/architecture/s13-decision-register.md#d-201

use std::collections::BTreeMap;

use super::subgraph::Subgraph;

/// A dense, integer-indexed view of one [`Subgraph`], borrowed from it.
///
/// Built per call rather than cached on the graph. A cache would have to be
/// invalidated by [`Subgraph::add_edge`] and [`Subgraph::insert_node`], which
/// are public — and, more decisively, it would roughly double the retained
/// footprint of the one structure in this crate with an explicit **byte
/// budget** ([D-007], [D-047]). Bounding that footprint is what
/// `load_subgraph` refuses loads to protect; silently doubling it to save a
/// build measured in tens of milliseconds is the wrong side of that trade.
///
/// [D-007]: ../../docs/architecture/s13-decision-register.md#d-007
/// [D-047]: ../../docs/architecture/s13-decision-register.md#d-047
pub(crate) struct Dense<'g> {
    /// `ids[u]` is dense node `u`'s id, in [`Subgraph::node_ids`] order — so
    /// this is sorted, and `binary_search` is the id lookup.
    ids: Vec<&'g str>,
    /// Outgoing edges, flat. `out[out_at[u]..out_at[u + 1]]` is `u`'s, in the
    /// order [`Subgraph::out_edges`] returns them.
    out: Vec<(u32, f64)>,
    /// `out_at` has `len() + 1` entries; the last is `out.len()`.
    out_at: Vec<u32>,
    /// Incoming edges, flat, as [`Subgraph::in_edges`] returns them.
    inn: Vec<(u32, f64)>,
    inn_at: Vec<u32>,
}

/// `pool_to_dense` entry for a pooled string that is not a hydrated node id —
/// an edge type, a timestamp, or a node the loader filtered out.
const NOT_A_NODE: u32 = u32::MAX;

impl<'g> Dense<'g> {
    /// Build the view for `graph`.
    pub(crate) fn of(graph: &'g Subgraph) -> Self {
        graph.build_dense()
    }

    /// Assemble from the parts only [`Subgraph`] can see.
    pub(crate) fn from_parts(
        ids: Vec<&'g str>,
        out: Vec<(u32, f64)>,
        out_at: Vec<u32>,
        inn: Vec<(u32, f64)>,
        inn_at: Vec<u32>,
    ) -> Self {
        debug_assert_eq!(out_at.len(), ids.len() + 1);
        debug_assert_eq!(inn_at.len(), ids.len() + 1);
        Self {
            ids,
            out,
            out_at,
            inn,
            inn_at,
        }
    }

    /// The sentinel [`Subgraph::build_dense`] fills unmapped pool slots with.
    pub(crate) const fn not_a_node() -> u32 {
        NOT_A_NODE
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    pub(crate) fn id(&self, u: usize) -> &'g str {
        self.ids[u]
    }

    pub(crate) fn ids(&self) -> &[&'g str] {
        &self.ids
    }

    pub(crate) fn out(&self, u: usize) -> &[(u32, f64)] {
        &self.out[self.out_at[u] as usize..self.out_at[u + 1] as usize]
    }

    pub(crate) fn inn(&self, u: usize) -> &[(u32, f64)] {
        &self.inn[self.inn_at[u] as usize..self.inn_at[u + 1] as usize]
    }

    /// Undirected edge count incident to `u`, as [`Subgraph::degree`] counts it.
    pub(crate) fn degree(&self, u: usize) -> usize {
        (self.out_at[u + 1] - self.out_at[u]) as usize
            + (self.inn_at[u + 1] - self.inn_at[u]) as usize
    }

    /// The dense index of `id`, or `None` when it is not a node of this graph.
    ///
    /// `ids` is sorted because it came from a `BTreeMap`'s keys, so this is a
    /// binary search — O(log V) string comparisons, once per call rather than
    /// once per edge.
    pub(crate) fn index_of(&self, id: &str) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }

    /// [`Subgraph::weighted_degree`] for every node, in one pass.
    pub(crate) fn weighted_degrees(&self) -> Vec<f64> {
        (0..self.len())
            .map(|u| {
                self.out(u).iter().map(|(_, w)| w).sum::<f64>()
                    + self.inn(u).iter().map(|(_, w)| w).sum::<f64>()
            })
            .collect()
    }

    /// [`Subgraph::total_weight`]: every stored edge counted once.
    pub(crate) fn total_weight(&self) -> f64 {
        self.out.iter().map(|(_, w)| w).sum()
    }

    /// Attach ids to a per-node array, keeping only the finite entries.
    ///
    /// The distance algorithms use `INFINITY` for "not reached" where the
    /// public return omits the node entirely.
    pub(crate) fn label_finite(&self, values: &[f64]) -> BTreeMap<String, f64> {
        self.ids
            .iter()
            .zip(values)
            .filter(|(_, v)| v.is_finite())
            .map(|(id, v)| ((*id).to_string(), *v))
            .collect()
    }

    /// Attach ids to a per-node array, keeping every entry.
    pub(crate) fn label<T: Copy>(&self, values: &[T]) -> BTreeMap<String, T> {
        self.ids
            .iter()
            .zip(values)
            .map(|(id, v)| ((*id).to_string(), *v))
            .collect()
    }
}
