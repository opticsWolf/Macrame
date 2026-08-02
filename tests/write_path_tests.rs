#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::prelude::*;

const T1: &str = "2026-01-01T00:00:00.000000Z";
const T2: &str = "2026-02-01T00:00:00.000000Z";
const T3: &str = "2026-03-01T00:00:00.000000Z";

/// Open a database with two concepts to hang edges off (`links` has a foreign
/// key into `concepts` and `PRAGMA foreign_keys` is ON).
async fn db_with_nodes(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    for id in ["A", "B"] {
        db.upsert_concept(ConceptUpsert::new(id, format!("Node {id}")).valid_from(T1))
            .await
            .unwrap();
    }
    db
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

fn open_edge() -> EdgeAssertion {
    EdgeAssertion::new("A", "B", "KNOWS")
        .valid_from(T1)
        .weight(0.8)
}

/// **The regression test for the responder-drop defect.**
///
/// Four of six high-priority commands and *all three* low-priority commands used
/// to fall through a `_ => LoopCtl::Continue` wildcard that dropped the
/// responder. The caller's `rx.await` resolved to a `RecvError` nothing mapped,
/// so the call never returned a value and never returned an error — a dropped
/// responder is indistinguishable from a hung database from the caller's side.
///
/// So the assertion is not "these succeed", it is **"these answer"**. Every
/// command is given five seconds; a timeout is the defect.
#[tokio::test]
async fn every_command_answers_its_caller() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;
    let limit = Duration::from_secs(5);

    macro_rules! answers {
        ($label:literal, $call:expr) => {
            tokio::time::timeout(limit, $call)
                .await
                .unwrap_or_else(|_| panic!("{} never answered its caller", $label))
        };
    }

    answers!("assert_edge", db.assert_edge(open_edge())).unwrap();
    answers!("retire_edge", db.retire_edge("A", "B", "KNOWS", T1, T2)).unwrap();
    answers!(
        "upsert_concept",
        db.upsert_concept(ConceptUpsert::new("A", "Renamed").valid_from(T1))
    )
    .unwrap();
    answers!(
        "write_bulk_atomic",
        db.write_bulk_atomic(vec![EdgeAssertion::new("A", "B", "LIKES").valid_from(T1)])
    )
    .unwrap();
    answers!("rebuild_current", db.rebuild_current()).unwrap();
    answers!(
        "bulk_import",
        db.bulk_import(vec![EdgeAssertion::new("B", "A", "KNOWS").valid_from(T1)])
    )
    .unwrap();
    answers!(
        "write_concepts",
        db.write_concepts(vec![ConceptUpsert::new("B", "Annotated").valid_from(T1)])
    )
    .unwrap();
    answers!("archive", db.archive(T3)).unwrap();

    db.close().await.unwrap();
}

#[tokio::test]
async fn assert_edge_writes_and_materializes() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    db.assert_edge(open_edge()).await.unwrap();

    assert_eq!(count(&db, "SELECT COUNT(*) FROM links").await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM links_current").await, 1);
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}

/// The guard exists in the schema; this proves the *typed* error reaches the
/// caller rather than an opaque engine failure. `SingleOpenViolation` was one of
/// the five `error.rs` variants nothing ever constructed.
#[tokio::test]
async fn a_second_open_interval_is_a_typed_violation() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;
    db.assert_edge(open_edge()).await.unwrap();

    let err = db
        .assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from(T2))
        .await
        .unwrap_err();

    match err {
        DbError::SingleOpenViolation {
            source_id,
            target_id,
            edge_type,
        } => {
            assert_eq!(
                (source_id.as_str(), target_id.as_str(), edge_type.as_str()),
                ("A", "B", "KNOWS")
            );
        }
        other => panic!("expected SingleOpenViolation, got {other:?}"),
    }
}

