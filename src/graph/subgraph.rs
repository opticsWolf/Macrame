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
///
/// # Why the fields are private (0.8.0, B1, D-114)
///
/// They were `pub` through 0.7.0, and the three maps were the crate's most
/// widely read data structure. That made **every detail of the representation
/// part of the public API** — the `BTreeMap`, the `String` keys, the fact that
/// adjacency is stored as two maps at all — none of which was ever a promise
/// anyone intended to make.
///
/// The immediate reason is D-087: interning the keys to `u32` cannot be done
/// at all while `EdgeRef::node` is a public `String`. The break is taken **once**, here, with the representation
/// unchanged, so that anything depending on the old shape fails against code
/// that still behaves identically.
///
/// Accessors return borrowed views, so nothing here costs an allocation that
/// field access did not.
#[derive(Debug, Clone, Default)]
pub struct Subgraph {
    nodes: BTreeMap<String, NodeData>,
    out_adj: BTreeMap<String, Vec<EdgeRef>>,
    in_adj: BTreeMap<String, Vec<EdgeRef>>,
    /// Every string an `EdgeRef` carries. See [`Interner`].
    pool: Interner,
}

/// The attributes of one node, as of the instant the graph was loaded.
///
/// Fields are private for the reason given on [`Subgraph`]; `content` is the
/// one whose type is expected to move.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeData {
    title: String,
    /// **`None` means "not loaded", not "empty" (0.8.0, B3, D-116).**
    ///
    /// Document text is not loaded unless a caller asks. No algorithm reads it
    /// — `dijkstra`, `astar`, `scc`, `k_core`, `louvain` and `modularity` touch
    /// topology and weight only — and at realistic document sizes it is most of
    /// the byte budget, so the default load spent the budget on bytes nothing
    /// would look at.
    ///
    /// An `Option` rather than an empty `String` because a sentinel that is a
    /// *valid value of the type* cannot be told apart from the real thing: a
    /// concept with genuinely empty content and one whose content was not
    /// requested are different facts, and they differ exactly when a caller is
    /// deciding whether to go back to the database. Same refusal
    /// [D-096](../../docs/architecture/s13-decision-register.md) made for the
    /// open interval.
    content: Option<String>,
    embedding_model: Option<String>,
    valid_from: String,
    valid_to: String,
}

impl NodeData {
    /// A node with no content and no embedding model — what the default load
    /// produces. Use [`Self::with_content`] and [`Self::with_embedding_model`]
    /// to add either.
    pub fn new(
        title: impl Into<String>,
        valid_from: impl Into<String>,
        valid_to: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            content: None,
            embedding_model: None,
            valid_from: valid_from.into(),
            valid_to: valid_to.into(),
        }
    }

    #[must_use]
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    #[must_use]
    pub fn with_embedding_model(mut self, model: Option<String>) -> Self {
        self.embedding_model = model;
        self
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The document text, or `None` when it was not requested.
    ///
    /// **`None` is not an empty document.** See the field's own note: the
    /// default load does not fetch content, so a caller that did not ask gets
    /// `None` and can tell that apart from a concept whose content really is
    /// `""`.
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn embedding_model(&self) -> Option<&str> {
        self.embedding_model.as_deref()
    }

    pub fn valid_from(&self) -> &str {
        &self.valid_from
    }

    pub fn valid_to(&self) -> &str {
        &self.valid_to
    }
}

/// The string pool an interned [`EdgeRef`] indexes into (0.8.0, B2, D-115).
///
/// One pool for every string an edge carries — node ids, edge types and the two
/// timestamps — because they dedupe against each other for free and the whole
/// point is that the cost is per **distinct string** rather than per edge.
///
/// Indices are handed out first-seen. **Nothing observable depends on them**:
/// node order comes from `nodes`, which is still a `BTreeMap` keyed by id, and
/// adjacency order is the order edges were added, exactly as before. That is
/// the deliberate answer to D-063's warning that "determinism stops being
/// structural and becomes procedural" — it does not, because the node map was
/// never what needed interning. `node_order_does_not_depend_on_construction_order`
/// is the gate that holds it.
#[derive(Debug, Clone, Default)]
struct Interner {
    strings: Vec<String>,
    index: BTreeMap<String, u32>,
    /// Running payload total, maintained on insert.
    ///
    /// **Not recomputed.** The first version of the loader called
    /// `estimated_bytes()` before and after every edge to charge the marginal
    /// pool cost, which is O(pool) per row and made loading quadratic — the
    /// exact defect [D-047](../../docs/architecture/s13-decision-register.md)
    /// diagnosed and fixed, re-introduced by the change that was supposed to
    /// make loading *cheaper*. `loading_scales_linearly_in_the_number_of_edges`
    /// caught it, which is what that test is for.
    bytes: usize,
}

