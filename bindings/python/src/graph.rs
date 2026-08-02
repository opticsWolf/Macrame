//! The read path: traversals and subgraphs (P4.2), and the algorithms over
//! them (P4.7).
//!
//! # `Subgraph` is a handle, not a dict (D-097)
//!
//! A `Subgraph` is three `BTreeMap`s and is the largest thing the crate
//! materialises — which is why it is the one load bounded by an explicit
//! `byte_budget` (D-047). Converting it to Python dicts on return **doubles the
//! peak memory of the one operation that already has a budget**, and does it
//! eagerly whether or not the caller reads more than `degree()`.
//!
//! So it is an opaque `#[pyclass]` with the accessors forwarded. Callers who
//! want the copy ask for it by name — [`PySubgraph::to_dict`] — having decided
//! to pay for it.
//!
//! # There is no `TraversalBuilder` in Python
//!
//! P3 settled that these bindings do not ship chained setters: the keyword
//! constructor is complete, and a second way to build the same value is API
//! surface with no capability behind it. A traversal is therefore a **call**,
//! not an object — `db.traverse(start, max_depth=3, edge_types=[…])` — and the
//! builder is assembled inside these methods.
//!
//! That has one consequence worth stating: `TraversalBuilder` is reusable in
//! Rust and there is nothing here to reuse. A caller running the same traversal
//! at ten instants passes the arguments ten times. The alternative is exporting
//! a mutable builder object across the GIL boundary, which is the shape P1
//! rejected for `Database` and for the same reason.
//!
//! # `attribute_mode` and `as_of` are two questions, and one of them is refused
//!
//! [`macrame::graph::TraversalBuilder::execute`] answers `Ok(vec![])` under
//! [`AttributeMode::Omit`] — there are no attributes to return — and its own
//! rustdoc says that is indistinguishable from a traversal that reached
//! nothing. Rust callers have `execute_ids`. So does Python, as `traverse_ids`,
//! and `traverse(attribute_mode=OMIT)` is **refused here** rather than
//! forwarded.
//!
//! That is this binding refusing something the library accepts, which
//! [`crate::types`] argues against in general. The distinction is what the
//! library does with it: `normalized()` not checking the calendar is a boundary
//! the crate *chose*, and tightening it would surprise a caller who read the
//! crate's docs. `Omit` reaching `execute` is a return type that cannot express
//! the answer, documented as such upstream. Forwarding it would re-export a
//! known-ambiguous empty list to callers with strictly less context.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use macrame::graph::{EdgeRef, NodeData, Subgraph};
use macrame::temporal::NodeAttributes;

use crate::timestamps::from_canonical;
use crate::types::PyAttributeMode;

// -- leaf values --------------------------------------------------------------

/// A node's attributes as a traversal hydrated them.
///
/// Carries no interval: `AttributeMode::AtTime` reconstructs the text believed
/// at an instant, and the instant is the traversal's, not the row's.
#[pyclass(
    name = "NodeAttributes",
    module = "macrame",
    frozen,
    eq,
    skip_from_py_object
)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyNodeAttributes {
    pub(crate) inner: NodeAttributes,
}

#[pymethods]
impl PyNodeAttributes {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }
    #[getter]
    fn title(&self) -> &str {
        &self.inner.title
    }
    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }
    #[getter]
    fn embedding_model(&self) -> Option<&str> {
        self.inner.embedding_model.as_deref()
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.NodeAttributes id={:?} title={:?}>",
            self.inner.id, self.inner.title
        )
    }
}

/// A hydrated concept inside a [`PySubgraph`].
///
/// Distinct from [`PyNodeAttributes`] because it carries the concept's own
/// validity interval and that one does not — a subgraph hydrates the live
/// concept row, so the interval is a fact about the row rather than about the
/// query.
#[pyclass(name = "NodeData", module = "macrame", frozen, eq, skip_from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyNodeData {
    inner: NodeData,
}

#[pymethods]
impl PyNodeData {
    #[getter]
    fn title(&self) -> &str {
        self.inner.title()
    }
    #[getter]
    fn content(&self) -> &str {
        self.inner.content()
    }
    #[getter]
    fn embedding_model(&self) -> Option<&str> {
        self.inner.embedding_model()
    }
    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, self.inner.valid_from())
    }
    /// `None` for an open interval, per P3's sentinel rule.
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, self.inner.valid_to())
    }
    fn __repr__(&self) -> String {
        format!("<macrame.NodeData title={:?}>", self.inner.title())
    }
}

