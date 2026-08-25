//! W10.3: `Subgraph`'s interior, measured before it was replaced and after.
//!
//! §2.5 of the 0.12.0 review says the algorithms "could build a dense
//! `Vec`-indexed view once (node id → `u32`, CSR adjacency), run on integers,
//! and translate back at the boundary", and estimates *"on a 10K-node subgraph
//! that is plausibly a 5–20× improvement in the analytics module"*. It closes
//! with the sentence that made this probe exist: **"Order-of-magnitude only —
//! this was read, not benchmarked."**
//!
//! ```text
//! cargo run --release --example subgraph_interior
//! ```
//!
//! # What it measured, and what it measures now
//!
//! In **0.13.27** the `louvain`, `dijkstra`, `scc` and `k_core` columns were the
//! string-keyed interior, and the dense arm was the proposal. The ratio came
//! back inside §2.5's range with the conversion excluded — and **below 1 for
//! `dijkstra` with the conversion charged**, which is what settled the design:
//! built at the boundary, mapping an edge's far end to an index costs a string
//! lookup *per edge endpoint*, and a single-pass algorithm never earns that
//! back. See [D-200].
//!
//! In **0.13.28** those columns are the dense interior, built in-crate where
//! the far end is already a `u32` ([D-201]). So the same table now reads as a
//! standing comparison rather than a one-off: the dense arm is the **boundary**
//! alternative that was rejected, and `x` below 1 is that rejection staying
//! true. `ceil` — the arm with its build struck out — is the remaining headroom,
//! and it is now small.
//!
//! [D-200]: ../docs/architecture/s13-decision-register.md#d-200
//! [D-201]: ../docs/architecture/s13-decision-register.md#d-201
//!
//! # The comparison charges the conversion, because the proposal included it
//!
//! The dense arm is not "the same algorithm on integers". It is what a caller
//! outside the crate would actually get: **build the dense view, run, translate
//! back to the `BTreeMap<String, _>` the public signature returns.** Timing
//! only the middle third would measure a function this crate cannot expose.
//! `build` and `back` are reported separately so the shape of the answer is
//! visible rather than summarised into one ratio.
//!
//! `Flat` and the transcribed local-moving step are lifted from
//! `examples/louvain_aggregation_probe.rs`, where a control already asserts
//! they produce the crate's partition exactly. Both arms are checked against
//! each other here too, per size, so a speedup can never be a divergence — and
//! that check is now also a cross-implementation test of the rewritten
//! interior.
//!
//! # Two id styles, because the string cost is a function of the string
//!
//! A `BTreeMap<String, _>` lookup is a tree descent with a full comparison at
//! each level, and comparison cost tracks the **shared prefix**. The crate's
//! own ids in practice are ULIDs — 26 characters, the first ten of which are a
//! timestamp that barely moves across one import. So the sweep runs short
//! (`c0000000`) and ULID-shaped ids over the same topology. Before the rewrite
//! the two differed by a third on `louvain`; after it they are within noise,
//! because the per-edge work no longer touches a string.
//!
//! # And a share, because a ratio on the interior is not a ratio on the caller
//!
//! A ratio on the interior only matters in proportion to what the interior is a
//! share of. The last section loads a subgraph from a real database and times
//! the load against the algorithms over it, so the answer is a fraction of the
//! caller's wall clock rather than of a function. It came back at **34%–64%**
//! before the rewrite, which is what made the item worth doing, and at
//! **10%–25%** after it.
//!
//! That section deliberately does **not** use the clique chain. Reaching all of
//! a 256-community chain takes 256 hops, and timing a walk that deep would
//! inflate the load and flatter the conclusion this probe is trying to test.
//! It uses the shape [`macrame::prelude::Database::load_subgraph`]'s own
//! rustdoc names — a dense graph in which a node reaches most of the database
//! in three hops — at two, three and four hops from one node.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};
use std::time::{Duration, Instant};

use macrame::graph::{dijkstra, k_core, louvain, scc, NodeData, Subgraph};
use macrame::prelude::*;

