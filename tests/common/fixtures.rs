//! The fixture matrix (T4.1, D-088).
//!
//! # Why this file exists
//!
//! Until it did, every performance claim in this project was measured on one
//! graph shape: [`Shape::StarOfStars`], the chain of stars `benches/budgets.rs`
//! seeds. That shape is a **tree**, and a tree has exactly one path to each
//! node, so any cost that grows with *path count* is identically flat on it.
//!
//! Three defects came out of that single frame:
//!
//! * [D-070] explained `load_subgraph`'s superlinearity as the `DISTINCT` sort,
//!   rigorously and wrongly — on a tree the walk emits 1,011 rows for 1,011
//!   nodes, and the term that dominates on a real graph is 1. [D-076] measured
//!   299,593 walk rows for 49 nodes on a layered graph.
//! * T0.1's path-enumeration cost was invisible for the same reason.
//! * [D-059] left an open note that the chunk constants are empty-database
//!   figures "and need a realistic fixture, which requires deciding what
//!   'realistic' means."
//!
//! This file decides what realistic means: **four shapes, each named for the
//! cost it is the worst case for**, so that a measurement taken on one of them
//! is a measurement of something stated rather than of whatever the fixture
//! happened to be.
//!
//! # The rule that goes with it
//!
//! A performance decision entry names the shape(s) it was measured on. D-070's
//! would have read *"inherent on `star_of_stars`"*, and the gap would have been
//! a sentence in the entry rather than a wave.
//!
//! # Why a path-included module rather than a feature-gated `src` module
//!
//! Fixture builders are dev-only code and do not belong in the shipped library,
//! and a `fixtures` feature would have to be listed in `required-features` on
//! `[[bench]]`, which makes a plain `cargo bench` skip the benchmarks silently.
//! `tests/common/harness.rs` already establishes the pattern; both live under
//! `tests/common/` because Cargo auto-targets every top-level `tests/*.rs` as
//! its own integration-test binary, and a shared module there compiles into an
//! extra binary with no tests in it.
//! benches and examples reach this file with
//! `#[path = "../tests/common/fixtures.rs"] mod fixtures;`.
//!
//! [D-059]: ../docs/architecture/s13-decision-register.md
//! [D-070]: ../docs/architecture/s13-decision-register.md
//! [D-076]: ../docs/architecture/s13-decision-register.md

// Every consumer uses a different subset: the benches want the seeders, the
// plan-pinning tests want the structural facts, `fixture_matrix_diag` wants
// both. Per-item `allow` would be noise on every item in the file.
#![allow(dead_code)]

use macrame::prelude::*;

pub const TS: &str = "2026-01-01T00:00:00.000000Z";
pub const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The four shapes, and what each one is the worst case for.
///
/// Every shape is a pure function of `nodes`: no randomness, no clock, no
/// environment. A measurement that cannot be re-run to the same edge set is not
/// a measurement, and a fixture seeded from a hash of the machine name is how
/// that happens by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Tree, high fan-out. Worst case for **hub out-degree** — the single-open
    /// probe's scan (D-059) and the overlap guard both key on `source_id`.
    ///
    /// This is the *existing* fixture, kept because it is a genuine worst case
    /// for a real cost and because every figure recorded before 0.6.0 was taken
    /// on it. Removing it would orphan the record.
    StarOfStars,
    /// Communities with dense intra-links and sparse bridges. Worst case for
    /// **path enumeration** (T0.1, D-076) and for Louvain, which is looking for
    /// exactly this structure.
    Clustered,
    /// A long path with occasional short branches. Worst case for **recursion
    /// depth** and for snapshot fold length.
    Chain,
    /// Near-complete on a few hundred nodes. Worst case for the **`DISTINCT`
    /// sort** and for the byte budget: edge count is quadratic in node count,
    /// so this is where a subgraph load runs out of budget first.
    DenseSmall,
}