/// Doctrine III: retiring asserts a successor row, it never updates the original.
/// If this ever becomes an `UPDATE`, `reconstruct` at an earlier instant stops
/// seeing the interval as open and the ledger has lost history.
#[tokio::test]
async fn retiring_asserts_a_successor_and_preserves_the_original() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;
    db.assert_edge(open_edge()).await.unwrap();

    db.retire_edge("A", "B", "KNOWS", T1, T2).await.unwrap();

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM links").await,
        2,
        "retire must add a row, not modify one"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM links WHERE valid_to = '9999-12-31T23:59:59.999999Z'"
        )
        .await,
        1,
        "the original open assertion must survive untouched"
    );
    // Weight is carried onto the successor rather than reset to the default.
    let closed_weight: f64 = db
        .read_conn()
        .query(
            "SELECT weight FROM links_current WHERE valid_to = ?1",
            libsql::params![T2],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert!((closed_weight - 0.8).abs() < 1e-9);
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}

#[tokio::test]
async fn retiring_something_that_is_not_there_is_not_found() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    let err = db.retire_edge("A", "B", "KNOWS", T1, T2).await.unwrap_err();

    assert!(matches!(err, DbError::NotFound(_)), "got {err:?}");
}

/// Validation happens before the value crosses the channel, so the caller gets a
/// typed error with their own stack rather than an engine `CHECK` failure
/// surfacing from inside an actor.
#[tokio::test]
async fn bad_input_is_rejected_at_the_boundary() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    let err = db
        .assert_edge(EdgeAssertion::new("A", "B", "knows_well").valid_from(T1))
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::InvalidEdgeType(_)), "got {err:?}");

    let err = db
        .assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from("2026-01-01"))
        .await
        .unwrap_err();
    // `InvalidTimestamp`, not `ReplayCorrupt` (Wave 4.5). The caller passed a
    // malformed string; nothing in the ledger is damaged, and the old variant
    // carried `seq: 0` — a sequence number `AUTOINCREMENT` never issues.
    assert!(
        matches!(err, DbError::InvalidTimestamp { .. }),
        "a bad caller timestamp must not be reported as ledger corruption: got {err:?}"
    );
}

/// A caller passing the legacy second-precision form succeeds and the row lands
/// canonical (D-029), rather than tripping the storage `CHECK` with an opaque
/// engine error.
#[tokio::test]
async fn second_precision_input_is_widened_not_rejected() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    db.assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from("2026-01-01T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM links WHERE valid_from = '2026-01-01T00:00:00.000000Z'"
        )
        .await,
        1
    );
}

/// D-014: the batch is one act, so it gets one stamp and one transaction. A
/// failure anywhere must leave nothing behind.
#[tokio::test]
async fn write_bulk_atomic_is_all_or_nothing() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    let err = db
        .write_bulk_atomic(vec![
            EdgeAssertion::new("A", "B", "KNOWS").valid_from(T1),
            // Second open interval for the same edge -- trips the guard.
            EdgeAssertion::new("A", "B", "KNOWS").valid_from(T2),
        ])
        .await
        .unwrap_err();

    assert!(
        matches!(err, DbError::SingleOpenViolation { .. }),
        "got {err:?}"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM links").await,
        0,
        "the first row must have rolled back with the second"
    );
}

#[tokio::test]
async fn a_successful_bulk_write_shares_one_stamp() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    let n = db
        .write_bulk_atomic(vec![
            EdgeAssertion::new("A", "B", "KNOWS").valid_from(T1),
            EdgeAssertion::new("B", "A", "KNOWS").valid_from(T1),
        ])
        .await
        .unwrap();

    assert_eq!(n, 2);
    assert_eq!(
        count(&db, "SELECT COUNT(DISTINCT recorded_at) FROM links").await,
        1,
        "one act, one transaction time (D-014)"
    );
}

/// The monotonicity trigger fires on every concept update, so a second upsert
/// only works if the actor stamps from the clock rather than echoing the
/// caller's timestamp.
#[tokio::test]
async fn repeated_upserts_advance_recorded_at() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;

    db.upsert_concept(ConceptUpsert::new("A", "First").valid_from(T1))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("A", "Second").valid_from(T1))
        .await
        .unwrap();

    let title: String = db
        .read_conn()
        .query("SELECT title FROM concepts WHERE id = 'A'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(title, "Second");
}

