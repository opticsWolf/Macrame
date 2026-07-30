mod harness;

use harness::TestHarness;
use macrame::graph::builder::{AttributeMode, TraversalBuilder};
use macrame::graph::{dijkstra, k_core, louvain, modularity, scc, EdgeRef, Subgraph};
use macrame::schema::migrations;
use macrame::{Database, DbError};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Build a graph directly, without a database, for the pure algorithm tests.
fn graph_of(edges: &[(&str, &str, f64)]) -> Subgraph {
    let mut g = Subgraph::default();
    for (s, t, w) in edges {
        for id in [s, t] {
            g.nodes.entry(id.to_string()).or_insert(macrame::graph::NodeData {
                title: id.to_string(),
                content: String::new(),
                embedding_model: None,
                valid_from: T0.to_string(),
                valid_to: OPEN.to_string(),
            });
        }
        let fwd = EdgeRef {
            node: t.to_string(),
            edge_type: "KNOWS".to_string(),
            weight: *w,
            valid_from: T0.to_string(),
            valid_to: OPEN.to_string(),
        };
        let mut back = fwd.clone();
        back.node = s.to_string();
        g.out_adj.entry(s.to_string()).or_default().push(fwd);
        g.in_adj.entry(t.to_string()).or_default().push(back);
    }
    g
}

async fn seeded_db(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    db
}

#[test]
fn test_cte_sql_compilation() {
    let builder = TraversalBuilder::new("node_100")
        .max_depth(4)
        .min_weight(0.5)
        .attribute_mode(AttributeMode::Current);

    let sql = builder.build_sql();
    assert!(sql.contains("WITH RECURSIVE walk"));
    // Delimited on both sides, so the check matches a whole path element rather
    // than a substring — ids are variable-length (D-061), and `INSTR(path, id)`
    // was only correct while they were not.
    assert!(sql.contains("INSTR(w.path, '/' || CAST(l.target_id AS BLOB) || '/') = 0"));
    assert!(
        sql.contains("SELECT ?1, 0, '/' || CAST(?1 AS BLOB) || '/'"),
        "the seed must carry both delimiters or the first hop cannot match"
    );
    assert!(sql.contains("w.depth < ?2"));
}

/// Edge types reach the CTE as bind parameters, never as SQL text.
///
/// The traversal builder is on the read path, where nothing validates edge
/// types — `validate_edge_type` runs from `EdgeAssertion::normalized`, which a
/// traversal never touches. So the compiled SQL must contain a placeholder and
/// must not contain the caller's string at all.
#[test]
fn a_hostile_edge_type_never_reaches_the_compiled_sql() {
    let hostile = "A') OR 1=1 --";
    let sql = TraversalBuilder::new("start")
        .edge_types(vec![hostile.to_string(), "KNOWS".to_string()])
        .build_sql();

    assert!(
        !sql.contains("OR 1=1"),
        "caller string was interpolated into SQL: {sql}"
    );
    assert!(!sql.contains(hostile));
    assert!(
        sql.contains("l.edge_type IN (?5, ?6)"),
        "expected two bind placeholders, got: {sql}"
    );
}

#[test]
fn no_edge_types_means_no_filter_clause() {
    let sql = TraversalBuilder::new("start").build_sql();
    assert!(!sql.contains("edge_type IN"));
}

