//! The valid-axis reconstruction (0.20.0, P2, D-292).
//!
//! `reconstruct_as_of` selects, for each valid start, the current belief whose
//! interval contains the valid instant, where `reconstruct` keeps only the row
//! written last per entity. These tests are the plan's gates 6–10, plus the
//! archive arm: a superseded start whose row has left the hot log must still
//! answer a valid read.

#[path = "common/harness.rs"]
mod harness;

use std::collections::BTreeMap;

use harness::{ensure_marks_table, mark_disordered, TestHarness};
use macrame::temporal::{AsOf, MaterializedState};
use macrame::{ConceptUpsert, Database, DbError, OverlapProvenance};

const PRE: &str = "2025-06-01T00:00:00.000000Z";
const JAN: &str = "2026-01-01T00:00:00.000000Z";
const MAR: &str = "2026-03-01T00:00:00.000000Z";
const APR: &str = "2026-04-01T00:00:00.000000Z";
const JUN: &str = "2026-06-01T00:00:00.000000Z";
const JUL: &str = "2026-07-01T00:00:00.000000Z";
const SEP: &str = "2026-09-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const AUG: &str = "2026-08-01T00:00:00.000000Z";
const FAR: &str = "2030-01-01T00:00:00.000000Z";

fn concept(id: &str, title: &str, from: &str, to: Option<&str>) -> ConceptUpsert {
    let c = ConceptUpsert::new(id, title).valid_from(from);
    match to {
        Some(to) => c.valid_to(to),
        None => c,
    }
}

async fn open_db(harness: &TestHarness) -> Database {
    Database::open(&harness.db_path).await.unwrap()
}

/// Every write waits a tick, so each log row carries its own stamp. The same
/// reason as in `concept_interval_guard_tests`: two writes to one concept in one
/// tick trip the monotonic `recorded_at` trigger before any guard is reached.
async fn upsert(db: &Database, concept: ConceptUpsert) {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    db.upsert_concept(concept).await.unwrap();
}

/// The `recorded_at` of each log row for `entity`, oldest first.
async fn stamps(db: &Database, entity: &str) -> Vec<String> {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT recorded_at FROM transaction_log WHERE entity_id = ?1 ORDER BY seq_id",
            libsql::params![entity],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(0).unwrap());
    }
    out
}

async fn count(db: &Database, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// The title of concept `a` as believed at `recorded` and held at `valid`.
async fn belief_at(db: &Database, recorded: &str, valid: &str) -> Option<String> {
    let state = db
        .reconstruct_as_of(AsOf::bitemporal(valid, recorded))
        .await
        .unwrap();
    titles(&state).get("a").cloned()
}

/// Concept id to title, which is what each state is compared and asserted by.
fn titles(state: &MaterializedState) -> BTreeMap<String, String> {
    state
        .concepts
        .iter()
        .map(|(id, attrs)| (id.clone(), attrs.title.clone()))
        .collect()
}

/// A concept row written straight to the log, past the write guard. Only raw SQL
/// can put a second belief for a claimed instant into the ledger, which is the
/// case the fold has to refuse by name.
async fn raw_concept(db: &Database, id: &str, title: &str, from: &str, to: &str, stamp: &str) {
    let conn = db.raw().connect().unwrap();
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', ?1, 'I', \
                 json_object('title', ?2, 'content', '', 'valid_from', ?3, \
                             'valid_to', ?4, 'retired', 0), ?5)",
        libsql::params![id, title, from, to, stamp],
    )
    .await
    .unwrap();
}

/// A link row written straight to the log, past the write guard.
async fn raw_link(db: &Database, entity: &str, from: &str, to: &str, stamp: &str) {
    let conn = db.raw().connect().unwrap();
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('links', ?1, 'I', \
                 json_object('source_id', 'a', 'target_id', 'b', 'edge_type', 'rel', \
                             'valid_from', ?2, 'valid_to', ?3), ?4)",
        libsql::params![entity, from, to, stamp],
    )
    .await
    .unwrap();
}

