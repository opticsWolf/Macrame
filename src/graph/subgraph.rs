//! The in-memory graph loaded from `links_current`, and its loader (§5.4).
//!
//! A `Subgraph` is derivative state (Doctrine VI): every field is re-derivable
//! from the ledger, nothing here is authoritative, and dropping one loses
//! nothing. That is what lets analytics run on a snapshot without a third clock
//! — the graph is the topology as of one instant, and the instant is the
//! caller's `now_ts`, not a property of the structure.

use std::collections::BTreeMap;

use crate::connection::{Annotation, Database};
use crate::error::{DbError, Result};

/// Edges returned to a caller asking for a node with no edges in that direction.
const NO_EDGES: &[EdgeRef] = &[];

/// A transient, in-memory graph loaded from `links_current`.
///
/// The maps are `BTreeMap`, not `HashMap`, so iteration follows node id order.
/// Every algorithm in [`super::algorithms`] inherits its determinism from that
/// choice, and Louvain in particular returns a different partition under a
/// randomised iteration order.
///
/// # Closure
///
/// **Every id appearing in `out_adj` or `in_adj` — as a key or as an
/// [`EdgeRef::node`] — is a key of `nodes`.** [`Subgraph::drop_dangling_adjacency`]
/// establishes it and [`Subgraph::is_closed`] checks it; every algorithm in
/// [`super::algorithms`] is written assuming it and none of them re-checks.
///
/// It did not hold before Wave 1 (defect Z), and the way it failed is the reason
/// it is now stated on the type rather than left to the loader. Adjacency comes
/// from `links_current`, which carries edges to retired concepts; `hydrate`
/// filters `retired = 0`. So a retired neighbour left an `EdgeRef` pointing at
/// an id with no `NodeData`, and the five algorithms each met that differently:
/// `louvain` panicked on the missing map entry, `scc` emitted the absent node as
/// a phantom component of its own, `k_core` counted a degree of 2 where one edge
/// was in the graph, and `dijkstra` returned a finite distance to a node the
/// caller could not then look up. Four handlings of one violated invariant, none
/// of them chosen — and the panic was the least damaging, because the other
/// three answer.
///
/// Dangling entries are **dropped** rather than admitted with a tombstone node.
/// A retired concept is not visible (§4.1), analytics over a graph is analytics
/// over what is visible, and the alternative pushes a three-state node onto
/// every present and future algorithm to preserve edges whose endpoint the
/// caller is not entitled to read. Retirement is the supported path — concepts
/// are never deleted (D-022) — so this is ordinary use, not a corner.
#[derive(Debug, Clone, Default)]
pub struct Subgraph {
    pub nodes: BTreeMap<String, NodeData>,
    pub out_adj: BTreeMap<String, Vec<EdgeRef>>,
    pub in_adj: BTreeMap<String, Vec<EdgeRef>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeData {
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
    pub valid_from: String,
    pub valid_to: String,
}

/// One end of an edge in an adjacency list.
///
/// `node` is the *other* end: the target in `out_adj`, the source in `in_adj`.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRef {
    pub node: String,
    pub edge_type: String,
    pub weight: f64,
    pub valid_from: String,
    pub valid_to: String,
}

impl Subgraph {
    /// Outgoing edges of `node`, empty when it has none or is absent.
    pub fn out_edges(&self, node: &str) -> &[EdgeRef] {
        self.out_adj.get(node).map_or(NO_EDGES, Vec::as_slice)
    }

    /// Incoming edges of `node`, empty when it has none or is absent.
    pub fn in_edges(&self, node: &str) -> &[EdgeRef] {
        self.in_adj.get(node).map_or(NO_EDGES, Vec::as_slice)
    }

    /// Undirected edge count incident to `node`, counting parallel edges once
    /// each and a self-loop twice.
    pub fn degree(&self, node: &str) -> usize {
        self.out_edges(node).len() + self.in_edges(node).len()
    }

    /// Undirected weight incident to `node`. Summed over both directions, so
    /// summing this over all nodes gives `2 * total_weight`.
    pub fn weighted_degree(&self, node: &str) -> f64 {
        self.out_edges(node).iter().map(|e| e.weight).sum::<f64>()
            + self.in_edges(node).iter().map(|e| e.weight).sum::<f64>()
    }