pub const ALL_SHAPES: &[Shape] = &[
    Shape::StarOfStars,
    Shape::Clustered,
    Shape::Chain,
    Shape::DenseSmall,
];

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Shape::StarOfStars => "star_of_stars",
            Shape::Clustered => "clustered",
            Shape::Chain => "chain",
            Shape::DenseSmall => "dense_small",
        }
    }

    /// What this shape is the worst case for, in one line — for a diagnostic to
    /// print beside its numbers, so a table cannot be read without it.
    pub fn worst_case_for(self) -> &'static str {
        match self {
            Shape::StarOfStars => "hub out-degree",
            Shape::Clustered => "path enumeration; Louvain",
            Shape::Chain => "recursion depth; snapshot fold length",
            Shape::DenseSmall => "the DISTINCT sort; byte budget",
        }
    }

    /// The node id this shape's traversals should start from.
    ///
    /// Not always `c0000000`: on [`Shape::Chain`] the interesting question is
    /// depth, which means starting at the head, and on [`Shape::Clustered`] it
    /// is path count, which means starting inside a community rather than at a
    /// bridge endpoint. A caller that hardcodes node zero measures a different
    /// question on each shape without noticing.
    pub fn start_node(self, nodes: usize) -> String {
        let i = match self {
            Shape::StarOfStars | Shape::Chain | Shape::DenseSmall => 0,
            // The second member of the first community: reachable, and not the
            // community's bridge endpoint.
            Shape::Clustered => 1.min(nodes.saturating_sub(1)),
        };
        node_id(i)
    }

    /// `nodes` concepts and this shape's edge set over them.
    ///
    /// Edges are returned rather than written so a caller can count them, hand
    /// them to `bulk_import` or to `write_bulk_atomic`, or seed them under a
    /// clock of its choosing.
    pub fn edges(self, nodes: usize) -> Vec<EdgeAssertion> {
        match self {
            Shape::StarOfStars => star_of_stars(nodes),
            Shape::Clustered => clustered(nodes),
            Shape::Chain => chain(nodes),
            Shape::DenseSmall => dense_small(nodes),
        }
    }

    /// The concepts the edge set refers to.
    ///
    /// Every shape uses ids `c0000000 … c{nodes-1}`, so this is shape
    /// independent — but it is a method rather than a free function because a
    /// future shape that needs typed or sized content should be able to say so
    /// without every call site changing.
    pub fn concepts(self, nodes: usize) -> Vec<ConceptUpsert> {
        (0..nodes).map(concept).collect()
    }
}

pub fn node_id(i: usize) -> String {
    format!("c{i:07}")
}

pub fn concept(i: usize) -> ConceptUpsert {
    ConceptUpsert::new(node_id(i), format!("Concept {i}"))
        .content(format!("body text for concept number {i}"))
        .valid_from(TS)
}

fn edge(src: usize, tgt: usize) -> EdgeAssertion {
    EdgeAssertion::new(node_id(src), node_id(tgt), "LINKS")
        .valid_from(TS)
        .valid_to(OPEN)
}

// ---------------------------------------------------------------------------
// The shapes
// ---------------------------------------------------------------------------
//
// Doctrine constraint every one of them is written under: `links` permits at
// most one **open** interval per (source, target, edge_type), enforced by
// `trg_links_single_open`. So a shape may not emit the same ordered pair twice.
// Each builder below is written so that it structurally cannot — the pair set
// is a function of distinct (i, k) with a stated injection — and
// `fixtures_tests` checks it rather than trusting the argument.

/// A three-tier chain of stars: one root, its children, their children.
///
/// This is `benches/budgets.rs`'s `seed_edges` preserved exactly, down to the
/// thirds arithmetic, because the value of keeping it is that pre-0.6.0 figures
/// remain comparable. Changing it "slightly while moving it" would silently
/// invalidate the record this fixture exists to preserve.
fn star_of_stars(nodes: usize) -> Vec<EdgeAssertion> {
    let edges = nodes.saturating_sub(1);
    let mut out = Vec::with_capacity(edges);
    for i in 1..=edges {
        let src = if i <= edges / 3 {
            0
        } else if i <= 2 * (edges / 3) {
            i - edges / 3
        } else {
            i - 2 * (edges / 3)
        };
        out.push(edge(src, i));
    }
    out
}

