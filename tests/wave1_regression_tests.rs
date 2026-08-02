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

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::AttributeMode;
use macrame::graph::EdgeAssertion;
use macrame::graph::{dijkstra, k_core, louvain, scc};
use macrame::schema::migrations;
use macrame::temporal::hydrate_attributes;
use macrame::{ConceptUpsert, Database};

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
        .query("SELECT embedding_model FROM concepts WHERE id = 'c1'", ())
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
    let c = state
        .concepts
        .get("old")
        .expect("v1 payload must still fold");
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
        graph.contains_node("b"),
        "`b` was pruned because `abc` contains it: {:?}",
        graph.node_ids().collect::<Vec<_>>()
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
    assert!(
        current.is_empty(),
        "Current: retired concept is not visible"
    );

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
async fn graph_with_a_retired_neighbour(
    harness: &TestHarness,
) -> (Database, macrame::graph::Subgraph) {
    let db = Database::open(&harness.db_path).await.unwrap();

    for id in ["a", "b", "c"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN)
            .weight(1.0),
    )
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN)
            .weight(1.0),
    )
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
    assert!(!graph.contains_node("c"), "the retired node is absent");
    assert_eq!(graph.edge_count(), 1, "and so is the edge into it");

    // louvain: used to panic here.
    let comm = louvain(&graph);
    assert_eq!(comm.len(), graph.node_count());
    assert!(comm.keys().all(|k| graph.contains_node(k)));

    // scc: used to emit `c` as a component of its own.
    let components = scc(&graph);
    for component in &components {
        for node in component {
            assert!(
                graph.contains_node(node),
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
            graph.contains_node(node),
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
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN)
            .weight(1.0),
    )
    .await
    .unwrap();
    db.upsert_concept(ConceptUpsert::new("a", "a").valid_from(T0).retired(true))
        .await
        .unwrap();

    let graph = db.load_subgraph("a", 3, NOW, 1 << 20).await.unwrap();

    assert!(graph.is_closed());
    assert!(!graph.contains_node("a"));
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
    assert_eq!(graph.node_count(), 40);
    assert!(graph.is_closed());

    // The budget the loader enforces and the figure a caller can compute are the
    // same arithmetic, not two descriptions of it.
    let budget = graph.estimated_bytes();
    let refused = db.load_subgraph("n000", 50, NOW, budget / 2).await;
    assert!(
        matches!(refused, Err(macrame::DbError::SubgraphTooLarge { .. })),
        "half the graph's own size must not fit"
    );

    // **At exactly its own size it must fit.** `budget / 2` above cannot see a
    // small drift between the loader's running total and `estimated_bytes()`,
    // and there was one: the loader doubled the *outgoing* edge estimate while
    // `add_edge` stores the incoming copy keyed on the source, so the two
    // disagreed by `target.len() - source.len()` per edge (D-073). One byte an
    // edge, enough to refuse a graph sized at its own estimate.
    db.load_subgraph("n000", 50, NOW, budget)
        .await
        .expect("a graph must fit a budget equal to its own estimated_bytes()");

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

    // Derived, not a literal: the fixture must straddle a chunk boundary, and a
    // hardcoded 450 stops doing that the moment the constant is tuned upward —
    // silently, since the test still passes while testing one chunk (T3.1).
    let n = macrame::util::limits::HYDRATE_CHUNK + 50;
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
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN)
            .weight(1.0),
    )
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
    assert!(
        !a.overlaps(&c),
        "half-open intervals that abut do not overlap"
    );
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
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();
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

// ---------------------------------------------------------------------------
// Wave 5 — load_subgraph_with: the missing filter (D-073)
// ---------------------------------------------------------------------------