#[tokio::test]
async fn test_traversal_execution_and_subgraph_bridge() {
    let harness = TestHarness::new();
    let db = seeded_db(&harness).await;
    let conn = db.read_conn();

    {
        // Seed through a plain connection: these tests are about the read path.
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let w = raw.connect().unwrap();
        migrations::run(&w).await.unwrap();
        for id in ["A", "B", "C"] {
            w.execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, ?2, ?3, ?3)",
                libsql::params![id, format!("Node {id}"), T0],
            )
            .await
            .unwrap();
        }
        for (s, t, wt) in [("A", "B", 0.8), ("B", "C", 0.9)] {
            w.execute(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
                 VALUES (?1, ?2, 'KNOWS', ?3, ?4, ?5, '{}', ?3)",
                libsql::params![s, t, T0, OPEN, wt],
            )
            .await
            .unwrap();
        }
    }

    let results = TraversalBuilder::new("A")
        .max_depth(2)
        .execute(conn, T0)
        .await
        .unwrap();
    assert_eq!(results.len(), 3, "A, B and C are reachable");

    let g = db.load_subgraph("A", 2, T0, 1 << 20).await.unwrap();
    assert_eq!(g.nodes.len(), 3);
    assert_eq!(g.edge_count(), 2);

    let distances = dijkstra(&g, "A");
    // Path costs are accumulated f64 sums, so compare within tolerance:
    // 0.8 + 0.9 is 1.7000000000000002 in binary floating point.
    let close = |got: f64, want: f64| (got - want).abs() < 1e-9;
    assert!(close(distances["A"], 0.0));
    assert!(close(distances["B"], 0.8));
    assert!(close(distances["C"], 1.7));

    db.close().await.unwrap();
}

/// `AttributeMode::Omit` must actually omit.
///
/// The builder stored `attribute_mode` and never read it, so all three modes
/// returned live attributes. A mode that is accepted and ignored is worse than
/// one that is unsupported.
#[tokio::test]
async fn attribute_mode_omit_returns_topology_only() {
    let harness = TestHarness::new();
    let db = seeded_db(&harness).await;

    {
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let w = raw.connect().unwrap();
        w.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('A', 'Node A', ?1, ?1)",
            libsql::params![T0],
        )
        .await
        .unwrap();
    }

    let conn = db.read_conn();
    let current = TraversalBuilder::new("A")
        .attribute_mode(AttributeMode::Current)
        .execute(conn, T0)
        .await
        .unwrap();
    assert_eq!(current.len(), 1);

    let omitted = TraversalBuilder::new("A")
        .attribute_mode(AttributeMode::Omit)
        .execute(conn, T0)
        .await
        .unwrap();
    assert!(omitted.is_empty(), "Omit must not hydrate attributes");

    db.close().await.unwrap();
}

/// A negative weight is refused at load rather than producing a wrong path.
#[tokio::test]
async fn a_negative_edge_weight_is_refused_at_load() {
    let harness = TestHarness::new();
    let db = seeded_db(&harness).await;

    {
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let w = raw.connect().unwrap();
        for id in ["A", "B"] {
            w.execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, ?1, ?2, ?2)",
                libsql::params![id, T0],
            )
            .await
            .unwrap();
        }
        w.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
             VALUES ('A', 'B', 'KNOWS', ?1, ?2, -1.5, '{}', ?1)",
            libsql::params![T0, OPEN],
        )
        .await
        .unwrap();
    }

    let err = db.load_subgraph("A", 2, T0, 1 << 20).await.unwrap_err();
    match err {
        DbError::NegativeEdgeWeight {
            source_id,
            target_id,
            weight,
        } => {
            assert_eq!((source_id.as_str(), target_id.as_str()), ("A", "B"));
            assert_eq!(weight, -1.5);
        }
        other => panic!("expected NegativeEdgeWeight, got {other:?}"),
    }

    db.close().await.unwrap();
}

/// The byte budget is enforced, and `SubgraphTooLarge` is reachable.
#[tokio::test]
async fn the_byte_budget_stops_an_oversized_load() {
    let harness = TestHarness::new();
    let db = seeded_db(&harness).await;

    {
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let w = raw.connect().unwrap();
        for i in 0..40 {
            w.execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, ?1, ?2, ?2)",
                libsql::params![format!("n{i:03}"), T0],
            )
            .await
            .unwrap();
        }
        for i in 0..39 {
            w.execute(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
                 VALUES (?1, ?2, 'KNOWS', ?3, ?4, 1.0, '{}', ?3)",
                libsql::params![format!("n{i:03}"), format!("n{:03}", i + 1), T0, OPEN],
            )
            .await
            .unwrap();
        }
    }

    let err = db.load_subgraph("n000", 40, T0, 256).await.unwrap_err();
    assert!(
        matches!(err, DbError::SubgraphTooLarge { .. }),
        "expected SubgraphTooLarge, got {err:?}"
    );

    // The same load succeeds with room to work in.
    let g = db.load_subgraph("n000", 40, T0, 1 << 20).await.unwrap();
    assert_eq!(g.edge_count(), 39);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Pure algorithm tests. No database, no clock.
