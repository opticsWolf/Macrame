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
//!
//! The fourth test runs the other way (T0.3, D-078). §4.7 also listed **NaN**
//! weights as a gap, and that was wrong: the storage layer refuses NaN outright,
//! through every door including raw SQL. So it is pinned in the failing
//! direction — if the engine ever starts accepting NaN, that test breaks and
//! §4.7 has to be corrected back. A claim that the schema *does* enforce
//! something needs a tripwire exactly as much as a claim that it does not.

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

/// A negative weight is now refused by the storage layer (T2.1, D-083).
///
/// **This test used to assert the opposite, and the reversal is the delivery.**
/// Through 0.5.6 `links.weight` was a bare `REAL NOT NULL`, so `assert_edge`
/// accepted a negative weight and only `load_subgraph` refused it — the one
/// §4.7 gap the register called genuinely open, and the asymmetric one: a
/// database this crate wrote by itself could hold a row it would not read back.
/// Schema v7 closes it with `CHECK (weight >= 0.0 AND typeof(weight) = 'real')`.
///
/// So this now runs in the failing direction, like the NaN case below: it breaks
/// if the constraint is ever removed.
///
/// The loader guard is **not** thereby redundant, and this asserts that too:
/// `links_current` carries no such CHECK, and neither do cold files created
/// before the rung. `graph_tests` covers that path.
#[tokio::test]
async fn a_negative_weight_is_refused_by_the_schema_through_every_door() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    let via_api = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from(T0)
                .valid_to(OPEN)
                .weight(-1.5),
        )
        .await;
    assert!(
        via_api.is_err(),
        "§4.7/D-083: schema v7 refuses a negative weight — got {via_api:?}"
    );

    // Raw SQL too: this is a CHECK, not an actor-level guard, so the connection
    // §4.7 is about does not get to skip it.
    let raw = db.raw().connect().unwrap();
    let via_raw = raw
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES ('a','b','CITES',?1,?2,-1.5,'{}',?1)",
            libsql::params![T0, OPEN],
        )
        .await;
    assert!(
        via_raw.is_err(),
        "§4.7/D-083: raw SQL must not be able to write a negative weight either \
         — got {via_raw:?}"
    );

    // But `links_current` is derivative and deliberately unconstrained, which is
    // what keeps `NegativeEdgeWeight` reachable and the loader guard earning its
    // place.
    let into_current = raw
        .execute(
            "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, \
             valid_to, weight, properties, recorded_at) \
             VALUES ('a','b','KNOWS',?1,?2,-1.5,'{}',?1)",
            libsql::params![T0, OPEN],
        )
        .await;
    assert!(
        into_current.is_ok(),
        "links_current must stay unconstrained — got {into_current:?}"
    );
    let err = db.load_subgraph("a", 2, T0, 1 << 20).await.unwrap_err();
    assert!(
        matches!(err, DbError::NegativeEdgeWeight { weight, .. } if weight == -1.5),
        "got {err:?}"
    );

    db.close().await.unwrap();
}

/// A text weight is refused, and the reason is a panic rather than a bad answer.
///
/// `REAL` is an affinity, not a type. `'abc'` cannot be converted, so SQLite
/// stores it as TEXT — and since every text value sorts above every numeric one,
/// `'abc' >= 0.0` is **true**. A `CHECK (weight >= 0.0)` alone therefore lets it
/// through, which is why the shipped constraint also pins `typeof`.
///
/// What made this worth the extra clause: reading a text `weight` as `f64`
/// does not return an error. It reaches `unreachable!("invalid value type")`
/// inside libsql 0.9.30 and panics — in whatever query first touches the row,
/// arbitrarily far from the write that planted it.
#[tokio::test]
async fn a_text_weight_is_refused_because_reading_one_panics() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    let raw = db.raw().connect().unwrap();
    let planted = raw
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES ('a','b','CITES',?1,?2,'abc','{}',?1)",
            libsql::params![T0, OPEN],
        )
        .await;
    assert!(
        planted.is_err(),
        "§4.7/D-083: `weight >= 0.0` alone admits text, so the constraint pins \
         typeof too — got {planted:?}"
    );

    db.close().await.unwrap();
}

