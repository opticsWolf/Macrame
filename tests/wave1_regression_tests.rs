//! One test per Wave 1 defect: the test that would have caught it.
//!
//! §8.7 records why this file exists rather than the fixes being spread through
//! the existing suites. Defect H was moved to **Fixed** by a commit that touched
//! the right file and changed nothing observable, and survived a full cycle that
//! way. A defect closed by a test is closed by something that fails if the fix is
//! reverted; a defect closed by a commit is closed by somebody's recollection.
//!
//! **Verified by reverting the fixes and re-running**, because a regression test
//! nobody has seen fail is a regression test nobody has tested. Nine of the
//! twelve fail against the pre-Wave-1 tree; the three that do not are named here
//! rather than left to look like coverage they are not:
//!
//! - `a_legal_archive_is_unaffected_by_the_classified_deletes` — a control. It
//!   passes before and after by design; its job is to fail if wiring `classify`
//!   into `archive()` broke the ordinary path.
//! - `deleting_a_link_outside_an_archive_session_is_a_typed_violation` — the
//!   classifier it exercises was always correct. Defect AC was that *nothing
//!   called it*, which is a property of the call graph and not observable from a
//!   test that calls it directly. What this pins is that the behaviour survives,
//!   so the duplicate classifier stays deleted.
//! - `load_subgraph_totals_agree_with_the_derivation` — named by a doc comment
//!   since 0.5.4 and never written. Not a Wave 1 defect; written because
//!   batching the hydrate moved the accounting it describes.

mod harness;

use harness::TestHarness;
use macrame::graph::{dijkstra, k_core, louvain, scc};
use macrame::graph::AttributeMode;
use macrame::schema::migrations;
use macrame::temporal::hydrate_attributes;
use macrame::{ConceptUpsert, Database};
use macrame::graph::EdgeAssertion;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-02-01T00:00:00.000000Z";
const T2: &str = "2026-03-01T00:00:00.000000Z";
const NOW: &str = "2026-12-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

// ---------------------------------------------------------------------------
// V — every temporal read used to lose `embedding_model`
// ---------------------------------------------------------------------------

/// The live column, the reconstruction and `AtTime` must agree about the model.
///
/// They did not. The concept log payload was built from five columns and
/// `embedding_model` was not among them, while both readers asked the payload
/// for it — so the field was written by nobody and read by two, and the mode
/// Doctrine VIII exists to offer returned a *less* faithful record than the mode
/// §5.2 documents as wrong for historical text.
#[tokio::test]
async fn embedding_model_survives_every_temporal_read() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(
        ConceptUpsert::new("c1", "Titled")
            .content("Body")
            .embedding_model("nomic_v1")
            .valid_from(T0),
    )
    .await
    .unwrap();

    let live: Option<String> = db
        .read_conn()
        .query(
            "SELECT embedding_model FROM concepts WHERE id = 'c1'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .ok();
    assert_eq!(live.as_deref(), Some("nomic_v1"), "live column");

    let state = db.reconstruct(NOW).await.unwrap();
    assert_eq!(
        state.concepts["c1"].embedding_model.as_deref(),
        Some("nomic_v1"),
        "reconstruct() must not lose the model"
    );

    let at_time = hydrate_attributes(
        db.read_conn(),
        &["c1".to_string()],
        NOW,
        AttributeMode::AtTime,
    )
    .await
    .unwrap();
    assert_eq!(
        at_time[0].embedding_model.as_deref(),
        Some("nomic_v1"),
        "AttributeMode::AtTime must not lose the model"
    );

    db.close().await.unwrap();
}

/// A v1 payload — the shape every database written before Wave 1 carries — still
/// folds, with the field absent rather than the fold refused.
///
/// This is the half of the compat surface that no amount of testing the *new*
/// shape would cover, and the reason the payload carries a version at all. It is
/// also the first exercise `DbError::PayloadVersion` has ever had.
#[tokio::test]
async fn a_v1_concept_payload_still_folds() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // Written by hand in the old shape: no `embedding_model`, `'v', 1`.
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'old', 'I', \
                 json_object('v', 1, 'title', 'Old', 'content', 'Body', \
                             'valid_from', ?1, 'valid_to', ?2, 'retired', 0), ?1)",
        libsql::params![T0, OPEN],
    )
    .await
    .unwrap();

    let state = macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap();
    let c = state.concepts.get("old").expect("v1 payload must still fold");
    assert_eq!(c.title, "Old");
    assert_eq!(c.embedding_model, None, "absent, not an error");
}