// ---------------------------------------------------------------------------

#[test]
fn dijkstra_prefers_the_cheaper_of_two_routes() {
    // A->B->D costs 2, A->C->D costs 11. The direct-looking route is the dear one.
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "D", 1.0),
        ("A", "C", 10.0),
        ("C", "D", 1.0),
    ]);
    let d = dijkstra(&g, "A");
    assert_eq!(d["D"], 2.0);
    assert_eq!(d["C"], 10.0);
}

#[test]
fn dijkstra_omits_unreachable_nodes_rather_than_reporting_infinity() {
    let g = graph_of(&[("A", "B", 1.0), ("C", "D", 1.0)]);
    let d = dijkstra(&g, "A");
    assert!(d.contains_key("A") && d.contains_key("B"));
    assert!(!d.contains_key("C"), "C is unreachable from A");
}

#[test]
fn astar_finds_the_same_cost_as_dijkstra_and_returns_the_path() {
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "D", 1.0),
        ("A", "C", 10.0),
        ("C", "D", 1.0),
    ]);
    let (cost, path) = macrame::graph::astar(&g, "A", "D", |_, _| 0.0).unwrap();
    assert_eq!(cost, dijkstra(&g, "A")["D"]);
    assert_eq!(path, vec!["A", "B", "D"]);
}

#[test]
fn astar_returns_none_when_the_goal_is_unreachable() {
    let g = graph_of(&[("A", "B", 1.0), ("C", "D", 1.0)]);
    assert!(macrame::graph::astar(&g, "A", "D", |_, _| 0.0).is_none());
    assert!(macrame::graph::astar(&g, "A", "nobody", |_, _| 0.0).is_none());
}

#[test]
fn scc_finds_the_cycle_and_leaves_singletons_alone() {
    // A->B->C->A is one component; D hangs off it alone.
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("C", "A", 1.0),
        ("C", "D", 1.0),
    ]);
    let comps = scc(&g);
    assert_eq!(comps.len(), 2);
    assert!(comps.contains(&vec!["A".to_string(), "B".to_string(), "C".to_string()]));
    assert!(comps.contains(&vec!["D".to_string()]));
}

/// A chain far longer than a comfortable recursion depth still resolves.
///
/// Traversal depth lives on the heap here, not the call stack, so the limit is
/// memory rather than stack size. 20k nodes is chosen as clearly past the depth
/// at which a frame-per-node DFS becomes a risk on a default 8 MiB stack; this
/// pins that the iterative form has no such ceiling, not that a recursive one
/// would fail at exactly this size.
#[test]
fn scc_survives_a_chain_far_deeper_than_the_call_stack() {
    let ids: Vec<String> = (0..20_000).map(|i| format!("n{i:06}")).collect();
    let pairs: Vec<(&str, &str, f64)> = ids
        .windows(2)
        .map(|w| (w[0].as_str(), w[1].as_str(), 1.0))
        .collect();
    let g = graph_of(&pairs);

    let comps = scc(&g);
    assert_eq!(comps.len(), 20_000, "a simple chain is all singletons");
}

#[test]
fn k_core_peels_the_fringe_and_keeps_the_dense_middle() {
    // A triangle with two pendant nodes hanging off it.
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("C", "A", 1.0),
        ("A", "X", 1.0),
        ("B", "Y", 1.0),
    ]);
    let core = k_core(&g, 2);
    assert!(core.contains("A") && core.contains("B") && core.contains("C"));
    assert!(!core.contains("X") && !core.contains("Y"));
}