/// Matches `tests/common/fixtures.rs` and the aggregation probe.
const CLUSTER_SIZE: usize = 12;
const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The budget `benches/budgets.rs` loads its graph fixtures under.
const BUDGET: usize = 64 << 20;

/// How an id is spelled. Same topology, same edge count, different key bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ids {
    /// Eight characters, diverging by the third.
    Short,
    /// Twenty-six, ULID-shaped: a ten-character timestamp prefix every id
    /// shares, then a counter. What a real load is keyed by.
    Ulid,
}

impl Ids {
    fn name(self) -> &'static str {
        match self {
            Ids::Short => "short (8 ch)",
            Ids::Ulid => "ulid  (26 ch)",
        }
    }

    fn node(self, i: usize) -> String {
        match self {
            Ids::Short => format!("c{i:07}"),
            Ids::Ulid => format!("01JQ8ZK4T0{i:016}"),
        }
    }
}

/// A chain of cliques, built directly into a `Subgraph`.
///
/// Dense by construction — 132 directed edges per 12-node community — because
/// the finding's worst case is the inner loop over a node's edges, and a sparse
/// fixture would understate the thing being measured.
fn clustered(communities: usize, ids: Ids) -> Subgraph {
    let mut g = Subgraph::default();

    let nodes = communities * CLUSTER_SIZE;
    for i in 0..nodes {
        g.insert_node(ids.node(i), NodeData::new("N", TS, OPEN));
    }

    for c in 0..communities {
        let base = c * CLUSTER_SIZE;
        for i in 0..CLUSTER_SIZE {
            for j in 0..CLUSTER_SIZE {
                if i != j {
                    g.add_edge(
                        &ids.node(base + i),
                        &ids.node(base + j),
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
                &ids.node(base),
                &ids.node(base + CLUSTER_SIZE),
                "BRIDGE",
                1.0,
                TS,
                OPEN,
            );
        }
    }
    g
}

// ---------------------------------------------------------------- the dense view

/// An undirected weighted graph as integer-indexed adjacency — §2.5's proposal.
///
/// `out` is kept apart from `both` because Dijkstra follows direction and
/// Louvain does not, and collapsing them would give one of the two arms a graph
/// the crate's version is not running on.
struct Flat {
    /// `both[u] = [(v, w)]`, both directions, as `Subgraph::weighted_degree`
    /// counts them.
    both: Vec<Vec<(usize, f64)>>,
    /// `out[u] = [(v, w)]`, outgoing only.
    out: Vec<Vec<(usize, f64)>>,
    deg: Vec<f64>,
    m: f64,
}

impl Flat {
    /// The conversion the proposal calls "build a dense view once".
    ///
    /// The `BTreeMap<&str, usize>` here is the honest cost: the ids have to be
    /// looked up by string exactly once per edge endpoint, because that is the
    /// only handle the `Subgraph` offers. Interning inside the type would
    /// remove this — and that is precisely the change being priced.
    fn from_subgraph(g: &Subgraph) -> (Self, Vec<String>) {
        let ids: Vec<String> = g.node_ids().map(str::to_string).collect();
        let index: BTreeMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();

        let mut both = vec![Vec::new(); ids.len()];
        let mut out = vec![Vec::new(); ids.len()];
        let mut deg = vec![0.0; ids.len()];
        for (u, id) in ids.iter().enumerate() {
            for e in g.out_edges(id) {
                out[u].push((index[e.node(g)], e.weight()));
            }
            for e in g.out_edges(id).iter().chain(g.in_edges(id)) {
                both[u].push((index[e.node(g)], e.weight()));
                deg[u] += e.weight();
            }
        }
        let m = g.total_weight();
        (Flat { both, out, deg, m }, ids)
    }
}

const MAX_SWEEPS: usize = 100;
const MIN_GAIN: f64 = 1e-12;

/// The crate's `louvain`, transcribed onto `Flat`.
///
/// Deliberately a transcription and not an improvement: same `dQ`, same
/// epsilon, same withdraw-before-scoring, same lowest-index tie-break by
/// scanning a `BTreeMap`. Anything else and the timing would be measuring two
/// algorithms.
fn flat_louvain(f: &Flat) -> Vec<usize> {
    let n = f.both.len();
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
            for &(v, w) in &f.both[u] {
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

/// `algorithms::renumber`, on indices.
fn flat_renumber(comm: &[usize]) -> Vec<usize> {
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

/// The crate's `dijkstra`, transcribed onto `Flat`.
///
/// The heap carries a `usize` where the crate's carries a `String`, which is
/// the second string cost §2.5 does not mention: every sift compares keys, and
/// the crate's key is `(f64, String)`.
fn flat_dijkstra(f: &Flat, start: usize) -> Vec<f64> {
    let mut dist = vec![f64::INFINITY; f.out.len()];
    let mut heap = BinaryHeap::new();

    dist[start] = 0.0;
    heap.push(Reverse((OrdF64(0.0), start)));

    while let Some(Reverse((OrdF64(d), u))) = heap.pop() {
        if d > dist[u] {
            continue;
        }
        for &(v, w) in &f.out[u] {
            let next = d + w;
            if next < dist[v] {
                dist[v] = next;
                heap.push(Reverse((OrdF64(next), v)));
            }
        }
    }
    dist
}

/// `algorithms::OrdF64`, which is private there.
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

// ------------------------------------------------------------------- timing

/// Minimum of `reps` runs. The minimum rather than the mean because the thing
/// being separated is two implementations, and scheduler noise only ever adds.
fn best<T>(reps: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut out = None;
    let mut lo = Duration::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        let v = f();
        let e = t.elapsed();
        if e < lo {
            lo = e;
        }
        out = Some(v);
    }
    (out.unwrap(), lo)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
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

// -------------------------------------------------------------- the interior

struct Row {
    nodes: usize,
    edges: usize,
    mib: f64,
    crate_louvain: Duration,
    dense_build: Duration,
    dense_louvain: Duration,
    dense_back: Duration,
    crate_dijkstra: Duration,
    dense_dijkstra_run: Duration,
    dense_dijkstra_back: Duration,
    crate_scc: Duration,
    crate_k_core: Duration,
}

fn measure(communities: usize, ids: Ids) -> Row {
    let g = clustered(communities, ids);
    let reps = if g.node_count() > 4_000 { 1 } else { 3 };
    let start = ids.node(0);

    // ---- Louvain, the finding's worst case.
    let (crate_answer, crate_louvain) = best(reps, || louvain(&g));

    let ((flat, id_table), dense_build) = best(reps, || Flat::from_subgraph(&g));
    let (raw, dense_louvain) = best(reps, || flat_renumber(&flat_louvain(&flat)));
    let (dense_answer, dense_back) = best(reps, || -> BTreeMap<String, usize> {
        id_table.iter().cloned().zip(raw.iter().copied()).collect()
    });

    assert!(
        same_partition(&crate_answer, &dense_answer),
        "the dense arm diverged from `louvain` at {communities} communities, so \
         the timings below would be comparing two algorithms"
    );

    // ---- Dijkstra, a different shape: the strings are in the heap, not just
    // ---- in the adjacency lookup.
    let (crate_dist, crate_dijkstra) = best(reps, || dijkstra(&g, &start));
    let (raw_dist, dense_dijkstra_run) = best(reps, || flat_dijkstra(&flat, 0));
    let (dense_dist, dense_dijkstra_back) = best(reps, || -> BTreeMap<String, f64> {
        id_table
            .iter()
            .zip(raw_dist.iter())
            .filter(|(_, d)| d.is_finite())
            .map(|(id, d)| (id.clone(), *d))
            .collect()
    });

    assert!(
        crate_dist == dense_dist,
        "the dense Dijkstra diverged from the crate's at {communities} communities"
    );

    // ---- The rest of the module, crate-only, so the two measured algorithms
    // ---- can be read as a share of what analytics costs.
    let (_, crate_scc) = best(reps, || scc(&g));
    let (_, crate_k_core) = best(reps, || k_core(&g, 3));

    Row {
        nodes: g.node_count(),
        edges: g.edge_count(),
        mib: g.estimated_bytes() as f64 / (1 << 20) as f64,
        crate_louvain,
        dense_build,
        dense_louvain,
        dense_back,
        crate_dijkstra,
        dense_dijkstra_run,
        dense_dijkstra_back,
        crate_scc,
        crate_k_core,
    }
}

// ------------------------------------------------- what the interior is part of

/// Out-degree of the fixture the share is measured on. See `seed`.
const DEG: usize = 8;

/// A deterministic degree-`DEG` graph, imported once and loaded from repeatedly.
///
/// **The chain of cliques above is the wrong fixture for this question.**
/// Reaching all of it takes one hop per community, and a 256-deep recursive
/// walk is not a shape any caller runs; timing one would inflate the load and
/// flatter the conclusion. [`Database::load_subgraph`]'s own rustdoc names the
/// realistic case instead — *"a hub node in a dense graph can reach most of the
/// database in three hops"* — and that is what this builds. The stride is a
/// coprime multiple so the neighbourhood expands rather than folding back on
/// itself.
async fn seed(path: &std::path::Path, nodes: usize) -> (Database, usize) {
    let db = Database::open_with_cadence(path, None).await.unwrap();

    let concepts: Vec<_> = (0..nodes)
        .map(|i| ConceptUpsert::new(Ids::Ulid.node(i), "N").valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();

    let mut edges = Vec::new();
    for i in 0..nodes {
        for k in 1..=DEG {
            let j = (i * 7919 + k * 131) % nodes;
            if j != i {
                edges.push(
                    EdgeAssertion::new(Ids::Ulid.node(i), Ids::Ulid.node(j), "KNOWS")
                        .valid_from(TS)
                        .valid_to(OPEN),
                );
            }
        }
    }
    let count = edges.len();
    db.bulk_import(edges).await.unwrap();
    (db, count)
}

/// Time one load against the algorithms over what it returned.
async fn share(db: &Database, hops: u32) {
    let start = Ids::Ulid.node(0);

    let t = Instant::now();
    let g = db.load_subgraph(&start, hops, TS, BUDGET).await.unwrap();
    let load = t.elapsed();

    let t = Instant::now();
    let comm = louvain(&g);
    let lv = t.elapsed();
    let t = Instant::now();
    let _ = dijkstra(&g, &start);
    let dj = t.elapsed();
    let t = Instant::now();
    let _ = scc(&g);
    let sc = t.elapsed();
    let t = Instant::now();
    let _ = k_core(&g, 3);
    let kc = t.elapsed();

    let algos = lv + dj + sc + kc;
    println!(
        "{:>5} {:>8} {:>9} {:>10} {:>10} {:>8} {:>8} {:>8} {:>8} {:>8}",
        hops,
        g.node_count(),
        g.edge_count(),
        format!("{:.1}", ms(load)),
        format!("{:.1}", ms(algos)),
        format!(
            "{:.1}%",
            100.0 * algos.as_secs_f64() / (load + algos).as_secs_f64()
        ),
        format!("{:.1}", ms(lv)),
        format!("{:.1}", ms(dj)),
        format!("{:.1}", ms(sc)),
        format!("{:.1}", ms(kc)),
    );
    let _ = comm.len();
}

#[tokio::main]
async fn main() {
    // The ceiling, so the sweep reaches the largest graph the budget admits and
    // §2.5's "10K-node subgraph" is inside it rather than extrapolated to.
    let mut communities = 64;
    let mut ceiling = communities;
    loop {
        let g = clustered(communities, Ids::Ulid);
        if g.estimated_bytes() > BUDGET {
            break;
        }
        ceiling = communities;
        communities *= 2;
        if communities > 1 << 14 {
            break;
        }
    }

    let mut sizes: Vec<usize> = vec![4, 16, 64, 256];
    while *sizes.last().unwrap() * 4 <= ceiling {
        let n = sizes.last().unwrap() * 4;
        sizes.push(n);
    }
    if sizes.last() != Some(&ceiling) {
        sizes.push(ceiling);
    }

    println!(
        "chain of {CLUSTER_SIZE}-cliques, {} MiB budget, ceiling {} communities\n\
         dense arm = build the CSR view + run on integers + translate back to \
         BTreeMap<String, _>\n",
        BUDGET >> 20,
        ceiling,
    );

    for ids in [Ids::Short, Ids::Ulid] {
        println!("===== ids: {} =====", ids.name());
        println!(
            "{:>7} {:>8} {:>7} | {:>9} {:>8} {:>8} {:>8} {:>8} {:>6} {:>6} | {:>9} {:>8} {:>8} {:>6} {:>6} | {:>8} {:>8}",
            "nodes",
            "edges",
            "MiB",
            "louvain",
            "build",
            "run",
            "back",
            "dense",
            "x",
            "ceil",
            "dijkstra",
            "run",
            "back",
            "x",
            "ceil",
            "scc",
            "k_core",
        );
        for &c in &sizes {
            let r = measure(c, ids);
            let dense_total = r.dense_build + r.dense_louvain + r.dense_back;
            let dj_total = r.dense_build + r.dense_dijkstra_run + r.dense_dijkstra_back;
            println!(
                "{:>7} {:>8} {:>7.2} | {:>9.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>6.2} {:>6.1} | {:>9.2} {:>8.2} {:>8.2} {:>6.2} {:>6.1} | {:>8.2} {:>8.2}",
                r.nodes,
                r.edges,
                r.mib,
                ms(r.crate_louvain),
                ms(r.dense_build),
                ms(r.dense_louvain),
                ms(r.dense_back),
                ms(dense_total),
                r.crate_louvain.as_secs_f64() / dense_total.as_secs_f64(),
                r.crate_louvain.as_secs_f64()
                    / (r.dense_louvain + r.dense_back).as_secs_f64(),
                ms(r.crate_dijkstra),
                ms(r.dense_dijkstra_run),
                ms(r.dense_dijkstra_back),
                r.crate_dijkstra.as_secs_f64() / dj_total.as_secs_f64(),
                r.crate_dijkstra.as_secs_f64()
                    / (r.dense_dijkstra_run + r.dense_dijkstra_back).as_secs_f64(),
                ms(r.crate_scc),
                ms(r.crate_k_core),
            );
        }
        println!();
    }

    println!(
        "`x` is the crate / the same work built at the **boundary**: build + run + back.\n\
         Since 0.13.28 the crate runs a dense interior of its own, so x below 1 is the\n\
         boundary form staying rejected -- it pays one string lookup per edge endpoint\n\
         where the in-crate build pays one per node. `ceil` strikes the boundary build\n\
         out entirely: it was the upper bound on the rewrite, and what is left of it is\n\
         the headroom still on the table. `back` is in both arms and cannot leave\n\
         either: the return type is BTreeMap<String, _>, and that is the public\n\
         signature.\n"
    );

    println!("===== what the interior is a share of =====");
    let dir =
        std::env::temp_dir().join(format!("macrame_subgraph_interior_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    const CORPUS: usize = 10_000;
    let (db, imported) = seed(&dir.join("share.db"), CORPUS).await;
    println!(
        "  {CORPUS} concepts, out-degree {DEG}, {imported} edges imported; \
         neighbourhoods of node 0\n"
    );
    println!(
        "{:>5} {:>8} {:>9} {:>10} {:>10} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "hops",
        "nodes",
        "edges",
        "load ms",
        "algos ms",
        "algos %",
        "louvain",
        "dijkstra",
        "scc",
        "k_core",
    );
    for hops in [2u32, 3, 4] {
        share(&db, hops).await;
    }
    db.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    println!(
        "\n`algos %` is the share of the caller's wall clock the interior can act on at all.\n\
         It was 34%-64% against the string-keyed interior and is 10%-25% against the\n\
         dense one, which is the same work measured from the other side of 0.13.28.\n\
         Almost all of it is still `louvain`, exactly as section 2.5 said."
    );
}