    /// Total edge weight, each edge counted once — the `m` of the modularity
    /// formulas.
    pub fn total_weight(&self) -> f64 {
        self.out_adj
            .values()
            .flat_map(|edges| edges.iter().map(|e| e.weight))
            .sum()
    }

    pub fn edge_count(&self) -> usize {
        self.out_adj.values().map(Vec::len).sum()
    }

    /// Remove adjacency entries whose endpoint is not a hydrated node.
    ///
    /// This is what establishes the closure invariant on the type's docs, and it
    /// runs after `hydrate` because that is the first moment the set of visible
    /// nodes is known — the walk is over `links_current`, which does not record
    /// retirement.
    ///
    /// A node left with no edges keeps its (now empty) entry only if it had one;
    /// entries emptied by the prune are removed outright, so `out_adj` and
    /// `in_adj` do not accumulate keys for nodes that turned out to have nothing.
    /// Keys that are themselves not hydrated go too, which covers the case where
    /// the *source* is the retired concept rather than the target.
    ///
    /// The byte accounting is deliberately not rewound. `bytes` bounded the load
    /// as it ran and refused early on that basis, so a graph that would have fit
    /// after pruning can still be refused before it. That is conservative in the
    /// safe direction — the budget exists to stop an allocation, and the
    /// allocation happens during the walk, not after it.
    fn drop_dangling_adjacency(&mut self) {
        // Destructured so `nodes` is borrowed separately from the two maps being
        // mutated — the same borrow through `self` inside the closure would not
        // compile.
        let Subgraph {
            nodes,
            out_adj,
            in_adj,
        } = self;

        for adj in [out_adj, in_adj] {
            adj.retain(|id, edges| {
                if !nodes.contains_key(id) {
                    return false;
                }
                edges.retain(|e| nodes.contains_key(&e.node));
                !edges.is_empty()
            });
        }
    }

    /// Whether the closure invariant holds. Used by tests and `debug_assert`s.
    ///
    /// Cheap enough to call in a test and O(V + E), so not on any hot path.
    pub fn is_closed(&self) -> bool {
        self.out_adj
            .iter()
            .chain(self.in_adj.iter())
            .all(|(id, edges)| {
                self.nodes.contains_key(id)
                    && edges.iter().all(|e| self.nodes.contains_key(&e.node))
            })
    }

    /// Record an edge in both directions.
    ///
    /// Both indices are maintained together because every undirected quantity
    /// here — degree, k-core peeling, Louvain's `k_i` — reads them as a pair. An
    /// `in_adj` that lags `out_adj` would not fail loudly; it would return a
    /// plausible wrong number.
    fn add_edge(&mut self, source: String, target: String, edge: EdgeRef) {
        let mut incoming = edge.clone();
        incoming.node = source.clone();
        self.out_adj.entry(source).or_default().push(edge);
        self.in_adj.entry(target).or_default().push(incoming);
    }

    /// Estimated payload bytes for one node, keyed by `id`.
    ///
    /// The per-item functions are the single definition of the estimate.
    /// [`Self::estimated_bytes`] sums them over a whole graph; the loader adds
    /// them as it inserts, so the running total it checks against the budget and
    /// the total a caller can compute are the same arithmetic rather than two
    /// descriptions of it. `load_subgraph_totals_agree_with_the_derivation`
    /// pins that they stay equal.
    fn node_bytes(id: &str, d: &NodeData) -> usize {
        id.len()
            + d.title.len()
            + d.content.len()
            + d.embedding_model.as_ref().map_or(0, String::len)
            + d.valid_from.len()
            + d.valid_to.len()
            + std::mem::size_of::<NodeData>()
    }

    /// Estimated payload bytes for one adjacency entry.
    ///
    /// An edge occupies two of these — one in `out_adj`, one in `in_adj` — so a
    /// caller accounting for a newly added edge counts it twice.
    fn edge_bytes(e: &EdgeRef) -> usize {
        e.node.len()
            + e.edge_type.len()
            + e.valid_from.len()
            + e.valid_to.len()
            + std::mem::size_of::<EdgeRef>()
    }