impl Interner {
    /// Intern `s`, returning its index and **how many bytes that cost** — zero
    /// when the string was already pooled.
    ///
    /// The caller needs the marginal figure to charge the byte budget as it
    /// loads, and it has to be O(1) or the budget check is quadratic again.
    fn intern(&mut self, s: &str) -> (u32, usize) {
        if let Some(&i) = self.index.get(s) {
            return (i, 0);
        }
        let i = u32::try_from(self.strings.len())
            .expect("a subgraph cannot hold 2^32 distinct strings within any byte budget");
        self.strings.push(s.to_string());
        self.index.insert(s.to_string(), i);
        let cost = Self::entry_bytes(s);
        self.bytes += cost;
        (i, cost)
    }

    /// Once in `strings`, once as the key of `index`, plus both containers'
    /// per-entry overhead.
    fn entry_bytes(s: &str) -> usize {
        2 * s.len() + std::mem::size_of::<String>() + std::mem::size_of::<u32>()
    }

    fn get(&self, i: u32) -> &str {
        &self.strings[i as usize]
    }

    /// Payload bytes held by the pool, counted the way [`Subgraph::node_bytes`]
    /// counts: string bytes plus per-item overhead.
    ///
    /// **This is the arithmetic D-063 asked for.** Its objection to interning
    /// was that an id table "stores every id a second time, partly cancelling
    /// the memory win". It is counted here rather than argued about: the
    /// duplication is per distinct string, the saving is per edge entry, and
    /// `estimated_bytes()` reports the sum so a caller can see both.
    fn estimated_bytes(&self) -> usize {
        self.bytes
    }
}

/// One end of an edge in an adjacency list — **interned** (0.8.0, B2, D-115).
///
/// Five fields, no heap payload, `size_of` 24 bytes against 104 bytes of struct
/// plus around 250 of strings before. Every field but the weight is an index
/// into its [`Subgraph`]'s pool, so reading one needs the graph:
///
/// ```ignore
/// for e in graph.out_edges("a") {
///     println!("{} {} {}", e.node(&graph), e.edge_type(&graph), e.weight());
/// }
/// ```
///
/// That is the visible cost of the change, and it is the reason B1 had to
/// privatise these fields first: a public `node: String` cannot become a `u32`.
/// The win is **reachability**, not speed ([D-073](../../docs/architecture/s13-decision-register.md)'s
/// category): graphs that did not fit the byte budget start fitting.
///
/// # Invariants
///
/// An `EdgeRef` is tied to the specific [`Subgraph`] it was retrieved from.
/// Querying it against a different one — via an accessor like [`Self::node`],
/// or via derived `PartialEq` — is a **logic error** that will silently return
/// incorrect data or report equality where none exists. Because the handle is
/// `Copy` it can be stored in a struct that outlives the graph; it stays
/// well-formed and becomes meaningless without its pool.
///
/// `PartialEq` is the sharp edge, and it is kept rather than removed: *within*
/// one graph, index equality is exactly the comparison a caller wants, and it
/// is cheaper and stricter than comparing five strings. Across two graphs it
/// compares indices that mean different things — a wrong answer that needs no
/// accessor call at all, so it sits outside the mental model of "querying".
/// Before interning, `==` compared the strings and could not be wrong this way.
///
/// This logic error does not result in undefined behaviour — every index goes
/// through bounds-checked slice indexing and there is no `unsafe` here — but
/// the results are otherwise unspecified.
///
/// The handle is intentionally **not** lifetime-branded, which would make the
/// invariant a compile error, because that propagates a generic parameter
/// through every algorithm and every signature that mentions a `Subgraph`. See
/// D-115 for the argument and for what to do if this is ever hit in practice.
#[derive(Clone, Copy, PartialEq)]
pub struct EdgeRef {
    node: u32,
    edge_type: u32,
    weight: f64,
    valid_from: u32,
    valid_to: u32,
}

