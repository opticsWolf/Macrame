//! The concept valid-interval guard (0.19.2, D-291, plan P1).
//!
//! A concept has one belief in force per valid start, and at most one of those
//! beliefs is open. These tests are the plan's gates 1–4, plus the cases the
//! current-belief rule exists for: a start that is re-asserted or closed must
//! stop blocking later intervals, because the history it leaves behind is
//! superseded and not live (Doctrine III).

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::{BranchId, ConceptUpsert, Database, DbError, OverlapProvenance};

const JAN: &str = "2026-01-01T00:00:00.000000Z";
const MAR: &str = "2026-03-01T00:00:00.000000Z";
const JUN: &str = "2026-06-01T00:00:00.000000Z";
const SEP: &str = "2026-09-01T00:00:00.000000Z";
const DEC: &str = "2026-12-01T00:00:00.000000Z";
const FAR: &str = "2030-01-01T00:00:00.000000Z";

fn concept(id: &str, from: &str, to: Option<&str>) -> ConceptUpsert {
    let c = ConceptUpsert::new(id, id).valid_from(from);
    match to {
        Some(to) => c.valid_to(to),
        None => c,
    }
}

async fn open_db(harness: &TestHarness) -> Database {
    Database::open(&harness.db_path).await.unwrap()
}

async fn count(db: &Database, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Every write waits a tick first. A transaction stamp is the clock's reading,
/// and two writes to one concept in the same tick are refused by the monotonic
/// `recorded_at` trigger (`RecordedAtRegression`) before the guard is reached.
/// That refusal is real and not this guard's, so the tests keep it out of the way
/// rather than pinning it.
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

async fn upsert(db: &Database, concept: ConceptUpsert) -> macrame::Result<()> {
    tick().await;
    db.upsert_concept(concept).await
}

async fn write(db: &Database, concepts: Vec<ConceptUpsert>) -> macrame::BulkResult<usize> {
    tick().await;
    db.write_concepts(concepts).await
}

/// Gate 1: a second closed interval that overlaps the first is refused, and the
/// refusal names both intervals.
#[tokio::test]
async fn an_overlapping_closed_interval_is_refused_naming_both() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    let err = upsert(&db, concept("a", MAR, Some(SEP)))
        .await
        .expect_err("[Mar, Sep) overlaps the recorded [Jan, Jun)");

    let DbError::OverlappingConcept { ref overlap } = err else {
        panic!("expected OverlappingConcept, got {err:?}");
    };
    assert_eq!(overlap.id, "a");
    assert_eq!((overlap.valid_from.as_str(), overlap.valid_to.as_str()), (MAR, SEP));
    assert_eq!(
        (overlap.existing_from.as_str(), overlap.existing_to.as_str()),
        (JAN, JUN)
    );
    assert_eq!(
        overlap.provenance,
        OverlapProvenance::AlreadyRecorded,
        "the collision is with a recorded row"
    );

    db.close().await.unwrap();
}

/// Gate 2: two open intervals are the single-open violation, not an overlap.
/// The partition is pinned because folding the two into one refusal would
/// leave the specific variant constructible by nothing.
#[tokio::test]
async fn two_open_intervals_are_the_single_open_violation() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    let err = upsert(&db, concept("a", MAR, None))
        .await
        .expect_err("a second open interval from a different start");

    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "the partition must hold: got {err:?}"
    );
    assert!(!matches!(err, DbError::OverlappingConcept { .. }));

    db.close().await.unwrap();
}