/// One end of an edge in an adjacency list.
///
/// **`node` is the *other* end** — the target in `out_edges`, the source in
/// `in_edges`. It is not named `target`, because half the time it is not one.
#[pyclass(name = "EdgeRef", module = "macrame", frozen, eq, skip_from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyEdgeRef {
    // Owned, not an `EdgeRef`. Since B2 (D-115) an `EdgeRef` is five integers
    // indexing its `Subgraph`'s string pool, so it cannot be read without the
    // graph and cannot outlive it. Python objects do both, so the strings are
    // resolved once at the boundary — which is where a copy has to happen
    // anyway, because a `#[pyclass]` cannot borrow from Rust-owned memory.
    node: String,
    edge_type: String,
    weight: f64,
    valid_from: String,
    valid_to: String,
}

impl PyEdgeRef {
    fn resolve(e: &EdgeRef, g: &Subgraph) -> Self {
        Self {
            node: e.node(g).to_string(),
            edge_type: e.edge_type(g).to_string(),
            weight: e.weight(),
            valid_from: e.valid_from(g).to_string(),
            valid_to: e.valid_to(g).to_string(),
        }
    }
}

#[pymethods]
impl PyEdgeRef {
    #[getter]
    fn node(&self) -> &str {
        &self.node
    }
    #[getter]
    fn edge_type(&self) -> &str {
        &self.edge_type
    }
    #[getter]
    fn weight(&self) -> f64 {
        self.weight
    }
    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.valid_from)
    }
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.valid_to)
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.EdgeRef node={:?} type={:?} weight={}>",
            self.node, self.edge_type, self.weight
        )
    }
}

// -- the subgraph handle ------------------------------------------------------

/// A loaded neighbourhood: nodes, and adjacency in both directions.
///
/// Returned by `Database.load_subgraph`. Immutable, and cheap to hold — the
/// Rust value is not copied into Python unless [`PySubgraph::to_dict`] is
/// called.
#[pyclass(name = "Subgraph", module = "macrame", frozen)]
pub(crate) struct PySubgraph {
    pub(crate) inner: Subgraph,
}

#[pymethods]
impl PySubgraph {
    /// Outgoing edges of `node`; empty when it has none **or is absent**.
    ///
    /// The two are not distinguished, which is the Rust API's choice and is kept:
    /// use `node in subgraph` to ask the other question.
    fn out_edges(&self, node: &str) -> Vec<PyEdgeRef> {
        self.inner
            .out_edges(node)
            .iter()
            .map(|e| PyEdgeRef::resolve(e, &self.inner))
            .collect()
    }

    /// Incoming edges of `node`; empty when it has none or is absent.
    fn in_edges(&self, node: &str) -> Vec<PyEdgeRef> {
        self.inner
            .in_edges(node)
            .iter()
            .map(|e| PyEdgeRef::resolve(e, &self.inner))
            .collect()
    }

    /// Undirected edge count incident to `node` — parallel edges once each, a
    /// self-loop twice.
    fn degree(&self, node: &str) -> usize {
        self.inner.degree(node)
    }

    /// Undirected incident weight. Summed over both directions, so summing this
    /// over every node gives `2 * total_weight()`.
    fn weighted_degree(&self, node: &str) -> f64 {
        self.inner.weighted_degree(node)
    }

    /// Total edge weight, each edge counted once — the `m` of the modularity
    /// formulas.
    fn total_weight(&self) -> f64 {
        self.inner.total_weight()
    }

    fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    /// The payload size the byte budget was checked against.
    ///
    /// **O(V + E)** — the loader used to call this per row, which made loading
    /// O(E²) (D-047). It is fine to call once and wrong to call in a loop.
    fn estimated_bytes(&self) -> usize {
        self.inner.estimated_bytes()
    }

    /// Whether every adjacency endpoint is a hydrated node.
    ///
    /// Always true for a graph this loader returned — the invariant is
    /// established by a prune after hydration, because a retired concept is not
    /// visible (§4.1) and its edges are dropped rather than kept as tombstones.
    /// Exposed because a false answer would be a defect worth being able to
    /// name, not because a caller is expected to check.
    ///
    /// Unrelated to `Database.is_closed`, which is about a handle. The name is
    /// the crate's.
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// The hydrated concept, or `None` if `node` is not in this graph.
    fn node(&self, node: &str) -> Option<PyNodeData> {
        self.inner
            .node(node)
            .map(|d| PyNodeData { inner: d.clone() })
    }

    /// Node count.
    fn __len__(&self) -> usize {
        self.inner.node_count()
    }

    fn __contains__(&self, node: &str) -> bool {
        self.inner.contains_node(node)
    }