/// Degree bookkeeping lands on zero exactly, never below it.
///
/// `k_core` subtracts with `-=` on a `usize`, so this is an assertion as much as
/// a test: it holds only because each edge contributes exactly one decrement to
/// each of its endpoints. Self-loops and parallel edges are the cases where that
/// accounting is easiest to get wrong, and peeling everything is what forces
/// every decrement to be taken.
#[test]
fn k_core_degree_accounting_is_exact_under_self_loops_and_parallel_edges() {
    for spec in [
        vec![("A", "A", 1.0)],
        vec![("A", "A", 1.0), ("A", "A", 1.0)],
        vec![("A", "B", 1.0), ("B", "A", 1.0)],
        vec![("A", "B", 1.0), ("A", "B", 1.0), ("B", "A", 1.0)],
        vec![("A", "A", 1.0), ("A", "B", 1.0), ("B", "B", 1.0), ("B", "A", 1.0)],
    ] {
        let g = graph_of(&spec);
        // k above every possible degree, so every node is peeled and every
        // decrement in the graph is executed.
        assert!(
            k_core(&g, 99).is_empty(),
            "nothing survives a 99-core of {spec:?}"
        );
    }
}

#[test]
fn k_core_of_zero_keeps_everything() {
    let g = graph_of(&[("A", "B", 1.0)]);
    assert_eq!(k_core(&g, 0).len(), 2);
}

/// Louvain must beat the partition it starts from, not merely equal it.
///
/// The previous implementation numbered every node into its own community and
/// returned. That satisfies "modularity did not decrease from the singleton
/// partition" by *being* the singleton partition, so the obvious test passes
/// against a detector that does nothing. Measuring Q against two barbells is
/// what separates the two.
#[test]
fn louvain_beats_the_singleton_partition_it_starts_from() {
    // Two triangles joined by a single thin edge: an unambiguous split.
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("C", "A", 1.0),
        ("D", "E", 1.0),
        ("E", "F", 1.0),
        ("F", "D", 1.0),
        ("C", "D", 0.05),
    ]);

    let comms = louvain(&g);
    let singletons: std::collections::BTreeMap<String, usize> =
        g.nodes.keys().enumerate().map(|(i, n)| (n.clone(), i)).collect();

    let q = modularity(&g, &comms);
    let q0 = modularity(&g, &singletons);
    assert!(
        q > q0 + 1e-9,
        "louvain must strictly improve on singletons: {q} vs {q0}"
    );

    // And the split it finds is the obvious one.
    let distinct: std::collections::BTreeSet<usize> = comms.values().copied().collect();
    assert_eq!(distinct.len(), 2, "expected two communities, got {comms:?}");
    assert_eq!(comms["A"], comms["B"]);
    assert_eq!(comms["B"], comms["C"]);
    assert_eq!(comms["D"], comms["E"]);
    assert_eq!(comms["E"], comms["F"]);
    assert_ne!(comms["A"], comms["D"]);
}

#[test]
fn louvain_on_an_edgeless_graph_is_all_singletons() {
    let mut g = Subgraph::default();
    for id in ["A", "B"] {
        g.nodes.insert(
            id.to_string(),
            macrame::graph::NodeData {
                title: id.to_string(),
                content: String::new(),
                embedding_model: None,
                valid_from: T0.to_string(),
                valid_to: OPEN.to_string(),
            },
        );
    }
    let comms = louvain(&g);
    assert_eq!(comms.len(), 2);
    assert_ne!(comms["A"], comms["B"]);
}

/// The whole point of the BTreeMap choice: same graph, same answer, every run.
#[test]
fn the_algorithms_are_deterministic_across_repeated_runs() {
    let g = graph_of(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("C", "A", 1.0),
        ("D", "E", 1.0),
        ("E", "F", 1.0),
        ("F", "D", 1.0),
        ("C", "D", 0.05),
    ]);

    let first_l = louvain(&g);
    let first_s = scc(&g);
    let first_d = dijkstra(&g, "A");
    let first_k = k_core(&g, 2);

    for _ in 0..64 {
        assert_eq!(louvain(&g), first_l);
        assert_eq!(scc(&g), first_s);
        assert_eq!(dijkstra(&g, "A"), first_d);
        assert_eq!(k_core(&g, 2), first_k);
    }
}

// -- D-047: the byte budget is enforced in linear time ----------------------