/// Gate 3: a batch that contradicts itself is refused, and nothing it named is
/// written. The database cannot see the batch's own rows, so this is the case a
/// per-row check alone would commit.
#[tokio::test]
async fn a_batch_that_contradicts_itself_is_refused_with_nothing_written() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    let err = write(&db, vec![
            concept("c", JAN, Some(JUN)),
            concept("c", MAR, Some(SEP)),
        ])
        .await
        .expect_err("the batch overlaps itself");

    assert_eq!(err.written, 0, "refused before any chunk committed");
    let DbError::OverlappingConcept { ref overlap } = err.cause else {
        panic!("expected OverlappingConcept, got {:?}", err.cause);
    };
    assert_eq!(
        overlap.provenance,
        OverlapProvenance::SameBatch,
        "the collision is inside the call: {overlap:?}"
    );
    assert!(err.cause.to_string().contains("this same batch"));

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'c'").await,
        0,
        "no current row was written"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'c'").await,
        0,
        "no log row was written"
    );

    db.close().await.unwrap();
}

/// Gate 4: successive intervals that abut do not overlap, and are admitted. The
/// positive control, so the guard is not a refusal of everything.
#[tokio::test]
async fn successive_closed_intervals_that_abut_are_admitted() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    upsert(&db, concept("a", JUN, Some(DEC)))
        .await
        .expect("[Jun, Dec) abuts [Jan, Jun) and does not overlap it");

    db.close().await.unwrap();
}

/// Re-asserting a start is a new belief about the same interval (Doctrine III),
/// so it supersedes the start's previous row and cannot clash with it.
#[tokio::test]
async fn re_asserting_a_start_supersedes_it() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    upsert(&db, ConceptUpsert::new("a", "renamed").valid_from(JAN))
        .await
        .expect("the same start is the same interval");
    upsert(&db, concept("a", JAN, Some(JUN)))
        .await
        .expect("closing the start at the same start is a correction");

    db.close().await.unwrap();
}

/// The current-belief case: once a start has been closed, its earlier open row
/// is superseded, so a later open interval from a new start is admitted. The
/// literal reading, which counts every row ever written, would refuse this and
/// every later version of the concept.
#[tokio::test]
async fn a_closed_start_stops_blocking_a_later_open_one() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    upsert(&db, concept("a", JUN, None))
        .await
        .expect("the Jan start is closed, so Jun is the only open interval now");

    db.close().await.unwrap();
}

/// A batch that supersedes the ledger's open start and opens a new one is
/// judged by the state it leaves, not by the rows it had to pass through.
/// The batch path checks against the committed ledger the same way the single
/// path does. A batch opening a second open start is the single-open violation
/// while the first is open, and is admitted once the first has been closed.
///
/// Two versions of one concept cannot share a batch at all: every row in a call
/// carries one stamp, and the monotonic `recorded_at` trigger refuses an equal
/// stamp on the same concept. That is pre-existing and is not what this test
/// measures, so each version gets its own call.
#[tokio::test]
async fn a_batch_opening_a_second_open_start_is_judged_against_the_ledger() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    let err = write(&db, vec![concept("a", JUN, None)])
        .await
        .expect_err("the Jan start is still open");
    assert_eq!(err.written, 0);
    assert!(
        matches!(err.cause, DbError::ConceptSingleOpen { .. }),
        "got {:?}",
        err.cause
    );

    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    let written = write(&db, vec![concept("a", JUN, None)])
        .await
        .expect("the Jan start is closed, so Jun is the one open interval");
    assert_eq!(written, 1);

    db.close().await.unwrap();
}

/// A batch clashing with what the ledger already believes is refused whole.
#[tokio::test]
async fn a_batch_clashing_with_the_ledger_is_refused_with_nothing_written() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    let before = count(&db, "SELECT COUNT(*) FROM transaction_log").await;

    let err = write(&db, vec![concept("a", MAR, Some(SEP))])
        .await
        .expect_err("[Mar, Sep) lies inside the open [Jan, inf)");

    assert_eq!(err.written, 0);
    let DbError::OverlappingConcept { ref overlap } = err.cause else {
        panic!("expected OverlappingConcept, got {:?}", err.cause);
    };
    assert_eq!(
        overlap.provenance,
        OverlapProvenance::AlreadyRecorded,
        "the collision is with a recorded row"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM transaction_log").await,
        before,
        "the refused batch left the log as it was"
    );

    db.close().await.unwrap();
}

