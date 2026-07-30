//! §4.7 — the three places where a check sits above the storage layer.
//!
//! The architecture document states one property in §4.7: *some invariants of
//! this ledger are enforced by the API and not by the schema, so a database
//! reached by another writer can hold rows this crate would never write.* That
//! statement is prose, and prose is exactly what stops being true quietly.
//!
//! These tests pin it from the other side. Each one asserts that the storage
//! layer **accepts** what a boundary above it refuses — so if a later migration
//! closes one of the three with a trigger or a `CHECK`, the test fails and §4.7
//! must be corrected rather than left describing a limit that no longer exists.
//! A passing suite here is not a virtue; it is a fact about where the checks are.
//!
//! The three are deliberately not identical, and §4.7 says why: two of them are
//! "the schema permits what the write API refuses", and the third — negative
//! weights — is permitted by the write API too, with the refusal at the *read*
//! boundary. Consolidating the statement must not flatten that difference.

mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database, DbError};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

const JAN: &str = "2026-01-01T00:00:00.000000Z";
const MAR: &str = "2026-03-01T00:00:00.000000Z";
const APR: &str = "2026-04-01T00:00:00.000000Z";
const JUN: &str = "2026-06-01T00:00:00.000000Z";
const SEP: &str = "2026-09-01T00:00:00.000000Z";

async fn two_concepts(db: &Database) {
    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, id).valid_from(T0))
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// 1. Overlapping closed intervals — D-060, enforced in the write actor
// ---------------------------------------------------------------------------

/// The actor refuses `[Mar, Sep)` against `[Jan, Jun)`; raw SQL writes it.
///
/// This is the honest half of D-060. Guarding closed intervals in
/// `trg_links_single_open` would have made the property structural, at the cost
/// of a second index probe on every insert — on the path D-059 had just finished
/// making fast — to constrain callers who were going through the actor anyway.
/// The decision was to take the cheaper enforcement and *say* what it does not
/// cover. This is that statement, executable.
#[tokio::test]
async fn raw_sql_writes_the_overlapping_pair_the_actor_refuses() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(JAN)
            .valid_to(JUN),
    )
    .await
    .unwrap();

    // Through the API: refused.
    let err = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from(MAR)
                .valid_to(SEP),
        )
        .await
        .expect_err("the actor guards this");
    assert!(
        matches!(err, DbError::OverlappingInterval { .. }),
        "got {err:?}"
    );

    // Through a connection of one's own: accepted, no trigger objects.
    let raw = db.raw().connect().unwrap();
    raw.execute(
        "INSERT INTO links \
         (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('a', 'b', 'KNOWS', ?1, ?2, 1.0, '{}', ?3)",
        libsql::params![MAR, SEP, T0],
    )
    .await
    .expect(
        "§4.7: the schema does not guard closed-interval overlap. If this now \
         fails, a migration has closed the gap and §4.7 and D-060 need updating.",
    );

    // And the consequence D-060 exists to prevent is present in that file.
    let edges = macrame::temporal::query_as_of_edges(db.read_conn(), APR)
        .await
        .unwrap();
    assert_eq!(
        edges.len(),
        2,
        "one relationship returned twice — the wrong answer the actor's guard \
         prevents for writers who go through it"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// 2. Negative weights — refused at the read boundary, by nothing before it
// ---------------------------------------------------------------------------

/// `assert_edge` accepts a negative weight; `load_subgraph` refuses the graph.
///
/// The odd one of the three. `links.weight` is a bare `REAL NOT NULL` with no
/// `CHECK` — adding one is a D-036 schema change not taken unilaterally — and
/// `EdgeAssertion::normalized` validates ids, edge type and timestamps but not
/// weight. So the value lands through the supported write path and is refused
/// only when something tries to run Dijkstra or A\* over it, which are unsound
/// for negative weights: the result would be a shortest path that is merely a
/// path (D-039).
///
/// Worth naming because it makes the §4.7 property asymmetric: a database this
/// crate wrote by itself *can* hold a row the crate refuses to read back.
#[tokio::test]
async fn the_write_api_accepts_a_negative_weight_that_the_loader_refuses() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN)
            .weight(-1.5),
    )
    .await
    .expect(
        "§4.7: neither the schema nor EdgeAssertion::normalized checks weight. \
         If this now fails, the check has moved to the write boundary and §4.7 \
         should say so.",
    );

    let err = db.load_subgraph("a", 2, T0, 1 << 20).await.unwrap_err();
    assert!(
        matches!(err, DbError::NegativeEdgeWeight { weight, .. } if weight == -1.5),
        "got {err:?}"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// 3. The mechanism — D-068: read_conn is protected, raw() is not
// ---------------------------------------------------------------------------

/// The two connections the handle hands out differ in exactly one way.
///
/// `read_conn()` carries `PRAGMA query_only = ON` (D-019), which is the runtime
/// half of write serialisation; `raw()` carries nothing, which is what makes
/// test 1 above possible at all. D-068 kept `raw()` public because making it
/// private would not make single-writer true — the file is reachable by any
/// SQLite client on the machine — it would only remove the supported way to do
/// the thing. This test is what keeps that statement from drifting: it fails if
/// `read_conn` ever loses its pragma, and it fails if `raw()` ever gains one.
#[tokio::test]
async fn read_conn_refuses_a_write_and_raw_does_not() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let refused = db
        .read_conn()
        .execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES ('x', 'x', ?1, ?1)",
            libsql::params![T0],
        )
        .await;
    assert!(
        refused.is_err(),
        "read_conn must stay query_only (D-019) — got {refused:?}"
    );

    let permitted = db
        .raw()
        .connect()
        .unwrap()
        .execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES ('x', 'x', ?1, ?1)",
            libsql::params![T0],
        )
        .await;
    assert!(
        permitted.is_ok(),
        "§4.7/D-068: raw() is an unprotected connection by design — got \
         {permitted:?}"
    );

    db.close().await.unwrap();
}
