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
    /// [`crate::connection::CHUNK_ROWS`] and sends on the low-priority channel,
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
    pub async fn load_subgraph(
        &self,
        start_node: &str,
        max_hops: u32,
        now_ts: &str,
        byte_budget: usize,
    ) -> Result<Subgraph> {
        let conn = self.read_conn();
        let mut graph = Subgraph::default();
        // Running payload total, carried through the load and into `hydrate`.
        // See `estimated_bytes` for why this is not recomputed per row (D-047).
        let mut bytes = 0usize;

        // Topology first: the walk is over links_current, bounded by hop count
        // and by the path check that stops it revisiting a node.
        let sql = r#"
WITH RECURSIVE walk(node_id, depth, path) AS (
    SELECT ?1, 0, CAST(?1 AS BLOB)
    UNION ALL
    SELECT l.target_id, w.depth + 1, w.path || '/' || CAST(l.target_id AS BLOB)
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND INSTR(w.path, CAST(l.target_id AS BLOB)) = 0
)
SELECT DISTINCT l.source_id, l.target_id, l.edge_type, l.weight, l.valid_from, l.valid_to
FROM walk w
JOIN links_current l ON l.source_id = w.node_id
WHERE l.valid_from <= ?3 AND ?3 < l.valid_to
ORDER BY l.source_id, l.target_id, l.edge_type
"#;

        let mut rows = conn
            .query(sql, libsql::params![start_node, max_hops as i64, now_ts])
            .await?;

        while let Some(row) = rows.next().await? {
            let source: String = row.get(0)?;
            let target: String = row.get(1)?;
            let weight: f64 = row.get(3)?;

            // Dijkstra and A* are only correct for non-negative weights, and the
            // schema does not constrain the column. Refusing here keeps the
            // wrongness at the boundary: the alternative is a shortest path that
            // is merely a path, returned with no indication of it.
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
            // Accounted before the move, and doubled: `add_edge` stores the
            // entry in `out_adj` and a clone of it in `in_adj`, so an edge costs
            // two adjacency entries. Counting one is the kind of undercount that
            // makes a budget hold in tests and not in production.
            bytes += 2 * Subgraph::edge_bytes(&edge);
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
        Ok(graph)
    }
}

/// Fill in `nodes` from `concepts` for the ids the topology touched.
/// Attach node attributes, continuing the caller's byte accounting.
///
/// `bytes_so_far` is the topology's payload total; this adds each node as it
/// lands and refuses inside the loop rather than after it. Checking once at the
/// end would allocate the whole oversized result before declining to return it,
/// which is the failure the budget exists to prevent rather than to report.
async fn hydrate(
    conn: &libsql::Connection,
    graph: &mut Subgraph,
    ids: &[String],
    bytes_so_far: usize,
    byte_budget: usize,
) -> Result<()> {
    let mut bytes = bytes_so_far;
    for id in ids {
        let mut rows = conn
            .query(
                "SELECT title, content, embedding_model, valid_from, valid_to \
                 FROM concepts WHERE id = ?1 AND retired = 0",
                libsql::params![id.as_str()],
            )
            .await?;

        if let Some(row) = rows.next().await? {
            let data = NodeData {
                title: row.get(0)?,
                content: row.get(1)?,
                embedding_model: row.get(2).ok(),
                valid_from: row.get(3)?,
                valid_to: row.get(4)?,
            };
            bytes += Subgraph::node_bytes(id, &data);
            graph.nodes.insert(id.clone(), data);

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