/// A start that lies entirely before the ledger's open start is not an overlap,
/// so a backfilled earlier interval is admitted. P1 does not claim more than
/// that: which row `reconstruct(now)` returns for such a ledger is P2's
/// question, and this test only pins what the write side admits.
#[tokio::test]
async fn a_non_overlapping_backfill_is_admitted() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JUN, None)).await.unwrap();
    write(&db, vec![concept("a", JAN, Some(JUN))])
        .await
        .expect("[Jan, Jun) abuts [Jun, inf) and is admitted");

    db.close().await.unwrap();
}

/// A concept minted on a branch belongs to that branch, and the trunk cannot
/// restate it. The refusal is the trigger's (`CrossLineage`), and it has to stay
/// that one: the guard reads only the writing lineage's rows, so the branch's
/// belief is not charged to the trunk as an overlap (D-291).
#[tokio::test]
async fn a_trunk_restatement_of_a_branch_concept_is_cross_lineage_not_an_overlap() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();

    upsert(&db, concept("mine", JAN, Some(JUN)).on_branch(alt))
        .await
        .unwrap();
    let err = upsert(&db, concept("mine", MAR, Some(SEP)))
        .await
        .expect_err("the trunk may not restate a concept the branch minted");
    assert!(matches!(err, DbError::CrossLineage { .. }), "got {err:?}");

    db.close().await.unwrap();
}

/// Archiving keeps each start's current belief hot (0.19.5, D-298). Until then
/// `LOG_ARCHIVABLE` keyed on the entity, so a later start sent `[Jan, Jun)` cold,
/// the guard stopped seeing it, and a write inside it was admitted.
#[tokio::test]
async fn a_start_a_later_start_superseded_still_blocks_after_archiving() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    upsert(&db, concept("a", JUN, None)).await.unwrap();
    db.archive(FAR).await.unwrap();

    let err = upsert(&db, concept("a", MAR, Some(JUN)))
        .await
        .expect_err("[Mar, Jun) overlaps the archived ledger's [Jan, Jun)");
    let DbError::OverlappingConcept { ref overlap } = err else {
        panic!("expected OverlappingConcept, got {err:?}");
    };
    assert_eq!(
        (overlap.existing_from.as_str(), overlap.existing_to.as_str()),
        (JAN, JUN)
    );
    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}

/// One concept's rows travel in one chunk, so where they sit in the batch does not
/// change the verdict (0.19.5, D-298). The batch opens a new start and closes the
/// old one. Until 0.19.5 that depended on the adaptive chunker: with the two rows
/// in different chunks the first was refused as `ConceptSingleOpen` after earlier
/// chunks had committed, and in one chunk the shared stamp was refused by the
/// monotonic `recorded_at` trigger. Now both placements meet the same refusal, and
/// the concept's ledger is untouched by either.
#[tokio::test]
async fn a_concepts_rows_meet_the_same_verdict_wherever_the_chunker_would_split() {
    for gap in [0usize, 100] {
        let harness = TestHarness::new();
        let db = open_db(&harness).await;

        upsert(&db, concept("a", JAN, None)).await.unwrap();
        let mut batch = vec![concept("a", JUN, None)];
        batch.extend((0..gap).map(|i| concept(&format!("b{i}"), JAN, None)));
        batch.push(concept("a", JAN, Some(JUN)));

        let err = write(&db, batch)
            .await
            .expect_err("one concept twice in one chunk shares a stamp");
        assert!(
            matches!(err.cause, DbError::RecordedAtRegression { .. }),
            "gap {gap}: {:?}",
            err.cause
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'a'"
            )
            .await,
            1,
            "gap {gap}: the concept's ledger is untouched"
        );

        db.close().await.unwrap();
    }
}