/// The payload version the triggers write and the one the readers accept are one
/// number. Nothing else would notice them drifting apart.
#[test]
fn the_trigger_payload_version_matches_the_reader_ceiling() {
    let concept_triggers: Vec<&&str> = macrame::schema::ddl::CREATE_TRIGGERS
        .iter()
        .filter(|t| t.contains("INSERT INTO transaction_log") && t.contains("'concepts'"))
        .collect();
    assert_eq!(concept_triggers.len(), 2, "insert and update");

    for t in concept_triggers {
        assert!(
            t.contains("'v', 2"),
            "concept log trigger writes a payload version the reader does not expect:\n{t}"
        );
        assert!(
            t.contains("'embedding_model'"),
            "defect V: the payload omits embedding_model again:\n{t}"
        );
    }
}

// ---------------------------------------------------------------------------
// W — the fold used to partition on `entity_id` alone
// ---------------------------------------------------------------------------

/// **Layer one (Wave 2.3, D-061): the collision is now unreachable.**
///
/// A concept id that *is* a link key can no longer be written, because `|` is
/// reserved. This is the durable fix defect W's write-up named — the partition
/// made the collision harmless, this makes it impossible — and it is asserted
/// separately from the fold so that removing one does not silently take the
/// other with it.
#[tokio::test]
async fn a_concept_id_shaped_like_an_edge_key_is_refused() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let colliding_id = format!("a|b|KNOWS|{T0}");
    let err = db
        .upsert_concept(ConceptUpsert::new(colliding_id, "Collides").valid_from(T0))
        .await
        .expect_err("an id carrying the log's delimiter must be refused");

    assert!(
        matches!(err, macrame::DbError::InvalidId { .. }),
        "and refused as invalid, not as missing — defect J: got {err:?}"
    );

    db.close().await.unwrap();
}

/// **Layer two (Wave 1.2): the fold survives one anyway.**
///
/// Defence in depth, and the reason it is worth keeping now that the API refuses
/// the id: `transaction_log` is written by triggers and can be reached by raw
/// SQL, so "no caller can create this" is a weaker claim than "the fold is
/// correct if one exists". The colliding row is inserted directly for exactly
/// that reason — going through the API is now impossible, which is the point of
/// the test above.
///
/// With the partition on `entity_id` alone the higher `seq_id` took the whole
/// window and the loser vanished from the reconstruction while sitting plainly
/// in both `concepts` and `transaction_log`.
#[tokio::test]
async fn a_concept_whose_id_looks_like_an_edge_key_survives_reconstruction() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    let colliding_id = format!("a|b|KNOWS|{T0}");

    // A concept log row, then a link log row under the same entity_id with a
    // greater seq_id — the shape the trigger pair produces when the two
    // namespaces meet.
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', ?1, 'I', \
                 json_object('v', 2, 'title', 'Collides', 'content', '', \
                             'valid_from', ?2, 'valid_to', ?3, 'retired', 0, \
                             'embedding_model', null), ?2)",
        libsql::params![colliding_id.as_str(), T0, OPEN],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('links', ?1, 'I', \
                 json_object('v', 1, 'source_id', 'a', 'target_id', 'b', \
                             'edge_type', 'KNOWS', 'valid_from', ?2, 'valid_to', ?3, \
                             'weight', 1.0, 'properties', json('{}')), ?2)",
        libsql::params![colliding_id.as_str(), T0, OPEN],
    )
    .await
    .unwrap();

    let state = macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap();

    assert!(
        state.concepts.contains_key(&colliding_id),
        "defect W: the concept was conflated with the edge and dropped"
    );
    assert_eq!(state.concepts[&colliding_id].title, "Collides");
    assert!(
        state
            .edges
            .iter()
            .any(|e| e.0 == "a" && e.1 == "b" && e.2 == "KNOWS"),
        "and the edge is still there too"
    );
}

