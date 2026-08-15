#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::builder::{AttributeMode, TraversalBuilder};
use macrame::graph::{dijkstra, k_core, louvain, modularity, scc, Subgraph};
use macrame::schema::migrations;
use macrame::{Database, DbError};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Build a graph directly, without a database, for the pure algorithm tests.
fn graph_of(edges: &[(&str, &str, f64)]) -> Subgraph {
    let mut g = Subgraph::default();
    for (s, t, w) in edges {
        for id in [s, t] {
            if !g.contains_node(id) {
                g.insert_node(
                    id.to_string(),
                    macrame::graph::NodeData::new(id.to_string(), T0.to_string(), OPEN.to_string()),
                );
            }
        }
        g.add_edge(s, t, "KNOWS", *w, T0, OPEN);
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
    assert!(sql.contains("WITH RECURSIVE walk(node_id, depth)"));
    assert!(sql.contains("w.depth < ?2"));

    // T0.1: `UNION`, not `UNION ALL`. This is the whole optimisation — `UNION`
    // dedupes on `(node_id, depth)` as rows enter the queue, so `walk` is bounded
    // by V × (depth+1). With `UNION ALL` it holds one row per distinct *path*,
    // which is multiplicative in branching factor per hop: 328 edges at depth 6
    // measured 299,593 rows and 428 ms before this changed.
    assert!(
        sql.contains("UNION\n") && !sql.contains("UNION ALL"),
        "the walk must dedupe on entry, or it enumerates paths: {sql}"
    );

    // The path column and its cycle check are gone, and must stay gone. They were
    // what restricted the walk to simple paths; termination is the depth bound
    // now. Asserted as absence because that is the regression that would be
    // invisible — reinstating them returns correct answers, slowly.
    assert!(
        !sql.contains("path") && !sql.contains("INSTR"),
        "the path column and INSTR cycle check must not come back: {sql}"
    );
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
    assert_eq!(g.node_count(), 3);
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
///
/// **Planted in `links_current`, not in `links` (T2.1, D-083).** Since v7 the
/// hot ledger table carries `CHECK (weight >= 0.0 AND weight < 9e999 AND
/// typeof(weight) = 'real')`, so this test can no longer write its own fixture
/// there — which is the change working. `NegativeEdgeWeight` is *not* thereby unreachable, and this is the
/// shape of the case that keeps it reachable: `links_current` is derivative and
/// carries no such CHECK, so a projection built from a pre-v7 `links`, or a cold
/// file written before the rung, still reaches the loader with a negative
/// weight. That is the division of labour §4.7 describes and the reason the
/// guard stays.
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
            "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
             VALUES ('A', 'B', 'KNOWS', ?1, ?2, -1.5, '{}', ?1)",
            libsql::params![T0, OPEN],
        )
        .await
        .expect(
            "links_current carries no weight CHECK by design — if this now \
             fails, the constraint has been added there too and the guard's \
             remaining reachable cases are cold files alone",
        );
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
        vec![
            ("A", "A", 1.0),
            ("A", "B", 1.0),
            ("B", "B", 1.0),
            ("B", "A", 1.0),
        ],
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
    let singletons: std::collections::BTreeMap<String, usize> = g
        .node_ids()
        .enumerate()
        .map(|(i, n)| (n.to_string(), i))
        .collect();

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
        g.insert_node(
            id.to_string(),
            macrame::graph::NodeData::new(id.to_string(), T0.to_string(), OPEN.to_string()),
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
    db.write_concepts(concepts).await.unwrap();
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
    db_s.load_subgraph("N0000000", 2, T0, 1 << 30)
        .await
        .unwrap();
    db_l.load_subgraph("N0000000", 2, T0, 1 << 30)
        .await
        .unwrap();

    let t = std::time::Instant::now();
    db_s.load_subgraph("N0000000", 2, T0, 1 << 30)
        .await
        .unwrap();
    let small_ns = t.elapsed().as_nanos().max(1);

    let t = std::time::Instant::now();
    db_l.load_subgraph("N0000000", 2, T0, 1 << 30)
        .await
        .unwrap();
    let large_ns = t.elapsed().as_nanos().max(1);

    let ratio = large_ns as f64 / small_ns as f64;
    assert!(
        ratio < 16.0,
        "8x the edges took {ratio:.1}x the time ({small_ns} ns -> {large_ns} ns); \
         linear is ~8x and quadratic ~26x — the per-row byte check is back"
    );
}

// ---------------------------------------------------------------------------
// T3.2 / D-085 — a historical traversal must state which text it wants
// ---------------------------------------------------------------------------

/// The wrong answer the boundary exists to prevent, demonstrated and then refused.
///
/// This is the test that makes T3.2 more than a signature change. It renames a
/// concept *after* the instant being asked about, so the two attribute modes
/// genuinely disagree — without that, every mode returns the same string and the
/// whole question is invisible, which is precisely how this survived to 0.6.0.
///
/// Three assertions, in the order that matters:
///
///   1. defaulted mode + `as_of` is an **error**, not a guess;
///   2. `AtTime` returns the title as it was — the answer most callers meant;
///   3. `Current` returns today's title — still available, now stated.
#[tokio::test]
async fn a_historical_traversal_must_say_which_titles_it_wants() {
    // The clock starts at the valid-time instant the fixture uses, because the
    // single `as_of` value has to satisfy two different axes at once and the
    // default epoch-1970 clock makes that impossible to arrange. The topology
    // filter is **valid** time (`valid_from <= ts < valid_to`), while `AtTime`
    // hydration is **transaction** time (what was believed as of `ts`). One
    // parameter, two clocks — Doctrine II, met in a test fixture.
    let tuesday = "2026-01-06T00:00:00.000000Z";
    let harness =
        TestHarness::starting_at(macrame::util::clock::parse_iso8601_utc(tuesday).unwrap());
    let db = harness.db_with_fake_clock().await;

    for (id, title) in [("a", "Original A"), ("b", "Original B")] {
        db.upsert_concept(macrame::ConceptUpsert::new(id, title).valid_from(tuesday))
            .await
            .unwrap();
    }
    db.assert_edge(
        macrame::graph::EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(tuesday)
            .valid_to(OPEN),
    )
    .await
    .unwrap();

    // The instant being asked about: after the original writes, before the
    // renames. Taken from the clock rather than written as a literal, so it is
    // a `recorded_at` the ledger actually straddles.
    let as_of = harness.clock.peek();

    // Later: the titles change. The topology does not.
    harness.advance(std::time::Duration::from_secs(86_400 * 7));
    let now = harness.clock.peek();
    for (id, title) in [("a", "Renamed A"), ("b", "Renamed B")] {
        db.upsert_concept(macrame::ConceptUpsert::new(id, title).valid_from(tuesday))
            .await
            .unwrap();
    }

    let conn = db.read_conn();

    // 1. The combination that used to be a warn! is now a value.
    let err = TraversalBuilder::new("a")
        .max_depth(2)
        .as_of(&as_of)
        .execute(conn, &now)
        .await
        .expect_err("as_of with a defaulted attribute mode must not guess");
    assert!(
        matches!(&err, DbError::AttributeModeUnstated { as_of: got } if *got == as_of),
        "got {err:?}"
    );

    // 2. What the caller almost certainly meant.
    let then = TraversalBuilder::new("a")
        .max_depth(2)
        .as_of(&as_of)
        .attribute_mode(AttributeMode::AtTime)
        .execute(conn, &now)
        .await
        .unwrap();
    let titles: Vec<&str> = then.iter().map(|n| n.title.as_str()).collect();
    assert!(
        titles.contains(&"Original A"),
        "AtTime must return the title as believed at the instant asked about, \
         got {titles:?}"
    );

    // 3. The fast, mixed answer — legitimate, and now impossible to get by
    //    accident. If this ever returns "Original A", `Current` has stopped
    //    meaning live and the error above is guarding nothing.
    let mixed = TraversalBuilder::new("a")
        .max_depth(2)
        .as_of(&as_of)
        .attribute_mode(AttributeMode::Current)
        .execute(conn, &now)
        .await
        .unwrap();
    let titles: Vec<&str> = mixed.iter().map(|n| n.title.as_str()).collect();
    assert!(
        titles.contains(&"Renamed A"),
        "Current must return live text, got {titles:?}"
    );

    db.close().await.unwrap();
}

/// A traversal about the present is unaffected, and that is the compatibility
/// claim T3.2 rests on.
///
/// If this needed a `.attribute_mode()` call, the change would be a breaking one
/// for every existing caller rather than for the one combination that was wrong.
#[tokio::test]
async fn a_live_traversal_needs_no_attribute_mode() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(macrame::ConceptUpsert::new("a", "A").valid_from(T0))
        .await
        .unwrap();

    let found = TraversalBuilder::new("a")
        .execute(db.read_conn(), T0)
        .await
        .expect("a query about now has nothing to disambiguate");
    assert_eq!(found.len(), 1);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Node order does not depend on construction order (0.8.0, B2, D-063's gate)
// ---------------------------------------------------------------------------
//
// **Pre-registered by D-063 before the change it guards, and landed before it.**
// D-063 declined the interning rewrite in 0.5.6 and wrote down what the real
// cost would be when it happened: *determinism stops being structural and
// becomes procedural.*
//
// Today a `Subgraph` keys everything by `String` in a `BTreeMap`, so iteration
// is in id order **because of the data structure** — nothing has to remember to
// sort. Once ids are interned to `u32`, iteration is in *index* order, and the
// indices are whatever the graph handed out as ids arrived. D-063 asked for a
// test rather than a comment saying to sort.
//
// # The first version of this test was vacuous, and its own guard caught it
//
// It built the same graph in two databases with the edges **inserted in reverse
// order**, on the assumption that row order follows insertion order. It does
// not: the walk goes through `idx_lc_traversal_cover` (D-042 pins that plan),
// so rows arrive in *index* order whatever order they were written in. Both
// databases scanned identically, and the test would have passed against any
// implementation — including the one it exists to catch.
//
// That is why the vacuity guard below is not decoration. A determinism test
// that does not actually vary the thing it claims to vary is the worst kind of
// green.
//
// # What varies here instead
//
// The **construction order of a hand-built graph**, which is a supported public
// path (`insert_node` / `add_edge`, public since B1) and is exactly where
// first-seen index assignment would bite. A graph built in descending id order
// is compared against the same graph loaded from the database in ascending
// order. Node order and every algorithm's answer must agree.
//
// # Why the failure would otherwise be silent
//
// A renumbering would not error. Louvain would return a *different valid
// partition* — it breaks ties by lowest community index, so renumbering moves
// the tie-break — and §8's property oracle would still pass, because that
// oracle is an **upper bound on modularity** and cannot tell one valid answer
// from another. The suite would stay green while the crate answered the same
// question two ways.

/// Two triangles joined by one edge: two communities of equal weight, so
/// Louvain's answer depends on visit order. A shape with no ties would not
/// exercise the thing being tested.
const DETERMINISM_EDGES: &[(&str, &str, f64)] = &[
    ("n1", "n2", 1.0),
    ("n2", "n3", 1.0),
    ("n3", "n1", 1.0),
    ("n4", "n5", 1.0),
    ("n5", "n6", 1.0),
    ("n6", "n4", 1.0),
    ("n3", "n4", 1.0),
];

/// Build the fixture by hand, offering nodes and edges in `order`.
fn hand_built(order: impl Fn(&mut Vec<&'static str>)) -> (Subgraph, Vec<&'static str>) {
    let mut ids: Vec<&str> = DETERMINISM_EDGES
        .iter()
        .flat_map(|(s, t, _)| [*s, *t])
        .collect();
    ids.sort();
    ids.dedup();
    order(&mut ids);

    let mut g = Subgraph::default();
    for id in &ids {
        g.insert_node(*id, macrame::graph::NodeData::new(*id, T0, OPEN));
    }
    let mut edges: Vec<&(&str, &str, f64)> = DETERMINISM_EDGES.iter().collect();
    if ids.first() > ids.last() {
        edges.reverse();
    }
    for (s, t, w) in edges {
        g.add_edge(s, t, "KNOWS", *w, T0, OPEN);
    }
    (g, ids)
}

/// Adjacency as a sorted set, because edge order *within* a node legitimately
/// differs between a walk and a hand build. What must not differ is the set of
/// edges and the order of the **nodes**.
fn adjacency(g: &Subgraph) -> Vec<(String, Vec<(String, String)>)> {
    g.node_ids()
        .map(|id| {
            let mut out: Vec<(String, String)> = g
                .out_edges(id)
                .iter()
                .map(|e| (e.node(g).to_string(), format!("{:?}", e.weight())))
                .collect();
            out.sort();
            (id.to_string(), out)
        })
        .collect()
}

#[tokio::test]
async fn node_order_does_not_depend_on_construction_order() {
    let (ascending, asc_ids) = hand_built(|_| {});
    let (descending, desc_ids) = hand_built(|ids| ids.reverse());

    // **Vacuity guard.** If the two builds did not actually differ in the order
    // ids were offered, this test varies nothing and passes against any
    // implementation. An earlier version of it did exactly that.
    assert_ne!(
        asc_ids, desc_ids,
        "both builds offered ids in the same order, so this test is not \
         exercising construction-order independence at all"
    );

    // The property itself: index order is id order, not arrival order.
    let a: Vec<&str> = ascending.node_ids().collect();
    let d: Vec<&str> = descending.node_ids().collect();
    assert_eq!(
        a, d,
        "node order followed the order ids were offered in — a BTreeMap gave \
         id order for free and an interner has to be told"
    );
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(a, sorted, "node order is not id order");

    assert_eq!(adjacency(&ascending), adjacency(&descending));
    assert_eq!(ascending.estimated_bytes(), descending.estimated_bytes());

    // And the answers. Louvain is the one that moves silently.
    assert_eq!(
        louvain(&ascending),
        louvain(&descending),
        "Louvain returned a different partition for the same graph built in a \
         different order — the exact failure D-063 pre-registered, and one the \
         modularity oracle cannot see"
    );
    assert_eq!(dijkstra(&ascending, "n1"), dijkstra(&descending, "n1"));
    assert_eq!(scc(&ascending), scc(&descending));
    assert_eq!(k_core(&ascending, 2), k_core(&descending, 2));

    // Finally, against the loader — the graph the database produces must agree
    // with both hand-built ones, so the two paths cannot drift apart.
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let mut ids: Vec<&str> = DETERMINISM_EDGES
        .iter()
        .flat_map(|(s, t, _)| [*s, *t])
        .collect();
    ids.sort();
    ids.dedup();
    db.write_concepts(
        ids.iter()
            .map(|id| macrame::prelude::ConceptUpsert::new(*id, *id).valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();
    for (s, t, w) in DETERMINISM_EDGES {
        db.assert_edge(
            macrame::prelude::EdgeAssertion::new(*s, *t, "KNOWS")
                .valid_from(T0)
                .weight(*w),
        )
        .await
        .unwrap();
    }
    // Not `OPEN`: the predicate is `valid_from <= ts AND ts < valid_to`, so
    // querying *at* the open-interval sentinel matches nothing.
    let loaded = db.load_subgraph("n1", 6, T0, 1 << 22).await.unwrap();
    db.close().await.unwrap();

    assert_eq!(loaded.node_count(), 6, "fixture did not load as expected");
    assert_eq!(
        loaded.node_ids().collect::<Vec<_>>(),
        a,
        "the loaded graph orders its nodes differently from a hand-built one"
    );
    assert_eq!(louvain(&loaded), louvain(&ascending));
}

// ---------------------------------------------------------------------------
// Content is not loaded by default, and no algorithm misses it (0.8.0, B3, D-116)
// ---------------------------------------------------------------------------
//
// B3's claim is that `concepts.content` is dead weight in a loaded `Subgraph`:
// none of the six algorithms reads it, and at realistic document sizes it is
// the large majority of the byte budget. That is the one thing here a test can
// settle outright, so it does.
//
// The claim has two halves and both are asserted:
//
// * **the answers do not move** — every algorithm returns the same thing with
//   content loaded and not loaded, on a graph whose concepts carry real text;
// * **the budget does move** — `estimated_bytes()` is markedly smaller without
//   it, which is the entire reason for the change.
//
// The second is what makes the first worth having. Without it the test would
// pass trivially on a fixture whose content happened to be empty.

/// A concept body big enough that its absence is unmistakable in the budget,
/// and small enough that the fixture stays quick.
const CONTENT: &str = "lorem ipsum dolor sit amet, consectetur adipiscing elit; ";

async fn graph_with_content(harness: &TestHarness, want_content: bool) -> Subgraph {
    let db = Database::open(&harness.db_path).await.unwrap();

    let ids: Vec<String> = (0..24).map(|i| format!("c{i:04}")).collect();
    db.write_concepts(
        ids.iter()
            .map(|id| {
                macrame::prelude::ConceptUpsert::new(id.clone(), format!("title {id}"))
                    .content(CONTENT.repeat(40)) // ~2.2 KB per concept
                    .valid_from(T0)
            })
            .collect(),
    )
    .await
    .unwrap();

    for i in 0..ids.len() {
        for step in [1usize, 5] {
            let j = (i + step) % ids.len();
            if i != j {
                db.assert_edge(
                    macrame::prelude::EdgeAssertion::new(&ids[i], &ids[j], "KNOWS")
                        .valid_from(T0)
                        .weight(1.0),
                )
                .await
                .unwrap();
            }
        }
    }

    let traversal = TraversalBuilder::new("c0000")
        .max_depth(6)
        .content(want_content);
    let g = db
        .load_subgraph_with(&traversal, T0, 1 << 24)
        .await
        .unwrap();
    db.close().await.unwrap();
    g
}

#[tokio::test]
async fn content_is_absent_by_default_and_no_algorithm_notices() {
    let h1 = TestHarness::new();
    let h2 = TestHarness::new();
    let without = graph_with_content(&h1, false).await;
    let with = graph_with_content(&h2, true).await;

    // The two must be the same graph, or nothing below means anything.
    assert_eq!(without.node_count(), with.node_count());
    assert_eq!(without.edge_count(), with.edge_count());
    assert!(without.node_count() > 1, "fixture did not load");

    // **`None` is not `""`.** The distinction is the reason this is an `Option`
    // at all: a caller that did not ask can tell that apart from a concept
    // whose text really is empty.
    for id in without.node_ids() {
        assert_eq!(
            without.node(id).unwrap().content(),
            None,
            "the default load fetched content for {id}"
        );
        assert!(
            with.node(id)
                .unwrap()
                .content()
                .is_some_and(|c| !c.is_empty()),
            "the requesting load did not fetch content for {id}"
        );
    }

    // **The budget moves**, which is what the change is for — and what stops
    // the assertions above passing on an empty fixture.
    assert!(
        with.estimated_bytes() > without.estimated_bytes() * 2,
        "content is supposed to dominate this fixture: {} bytes with, {} without",
        with.estimated_bytes(),
        without.estimated_bytes()
    );

    // **The answers do not move.** This is B3's actual claim.
    assert_eq!(dijkstra(&without, "c0000"), dijkstra(&with, "c0000"));
    assert_eq!(
        macrame::graph::astar(&without, "c0000", "c0012", |_, _| 0.0),
        macrame::graph::astar(&with, "c0000", "c0012", |_, _| 0.0)
    );
    assert_eq!(scc(&without), scc(&with));
    assert_eq!(k_core(&without, 2), k_core(&with, 2));
    assert_eq!(louvain(&without), louvain(&with));
    assert_eq!(
        modularity(&without, &louvain(&without)),
        modularity(&with, &louvain(&with))
    );
}

// ---------------------------------------------------------------------------
// Why Louvain stays phase-one-only (0.8.0, B6, D-122)
// ---------------------------------------------------------------------------

/// **At the post-interning ceiling, maximising modularity harder moves *away*
/// from the right answer.**
///
/// B6 asked whether `louvain`'s missing aggregation phase still deserves to be
/// missing now that [D-115](../docs/architecture/s13-decision-register.md)'s
/// interning raised what fits the byte budget by 5.8×–6.8×. The rustdoc's
/// stated reason was graph size — *"the aggregation phase would matter on
/// graphs far larger than the byte budget admits"* — and
/// `examples/louvain_aggregation_probe.rs` measured that reason **false**:
/// two-phase Louvain returns a different partition from 6,144 nodes upward,
/// which is comfortably inside the budget.
///
/// It also measured what the difference *is*, which is the part that settles
/// it. On `clustered` — cliques joined by a single bridge each — phase-one
/// recovers the ground truth exactly at every size up to the ceiling, and
/// two-phase scores higher Q by **merging whole cliques**: two per community at
/// 512, four at 4,096. Never splitting, always merging. That is the modularity
/// resolution limit, a property of the objective rather than of any algorithm.
///
/// This test pins the underlying fact without needing a two-phase
/// implementation in the crate: **the merged partition scores better than the
/// truth**. Anything that optimises Q more aggressively therefore has a reason
/// to prefer it, so a Q comparison alone — which is what B6 specified as its
/// gate — cannot decide the question. The scope limit survives on a better
/// argument than the one it had.
#[test]
fn modularity_prefers_a_merged_partition_over_the_true_one_at_scale() {
    use macrame::graph::NodeData;
    use std::collections::BTreeMap;

    const CLUSTER: usize = 12;
    const COMMUNITIES: usize = 512;
    const TS0: &str = "2026-01-01T00:00:00.000000Z";
    const OPEN: &str = "9999-12-31T23:59:59.999999Z";

    let id = |i: usize| format!("c{i:07}");

    let mut g = Subgraph::default();
    let mut truth: BTreeMap<String, usize> = BTreeMap::new();
    // Every *pair* of adjacent cliques as one community — exactly what the
    // probe measured two-phase Louvain converging to at this size.
    let mut merged: BTreeMap<String, usize> = BTreeMap::new();

    for i in 0..COMMUNITIES * CLUSTER {
        g.insert_node(id(i), NodeData::new("N", TS0, OPEN));
        truth.insert(id(i), i / CLUSTER);
        merged.insert(id(i), i / (CLUSTER * 2));
    }
    for c in 0..COMMUNITIES {
        let base = c * CLUSTER;
        for i in 0..CLUSTER {
            for j in 0..CLUSTER {
                if i != j {
                    g.add_edge(&id(base + i), &id(base + j), "KNOWS", 1.0, TS0, OPEN);
                }
            }
        }
        if c + 1 < COMMUNITIES {
            g.add_edge(
                &id(base + CLUSTER - 2),
                &id(base + CLUSTER),
                "KNOWS",
                1.0,
                TS0,
                OPEN,
            );
        }
    }

    // The fixture is what it claims to be, or nothing below means anything.
    assert_eq!(g.node_count(), COMMUNITIES * CLUSTER);

    // **Phase-one is exactly right**, which is the thing aggregation would be
    // replacing. Compared as a grouping, since community labels are arbitrary.
    let answer = louvain(&g);
    let mut induced: BTreeMap<usize, usize> = BTreeMap::new();
    for (node, &t) in &truth {
        let a = answer[node];
        assert_eq!(
            *induced.entry(a).or_insert(t),
            t,
            "phase-one put two different cliques in one community at {node}"
        );
    }
    assert_eq!(
        induced.len(),
        COMMUNITIES,
        "phase-one did not recover one community per clique"
    );

    // **And the objective prefers the wrong answer.** This is the resolution
    // limit, and the reason a higher Q is not evidence of a better partition.
    let q_truth = modularity(&g, &truth);
    let q_merged = modularity(&g, &merged);
    assert!(
        q_merged > q_truth,
        "the resolution limit did not bite at {COMMUNITIES} communities \
         (truth {q_truth:.6}, merged {q_merged:.6}), so this test is not \
         measuring what it claims and the size needs re-checking against \
         examples/louvain_aggregation_probe.rs"
    );
}