/// Build `hub` with two edge types out of it, at two weights.
async fn mixed_graph(db: &Database) {
    for id in ["hub", "a", "b", "c", "d"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
    for (target, ty, w) in [
        ("a", "CITES", 1.0),
        ("b", "CITES", 0.2),
        ("c", "KNOWS", 1.0),
        ("d", "KNOWS", 0.2),
    ] {
        db.assert_edge(
            EdgeAssertion::new("hub", target, ty)
                .valid_from(T0)
                .weight(w),
        )
        .await
        .unwrap();
    }
}

/// **The filters bound the walk *and* the edges returned.**
///
/// This is the decision the change turned on. `TraversalBuilder` filters the
/// recursive step; `load_subgraph`'s projection returned every edge of every
/// reached node. Filtering only the walk would hand a caller who asked for
/// `CITES` a graph reached via `CITES` and populated with `KNOWS` edges too.
#[tokio::test]
async fn load_subgraph_with_filters_the_returned_edges_not_only_the_walk() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    mixed_graph(&db).await;

    let graph = db
        .load_subgraph_with(
            &macrame::graph::TraversalBuilder::new("hub")
                .max_depth(3)
                .edge_types(vec!["CITES".into()]),
            NOW,
            1 << 20,
        )
        .await
        .unwrap();

    let types: Vec<&str> = graph
        .out_edges("hub")
        .iter()
        .map(|e| e.edge_type(&graph))
        .collect();
    assert_eq!(
        types,
        ["CITES", "CITES"],
        "only the asked-for type: {types:?}"
    );
    assert!(graph.is_closed());
    assert!(
        !graph.contains_node("c") && !graph.contains_node("d"),
        "KNOWS-only neighbours are not reached either: {:?}",
        graph.node_ids().collect::<Vec<_>>()
    );

    db.close().await.unwrap();
}

/// `min_weight` applies on both halves as well.
#[tokio::test]
async fn load_subgraph_with_honours_min_weight() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    mixed_graph(&db).await;

    let graph = db
        .load_subgraph_with(
            &macrame::graph::TraversalBuilder::new("hub")
                .max_depth(3)
                .min_weight(0.5),
            NOW,
            1 << 20,
        )
        .await
        .unwrap();

    assert_eq!(graph.edge_count(), 2, "only the weight-1.0 edges survive");
    assert!(graph.out_edges("hub").iter().all(|e| e.weight() >= 0.5));
    assert!(graph.is_closed());

    db.close().await.unwrap();
}

/// **The reachability limit this closes**: the byte budget bounds the
/// *unfiltered* neighbourhood, so a filtered load succeeds where the plain one
/// is refused.
///
/// This is the whole reason the gap was worth closing rather than documenting —
/// filtering the returned `Subgraph` afterwards cannot help, because the refusal
/// happens during the walk.
#[tokio::test]
async fn a_filtered_load_fits_a_budget_the_unfiltered_one_exceeds() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(ConceptUpsert::new("hub", "hub").valid_from(T0))
        .await
        .unwrap();
    for i in 0..60 {
        let id = format!("n{i:03}");
        db.upsert_concept(ConceptUpsert::new(&id, &id).valid_from(T0))
            .await
            .unwrap();
        // Half CITES, half KNOWS.
        let ty = if i % 2 == 0 { "CITES" } else { "KNOWS" };
        db.assert_edge(EdgeAssertion::new("hub", &id, ty).valid_from(T0))
            .await
            .unwrap();
    }

    // A budget that the CITES half fits inside and the whole graph does not.
    let filtered = db
        .load_subgraph_with(
            &macrame::graph::TraversalBuilder::new("hub")
                .max_depth(1)
                .edge_types(vec!["CITES".into()]),
            NOW,
            1 << 20,
        )
        .await
        .unwrap();
    let budget = filtered.estimated_bytes();

    assert!(
        matches!(
            db.load_subgraph("hub", 1, NOW, budget).await,
            Err(macrame::DbError::SubgraphTooLarge { .. })
        ),
        "the unfiltered load must exceed a budget sized for the filtered one"
    );
    assert_eq!(
        db.load_subgraph_with(
            &macrame::graph::TraversalBuilder::new("hub")
                .max_depth(1)
                .edge_types(vec!["CITES".into()]),
            NOW,
            budget,
        )
        .await
        .unwrap()
        .edge_count(),
        30,
        "and the filtered one fits it"
    );

    db.close().await.unwrap();
}