/// A variable-length id that is a substring of another does not prune the walk.
///
/// The traversal CTE's cycle check was `INSTR(path, id)`, correct only while
/// every id was the same fixed width. `b` is a substring of `abc`, so a path
/// through `abc` made `b` look already-visited and cut a live branch — silently,
/// as a smaller result rather than an error (D-061).
#[tokio::test]
async fn a_short_id_inside_a_longer_one_does_not_prune_the_walk() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["start", "abc", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(EdgeAssertion::new("start", "abc", "KNOWS").valid_from(T0))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("abc", "b", "KNOWS").valid_from(T0))
        .await
        .unwrap();

    let graph = db.load_subgraph("start", 3, NOW, 1 << 20).await.unwrap();
    assert!(
        graph.nodes.contains_key("b"),
        "`b` was pruned because `abc` contains it: {:?}",
        graph.nodes.keys().collect::<Vec<_>>()
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// AB — the three readers used to disagree about retirement
// ---------------------------------------------------------------------------

/// A concept retired before `ts` is absent from all three readers.
///
/// `Current` filtered `retired = 0`, `reconstruct` treated retirement as a
/// tombstone, and `AtTime` read the payload and never looked at `retired` at
/// all — so the one reader that consulted the ledger most faithfully was the one
/// that answered the visibility question wrong.
#[tokio::test]
async fn all_three_readers_agree_a_retired_concept_is_not_visible() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(ConceptUpsert::new("c1", "Live").valid_from(T0))
        .await
        .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("c1", "Live")
            .valid_from(T0)
            .retired(true),
    )
    .await
    .unwrap();

    let ids = vec!["c1".to_string()];

    let current = hydrate_attributes(db.read_conn(), &ids, NOW, AttributeMode::Current)
        .await
        .unwrap();
    assert!(current.is_empty(), "Current: retired concept is not visible");

    let at_time = hydrate_attributes(db.read_conn(), &ids, NOW, AttributeMode::AtTime)
        .await
        .unwrap();
    assert!(
        at_time.is_empty(),
        "defect AB: AtTime returned a concept retired before ts"
    );

    let state = db.reconstruct(NOW).await.unwrap();
    assert!(
        !state.concepts.contains_key("c1"),
        "reconstruct: retired concept is not visible"
    );

    db.close().await.unwrap();
}

/// Retirement is *as of the instant asked about*, so `AtTime` before the
/// retirement still sees the concept.
///
/// The companion to the test above, and the one that stops "honour `retired`"
/// being implemented as "filter the live column" — which would agree with
/// `Current` by giving up the second clock.
#[tokio::test]
async fn at_time_before_the_retirement_still_sees_the_concept() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, content, valid_from, recorded_at) \
         VALUES ('c1', 'Live', '', ?1, ?1)",
        libsql::params![T0],
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE concepts SET retired = 1, recorded_at = ?1 WHERE id = 'c1'",
        libsql::params![T2],
    )
    .await
    .unwrap();

    let ids = vec!["c1".to_string()];

    let before = hydrate_attributes(&conn, &ids, T1, AttributeMode::AtTime)
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        1,
        "as believed at T1 the concept was not yet retired"
    );

    let after = hydrate_attributes(&conn, &ids, NOW, AttributeMode::AtTime)
        .await
        .unwrap();
    assert!(after.is_empty(), "and by NOW it is");
}

// ---------------------------------------------------------------------------
// Z — Subgraph adjacency used to reference nodes absent from `nodes`
// ---------------------------------------------------------------------------

/// Build the exact graph the review reproduced on: three nodes, one retired.
async fn graph_with_a_retired_neighbour(harness: &TestHarness) -> (Database, macrame::graph::Subgraph) {
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b", "c"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0).valid_to(OPEN).weight(1.0))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("a", "c", "KNOWS").valid_from(T0).valid_to(OPEN).weight(1.0))
        .await
        .unwrap();

    // `c` retires. links_current still carries a -> c: retirement is the
    // application axis and does not touch the topology table.
    db.upsert_concept(ConceptUpsert::new("c", "c").valid_from(T0).retired(true))
        .await
        .unwrap();

    let graph = db.load_subgraph("a", 3, NOW, 1 << 20).await.unwrap();
    (db, graph)
}