/// Gate 6: a recorded-only read is `reconstruct` at the same instant. The
/// valid-axis code must not perturb the recorded axis it shares a function with.
#[tokio::test]
async fn a_recorded_only_read_is_reconstruct_at_that_instant() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "v1", JAN, None)).await;
    upsert(&db, concept("a", "v2", JAN, Some(JUN))).await;
    let [s1, s2]: [String; 2] = stamps(&db, "a").await.try_into().unwrap();

    for ts in [s1.as_str(), s2.as_str(), FAR] {
        let via_as_of = db.reconstruct_as_of(AsOf::recorded_at(ts)).await.unwrap();
        let via_reconstruct = db.reconstruct(ts).await.unwrap();
        assert_eq!(titles(&via_as_of), titles(&via_reconstruct), "at {ts}");
        assert_eq!(via_as_of.timestamp, via_reconstruct.timestamp);
        assert_eq!(via_as_of.edges.len(), via_reconstruct.edges.len());
    }

    db.close().await.unwrap();
}

/// Gate 7: the worked example. One concept, two valid starts. A valid instant
/// selects the start whose interval holds it, and the recorded-only read still
/// answers with whichever row was written last.
#[tokio::test]
async fn a_valid_instant_selects_the_start_whose_interval_holds_it() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "old", JAN, Some(JUN))).await;
    upsert(&db, concept("a", "new", JUN, None)).await;

    let in_march = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    assert_eq!(titles(&in_march)["a"], "old");

    let in_july = db.reconstruct_as_of(AsOf::valid_at(JUL)).await.unwrap();
    assert_eq!(titles(&in_july)["a"], "new");

    let recorded = db.reconstruct_as_of(AsOf::recorded_at(FAR)).await.unwrap();
    assert_eq!(
        titles(&recorded)["a"],
        "new",
        "the recorded axis keeps one row per entity, the last written"
    );

    db.close().await.unwrap();
}

/// Gate 8: the bitemporal cell. A correction recorded after an earlier belief
/// changes what a valid read answers when it is bounded at the later record, and
/// leaves the earlier answer for a read bounded before it.
#[tokio::test]
async fn a_valid_read_bounded_at_a_recorded_instant_sees_the_belief_then_held() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "v1", JAN, None)).await;
    upsert(&db, concept("a", "v2", JAN, Some(JUN))).await;
    let [s1, s2]: [String; 2] = stamps(&db, "a").await.try_into().unwrap();

    assert_eq!(belief_at(&db, &s1, MAR).await.as_deref(), Some("v1"));
    assert_eq!(belief_at(&db, &s2, MAR).await.as_deref(), Some("v2"));

    let present = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    assert_eq!(
        titles(&present).get("a").map(String::as_str),
        Some("v2"),
        "with no recorded bound, the present belief answers"
    );

    // The open v1 start covers July as believed at s1. The correction closed it
    // at June, so at s2 nothing covers July.
    assert_eq!(belief_at(&db, &s1, JUL).await.as_deref(), Some("v1"));
    assert_eq!(belief_at(&db, &s2, JUL).await, None);

    db.close().await.unwrap();
}

/// Gate 9: an instant before the ledger is an empty state, and says nothing about
/// the recorded axis. `predates_recorded_history` answers a recorded question, so
/// a valid miss must not set it.
#[tokio::test]
async fn a_valid_instant_before_the_ledger_is_an_empty_state() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "only", JAN, None)).await;

    let state = db.reconstruct_as_of(AsOf::valid_at(PRE)).await.unwrap();
    assert!(state.concepts.is_empty(), "nothing was valid in 2025");
    assert!(
        !state.predates_recorded_history,
        "a valid miss is not a recorded-history miss"
    );
    assert_eq!(state.timestamp, PRE, "the state is at the valid instant");

    db.close().await.unwrap();
}

/// Gate 10, concept case: two open starts that both claim the instant are refused
/// by name, not answered with one of them.
#[tokio::test]
async fn two_open_starts_claiming_one_instant_are_refused_by_name() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_concept(&db, "a", "one", JAN, OPEN, "2026-02-01T00:00:00.000001Z").await;
    raw_concept(&db, "a", "two", MAR, OPEN, "2026-02-01T00:00:00.000002Z").await;

    let err = db
        .reconstruct_as_of(AsOf::valid_at(APR))
        .await
        .expect_err("both open starts contain April");
    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "got {err:?}"
    );
    // Nothing was written, so the read does not word itself as a refused write
    // (0.19.5, D-298).
    let message = err.to_string();
    assert!(
        message.contains("is recorded") && !message.contains("is refused"),
        "{message}"
    );

    db.close().await.unwrap();
}