/// `load_subgraph` is unchanged: a default builder means no filter at all.
#[tokio::test]
async fn the_unfiltered_loader_still_returns_everything() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    mixed_graph(&db).await;

    let graph = db.load_subgraph("hub", 3, NOW, 1 << 20).await.unwrap();
    assert_eq!(
        graph.edge_count(),
        4,
        "all four edges, both types, both weights"
    );
    assert_eq!(graph.node_count(), 5);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Wave 5 — the FTS index, and why VACUUM cannot disturb it (D-071)
// ---------------------------------------------------------------------------

/// **`VACUUM` does not renumber `concepts.rowid`, so the FTS index survives it.**
///
/// External-content FTS5 is keyed on `concepts.rowid`, which is implicit, and
/// `VACUUM` renumbers implicit rowids — the standard hazard, and the one this
/// schema was flagged for by inspection.
///
/// It does not arise, and this pins why: `concepts` can never be deleted from
/// (D-022, `trg_concepts_guard_delete`, unconditional) and upserts go through
/// `ON CONFLICT DO UPDATE`, which preserves rowids. So the rowids are dense
/// `1..n` and `VACUUM`'s renumbering is the identity map.
///
/// **The delete guard is therefore load-bearing for the search index**, not only
/// for the ledger — which nothing recorded before D-071. If concept archival or
/// GDPR erasure ever lands (both deferred in Appendix C), rowids go sparse, this
/// test fails, and whichever change made them sparse owes a rebuild.
#[tokio::test]
async fn vacuum_does_not_disturb_the_fts_index() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for i in 0..20 {
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i:03}"), format!("Title {i}"))
                .content("searchable body text")
                .valid_from(T0),
        )
        .await
        .unwrap();
    }
    // Updates too: the upsert path must not churn rowids either.
    for i in [3usize, 7, 11] {
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i:03}"), "Updated")
                .content("searchable body text")
                .valid_from(T0),
        )
        .await
        .unwrap();
    }

    let rowids = |db: &Database| {
        let conn = db.read_conn().clone();
        async move {
            let mut rows = conn
                .query("SELECT rowid, id FROM concepts ORDER BY rowid", ())
                .await
                .unwrap();
            let mut v = Vec::new();
            while let Some(r) = rows.next().await.unwrap() {
                v.push((r.get::<i64>(0).unwrap(), r.get::<String>(1).unwrap()));
            }
            v
        }
    };

    let before = rowids(&db).await;
    assert!(
        before
            .iter()
            .enumerate()
            .all(|(i, (r, _))| *r == i as i64 + 1),
        "rowids must be dense for the argument to hold: {before:?}"
    );
    assert_eq!(
        hits(&db).await,
        20,
        "every concept is indexed to begin with"
    );

    // Through `raw()`: VACUUM is an operator action, not something the API does.
    db.raw()
        .connect()
        .unwrap()
        .execute("VACUUM", ())
        .await
        .unwrap();

    assert_eq!(rowids(&db).await, before, "VACUUM renumbered the rowids");
    // Asserted as *search behaviour*, not as an integrity check: FTS5's own
    // check cannot see this class of breakage — see the test below.
    assert_eq!(
        hits(&db).await,
        20,
        "the index must still find every concept after VACUUM"
    );

    db.close().await.unwrap();
}