    /// Estimated heap footprint (D-007).
    ///
    /// Deliberately an estimate of the *payload*, not a precise `size_of` walk:
    /// the budget exists to stop a dense neighbourhood exhausting memory, and a
    /// figure that tracks string bytes and per-item overhead is accurate enough
    /// for that.
    ///
    /// **O(V + E), and therefore not for use inside a loop over rows.** The
    /// loader used to call this per row, which made loading O(E²): 500 edges in
    /// 26 ms, 1,000 in 76 ms, 2,000 in 231 ms — time tripling for each doubling.
    /// The byte budget is what bounds a load, and the budget *check* was the
    /// thing that did not scale (D-047).
    pub fn estimated_bytes(&self) -> usize {
        let nodes: usize = self
            .nodes
            .iter()
            .map(|(id, d)| Self::node_bytes(id, d))
            .sum();
        let edges: usize = self
            .out_adj
            .values()
            .chain(self.in_adj.values())
            .flat_map(|v| v.iter())
            .map(Self::edge_bytes)
            .sum();
        nodes + edges
    }

    /// Write one derived result per node under `label` (§5.4, D-041).
    ///
    /// Goes through [`Database::write_analytics_annotations`], which chunks at
    /// [`crate::connection::chunk_rows::ANNOTATIONS`] and sends on the
    /// low-priority channel,
    /// so a community assignment over a large subgraph cannot starve interactive
    /// writes.
    ///
    /// Rows land in `analytics_annotations`, which carries no log trigger.
    /// Before 0.5.4 this method built a `ConceptUpsert` per node and put the
    /// value in `content`, so writing back a partition **overwrote every
    /// annotated concept's document text** — and, because the write went through
    /// the ledger, recorded each rerun of the algorithm as a fresh version of a
    /// world that had not changed. The old doc comment defended that as "a
    /// normal bitemporal write," which was true of the mechanism and false of
    /// the intent: it is the right mechanism for a domain fact, and a community
    /// label is not one.
    ///
    /// `values` is keyed by node id; nodes absent from it are not annotated.
    pub async fn write_back_annotations(
        &self,
        db: &Database,
        label: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<usize> {
        let rows: Vec<Annotation> = self
            .nodes
            .keys()
            .filter_map(|id| {
                values
                    .get(id)
                    .map(|value| Annotation::new(id.clone(), label, value.clone()))
            })
            .collect();

        db.write_analytics_annotations(rows).await
    }
}

impl Database {
    /// Load the topology reachable from `start_node` within `max_hops` (§5.4).
    ///
    /// Runs on the read connection, so it cannot contend with the write actor.
    /// `byte_budget` bounds the result: a hub node in a dense graph can reach
    /// most of the database in three hops, and the budget is what turns that
    /// into [`DbError::SubgraphTooLarge`] rather than into an allocation
    /// failure.
    ///
    /// Unfiltered: every edge type, **every weight**. See
    /// [`Self::load_subgraph_with`] for the filtered form, which this delegates
    /// to.
    ///
    /// `min_weight` is `NEG_INFINITY` rather than [`TraversalBuilder`]'s default
    /// of `0.0`, and the difference is load-bearing. A floor of `0.0` silently
    /// drops negative-weight edges — which is precisely the input
    /// [`DbError::NegativeEdgeWeight`] exists to *report*, since Dijkstra and A*
    /// are unsound over them and D-039 chose to refuse at the boundary rather
    /// than return a shortest path that is merely a path. Delegating with the
    /// builder default turned that typed refusal into a graph quietly missing
    /// edges; `a_negative_edge_weight_is_refused_at_load` caught it.
    ///
    /// So the two mechanisms are made to agree instead of overlapping: an edge a
    /// caller has **not** filtered out reaches the weight guard, and an edge they
    /// have is theirs to exclude. See [`Self::load_subgraph_with`] for what that
    /// means when a caller passes a default builder.
    pub async fn load_subgraph(
        &self,
        start_node: &str,
        max_hops: u32,
        now_ts: &str,
        byte_budget: usize,
    ) -> Result<Subgraph> {
        self.load_subgraph_with(
            &super::TraversalBuilder::new(start_node)
                .max_depth(max_hops as usize)
                .min_weight(f64::NEG_INFINITY),
            now_ts,
            byte_budget,
        )
        .await
    }