/// Gate 10, concept case: two closed intervals that overlap are refused with both
/// named, and the overlap is the concept's, not the single-open violation.
#[tokio::test]
async fn two_closed_concept_intervals_claiming_one_instant_are_refused() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_concept(&db, "a", "one", JAN, JUN, "2026-02-01T00:00:00.000001Z").await;
    raw_concept(&db, "a", "two", MAR, SEP, "2026-02-01T00:00:00.000002Z").await;

    let err = db
        .reconstruct_as_of(AsOf::valid_at(APR))
        .await
        .expect_err("[Jan, Jun) and [Mar, Sep) both contain April");
    let DbError::OverlappingConcept { ref overlap } = err else {
        panic!("expected OverlappingConcept, got {err:?}");
    };
    assert_eq!(overlap.id, "a");
    assert_eq!(
        overlap.provenance,
        OverlapProvenance::BothRecorded,
        "a read finds the collision in the ledger, asserted by nobody (D-295)"
    );
    assert!(
        err.to_string().contains("the recorded"),
        "a read must not word its refusal as a write: {err}"
    );

    db.close().await.unwrap();
}

/// Gate 10, link case: the same claim on an edge is `OverlappingInterval`, keyed on
/// the edge's `(source, target, type)` rather than on its entity id.
#[tokio::test]
async fn two_link_intervals_claiming_one_instant_are_refused() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_link(&db, "edge-1", JAN, JUN, "2026-02-01T00:00:00.000001Z").await;
    raw_link(&db, "edge-2", MAR, SEP, "2026-02-01T00:00:00.000002Z").await;

    let err = db
        .reconstruct_as_of(AsOf::valid_at(APR))
        .await
        .expect_err("the same edge is claimed twice for April");
    assert!(
        matches!(err, DbError::OverlappingInterval { .. }),
        "got {err:?}"
    );

    db.close().await.unwrap();
}

/// Edge selection: one edge with two valid starts answers each valid instant with
/// the start that holds it. The positive control for the link refusal above.
#[tokio::test]
async fn a_link_answers_each_valid_instant_with_the_start_that_holds_it() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_link(&db, "edge-1", JAN, JUN, "2026-02-01T00:00:00.000001Z").await;
    raw_link(&db, "edge-2", JUN, OPEN, "2026-02-01T00:00:00.000002Z").await;

    let in_march = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    assert_eq!(in_march.edges.len(), 1);
    assert_eq!(in_march.edges[0].valid_to, JUN);

    let in_july = db.reconstruct_as_of(AsOf::valid_at(JUL)).await.unwrap();
    assert_eq!(in_july.edges.len(), 1);
    assert_eq!(in_july.edges[0].valid_to, OPEN);

    db.close().await.unwrap();
}

/// The archive arm. Since 0.19.5 `LOG_ARCHIVABLE` sends a concept row cold only
/// once a newer row exists for the same start (D-298), so the superseded January
/// row goes and the June start's only belief stays hot, where the write guard
/// reads it. The valid read folds hot and cold together and answers each start.
#[tokio::test]
async fn a_superseded_start_archived_away_still_answers_its_valid_read() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "jan", JAN, Some(JUN))).await;
    upsert(&db, concept("a", "jun", JUN, None)).await;
    // Re-assert the January start. Its first row is now superseded by the entity's
    // newest row, which is what makes it archivable.
    upsert(&db, concept("a", "jan-corrected", JAN, Some(JUN))).await;

    let report = db.archive(FAR).await.unwrap();
    assert!(
        report.log_entries_archived >= 1,
        "the fixture needs the first January row archived: {report:?}"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE json_extract(payload, '$.title') = 'jan'"
        )
        .await,
        0,
        "the superseded January row has left the hot log"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE json_extract(payload, '$.title') = 'jun'"
        )
        .await,
        1,
        "the June start's current belief stays hot (D-298)"
    );

    let in_march = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    assert_eq!(titles(&in_march)["a"], "jan-corrected");

    let in_july = db.reconstruct_as_of(AsOf::valid_at(JUL)).await.unwrap();
    assert_eq!(
        titles(&in_july).get("a").map(String::as_str),
        Some("jun")
    );

    db.close().await.unwrap();
}