/// How many concepts the FTS index actually matches — the only honest signal.
async fn hits(db: &Database) -> i64 {
    db.read_conn()
        .query(
            "SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'searchable'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// **A tripwire, not a guarantee: FTS5's `integrity-check` cannot see a stale
/// index on this build.**
///
/// Wave 5 set out to add `Database::verify_fts()` as the missing half of
/// `rebuild_fts()`, using FTS5's own `'integrity-check'` — the engine-provided
/// answer this crate prefers over a hand-rolled one. It does not answer the
/// question: on libSQL 0.9.30 it verifies the index's *internal* consistency and
/// not its agreement with the content table.
///
/// So no `verify_fts()` shipped, because one built on this would report a
/// healthy index for an empty one — defect AC's shape, a function that looks
/// like it checks something and does not.
///
/// **This test fails when that stops being true**, which is the point: if a
/// later libSQL cross-checks content, `integrity-check` starts erroring here and
/// the failure says the method can now be built.
#[tokio::test]
async fn an_emptied_fts_index_still_passes_integrity_check() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    for i in 0..10 {
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i:03}"), format!("Title {i}"))
                .content("searchable body text")
                .valid_from(T0),
        )
        .await
        .unwrap();
    }
    assert_eq!(hits(&db).await, 10);

    let raw = db.raw().connect().unwrap();
    raw.execute(
        "INSERT INTO concepts_fts (concepts_fts) VALUES ('delete-all')",
        (),
    )
    .await
    .unwrap();

    assert_eq!(
        hits(&db).await,
        0,
        "the index is now empty — genuinely stale"
    );

    let checked = raw
        .execute(macrame::schema::ddl::VERIFY_CONCEPTS_FTS, ())
        .await;
    assert!(
        checked.is_ok(),
        "integrity-check now detects content desync — verify_fts() can be built \
         (D-071 says why it was not): {checked:?}"
    );

    // And the repair still works, which is what makes the missing check bearable.
    db.rebuild_fts().await.unwrap();
    assert_eq!(hits(&db).await, 10, "rebuild_fts restores the index");

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Wave 5 — a 'D' row is corruption, not a tombstone (D-072)
// ---------------------------------------------------------------------------

/// `reconstruct` refuses a `'D'` operation instead of folding it as a tombstone.
///
/// Doctrine V permits no physical delete outside an archive session, and the
/// archive *moves* rows rather than logging their removal — so nothing in the
/// schema writes a `'D'` and nothing in the crate can produce one. The fold used
/// to handle it anyway, which read as a claim that deletions are recorded and
/// reconstructible. Injected here through a raw connection, which is the only
/// way such a row can exist at all.
#[tokio::test]
async fn a_delete_row_in_the_log_is_refused_as_corruption() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'c1', 'D', '{}', ?1)",
        libsql::params![T0],
    )
    .await
    .unwrap();

    let err = macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .expect_err("a 'D' row must not fold as a tombstone");

    match err {
        macrame::DbError::ReplayCorrupt { seq, reason } => {
            assert!(
                seq > 0,
                "the error must name the offending row, got seq {seq}"
            );
            assert!(
                reason.contains("Doctrine V"),
                "the refusal should say which rule it enforces: {reason}"
            );
        }
        other => panic!("expected ReplayCorrupt, got {other:?}"),
    }
}

/// The refusal covers links as well as concepts, and names the table.
#[tokio::test]
async fn a_delete_row_for_a_link_is_refused_too() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('links', 'a|b|KNOWS|x', 'D', '{}', ?1)",
        libsql::params![T0],
    )
    .await
    .unwrap();

    let err = macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::ReplayCorrupt { ref reason, .. } if reason.contains("links")),
        "got {err:?}"
    );
}

/// **Retirement still removes a concept from a composed state.**
///
/// The control for the two tests above, and the reason `concepts_gone` survives
/// while `edges_gone` did not: a concept disappears by being *retired*, which
/// writes a `'U'` row carrying `retired = 1` — not a `'D'`. If refusing `'D'`
/// had broken this, the fold would have stopped honouring retirement and no
/// other test would have said so.
#[tokio::test]
async fn retirement_still_removes_a_concept_from_a_composed_fold() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(ConceptUpsert::new("keep", "Keep").valid_from(T0))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("gone", "Gone").valid_from(T0))
        .await
        .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("gone", "Gone")
            .valid_from(T0)
            .retired(true),
    )
    .await
    .unwrap();

    let state = db.reconstruct(NOW).await.unwrap();
    assert!(state.concepts.contains_key("keep"));
    assert!(
        !state.concepts.contains_key("gone"),
        "retirement is the mechanism that removes a concept, and it still works"
    );

    db.close().await.unwrap();
}