/// Communities of [`CLUSTER_SIZE`], densely linked inside, one bridge out.
///
/// Dense-inside is what makes this the path-enumeration case: within a
/// community of `k` nodes there are many distinct routes between any two, so a
/// depth-3 walk enumerates paths rather than nodes, and the ratio of walk rows
/// to reachable nodes is the number D-076 found and D-070 could not see.
///
/// **Intra-community links go both ways**, which is not decoration. An
/// `i -> j, i < j` clique is a DAG: path length is bounded by the community
/// size and the walk stays close to linear, which measured 9× and would have
/// made this shape a weaker `star_of_stars` rather than a different question.
/// Both directions make each community strongly connected — what Louvain and
/// `scc` are looking for, and what makes the walk revisit — and take the same
/// depth-3 walk to ~40×. The ordered pairs are still distinct, so the
/// single-open trigger is satisfied.
pub const CLUSTER_SIZE: usize = 12;

fn clustered(nodes: usize) -> Vec<EdgeAssertion> {
    let mut out = Vec::new();
    let communities = nodes / CLUSTER_SIZE;
    for c in 0..communities {
        let base = c * CLUSTER_SIZE;
        for i in 0..CLUSTER_SIZE {
            for j in 0..CLUSTER_SIZE {
                if i != j {
                    out.push(edge(base + i, base + j));
                }
            }
        }
        // One bridge to the next community, from a non-zero member so the
        // bridge endpoint is not also the community's most-linked node. The
        // target is outside this community, so it cannot collide with the
        // clique above.
        if c + 1 < communities {
            out.push(edge(base + CLUSTER_SIZE - 2, base + CLUSTER_SIZE));
        }
    }
    out
}

/// A long path `0 -> 1 -> 2 -> …`, with a short spur every
/// [`CHAIN_SPUR_EVERY`] nodes.
///
/// The spurs exist so the shape is not a straight line: a straight line makes
/// the recursive CTE's frontier exactly one row wide, which is a degenerate
/// case rather than a deep one, and the branching is what keeps the fold length
/// long *and* the frontier non-trivial. They are short so depth stays the
/// dominant property.
pub const CHAIN_SPUR_EVERY: usize = 10;

fn chain(nodes: usize) -> Vec<EdgeAssertion> {
    let mut out = Vec::new();
    for i in 0..nodes.saturating_sub(1) {
        out.push(edge(i, i + 1));
    }
    // Skip forward two: `i -> i+1` is already emitted by the spine, so a spur
    // must reach further than one to stay distinct from it.
    for i in (0..nodes.saturating_sub(2)).step_by(CHAIN_SPUR_EVERY) {
        out.push(edge(i, i + 2));
    }
    out
}

/// Near-complete: every ordered pair `i -> j`, `i != j`, up to
/// [`DENSE_SMALL_CAP`] nodes.
///
/// Capped rather than scaled, deliberately. Edge count is `n(n-1)`, so an
/// uncapped `dense_small` at a bench's default node count would be tens of
/// millions of edges and minutes of fixture construction — and the shape's
/// point is density at a *small* node count, which is where the `DISTINCT`
/// sort and the byte budget bind. A caller asking for more nodes than the cap
/// gets the cap, and `fixtures_tests` pins that so it is a stated property
/// rather than a surprise in a results table.
pub const DENSE_SMALL_CAP: usize = 300;