    /// Iterate node ids, in id order.
    ///
    /// Materialises the id list — keys only, which is a small fraction of the
    /// payload the budget bounded. Iterating lazily would mean holding a borrow
    /// of the map across arbitrary Python code, and this class is `frozen`
    /// precisely so nothing has to.
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ids: Vec<&str> = self.inner.node_ids().collect();
        PyList::new(py, ids)?
            .into_any()
            .try_iter()
            .map(|i| i.into_any())
    }

    /// A plain-`dict` copy, for callers who want one and have decided to pay.
    ///
    /// `{"nodes": {id: NodeData}, "out_adj": {id: [EdgeRef]}, "in_adj": …}`.
    /// This is the conversion D-097 declined to do on return: it allocates a
    /// second copy of a structure that was already the largest thing the crate
    /// materialises, so it is spelled out at the call site.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);

        let nodes = PyDict::new(py);
        for (id, data) in self.inner.nodes() {
            nodes.set_item(
                id,
                PyNodeData {
                    inner: data.clone(),
                }
                .into_pyobject(py)?,
            )?;
        }
        out.set_item("nodes", nodes)?;

        for (key, adj) in [
            ("out_adj", Box::new(self.inner.out_adjacency())
                as Box<dyn Iterator<Item = (&str, &[EdgeRef])>>),
            ("in_adj", Box::new(self.inner.in_adjacency())),
        ] {
            let d = PyDict::new(py);
            for (id, edges) in adj {
                let refs: Vec<PyEdgeRef> = edges
                    .iter()
                    .map(|e| PyEdgeRef::resolve(e, &self.inner))
                    .collect();
                d.set_item(id, refs)?;
            }
            out.set_item(key, d)?;
        }
        Ok(out)
    }

    // -- algorithms (P4.7) ---------------------------------------------------
    //
    // Methods on the graph rather than free functions taking one. The Rust API
    // is free functions because `algorithms.rs` is a module of them; in Python
    // `g.louvain()` is where a caller looks, and there is no second kind of
    // graph these could apply to.
    //
    // **All of these release the GIL except `astar`.** They are pure CPU over
    // Rust-owned data with no Python object in reach, and `louvain` on a
    // budget-sized graph is long enough that holding the GIL would stall every
    // other thread in the process for no reason. `astar` is the exception and
    // says why at its own docstring.

    /// Shortest-path distances from `start` (§5.4).
    ///
    /// `{node_id: distance}`, including `start` at 0.0. **Unreachable nodes are
    /// absent rather than present at infinity** — `d.get(x)` returning `None` is
    /// the answer, and a caller who wants infinity can default it.
    ///
    /// Sound only over non-negative weights, which is what
    /// `NegativeEdgeWeightError` at load time protects: leave `load_subgraph`'s
    /// `min_weight` unstated and a negative weight is refused there rather than
    /// producing a path here that is merely *a* path.
    fn dijkstra(&self, py: Python<'_>, start: &str) -> std::collections::BTreeMap<String, f64> {
        py.detach(|| macrame::graph::dijkstra(&self.inner, start))
    }

    /// A* from `start` to `goal`, as `(cost, path)` or `None` if unreachable
    /// (§5.4).
    ///
    /// `path` is inclusive of both endpoints.
    ///
    /// # The heuristic
    ///
    /// `heuristic(node, goal) -> float`, or `None` for a zero heuristic — which
    /// makes this Dijkstra that also returns the path, and is the right default
    /// because zero is the one heuristic that is admissible on every graph.
    ///
    /// **It must be admissible**: it must never overestimate the remaining cost.
    /// An inadmissible heuristic does not error, it returns a path that is a
    /// path and not the shortest one, which is the failure mode this note exists
    /// for.
    ///
    /// A value that is `NaN` or infinite is refused rather than fed to the
    /// priority queue, where a `NaN` makes the ordering incoherent and the
    /// comparison panics — a panic in a callback across an FFI boundary being a
    /// much worse outcome than an exception naming the node.
    ///
    /// # This one does **not** release the GIL
    ///
    /// It cannot: the heuristic is a Python callable and every evaluation needs
    /// the GIL back. Releasing it around the algorithm and re-attaching per node
    /// would pay two GIL transitions per expansion to hold it for the arithmetic
    /// in between — strictly worse than not releasing it. So an `astar` with a
    /// Python heuristic blocks other Python threads for its duration. Pass
    /// `heuristic=None` on a large graph, or use `dijkstra`, both of which
    /// release it.
    #[pyo3(signature = (start, goal, heuristic = None))]
    fn astar(
        &self,
        py: Python<'_>,
        start: &str,
        goal: &str,
        heuristic: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<(f64, Vec<String>)>> {
        let Some(h) = heuristic else {
            // No callable, no GIL problem: this is the detachable path.
            return Ok(py.detach(|| macrame::graph::astar(&self.inner, start, goal, |_, _| 0.0)));
        };

        // `astar` takes `Fn(&str, &str) -> f64` and cannot report failure, so a
        // raising or misbehaving heuristic is caught here and re-raised after
        // the search returns. `0.0` is what the search sees in the meantime:
        // admissible on every graph, so the search stays well-defined and
        // terminates rather than being left to whatever a poisoned ordering
        // does.
        let failure = std::cell::RefCell::new(None::<PyErr>);
        let out = macrame::graph::astar(&self.inner, start, goal, |node, goal| {
            if failure.borrow().is_some() {
                return 0.0;
            }
            match h.call1((node, goal)).and_then(|v| v.extract::<f64>()) {
                Ok(v) if v.is_finite() => v,
                Ok(v) => {
                    *failure.borrow_mut() = Some(pyo3::exceptions::PyValueError::new_err(format!(
                        "heuristic({node:?}, {goal:?}) returned {v}; it must be a \
                         finite number. A NaN makes the priority queue's ordering \
                         incoherent, and an infinite estimate is never admissible."
                    )));
                    0.0
                }
                Err(e) => {
                    *failure.borrow_mut() = Some(e);
                    0.0
                }
            }
        });
        match failure.into_inner() {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// Strongly connected components, by Tarjan (§5.4).
    fn scc(&self, py: Python<'_>) -> Vec<Vec<String>> {
        py.detach(|| macrame::graph::scc(&self.inner))
    }

    /// The k-core: the maximal induced subgraph where every node has degree ≥ `k`
    /// (§5.4).
    ///
    /// Undirected — in- and out-degree summed — and parallel edges count once
    /// each, so a node held in by three edges to one neighbour has degree three.
    /// Returned as a `set`, because that is what it is; iterate `sorted(...)` if
    /// order matters.
    fn k_core(&self, py: Python<'_>, k: usize) -> std::collections::BTreeSet<String> {
        py.detach(|| macrame::graph::k_core(&self.inner, k))
    }

    /// Newman–Girvan modularity of a partition, treating the graph as undirected.
    ///
    /// `communities` is `{node_id: community_index}`. This exists so `louvain`
    /// can be judged against what it claims to maximise rather than against its
    /// own output: a detector returning one node per community satisfies
    /// "modularity did not decrease" by *being* the singleton partition, and
    /// measuring Q is what tells the two apart.
    fn modularity(
        &self,
        py: Python<'_>,
        communities: std::collections::BTreeMap<String, usize>,
    ) -> f64 {
        py.detach(|| macrame::graph::modularity(&self.inner, &communities))
    }

    /// Community detection, as `{node_id: community_index}` (§5.4).
    ///
    /// **Phase one of Louvain only** — greedy local moving, repeated until no
    /// move increases modularity. It does not then aggregate communities and
    /// recurse, which is what the full method does to find coarser structure.
    /// For subgraph-sized analytics the local moving phase carries the signal;
    /// aggregation would matter on graphs far larger than a byte budget admits.
    ///
    /// The community indices are arbitrary labels. Comparing two runs means
    /// comparing the *partitions*, not the numbers.
    fn louvain(&self, py: Python<'_>) -> std::collections::BTreeMap<String, usize> {
        py.detach(|| macrame::graph::louvain(&self.inner))
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.Subgraph nodes={} edges={}>",
            self.inner.node_count(),
            self.inner.edge_count()
        )
    }
}

// -- traversal argument assembly ---------------------------------------------

/// Build the Rust builder from the keyword arguments a traversal call takes.
///
/// One place, because `traverse`, `traverse_ids` and `load_subgraph` take the
/// same filters and a difference between them would be a bug rather than a
/// design.
///
/// **`min_weight` is passed in rather than defaulted here, because the two
/// callers need different defaults and that is D-073's finding, not an
/// oversight.** `TraversalBuilder` defaults the floor to `0.0`, which is right
/// for a traversal — a weight filter is a filter — and wrong for a subgraph
/// load, because it silently drops negative-weight edges, precisely the input
/// `NegativeEdgeWeightError` exists to report given that Dijkstra and A* are
/// unsound over them. So `traverse` defaults to `0.0` and `load_subgraph`
/// defaults to `-inf`, exactly as their Rust counterparts do.
pub(crate) fn builder(
    start_node: &str,
    max_depth: usize,
    edge_types: Option<Vec<String>>,
    min_weight: f64,
    attribute_mode: Option<PyAttributeMode>,
    as_of: Option<String>,
) -> macrame::graph::TraversalBuilder {
    let mut b = macrame::graph::TraversalBuilder::new(start_node)
        .max_depth(max_depth)
        .min_weight(min_weight);
    if let Some(types) = edge_types {
        b = b.edge_types(types);
    }
    if let Some(mode) = attribute_mode {
        b = b.attribute_mode(mode.into());
    }
    if let Some(ts) = as_of {
        b = b.as_of(ts);
    }
    b
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyNodeAttributes>()?;
    m.add_class::<PyNodeData>()?;
    m.add_class::<PyEdgeRef>()?;
    m.add_class::<PySubgraph>()?;
    Ok(())
}