/// The closure invariant holds on a graph with a retired neighbour, and the four
/// algorithms that disagreed about it now agree.
///
/// Before Wave 1: `louvain` panicked on the missing map entry, `scc` returned the
/// retired node as a phantom component, `k_core` counted a degree of 2 where one
/// edge was in the graph, and `dijkstra` returned a finite distance to a node the
/// caller could not look up. Four handlings of one violated invariant.
#[tokio::test]
async fn a_retired_neighbour_leaves_no_dangling_adjacency() {
    let harness = TestHarness::new();
    let (db, graph) = graph_with_a_retired_neighbour(&harness).await;

    assert!(
        graph.is_closed(),
        "every adjacency endpoint must be a hydrated node"
    );
    assert!(!graph.nodes.contains_key("c"), "the retired node is absent");
    assert_eq!(graph.edge_count(), 1, "and so is the edge into it");

    // louvain: used to panic here.
    let comm = louvain(&graph);
    assert_eq!(comm.len(), graph.nodes.len());
    assert!(comm.keys().all(|k| graph.nodes.contains_key(k)));

    // scc: used to emit `c` as a component of its own.
    let components = scc(&graph);
    for component in &components {
        for node in component {
            assert!(
                graph.nodes.contains_key(node),
                "scc returned a phantom component member {node:?}"
            );
        }
    }

    // k_core: used to count a's degree as 2.
    let core = k_core(&graph, 2);
    assert!(
        core.is_empty(),
        "one edge cannot put anything in the 2-core; got {core:?}"
    );

    // dijkstra: used to return a distance to a node with no attributes.
    let dist = dijkstra(&graph, "a");
    for node in dist.keys() {
        assert!(
            graph.nodes.contains_key(node),
            "dijkstra reached {node:?}, which the caller cannot look up"
        );
    }

    db.close().await.unwrap();
}

/// Retiring the *source* prunes it too, not just retired targets.
///
/// The prune walks adjacency keys as well as edge endpoints, and this is the case
/// that separates the two.
#[tokio::test]
async fn retiring_the_start_node_yields_an_empty_graph() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0).valid_to(OPEN).weight(1.0))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("a", "a").valid_from(T0).retired(true))
        .await
        .unwrap();

    let graph = db.load_subgraph("a", 3, NOW, 1 << 20).await.unwrap();

    assert!(graph.is_closed());
    assert!(!graph.nodes.contains_key("a"));
    assert_eq!(graph.edge_count(), 0, "no edge can survive its source");

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// AE — the byte accounting the batched hydrate had to preserve
// ---------------------------------------------------------------------------

/// `estimated_bytes` over the returned graph equals the total the loader tracked.
///
/// Named by `Subgraph::node_bytes`'s doc comment since 0.5.4 and never written —
/// the same drift as defect H, one level down. Written now because batching the
/// hydrate moved the accounting, and an accounting change with no test is exactly
/// what the budget cannot afford.
#[tokio::test]
async fn load_subgraph_totals_agree_with_the_derivation() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for i in 0..40 {
        db.upsert_concept(
            ConceptUpsert::new(format!("n{i:03}"), format!("Node {i}"))
                .content("some content of a plausible length")
                .valid_from(T0),
        )
        .await
        .unwrap();
    }
    for i in 0..39 {
        db.assert_edge(
            EdgeAssertion::new(format!("n{i:03}"), format!("n{:03}", i + 1), "KNOWS")
                .valid_from(T0)
                .valid_to(OPEN)
                .weight(1.0),
        )
        .await
        .unwrap();
    }

    let graph = db.load_subgraph("n000", 50, NOW, 1 << 20).await.unwrap();
    assert_eq!(graph.nodes.len(), 40);
    assert!(graph.is_closed());

    // The budget the loader enforces and the figure a caller can compute are the
    // same arithmetic, not two descriptions of it.
    let budget = graph.estimated_bytes();
    let refused = db.load_subgraph("n000", 50, NOW, budget / 2).await;
    assert!(
        matches!(refused, Err(macrame::DbError::SubgraphTooLarge { .. })),
        "half the graph's own size must not fit"
    );

    db.close().await.unwrap();
}