/// Gate 7 (with gate 2's flip): §3.3's worked example. C2 is ingested first,
/// valid from June open, and C1 is backfilled after it, valid from January to
/// June. The valid axis answers per start, so August is C2 and March is C1;
/// the **recorded** read follows the one state rule now — the pair is marked
/// (the skip handler's job once v24 provisions the table, the rung's fill for
/// history), and its recorded read is C2, the open belief, *equal to
/// `reconstruct`* — the fold-equals-projection property (§2.2) is kept by the
/// same mechanism, not by coincidence. Asserting `reconstruct` beside it is
/// the property: a mutant that corrects one read and not the other dies here.
#[tokio::test]
async fn the_backfilled_ledger_answers_each_axis_its_own_way() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    // The pair the rung's fill will hold in history; same side table, same
    // semantics (marks never clear).
    ensure_marks_table(&db).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    let in_august = db.reconstruct_as_of(AsOf::valid_at(AUG)).await.unwrap();
    assert_eq!(titles(&in_august)["src/a.rs"], "C2");

    let in_march = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    assert_eq!(titles(&in_march)["src/a.rs"], "C1");

    let recorded = db.reconstruct_as_of(AsOf::recorded_at(FAR)).await.unwrap();
    assert_eq!(
        titles(&recorded)["src/a.rs"], "C2",
        "the rule answers the recorded read as it does every recorded read"
    );
    assert_eq!(
        titles(&recorded),
        titles(&db.reconstruct(FAR).await.unwrap()),
        "the recorded read is reconstruct, under the rule as before it"
    );

    db.close().await.unwrap();
}

/// Gate 7's control: the backfill is admitted and is not an overlap, so the check
/// that reports overlaps reports nothing for it.
#[tokio::test]
async fn a_backfilled_ledger_with_no_overlap_verifies_clean() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;

    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}

/// Gate 10: a pre-existing overlap is refused at read time, naming the concept and
/// both intervals, and `verify_concept_intervals` reports the same pair. The
/// comparison is with the error the read returns, not with a rebuilt expectation.
#[tokio::test]
async fn verify_reports_the_overlap_the_read_refuses() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_concept(&db, "a", "one", JAN, JUN, "2026-02-01T00:00:00.000001Z").await;
    raw_concept(&db, "a", "two", MAR, SEP, "2026-02-01T00:00:00.000002Z").await;

    let read = db
        .reconstruct_as_of(AsOf::valid_at(APR))
        .await
        .expect_err("[Jan, Jun) and [Mar, Sep) both contain April");
    let DbError::OverlappingConcept { overlap: refused } = read else {
        panic!("expected OverlappingConcept, got {read:?}");
    };

    let found = db.verify_concept_intervals().await.unwrap();
    assert_eq!(found.len(), 1, "one pair overlaps: {found:?}");
    assert_eq!(found[0], *refused);

    db.close().await.unwrap();
}

/// The single-open case is reported too, and as the same pair the read refuses.
#[tokio::test]
async fn verify_reports_two_open_starts_that_meet() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    raw_concept(&db, "a", "one", JAN, OPEN, "2026-02-01T00:00:00.000001Z").await;
    raw_concept(&db, "a", "two", MAR, OPEN, "2026-02-01T00:00:00.000002Z").await;

    let read = db
        .reconstruct_as_of(AsOf::valid_at(APR))
        .await
        .expect_err("both open starts contain April");
    let DbError::ConceptSingleOpen { overlap: refused } = read else {
        panic!("expected ConceptSingleOpen, got {read:?}");
    };

    let found = db.verify_concept_intervals().await.unwrap();
    assert_eq!(found, vec![*refused]);

    db.close().await.unwrap();
}

/// The check walks the archive as the read does. After superseded rows are archived
/// the ledger still verifies clean, and the check does not refuse the archived ledger
/// for want of rows it could not see.
#[tokio::test]
async fn verify_still_runs_once_superseded_rows_are_archived() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", "jan", JAN, Some(JUN))).await;
    upsert(&db, concept("a", "jun", JUN, None)).await;
    upsert(&db, concept("a", "jan-corrected", JAN, Some(JUN))).await;
    db.archive(FAR).await.unwrap();

    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}