/// Written by hand so a failing `assert_eq!` cannot be mistaken for one about
/// strings.
///
/// The derived form printed `EdgeRef { node: 3, edge_type: 1, .. }`, which
/// reads as data and is not: those are pool indices, meaningless without the
/// graph. The `#` is there to say so at a glance.
impl std::fmt::Debug for EdgeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "EdgeRef(node=#{}, type=#{}, w={}, from=#{}, to=#{})",
            self.node, self.edge_type, self.weight, self.valid_from, self.valid_to
        )
    }
}

impl EdgeRef {
    /// The far end of the edge: the target in `out_edges`, the source in
    /// `in_edges`.
    ///
    /// Takes the graph because the string lives in its pool. `graph` must be
    /// the one this edge came from; passing another is a programming error and
    /// will panic or answer nonsense, exactly as indexing the wrong slice would.
    pub fn node<'a>(&self, graph: &'a Subgraph) -> &'a str {
        graph.pool.get(self.node)
    }

    pub fn edge_type<'a>(&self, graph: &'a Subgraph) -> &'a str {
        graph.pool.get(self.edge_type)
    }

    /// The only field that is not interned, because an `f64` is already 8 bytes
    /// and a pool of them would cost more than it saved.
    pub fn weight(&self) -> f64 {
        self.weight
    }

    pub fn valid_from<'a>(&self, graph: &'a Subgraph) -> &'a str {
        graph.pool.get(self.valid_from)
    }

    pub fn valid_to<'a>(&self, graph: &'a Subgraph) -> &'a str {
        graph.pool.get(self.valid_to)
    }
}

impl Subgraph {
    /// Whether `id` is a hydrated node of this graph.
    ///
    /// By the closure invariant this is also the answer to "may an algorithm
    /// look this id up", which is why every algorithm asks it rather than
    /// probing adjacency.
    pub fn contains_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// The attributes of `id`, or `None` when it is not in the graph.
    pub fn node(&self, id: &str) -> Option<&NodeData> {
        self.nodes.get(id)
    }

    /// Node ids in ascending order.
    ///
    /// The order is `BTreeMap`'s and is load-bearing rather than incidental:
    /// Louvain breaks ties by first-seen community and returns a different
    /// partition under a randomised order.
    pub fn node_ids(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.nodes.keys().map(String::as_str)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every node with its attributes, in id order.
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = (&str, &NodeData)> + '_ {
        self.nodes.iter().map(|(id, d)| (id.as_str(), d))
    }