/// A hydrate wider than one `IN (…)` chunk still returns every node.
///
/// The chunking is a bind-variable ceiling, not a budget, and a graph that
/// straddles it is the case where an off-by-one silently truncates the answer.
#[tokio::test]
async fn hydrate_spans_more_than_one_chunk() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let n = 450; // > HYDRATE_CHUNK
    let ids: Vec<String> = (0..n).map(|i| format!("n{i:04}")).collect();
    for id in &ids {
        db.upsert_concept(ConceptUpsert::new(id.clone(), id.clone()).valid_from(T0))
            .await
            .unwrap();
    }

    let attrs = hydrate_attributes(db.read_conn(), &ids, NOW, AttributeMode::Current)
        .await
        .unwrap();
    assert_eq!(attrs.len(), n, "every node comes back");

    // Caller order, not row order — the property suite compares results for
    // equality and a permutation would fail for the wrong reason.
    let returned: Vec<&str> = attrs.iter().map(|a| a.id.as_str()).collect();
    let expected: Vec<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(returned, expected, "results follow node_ids order");

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// AC — DbError::ArchiveViolation was unconstructible
// ---------------------------------------------------------------------------

/// A physical delete outside an archive session is refused as a *typed* error.
///
/// The variant existed, the classifier existed, and no code path could produce
/// either — which is what made defect H closable by a commit that changed
/// nothing. `archive()`'s deletes now go through `classify`, and this asserts
/// that the guard's abort is reachable and lands as `ArchiveViolation` rather
/// than as an opaque engine error.
#[tokio::test]
async fn deleting_a_link_outside_an_archive_session_is_a_typed_violation() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0).valid_to(OPEN).weight(1.0))
        .await
        .unwrap();

    // The guard is on `links` and fires whenever the archive-session marker is
    // absent, which is every moment outside `archive()`.
    let err = db
        .raw()
        .connect()
        .unwrap()
        .execute("DELETE FROM links WHERE source_id = 'a'", ())
        .await
        .expect_err("the delete guard must refuse this");

    let typed = macrame::error::classify(
        db.read_conn(),
        err,
        macrame::error::WriteOp::Delete { table: "links" },
    )
    .await;

    assert!(
        matches!(typed, macrame::DbError::ArchiveViolation { ref table } if table == "links"),
        "defect AC: expected ArchiveViolation, got {typed:?}"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// AA — overlapping closed valid-time intervals were unguarded (Wave 2.2, D-060)
// ---------------------------------------------------------------------------

/// Two overlapping **closed** intervals for one relationship are refused.
///
/// The exact reproduction from the review: both asserts succeeded, and
/// `query_as_of_edges` at an instant inside both returned one relationship as
/// two edges, which every weighted algorithm downstream then double-counted.
#[tokio::test]
async fn overlapping_closed_intervals_are_refused() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }

    let jan = "2026-01-01T00:00:00.000000Z";
    let mar = "2026-03-01T00:00:00.000000Z";
    let jun = "2026-06-01T00:00:00.000000Z";
    let sep = "2026-09-01T00:00:00.000000Z";
    let apr = "2026-04-01T00:00:00.000000Z";

    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(jan)
            .valid_to(jun),
    )
    .await
    .unwrap();

    let err = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from(mar)
                .valid_to(sep),
        )
        .await
        .expect_err("defect AA: [Mar, Sep) overlaps [Jan, Jun)");

    assert!(
        matches!(err, macrame::DbError::OverlappingInterval { .. }),
        "got {err:?}"
    );

    // The reason the refusal matters, asserted rather than described.
    let edges = macrame::temporal::query_as_of_edges(db.read_conn(), apr)
        .await
        .unwrap();
    assert_eq!(edges.len(), 1, "one relationship, one edge at any instant");

    db.close().await.unwrap();
}

/// Abutting intervals are not overlapping — `[Jan, Jun)` and `[Jun, Sep)` are
/// the ordinary way a relationship changes, and refusing them would break the
/// half-open convention the whole schema is built on.
#[tokio::test]
async fn abutting_intervals_are_accepted() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }

    let jan = "2026-01-01T00:00:00.000000Z";
    let jun = "2026-06-01T00:00:00.000000Z";
    let sep = "2026-09-01T00:00:00.000000Z";

    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(jan)
            .valid_to(jun),
    )
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(jun)
            .valid_to(sep),
    )
    .await
    .expect("half-open intervals that abut do not overlap");

    db.close().await.unwrap();
}