    /// Load the topology a [`TraversalBuilder`] describes, as a [`Subgraph`]
    /// (§5.4, D-073).
    ///
    /// `load_subgraph` took neither `edge_types` nor `min_weight` while
    /// `TraversalBuilder` took both — the same walk over the same table with two
    /// fewer knobs. That was a **reachability** limit rather than a convenience
    /// one: the byte budget bounds the *unfiltered* neighbourhood, so a caller
    /// wanting one edge type out of a hub got [`DbError::SubgraphTooLarge`] for a
    /// graph whose filtered form would have fitted easily, and filtering the
    /// returned `Subgraph` afterwards cannot help because the refusal happens
    /// during the walk.
    ///
    /// # The filters apply to the walk *and* to the returned edges
    ///
    /// This is the decision the change turned on, and the two are separable.
    /// `TraversalBuilder` applies its filters to the **recursive step** — which
    /// edges are followed — while this loader's final projection returns every
    /// edge of every node it reached. Wiring the two together naively gives a
    /// caller who asked for `CITES` a graph reached via `CITES` and populated
    /// with `KNOWS` edges as well, which is surprising enough to be read as a
    /// bug.
    ///
    /// So both halves filter. If a caller names edge types or a minimum weight,
    /// they are asking for a subgraph **of those edges**: the walk uses them to
    /// bound which nodes are reached, and the projection uses them to decide
    /// which adjacency lands in the result. `load_subgraph` passes a default
    /// builder — no types, weight ≥ 0 — so its behaviour is unchanged.
    ///
    /// # `min_weight` and the negative-weight guard
    ///
    /// [`TraversalBuilder`] defaults `min_weight` to `0.0`, so a **default
    /// builder passed here filters negative-weight edges out** rather than
    /// letting them reach [`DbError::NegativeEdgeWeight`]. That is a real
    /// difference from [`Self::load_subgraph`], which passes `NEG_INFINITY`.
    ///
    /// It is deliberate and it is the coherent reading: a caller who states a
    /// weight floor has asked to exclude what falls below it, and excluding it
    /// is not an error. A caller who states none should be told, because
    /// Dijkstra and A* are unsound over negative weights. Pass
    /// `.min_weight(f64::NEG_INFINITY)` to get the guard with a filtered builder.
    ///
    /// `attribute_mode` is ignored: hydration here is always the live concept
    /// row, which is what a `Subgraph` has always carried.
    pub async fn load_subgraph_with(
        &self,
        traversal: &super::TraversalBuilder,
        now_ts: &str,
        byte_budget: usize,
    ) -> Result<Subgraph> {
        let start_node = traversal.start_node.as_str();
        let max_hops = traversal.max_depth as u32;
        let conn = self.read_conn();
        let mut graph = Subgraph::default();
        // Running payload total, carried through the load and into `hydrate`.
        // See `estimated_bytes` for why this is not recomputed per row (D-047).
        let mut bytes = 0usize;

        // `?1..?4` are start, depth, ts and min_weight; edge types take `?5`
        // onwards. Bound, never spliced — an edge type is a value, and the only
        // validation in the crate runs on the *write* path (D-039), so a
        // traversal never passes through it.
        let edge_filter = traversal.edge_filter_sql();

        // Topology first. The recursion itself is `TraversalBuilder::walk_cte`
        // and is **not** duplicated here (T0.1): this file and `builder.rs` held
        // byte-identical copies, and they had already drifted once — D-073 found
        // this loader taking neither `edge_types` nor `min_weight` while the
        // builder took both.
        let sql = format!(
            "{}{}",
            traversal.walk_cte(),
            format_args!(
                r#"
-- **The `DISTINCT` is why this query is superlinear, and it is not removable.**
--
-- Wave 3 measured `load_subgraph` at 12.5x for 10x the nodes and could not say
-- why; Wave 4 answered it from the plan. `EXPLAIN` reports
-- `USE TEMP B-TREE FOR DISTINCT`: an O(E log E) sort over the output, and
-- n log n predicts ~13.3x for 10x, against the 12.5x measured. That is the term.
--
-- It is load-bearing: two branches can reach the same node, so a node appears in
-- `walk` at more than one depth and the join would otherwise emit its edges once
-- per depth. Without `DISTINCT` a caller gets duplicate edges.
--
-- **Corrected in 0.6.0 (T0.1), and the correction is not that the analysis was
-- wrong.** Everything above holds, and D-070's two rejected fixes were measured
-- honestly. What was wrong was the fixture: `benches/` seeds a chain of stars,
-- which is a *tree*, and in a tree there is exactly one path to each node — so
-- the term that actually dominated was identically 1 and invisible. D-070
-- concluded the growth was "inherent to producing a deduplicated result", which
-- is true of trees and false of graphs. The real cost was the walk enumerating
-- **paths** rather than nodes; see `walk_cte`. On a 328-edge layered graph at
-- depth 6 that was 299,593 walk rows and 428 ms, against 49 rows and 0.1 ms now.
-- The `DISTINCT` stays, and it is no longer the leading term.
--
-- The filters appear **twice**, and that is the contract (D-073). The walk uses
-- them to bound which nodes are reached; the projection uses them to decide
-- which adjacency lands in the result. Filtering only the walk would hand a
-- caller who asked for `CITES` a graph reached via `CITES` and populated with
-- every other edge type those nodes happen to have.
SELECT DISTINCT l.source_id, l.target_id, l.edge_type, l.weight, l.valid_from, l.valid_to
FROM walk w
JOIN links_current l ON l.source_id = w.node_id
WHERE l.valid_from <= ?3 AND ?3 < l.valid_to
  AND l.weight >= ?4
  {edge_filter}
ORDER BY l.source_id, l.target_id, l.edge_type
"#
            )
        );

        let mut params: Vec<libsql::Value> = vec![
            start_node.into(),
            (max_hops as i64).into(),
            now_ts.into(),
            traversal.min_weight.into(),
        ];
        params.extend(traversal.edge_types.iter().map(|t| t.as_str().into()));

        let mut rows = conn.query(&sql, params).await?;

        while let Some(row) = rows.next().await? {
            let source: String = row.get(0)?;
            let target: String = row.get(1)?;
            let weight: f64 = row.get(3)?;

            // Dijkstra and A* are only correct for non-negative weights, and the
            // schema does not constrain the column. Refusing here keeps the
            // wrongness at the boundary: the alternative is a shortest path that
            // is merely a path, returned with no indication of it.
            //
            // **The `is_nan()` arm is unreachable on a file this schema created
            // (T0.3, D-078).** SQLite stores a NaN double as NULL, so
            // `weight REAL NOT NULL` refuses it — measured on libSQL 0.9.30
            // through `assert_edge`, through a raw `INSERT` binding NaN, and
            // through a raw `INSERT` computing `0.0/0.0` in the engine; all three
            // fail with `NOT NULL constraint failed`. §4.7 used to list NaN as a
            // gap this loader covered, which had it backwards.
            //
            // Kept anyway, as defence rather than decoration: a future engine
            // that stores NaN as a real double would make it live again, and the
            // cost of a comparison per edge against reading a shortest path
            // computed over NaN is not a close call. `storage_boundary_tests`
            // pins the engine's current behaviour, so that change would arrive
            // as a failing test rather than as a silent answer.
            if weight < 0.0 || weight.is_nan() {
                return Err(DbError::NegativeEdgeWeight {
                    source_id: source,
                    target_id: target,
                    weight,
                });
            }

            let edge = EdgeRef {
                node: target.clone(),
                edge_type: row.get(2)?,
                weight,
                valid_from: row.get(4)?,
                valid_to: row.get(5)?,
            };
            // Accounted before the move, and *not* simply doubled.
            //
            // `add_edge` stores this entry in `out_adj` and a clone in `in_adj`
            // with `node` rewritten to the **source**, so the two differ by
            // exactly the two id lengths — `edge_bytes` sums `e.node.len()`.
            // `2 * edge_bytes(&edge)` counted the target twice and the source
            // never, so the running total drifted from `estimated_bytes()` by
            // `target.len() - source.len()` per edge, in whichever direction the
            // ids happened to differ.
            //
            // A byte an edge, and it made the loader refuse a graph sized at its
            // own `estimated_bytes()` — found by `a_filtered_load_fits_a_budget_
            // the_unfiltered_one_exceeds`, and invisible to
            // `load_subgraph_totals_agree_with_the_derivation` because that one
            // asserts refusal at *half* the budget, where a one-byte-per-edge
            // drift cannot show. The doc on `node_bytes` claims the loader's
            // arithmetic and the caller's are the same; now they are.
            let outgoing = Subgraph::edge_bytes(&edge);
            let incoming = outgoing - target.len() + source.len();
            bytes += outgoing + incoming;
            graph.add_edge(source, target, edge);

            if bytes > byte_budget {
                return Err(DbError::SubgraphTooLarge {
                    n: bytes,
                    budget: byte_budget,
                });
            }
        }

        // Every endpoint is a node, plus the start itself so a lone node still
        // loads as a one-node graph rather than an empty one.
        let mut ids: Vec<String> = graph
            .out_adj
            .keys()
            .chain(graph.in_adj.keys())
            .cloned()
            .collect();
        ids.push(start_node.to_string());
        ids.sort();
        ids.dedup();

        hydrate(conn, &mut graph, &ids, bytes, byte_budget).await?;
        graph.drop_dangling_adjacency();
        Ok(graph)
    }
}

use crate::util::limits::HYDRATE_CHUNK;

/// Fill in `nodes` from `concepts` for the ids the topology touched.
/// Attach node attributes, continuing the caller's byte accounting.
///
/// `bytes_so_far` is the topology's payload total; this adds each node as it
/// lands and refuses as soon as the running total passes the budget rather than
/// after the whole set is in hand. Checking once at the end would allocate the
/// whole oversized result before declining to return it, which is the failure
/// the budget exists to prevent rather than to report.
///
/// **One query per [`HYDRATE_CHUNK`] ids, not one per node (defect AE).** The
/// previous version issued a round trip per id: 400 nodes cost 400 of them and
/// 13.2 ms, essentially all of it latency rather than work, and linear in node
/// count on a path whose whole purpose is to bound the result by *bytes*.
async fn hydrate(
    conn: &libsql::Connection,
    graph: &mut Subgraph,
    ids: &[String],
    bytes_so_far: usize,
    byte_budget: usize,
) -> Result<()> {
    let mut bytes = bytes_so_far;

    for chunk in ids.chunks(HYDRATE_CHUNK) {
        // Only the placeholders are built; the ids themselves are bound.
        let list = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, title, content, embedding_model, valid_from, valid_to \
             FROM concepts WHERE retired = 0 AND id IN ({list})"
        );
        let params: Vec<libsql::Value> = chunk
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            let data = NodeData {
                title: row.get(1)?,
                content: row.get(2)?,
                embedding_model: row.get(3).ok(),
                valid_from: row.get(4)?,
                valid_to: row.get(5)?,
            };
            bytes += Subgraph::node_bytes(&id, &data);
            graph.nodes.insert(id, data);

            if bytes > byte_budget {
                return Err(DbError::SubgraphTooLarge {
                    n: bytes,
                    budget: byte_budget,
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(node: &str, weight: f64) -> EdgeRef {
        EdgeRef {
            node: node.to_string(),
            edge_type: "KNOWS".to_string(),
            weight,
            valid_from: "2026-01-01T00:00:00.000000Z".to_string(),
            valid_to: "9999-12-31T23:59:59.999999Z".to_string(),
        }
    }

    #[test]
    fn adding_an_edge_indexes_it_in_both_directions() {
        let mut g = Subgraph::default();
        g.add_edge("A".into(), "B".into(), edge("B", 0.5));

        assert_eq!(g.out_edges("A").len(), 1);
        assert_eq!(g.out_edges("A")[0].node, "B");
        assert_eq!(g.in_edges("B").len(), 1);
        assert_eq!(g.in_edges("B")[0].node, "A", "in_adj holds the source");

        // The undirected view has to agree with itself: total degree is twice
        // the edge weight total, which is the identity every undirected
        // quantity in `algorithms` is derived from.
        assert_eq!(g.degree("A") + g.degree("B"), 2);
        assert_eq!(g.weighted_degree("A") + g.weighted_degree("B"), 1.0);
        assert_eq!(g.total_weight(), 0.5);
    }

    #[test]
    fn a_missing_node_has_no_edges_rather_than_panicking() {
        let g = Subgraph::default();
        assert!(g.out_edges("nobody").is_empty());
        assert!(g.in_edges("nobody").is_empty());
        assert_eq!(g.degree("nobody"), 0);
        assert_eq!(g.weighted_degree("nobody"), 0.0);
    }
}