/// Seed a star: `centre` -> N leaves, all concepts present.
async fn star(harness: &TestHarness, n: usize) -> Database {
    use macrame::prelude::{ConceptUpsert, EdgeAssertion};
    let db = Database::open(&harness.db_path).await.unwrap();
    let mut concepts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        concepts.push(ConceptUpsert::new(format!("N{i:07}"), format!("node {i}")).valid_from(T0));
    }
    db.write_annotations(concepts).await.unwrap();
    let mut edges = Vec::with_capacity(n);
    for i in 1..=n {
        edges.push(EdgeAssertion::new("N0000000", format!("N{i:07}"), "KNOWS").valid_from(T0));
    }
    db.bulk_import(edges).await.unwrap();
    db
}

/// **The running total and the derivation must be the same number.**
///
/// `load_subgraph` accumulates payload bytes as it inserts rather than
/// recomputing the whole graph per row (D-047). That makes the budget check a
/// second account of a quantity `estimated_bytes()` also computes, and two
/// accounts of one quantity drift — which is the failure D-035 is about. They
/// share the per-item functions so they cannot disagree by construction; this
/// asserts it anyway, because "cannot disagree" is a claim about code that
/// changes.
#[tokio::test]
async fn the_loaders_running_total_agrees_with_the_derivation() {
    let harness = TestHarness::new();
    let db = star(&harness, 40).await;

    let g = db.load_subgraph("N0000000", 2, T0, 1 << 30).await.unwrap();
    let derived = g.estimated_bytes();

    // Budget one byte under the derived total: if the running total the loader
    // checks were smaller (an undercount — e.g. counting an edge once when it
    // is stored twice), the load would succeed and this would fail.
    let err = db
        .load_subgraph("N0000000", 2, T0, derived - 1)
        .await
        .unwrap_err();
    match err {
        DbError::SubgraphTooLarge { n, budget } => {
            assert_eq!(budget, derived - 1);
            assert!(
                n > budget,
                "reported total {n} does not exceed the budget {budget}"
            );
        }
        other => panic!("expected SubgraphTooLarge, got {other:?}"),
    }
}

/// **The regression test for the quadratic loader.**
///
/// `estimated_bytes()` is O(V + E) and used to be called once per row, so
/// loading was O(E²): 500 edges in 26 ms, 1,000 in 76 ms, 2,000 in 231 ms —
/// time tripling for each doubling of the input. The byte budget is what bounds
/// a load, and the budget *check* was the part that did not scale.
///
/// Asserted as a ratio rather than an absolute duration: absolute timings are a
/// property of the machine, the growth rate is a property of the algorithm.
///
/// The sizes are 8x apart, and that spread is deliberate. At 4x the quadratic
/// term does not yet dominate — the mutation (reinstating the per-row
/// `estimated_bytes()`) measured ~8x against a linear expectation of ~4x, which
/// no bound loose enough to survive CI noise would catch. At 8x it is ~26x
/// against ~8x, and the bound below sits between them with room on both sides.
/// Verified by running the mutation, not by choosing a number that looked safe.
#[tokio::test]
async fn loading_scales_linearly_in_the_number_of_edges() {
    let small = TestHarness::new();
    let db_s = star(&small, 250).await;
    let large = TestHarness::new();
    let db_l = star(&large, 2000).await;

    // One untimed load each: the first touches cold pages the second does not.
    db_s.load_subgraph("N0000000", 2, T0, 1 << 30).await.unwrap();
    db_l.load_subgraph("N0000000", 2, T0, 1 << 30).await.unwrap();

    let t = std::time::Instant::now();
    db_s.load_subgraph("N0000000", 2, T0, 1 << 30).await.unwrap();
    let small_ns = t.elapsed().as_nanos().max(1);

    let t = std::time::Instant::now();
    db_l.load_subgraph("N0000000", 2, T0, 1 << 30).await.unwrap();
    let large_ns = t.elapsed().as_nanos().max(1);

    let ratio = large_ns as f64 / small_ns as f64;
    assert!(
        ratio < 16.0,
        "8x the edges took {ratio:.1}x the time ({small_ns} ns -> {large_ns} ns); \
         linear is ~8x and quadratic ~26x — the per-row byte check is back"
    );
}