/// An open interval overlapping a *closed* one is refused too.
///
/// This is the case neither guard covered before: `trg_links_single_open` fires
/// only when an existing interval is also open, so an open assertion landing on
/// top of a closed one went through.
#[tokio::test]
async fn an_open_interval_overlapping_a_closed_one_is_refused() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }

    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from("2026-01-01T00:00:00.000000Z")
            .valid_to("2026-06-01T00:00:00.000000Z"),
    )
    .await
    .unwrap();

    let err = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS").valid_from("2026-03-01T00:00:00.000000Z"),
        )
        .await
        .expect_err("an open interval starting inside a closed one overlaps it");
    assert!(
        matches!(err, macrame::DbError::OverlappingInterval { .. }),
        "got {err:?}"
    );

    db.close().await.unwrap();
}

/// Two open intervals stay `SingleOpenViolation`, not `OverlappingInterval`.
///
/// The two guards partition the space; they do not layer. If this ever reports
/// `OverlappingInterval`, `SingleOpenViolation` has become a variant nothing
/// constructs — which is the defect the general guard was added next to, not a
/// tidier version of it.
#[tokio::test]
async fn two_open_intervals_remain_the_specific_violation() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }

    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0))
        .await
        .unwrap();
    let err = db
        .assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T1))
        .await
        .unwrap_err();

    assert!(
        matches!(err, macrame::DbError::SingleOpenViolation { .. }),
        "the storage guard must keep this case: got {err:?}"
    );

    db.close().await.unwrap();
}

/// A batch that contradicts itself is refused before anything is written.
///
/// The database check cannot see rows that are not in the database yet, so this
/// is the case a per-row guard alone would commit in one transaction.
#[tokio::test]
async fn a_batch_carrying_its_own_overlap_is_refused_whole() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }

    let err = db
        .write_bulk_atomic(vec![
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from("2026-01-01T00:00:00.000000Z")
                .valid_to("2026-06-01T00:00:00.000000Z"),
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from("2026-03-01T00:00:00.000000Z")
                .valid_to("2026-09-01T00:00:00.000000Z"),
        ])
        .await
        .expect_err("the batch overlaps itself");

    assert!(
        matches!(err, macrame::DbError::OverlappingInterval { .. }),
        "got {err:?}"
    );

    let n: i64 = db
        .read_conn()
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(n, 0, "nothing may land from a batch that is refused");

    db.close().await.unwrap();
}

/// `Interval::overlaps` is no longer dead code (**AG**).
///
/// It was the crate's only overlap arithmetic and no production path called it —
/// the missing half of AA. The guard uses it rather than restating the
/// comparison in SQL, which is why the SQL narrows to a provable superset and
/// leaves the decision to this function.
#[test]
fn the_overlap_arithmetic_has_a_production_caller() {
    use macrame::temporal::Interval;

    let a = Interval::new("2026-01-01T00:00:00.000000Z", "2026-06-01T00:00:00.000000Z");
    let b = Interval::new("2026-03-01T00:00:00.000000Z", "2026-09-01T00:00:00.000000Z");
    let c = Interval::new("2026-06-01T00:00:00.000000Z", "2026-09-01T00:00:00.000000Z");

    assert!(a.overlaps(&b));
    assert!(!a.overlaps(&c), "half-open intervals that abut do not overlap");
}

// ---------------------------------------------------------------------------
// K — the clock is injectable (Wave 2.4, D-062)
// ---------------------------------------------------------------------------

/// An injected clock actually drives `recorded_at`.
///
/// Until now this could not be asserted at all: `Database::open` hardcoded
/// `SystemClock`, so every `recorded_at` in every test was wall-clock time and
/// the transaction-time axis was untestable through the public API. `FakeClock`
/// was public and constructed in `harness.rs` for three releases with nothing to
/// inject it into.
#[tokio::test]
async fn an_injected_clock_stamps_the_transaction_time() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    db.upsert_concept(ConceptUpsert::new("c1", "First").valid_from(T0))
        .await
        .unwrap();

    let stamp: String = db
        .read_conn()
        .query("SELECT recorded_at FROM concepts WHERE id = 'c1'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();

    assert!(
        stamp.starts_with("1970-01-01T00:00:00"),
        "the fake clock starts at the epoch; got {stamp}"
    );

    db.close().await.unwrap();
}