#[tokio::test]
async fn rebuild_reports_what_it_rebuilt() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;
    db.assert_edge(open_edge()).await.unwrap();

    let report = db.rebuild_current().await.unwrap();

    assert_eq!(report.rows_rebuilt, 1);
    assert_eq!(report.drift_after, 0);
}

// -- D-041: derived analytics output stays out of the ledger ----------------

/// The regression test for the defect D-041 records.
///
/// Before 0.5.4 the write-back built a `ConceptUpsert` with the annotation in
/// `content`, so saving a partition replaced every annotated concept's document
/// text with a community label. The content is the user's; nothing derived may
/// touch it.
#[tokio::test]
async fn an_annotation_write_back_leaves_concept_content_alone() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new("A", "Node A")
            .content("the document text that must survive")
            .valid_from(T1),
    )
    .await
    .unwrap();

    let written = db
        .write_analytics_annotations(vec![Annotation::new("A", "louvain.community", "3")])
        .await
        .unwrap();
    assert_eq!(written, 1);

    let content: String = db
        .read_conn()
        .query("SELECT content FROM concepts WHERE id = 'A'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(content, "the document text that must survive");
}

/// Doctrine VII's reasoning, applied to the other derived artifact: an
/// annotation is a function of an algorithm and a graph, not a statement about
/// the world, so the ledger must not carry it.
#[tokio::test]
async fn annotations_never_reach_the_transaction_log() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("A", "Node A").valid_from(T1))
        .await
        .unwrap();

    let before = log_len(&db).await;
    db.write_analytics_annotations(vec![Annotation::new("A", "kcore.shell", "2")])
        .await
        .unwrap();
    assert_eq!(
        log_len(&db).await,
        before,
        "an annotation write appended to transaction_log"
    );
}

/// Rerunning an algorithm on an unchanged graph must replace the previous pass,
/// not accumulate rows and not version the concept. The second half is the
/// subtler one: a concept `UPDATE` needs a strictly advancing `recorded_at`, so
/// routing annotations through the ledger made the analytics *schedule* look
/// like history.
#[tokio::test]
async fn rerunning_replaces_the_annotation_without_versioning_the_concept() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("A", "Node A").valid_from(T1))
        .await
        .unwrap();

    let after_seed = log_len(&db).await;

    for value in ["3", "7"] {
        db.write_analytics_annotations(vec![Annotation::new("A", "louvain.community", value)])
            .await
            .unwrap();
    }

    let rows = count(&db, "SELECT COUNT(*) FROM analytics_annotations").await;
    assert_eq!(rows, 1, "the upsert key is (concept_id, label)");

    let value: String = db
        .read_conn()
        .query(
            "SELECT value FROM analytics_annotations WHERE concept_id='A' AND label='louvain.community'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(value, "7", "the rerun must win");

    assert_eq!(
        log_len(&db).await,
        after_seed,
        "two analytics passes versioned the concept"
    );
}

/// The same guarantees through the surface analytics actually calls.
#[tokio::test]
async fn write_back_annotations_routes_through_the_derivative_table() {
    let harness = TestHarness::new();
    let db = db_with_nodes(&harness).await;
    db.assert_edge(open_edge()).await.unwrap();

    let graph = db.load_subgraph("A", 2, T2, 1 << 20).await.unwrap();
    let values: std::collections::BTreeMap<String, String> = graph
        .node_ids()
        .map(|id| (id.to_string(), "0".into()))
        .collect();

    let before = log_len(&db).await;
    let written = graph
        .write_back_annotations(&db, "louvain.community", &values)
        .await
        .unwrap();

    assert_eq!(written, graph.node_count());
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM analytics_annotations").await,
        written as i64
    );
    assert_eq!(log_len(&db).await, before);
}

async fn log_len(db: &Database) -> i64 {
    count(db, "SELECT COUNT(*) FROM transaction_log").await
}