    /// The outgoing index: each node that has outgoing edges, with them.
    ///
    /// For one node prefer [`Self::out_edges`]. This exists for callers that
    /// must walk the whole index — the Python `to_dict`, and the diagnostics.
    pub fn out_adjacency(&self) -> impl Iterator<Item = (&str, &[EdgeRef])> + '_ {
        self.out_adj.iter().map(|(id, e)| (id.as_str(), e.as_slice()))
    }

    /// The incoming index. See [`Self::out_adjacency`].
    pub fn in_adjacency(&self) -> impl Iterator<Item = (&str, &[EdgeRef])> + '_ {
        self.in_adj.iter().map(|(id, e)| (id.as_str(), e.as_slice()))
    }

    /// Add or replace a node, returning what was there before.
    ///
    /// Public so that callers who build a graph by hand — the test fixtures,
    /// the diagnostics — can still do so now the fields are private. It does
    /// **not** establish the closure invariant on its own: adjacency naming an
    /// id never inserted is still dangling, exactly as before.
    pub fn insert_node(&mut self, id: impl Into<String>, data: NodeData) -> Option<NodeData> {
        self.nodes.insert(id.into(), data)
    }

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
            pool,
        } = self;

        for adj in [out_adj, in_adj] {
            adj.retain(|id, edges| {
                if !nodes.contains_key(id) {
                    return false;
                }
                edges.retain(|e| nodes.contains_key(pool.get(e.node)));
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
                    && edges
                        .iter()
                        .all(|e| self.nodes.contains_key(self.pool.get(e.node)))
            })
    }

    /// Record an edge in both directions.
    ///
    /// Both indices are maintained together because every undirected quantity
    /// here — degree, k-core peeling, Louvain's `k_i` — reads them as a pair. An
    /// `in_adj` that lags `out_adj` would not fail loudly; it would return a
    /// plausible wrong number.
    /// **Public since 0.8.0.** The callers that used to push into both maps by
    /// hand cannot now the fields are private, and routing them through the one
    /// function that maintains the pair is the point rather than a consolation:
    /// hand-written adjacency was two chances to get the reverse edge wrong,
    /// and every such call site was already doing the `back.node = source`
    /// dance itself. `edge.node` is expected to be `target`; the reverse entry
    /// is derived here.
    /// Returns the bytes this edge added to [`Self::estimated_bytes`] — the two
    /// fixed-size entries plus whatever strings were genuinely new. The loader
    /// charges its budget with it, and it is O(1) by construction.
    pub fn add_edge(
        &mut self,
        source: &str,
        target: &str,
        edge_type: &str,
        weight: f64,
        valid_from: &str,
        valid_to: &str,
    ) -> usize {
        let (src, b1) = self.pool.intern(source);
        let (tgt, b2) = self.pool.intern(target);
        let (ty, b3) = self.pool.intern(edge_type);
        let (from, b4) = self.pool.intern(valid_from);
        let (to, b5) = self.pool.intern(valid_to);
        let pooled = b1 + b2 + b3 + b4 + b5;

        self.out_adj.entry(source.to_string()).or_default().push(EdgeRef {
            node: tgt,
            edge_type: ty,
            weight,
            valid_from: from,
            valid_to: to,
        });
        self.in_adj.entry(target.to_string()).or_default().push(EdgeRef {
            node: src,
            edge_type: ty,
            weight,
            valid_from: from,
            valid_to: to,
        });
        2 * std::mem::size_of::<EdgeRef>() + pooled
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
            + d.content.as_ref().map_or(0, String::len)
            + d.embedding_model.as_ref().map_or(0, String::len)
            + d.valid_from.len()
            + d.valid_to.len()
            + std::mem::size_of::<NodeData>()
    }

    /// Estimated payload bytes for one adjacency entry.
    ///
    /// An edge occupies two of these — one in `out_adj`, one in `in_adj` — so a
    /// caller accounting for a newly added edge counts it twice.
    /// **24 bytes, and nothing else** since B2 (D-115).
    ///
    /// Before interning this summed four string lengths as well, around 189
    /// bytes for a ULID-keyed edge. The strings did not disappear — they moved
    /// into the pool, where they are counted once per *distinct* value by
    /// [`Interner::estimated_bytes`] rather than once per edge entry.
    fn edge_bytes(_e: &EdgeRef) -> usize {
        std::mem::size_of::<EdgeRef>()
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
        nodes + edges + self.pool.estimated_bytes()
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

            let edge_type: String = row.get(2)?;
            let valid_from: String = row.get(4)?;
            let valid_to: String = row.get(5)?;

            // Accounted before the insert, and the arithmetic is far simpler
            // than it was: an interned entry is a fixed 24 bytes whichever
            // endpoint it names, so the two entries `add_edge` writes cost the
            // same and there is no id-length asymmetry to get wrong.
            //
            // The strings have not vanished, they have moved into the pool, so
            // what a *new* distinct string costs is charged here too. Only the
            // ones actually new: `intern` dedupes, and charging every edge for
            // its type and timestamps would re-introduce exactly the per-edge
            // cost B2 removes.
            bytes += graph.add_edge(&source, &target, &edge_type, weight, &valid_from, &valid_to);

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

        hydrate(conn, &mut graph, &ids, bytes, byte_budget, traversal.content).await?;
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
    with_content: bool,
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
                content: if with_content { row.get(2).ok() } else { None },
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

    #[test]
    fn adding_an_edge_indexes_it_in_both_directions() {
        let mut g = Subgraph::default();
        g.add_edge("A", "B", "KNOWS", 0.5, "2026-01-01T00:00:00.000000Z", "9999-12-31T23:59:59.999999Z");

        assert_eq!(g.out_edges("A").len(), 1);
        assert_eq!(g.out_edges("A")[0].node(&g), "B");
        assert_eq!(g.in_edges("B").len(), 1);
        assert_eq!(g.in_edges("B")[0].node(&g), "A", "in_adj holds the source");

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