/// Advancing the fake clock is visible as transaction time, and two beliefs
/// about one concept are separated by it.
///
/// This is the Doctrine VIII shape that previously had to be written against a
/// raw connection with hand-written `recorded_at` literals — which tests the
/// trigger but not the crate's own stamping.
#[tokio::test]
async fn transaction_time_follows_the_injected_clock() {
    let harness = TestHarness::starting_at(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600),
    );
    let db = harness.db_with_fake_clock().await;

    db.upsert_concept(ConceptUpsert::new("c1", "Monday").valid_from(T0))
        .await
        .unwrap();
    harness.advance(std::time::Duration::from_secs(172_800));
    db.upsert_concept(ConceptUpsert::new("c1", "Wednesday").valid_from(T0))
        .await
        .unwrap();

    let mut rows = db
        .read_conn()
        .query(
            "SELECT recorded_at FROM transaction_log \
             WHERE table_name = 'concepts' AND entity_id = 'c1' ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    let mut stamps = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        stamps.push(r.get::<String>(0).unwrap());
    }

    assert_eq!(stamps.len(), 2);
    assert!(stamps[0] < stamps[1], "{stamps:?}");
    // Two days apart, exactly, because nothing here consults a wall clock.
    assert!(stamps[0].starts_with("2026-01-01"), "{stamps:?}");
    assert!(stamps[1].starts_with("2026-01-03"), "{stamps:?}");

    db.close().await.unwrap();
}

/// Reopening a populated database with a clock behind it does not break writes.
///
/// **The reason defect K stalled.** A fake starting at the epoch issues stamps
/// below every stored `recorded_at`, and the next concept write aborts on
/// `trg_concepts_monotonic_ra`. `open_with_clock` raises the clock to the
/// ledger's newest stamp first, so the injected clock is floored the same way
/// `SystemClock::new` floors itself.
#[tokio::test]
async fn an_injected_clock_is_floored_against_an_existing_database() {
    let harness = TestHarness::new();

    // First session: ordinary wall-clock stamps, well ahead of the epoch.
    let db = Database::open_with_cadence(&harness.db_path, None).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("c1", "Existing").valid_from(T0))
        .await
        .unwrap();
    db.close().await.unwrap();

    // Second session: a clock starting at the epoch, far behind the file.
    let reopened = harness.db_with_fake_clock().await;
    reopened
        .upsert_concept(ConceptUpsert::new("c1", "Updated").valid_from(T0))
        .await
        .expect("a floored clock must not trip the monotonicity guard");

    let stamps: Vec<String> = {
        let mut rows = reopened
            .read_conn()
            .query(
                "SELECT recorded_at FROM transaction_log \
                 WHERE table_name = 'concepts' ORDER BY seq_id",
                (),
            )
            .await
            .unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            v.push(r.get::<String>(0).unwrap());
        }
        v
    };
    assert_eq!(stamps.len(), 2);
    assert!(
        stamps[0] < stamps[1],
        "the second session's stamp must be above the first's: {stamps:?}"
    );

    reopened.close().await.unwrap();
}

/// The archive still succeeds through the classified delete path.
///
/// Wiring `classify` into `archive()` must not change what a legal archive does;
/// this is the control for the test above.
#[tokio::test]
async fn a_legal_archive_is_unaffected_by_the_classified_deletes() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    // A closed interval, superseded, and therefore archivable.
    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0).valid_to(T1).weight(1.0))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("a", "b", "KNOWS").valid_from(T1).valid_to(OPEN).weight(2.0))
        .await
        .unwrap();

    // The cutoff must be after `recorded_at`, which `SystemClock` sets to now —
    // LINKS_ARCHIVABLE requires `recorded_at < :cutoff` as well as a closed valid
    // interval, and the two clocks are why a valid-time cutoff alone archives
    // nothing (Doctrine II).
    let report = db.archive("2099-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        report.links_archived > 0,
        "the fixture must actually archive something, or this proves nothing"
    );

    db.close().await.unwrap();
}