/// A valid-axis state is not a belief at a recorded instant, and `save_snapshot`
/// refuses it rather than let it become an anchor (0.19.5, D-298).
#[tokio::test]
async fn a_valid_axis_state_cannot_be_saved_as_a_snapshot() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    upsert(&db, concept("a", "jan", JAN, None)).await;

    let state = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    let dir = harness.db_path.with_extension("valid-snapshots");
    let err = macrame::temporal::save_snapshot(&dir, &state)
        .expect_err("a valid-axis state is refused");
    assert!(matches!(err, DbError::SnapshotWriteFailed { .. }), "got {err:?}");

    db.close().await.unwrap();
}

/// Concurrent valid reads on an archived database do not DETACH the archive under
/// each other (0.19.5, D-298): each fold ATTACHes it, so they run on a connection
/// of their own rather than on the shared `read_conn`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_valid_reads_on_an_archived_database_do_not_collide() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    upsert(&db, concept("a", "jan", JAN, Some(JUN))).await;
    upsert(&db, concept("a", "jun", JUN, None)).await;
    upsert(&db, concept("a", "jan-corrected", JAN, Some(JUN))).await;
    db.archive(FAR).await.unwrap();

    let db = std::sync::Arc::new(db);
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let db = db.clone();
            tokio::spawn(async move {
                for _ in 0..25 {
                    if i % 2 == 0 {
                        let s = db.reconstruct_as_of(AsOf::valid_at(JUL)).await.unwrap();
                        assert_eq!(titles(&s)["a"], "jun");
                    } else {
                        assert!(db.verify_concept_intervals().await.unwrap().is_empty());
                    }
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }

    std::sync::Arc::into_inner(db).unwrap().close().await.unwrap();
}

/// The valid-axis mark survives a serde round trip, so a state decoded from a
/// cache is still refused as a snapshot (0.19.6, D-299).
#[tokio::test]
async fn a_valid_axis_state_round_tripped_through_serde_is_still_refused() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    upsert(&db, concept("a", "jan", JAN, None)).await;

    let state = db.reconstruct_as_of(AsOf::valid_at(MAR)).await.unwrap();
    let json = serde_json::to_string(&state).unwrap();
    let decoded: MaterializedState = serde_json::from_str(&json).unwrap();
    let dir = harness.db_path.with_extension("valid-snapshots");
    let err = macrame::temporal::save_snapshot(&dir, &decoded)
        .expect_err("the decoded state is still at a valid instant");
    assert!(matches!(err, DbError::SnapshotWriteFailed { .. }), "got {err:?}");

    db.close().await.unwrap();
}

/// A start whose only belief is in the archive, as an archive written before
/// 0.19.5 can hold (D-298): the valid read and the interval check both reach it
/// (0.19.6, D-299). The fixture moves the row cold by hand, as the old
/// `LOG_ARCHIVABLE` did once a later start existed.
#[tokio::test]
async fn a_start_held_only_in_the_archive_answers_its_valid_read() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    upsert(&db, concept("a", "jan", JAN, Some(JUN))).await;
    upsert(&db, concept("a", "jun", JUN, None)).await;
    upsert(&db, concept("a", "jan-corrected", JAN, Some(JUN))).await;
    db.archive(FAR).await.unwrap();
    let archive = db.archive_path().to_path_buf();
    db.close().await.unwrap();

    let raw = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    raw.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive.to_string_lossy().to_string()],
    )
    .await
    .unwrap();
    raw.execute_batch(
        "CREATE TABLE macrame_archive_session (x);
         INSERT INTO cold.transaction_log
             (seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id)
         SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id
         FROM main.transaction_log WHERE json_extract(payload, '$.title') = 'jun';
         DELETE FROM main.transaction_log WHERE json_extract(payload, '$.title') = 'jun';
         DROP TABLE macrame_archive_session;
         DETACH DATABASE cold;",
    )
    .await
    .unwrap();
    drop(raw);

    let db = open_db(&harness).await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE json_extract(payload, '$.title') = 'jun'"
        )
        .await,
        0,
        "the June start's only belief is cold"
    );
    let in_july = db.reconstruct_as_of(AsOf::valid_at(JUL)).await.unwrap();
    assert_eq!(
        titles(&in_july).get("a").map(String::as_str),
        Some("jun"),
        "the June belief lives only in the archive"
    );
    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}