fn dense_small(nodes: usize) -> Vec<EdgeAssertion> {
    let n = nodes.min(DENSE_SMALL_CAP);
    let mut out = Vec::with_capacity(n.saturating_mul(n.saturating_sub(1)));
    for i in 0..n {
        for j in 0..n {
            if i != j {
                out.push(edge(i, j));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Structural facts
// ---------------------------------------------------------------------------

/// What a shape *is*, computed from its edge set rather than asserted about it.
///
/// `simple_paths` is the discriminating number, and it needs a caveat stated
/// here rather than discovered later. It is the count of distinct **simple
/// paths** of length `<= depth` from the start node. That is *not* the row
/// count of the shipped `walk` CTE: T0.1 replaced `UNION ALL` + a `path` column
/// with a plain `UNION`, which dedupes on `(node_id, depth)` and bounds the
/// walk at `reached × (depth+1)`. Calling this "the CTE's row count" would be
/// D-070's error committed inside the file written to prevent it.
///
/// It is kept, and it is the right structural metric, for two reasons:
///
/// * It is what separates a tree from a graph. On a tree it equals `reached`;
///   on [`Shape::DenseSmall`] it is four orders larger over the same node
///   count. Any cost that is multiplicative in branching per hop — path
///   enumeration, and anything downstream that materialises per-path state —
///   tracks it, and is identically flat on the fixture everything used to be
///   measured on.
/// * It is the cost that **returns** the moment path semantics are
///   reintroduced, which T0.1's own rustdoc argues is a live risk (the two
///   forms have equal reachability, so a reviewer can swap them and every test
///   still passes).
#[derive(Debug, Clone)]
pub struct Facts {
    pub shape: &'static str,
    pub nodes: usize,
    pub edges: usize,
    pub max_out_degree: usize,
    /// Nodes reachable from `start` within `depth` hops.
    pub reached: usize,
    /// Distinct simple paths from `start` of length `<= depth`.
    pub simple_paths: usize,
}

impl Facts {
    /// `simple_paths / reached`. 1.0 on a tree; the whole point on anything
    /// else. See the caveat on [`Facts`] for what this is and is not.
    pub fn path_multiplier(&self) -> f64 {
        if self.reached == 0 {
            0.0
        } else {
            self.simple_paths as f64 / self.reached as f64
        }
    }

    /// The shipped CTE's row bound after T0.1: `reached × (depth+1)`.
    ///
    /// Reported beside `simple_paths` so the gap between them is visible —
    /// that gap is what T0.1 bought, and on `star_of_stars` it is zero, which
    /// is why the win was invisible on the only fixture that existed.
    pub fn union_bound(&self, depth: usize) -> usize {
        self.reached * (depth + 1)
    }
}

/// Compute [`Facts`] for a shape from its edge set.
///
/// A deliberate second implementation of the recursion, in Rust, over the same
/// edges the database is given — a shape whose properties are only knowable by
/// running the query they are meant to characterise cannot pin anything.
///
/// The cycle rule is the **pre-T0.1** one: a path may not revisit a node it
/// already contains. That is what makes `simple_paths` the number it is; see
/// [`Facts`]. Without a cycle rule of some kind this does not terminate on
/// [`Shape::Clustered`] or [`Shape::DenseSmall`], both of which have cycles.
pub fn facts(shape: Shape, nodes: usize, depth: usize) -> Facts {
    use std::collections::{HashMap, HashSet};

    let edges = shape.edges(nodes);
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut out_degree: HashMap<&str, usize> = HashMap::new();
    for e in &edges {
        adj.entry(&e.source).or_default().push(&e.target);
        *out_degree.entry(&e.source).or_default() += 1;
    }

    let start = shape.start_node(nodes);
    let mut reached: HashSet<&str> = HashSet::new();
    let mut simple_paths = 0usize;

    // Explicit stack of (node, depth, path-so-far). Recursion here would blow
    // the Rust stack on `chain` at any interesting size, which is precisely the
    // cost this shape exists to expose.
    let mut stack: Vec<(&str, usize, Vec<&str>)> = vec![(start.as_str(), 0, vec![start.as_str()])];
    while let Some((node, d, path)) = stack.pop() {
        simple_paths += 1;
        reached.insert(node);
        if d >= depth {
            continue;
        }
        for &next in adj.get(node).map(|v| v.as_slice()).unwrap_or(&[]) {
            if !path.contains(&next) {
                let mut p = path.clone();
                p.push(next);
                stack.push((next, d + 1, p));
            }
        }
    }

    Facts {
        shape: shape.name(),
        nodes,
        edges: edges.len(),
        max_out_degree: out_degree.values().copied().max().unwrap_or(0),
        reached: reached.len(),
        simple_paths,
    }
}

/// The smallest depth from which the shape's start node reaches `fraction` of
/// its nodes, or `cap` if it never does.
///
/// **A fixed depth is not a comparable unit of work across shapes**, and this
/// is the correction that makes cross-shape tables mean anything. Measured at
/// depth 3 over 600 requested nodes, the four shapes reach 600, 24, 5 and 300
/// nodes respectively. A table indexed by depth is therefore comparing a
/// 600-node problem against a 5-node one and reporting the difference as a
/// property of the shape — which is D-070's error in a new place: a frame
/// nobody checked.
///
/// So a shape-crossing measurement states which it holds fixed. Depth is the
/// right control for "what does one more hop cost"; this is the right control
/// for "what does this shape cost at comparable size".
pub fn depth_to_cover(shape: Shape, nodes: usize, fraction: f64, cap: usize) -> usize {
    let total = match shape {
        Shape::DenseSmall => nodes.min(DENSE_SMALL_CAP),
        _ => nodes,
    };
    let want = (total as f64 * fraction).ceil() as usize;
    let by_depth = reach_by_depth(shape, nodes, cap);
    by_depth
        .iter()
        .position(|&r| r >= want)
        .unwrap_or(by_depth.len() - 1)
}

/// Nodes reachable from the shape's start within `depth` hops.
pub fn reach(shape: Shape, nodes: usize, depth: usize) -> usize {
    *reach_by_depth(shape, nodes, depth).last().unwrap()
}

/// Cumulative reach after `0, 1, … max_depth` hops, from one BFS.
///
/// Deliberately **not** derived from [`facts`], even though the two agree.
/// `facts` enumerates simple paths, which is `O(simple_paths)` — 26.5 million
/// at depth 3 on [`Shape::DenseSmall`] and unbounded above that. And it is one
/// sweep rather than a probe per depth, because [`Shape::Chain`] needs `nodes`
/// hops to cover itself by construction, so a probe loop would be
/// `O(edges × depth²)` on exactly the shape whose point is that depth is large.
///
/// The frontier stops early when it empties; the returned vector is padded to
/// `max_depth + 1` so a caller can index it by depth without a bounds check.
pub fn reach_by_depth(shape: Shape, nodes: usize, max_depth: usize) -> Vec<usize> {
    use std::collections::{HashMap, HashSet};

    let edges = shape.edges(nodes);
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &edges {
        adj.entry(&e.source).or_default().push(&e.target);
    }

    let start = shape.start_node(nodes);
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(start.as_str());
    let mut frontier: Vec<&str> = vec![start.as_str()];
    let mut out = vec![seen.len()];

    for _ in 0..max_depth {
        let mut next = Vec::new();
        for node in frontier {
            for &t in adj.get(node).map(|v| v.as_slice()).unwrap_or(&[]) {
                if seen.insert(t) {
                    next.push(t);
                }
            }
        }
        out.push(seen.len());
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    out.resize(max_depth + 1, seen.len());
    out
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// Write a shape's concepts and edges into `db`.
///
/// Chunked at 2,000, which is what the bench seeders already used — the bulk
/// paths chunk internally anyway ([`Database::bulk_import`]), so this only
/// bounds the size of the `Vec` handed across the channel.
pub async fn seed(db: &Database, shape: Shape, nodes: usize) -> usize {
    for chunk in shape.concepts(nodes).chunks(2_000) {
        db.write_concepts(chunk.to_vec()).await.unwrap();
    }
    let edges = shape.edges(nodes);
    let count = edges.len();
    for chunk in edges.chunks(2_000) {
        db.bulk_import(chunk.to_vec()).await.unwrap();
    }
    count
}