/// **An edge is superseded in place, never removed** — which is why there is no
/// `edges_gone`.
///
/// Retiring an edge asserts a successor over the same interval key, so the log
/// row is an `'I'` under the *same* `entity_id` and last-writer-wins replaces the
/// tuple. This pins that reasoning: after a retirement the fold shows one edge,
/// closed — not zero, and not two.
#[tokio::test]
async fn a_retired_edge_is_superseded_rather_than_removed() {
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
    db.retire_edge("a", "b", "KNOWS", T0, T2).await.unwrap();

    let state = db.reconstruct(NOW).await.unwrap();
    let edges: Vec<_> = state
        .edges
        .iter()
        .filter(|e| e.0 == "a" && e.1 == "b")
        .collect();

    assert_eq!(edges.len(), 1, "one interval key, one edge: {edges:?}");
    assert_eq!(edges[0].4, T2, "and it is closed, not absent");

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Wave 4 — hardening
// ---------------------------------------------------------------------------

/// An upgraded database is re-anchored at open, so the next reconstruct does
/// not fold from genesis (Wave 4.4).
///
/// D-043 makes a `SCHEMA_VERSION` bump invalidate every snapshot, which is
/// correct. What was missing is the other half: nothing wrote a replacement, so
/// the first `reconstruct` after an upgrade skipped every file as incompatible
/// and folded the whole log — correctly, and at the cost the snapshot existed to
/// avoid.
#[tokio::test]
async fn an_upgraded_database_is_re_anchored_at_open() {
    let harness = TestHarness::new();

    // A populated database, closed cleanly so it has an anchor.
    let db = Database::open(&harness.db_path).await.unwrap();
    for i in 0..5 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), "T").valid_from(T0))
            .await
            .unwrap();
    }
    // Where the snapshots are, captured before `close()` consumes the handle.
    let snaps_dir = db.snapshots_dir().to_path_buf();
    db.close().await.unwrap();

    let count = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|d| d.flatten().count())
            .unwrap_or(0)
    };
    assert!(count(&snaps_dir) > 0, "close() must leave an anchor");

    // Clear the directory and roll the stamp back a rung. Clearing is what makes
    // the assertion unambiguous: a snapshot's filename is its `seq_id`, and no
    // writes happen between the close and the reopen, so a re-anchor would
    // otherwise overwrite the existing file and leave the count unchanged —
    // which is exactly what the first version of this test could not tell apart
    // from doing nothing.
    for entry in std::fs::read_dir(&snaps_dir).unwrap().flatten() {
        std::fs::remove_file(entry.path()).unwrap();
    }
    assert_eq!(count(&snaps_dir), 0);

    {
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap();
        let conn = raw.connect().unwrap();
        conn.execute("DROP INDEX IF EXISTS idx_lc_open_interval", ())
            .await
            .unwrap();
        conn.execute("PRAGMA user_version = 5", ()).await.unwrap();
    }

    let reopened = Database::open(&harness.db_path).await.unwrap();
    // Against the constant, not a literal: this test is about *re-anchoring
    // after an upgrade*, and pinning a version number here made it fail on the
    // T2.1 rung for a reason that had nothing to do with snapshots.
    // `migration_tests` owns the "a version bump brings its own rung test"
    // obligation.
    assert_eq!(
        reopened.schema_version(),
        macrame::schema::migrations::SCHEMA_VERSION,
        "the rung must have run"
    );

    assert!(
        count(&snaps_dir) > 0,
        "an upgraded database must be re-anchored at open, or the next \
         reconstruct folds from genesis"
    );

    reopened.close().await.unwrap();
}

/// A *fresh* database is not an upgrade, and `open()` writes nothing.
///
/// The companion to the test above and the reason `MigrationOutcome::upgraded`
/// excludes `from == 0`: an `open()` that touches the disk when it was not asked
/// to is surprising, and two existing tests already pin that it does not.
#[tokio::test]
async fn a_fresh_database_is_not_re_anchored() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    assert_eq!(
        count_snapshots(&db),
        0,
        "opening a new database must not write a snapshot"
    );
    db.close().await.unwrap();
}

/// Asks the handle where its snapshots live rather than rebuilding the naming
/// convention here — `derive_snapshots_dir` is private and a second copy of it
/// would be one more thing to keep in step.
fn count_snapshots(db: &Database) -> usize {
    std::fs::read_dir(db.snapshots_dir())
        .map(|d| d.flatten().count())
        .unwrap_or(0)
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
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(T1)
            .weight(1.0),
    )
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T1)
            .valid_to(OPEN)
            .weight(2.0),
    )
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