/// `+∞` is refused, and the reason is the transaction log rather than the maths.
///
/// Two wrong predictions preceded this test, which is why it is worth its space.
/// The plan expected `CHECK (weight >= 0.0)` to admit `9e999` — correct — and
/// argued the loader guard would catch it; the guard tests `< 0.0` and
/// `is_nan()`, so it does not. The next reading was that this is harmless: IEEE
/// infinity propagates through addition and stays totally ordered, so Dijkstra
/// terminates and reports the edge as unusable, an odd answer rather than a
/// wrong one.
///
/// Both were wrong. Writing the edge and then closing the database produced
/// `ReplayCorrupt { reason: "number out of range" }`. The log trigger serialises
/// the row to JSON; **JSON has no infinity**; the payload cannot be read back.
/// Every later `reconstruct()` fails, including the one `close()` runs. Under
/// Doctrine III the log is the source of truth, so a weight that cannot survive
/// it is not eccentric, it is corrupt — and the schema now refuses it with
/// `weight < 9e999`.
///
/// The second assertion is the one that would have caught this originally, and
/// it is here rather than in a replay test on purpose: it belongs beside the
/// constraint whose absence caused it.
#[tokio::test]
async fn an_infinite_weight_is_refused_because_the_log_cannot_carry_it() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    let via_api = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from(T0)
                .valid_to(OPEN)
                .weight(f64::INFINITY),
        )
        .await;
    assert!(
        via_api.is_err(),
        "§4.7/D-083: +inf must not reach links.weight — got {via_api:?}"
    );

    let raw = db.raw().connect().unwrap();
    let via_raw = raw
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES ('a','b','CITES',?1,?2,9e999,'{}',?1)",
            libsql::params![T0, OPEN],
        )
        .await;
    assert!(
        via_raw.is_err(),
        "§4.7/D-083: raw SQL must not write an infinite weight either — got \
         {via_raw:?}"
    );

    // Nothing landed, so the log is intact and the fold that `close()` runs
    // succeeds. That is the actual property under test: a database that accepted
    // the row above could not be closed.
    db.close()
        .await
        .expect("an infinite weight in the log makes reconstruct() fail");
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

// ---------------------------------------------------------------------------
// 4. NaN — the one §4.7 got wrong: the storage layer already refuses it
// ---------------------------------------------------------------------------

/// Every write door refuses a NaN weight, including raw SQL (T0.3, D-078).
///
/// §4.7 listed NaN beside negative weights as something only `load_subgraph`
/// catches. It is not: SQLite stores a NaN double as NULL, so `weight REAL NOT
/// NULL` rejects it before any of this crate's code sees it. That makes NaN the
/// opposite of the other three cases in this file — an invariant the storage
/// layer enforces, where §4.7 had claimed it was silent.
///
/// Pinned in the failing direction on purpose. If a future libSQL begins storing
/// NaN as a real double, these inserts start succeeding, this test fails, and
/// §4.7's row 3 has to be corrected *back* — along with the note on
/// `load_subgraph`'s `is_nan()` arm, which is currently unreachable and says so.
/// Discovering that by way of a shortest path over NaN would be considerably
/// worse.
#[tokio::test]
async fn a_nan_weight_is_refused_by_the_storage_layer_not_the_loader() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    two_concepts(&db).await;

    // 1. The public write path.
    let via_api = db
        .assert_edge(
            EdgeAssertion::new("a", "b", "KNOWS")
                .valid_from(T0)
                .valid_to(OPEN)
                .weight(f64::NAN),
        )
        .await;
    assert!(
        via_api.is_err(),
        "§4.7/D-078: NaN must not reach links.weight — got {via_api:?}"
    );

    let raw = db.raw().connect().unwrap();

    // 2. Raw SQL, NaN bound as a parameter — the door §4.7 is about.
    let bound = raw
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES ('a','b','CITES',?1,?2,?3,'{}',?1)",
            libsql::params![T0, OPEN, f64::NAN],
        )
        .await;
    assert!(
        bound.is_err(),
        "§4.7/D-078: raw SQL binding NaN must still be refused — got {bound:?}"
    );

    // 3. Raw SQL computing NaN in the engine, which never crosses the binding
    //    layer at all. Checked separately because it is a different code path
    //    and could plausibly behave differently.
    let literal = raw
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) \
             VALUES ('a','b','CITES2',?1,?2, 0.0/0.0, '{}', ?1)",
            libsql::params![T0, OPEN],
        )
        .await;
    assert!(
        literal.is_err(),
        "§4.7/D-078: an engine-computed NaN must be refused too — got {literal:?}"
    );

    // Nothing landed.
    let mut rows = raw
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0, "a NaN weight reached links");

    db.close().await.unwrap();
}
