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
use macrame::{BranchId, ConceptUpsert, Database, DbError, OverlapProvenance, WriteBatch};

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
    assert_eq!(
        (overlap.valid_from.as_str(), overlap.valid_to.as_str()),
        (MAR, SEP)
    );
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

    let err = write(
        &db,
        vec![concept("c", JAN, Some(JUN)), concept("c", MAR, Some(SEP))],
    )
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
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'c'"
        )
        .await,
        0,
        "no log row was written"
    );

    db.close().await.unwrap();
}

/// The batch's concept phase stands on the ledger the single writes stand on
/// (0.22.0, M2, [D-308]): a batch that would open a second interval for a
/// concept is refused by the batch-vs-ledger guard ([D-291], the chunk path's
/// `check_batch_against_ledger`), and the refusal costs the whole batch — one
/// act, all or nothing.
///
/// The draft's `write_batch` ran neither guard, and this committed a second
/// open belief the single path refuses; this test is the plan §3.1 red test
/// for the batch-vs-ledger half, and it fails until the concept phase runs the
/// same pair of guards `write_concepts` runs.
///
/// [D-291]: ../../docs/architecture/s13-decision-register.md#d-291
/// [D-308]: ../../docs/architecture/s13-decision-register.md#d-308
#[tokio::test]
async fn a_batch_that_opens_a_second_interval_is_refused_with_nothing_written() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("c", JUN, None)).await.unwrap();

    let err = db
        .write_batch(WriteBatch::new().concept(concept("c", SEP, None)))
        .await
        .expect_err("a batch cannot open a second interval");
    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "two open intervals are the single-open case: {err}"
    );

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'c'").await,
        1,
        "the batch's refusal left the concept row the single write wrote"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'c'"
        )
        .await,
        1,
        "the refusal wrote no log row of its own"
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

/// No two rows of one concept share a chunk, so where they sit in the batch does
/// not change the verdict (0.19.6, D-299). The batch opens a new start and closes
/// the old one. Until 0.19.5 that depended on the adaptive chunker: in different
/// chunks the first row was refused as `ConceptSingleOpen`, and in one chunk the
/// shared stamp was refused by the monotonic `recorded_at` trigger. Now a repeat
/// always starts a chunk, so the rows apply in order and both placements meet the
/// `ConceptSingleOpen` the first row earns, leaving the concept's ledger untouched.
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
            .expect_err("[Jun, inf) opens while [Jan, inf) is still open");
        assert!(
            matches!(err.cause, DbError::ConceptSingleOpen { .. }),
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

/// A batch that names a concept twice commits, last row winning, wherever the rows
/// sit (0.19.6, D-299). In 0.19.5 they were held in one chunk, where the shared
/// stamp made the second a `RecordedAtRegression`.
#[tokio::test]
async fn a_concept_repeated_in_a_batch_commits_with_its_later_row() {
    for gap in [0usize, 100] {
        let harness = TestHarness::new();
        let db = open_db(&harness).await;

        let mut batch = vec![ConceptUpsert::new("x", "first").valid_from(JAN)];
        batch.extend((0..gap).map(|i| concept(&format!("b{i}"), JAN, None)));
        batch.push(ConceptUpsert::new("x", "corrected").valid_from(JAN));

        let written = write(&db, batch).await.expect("a repeat is a later write");
        assert_eq!(written, gap + 2, "gap {gap}");
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM concepts WHERE id = 'x' AND title = 'corrected'"
            )
            .await,
            1,
            "gap {gap}: the later row is the projection"
        );

        db.close().await.unwrap();
    }
}

/// A concept row with no `valid_from` in its payload is archived once a newer one
/// without one exists: the starts compare with `IS`, as the fold groups them
/// (0.19.6, D-299). With `=` such rows stayed hot forever.
#[tokio::test]
async fn concept_rows_without_a_start_still_archive() {
    let harness = TestHarness::new();
    let raw = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    macrame::schema::run_migrations(&raw).await.unwrap();
    for (title, at) in [("old", JAN), ("new", MAR)] {
        raw.execute(
            "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
             VALUES ('concepts', 'legacy', 'U', json_object('v', 1, 'title', ?1), ?2)",
            libsql::params![title, at],
        )
        .await
        .unwrap();
    }
    drop(raw);

    let db = open_db(&harness).await;
    db.archive(FAR).await.unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE entity_id = 'legacy'"
        )
        .await,
        1,
        "the superseded start-less row has left the hot log"
    );
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Supersede in one write (0.22.0, plan §4.1, M1b, [D-312])
// ---------------------------------------------------------------------------
//
// Since D-291 a later start over an open interval is refused, so the half of a
// conversation turn that moves a task ("the task, then the tasks it implies")
// was two calls, and a crash between them left the task with no open belief.
// `supersede` says *I am composing exactly this one open belief away*: the open
// interval starting before the new row's start is closed at that instant, under
// the same stamp, and the new belief opens — one call, one transaction.
//
// The close is a log row, not a projection update: two projection updates under
// one stamp would trip `trg_concepts_monotonic_ra`. Under D-303's fold the
// close wins its start (within one start, the row written last), so the
// projection — one upsert for the new belief — and the fold agree, which is
// what D-023 requires.
//
// [D-312]: ../../docs/architecture/s13-decision-register.md#d-312

const AUG: &str = "2026-08-01T00:00:00.000000Z";
const NOV: &str = "2026-11-01T00:00:00.000000Z";
const PRE: &str = "2025-06-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// `(recorded_at, title, valid_from, valid_to, branch_id)` per log row of one
/// concept, in sequence order — the shape every supersede assertion reads.
async fn log_rows(db: &Database, id: &str) -> Vec<(String, String, String, String, String)> {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT recorded_at, \
                    json_extract(payload, '$.title'), \
                    json_extract(payload, '$.valid_from'), \
                    json_extract(payload, '$.valid_to'), \
                    branch_id \
             FROM transaction_log \
             WHERE table_name = 'concepts' AND entity_id = ?1 \
             ORDER BY seq_id",
            libsql::params![id],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
            row.get::<String>(3).unwrap(),
            row.get::<String>(4).unwrap(),
        ));
    }
    out
}

/// The case: one write closes the open interval at the new start and opens the
/// new belief, under one stamp. The closed belief keeps its text; the
/// projection and the log agree; every as-of read before the cut returns the
/// old title.
#[tokio::test]
async fn a_supersede_cuts_the_open_interval_at_the_new_start_under_one_stamp() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    tick().await;
    db.upsert_concept_superseding(ConceptUpsert::new("a", "a2").content("c2").valid_from(SEP))
        .await
        .unwrap();

    let rows = log_rows(&db, "a").await;
    assert_eq!(rows.len(), 3, "one original, one close, one open: {rows:?}");
    let [orig, close, open] = rows.as_slice() else {
        panic!("expected three rows: {rows:?}");
    };
    assert_eq!(orig.1, "a", "the original Belief");
    assert_eq!(
        (orig.2.as_str(), orig.3.as_str()),
        (JAN, OPEN),
        "the original was open"
    );
    assert!(
        close.0 > orig.0,
        "the close stamps after the original: {close:?}"
    );
    assert_eq!(
        (close.1.as_str(), close.2.as_str(), close.3.as_str()),
        ("a", JAN, SEP),
        "the close restates the old belief, ended at the new start"
    );
    assert_eq!(
        close.0, open.0,
        "the close and the open carry the same stamp"
    );
    assert_eq!(
        (open.1.as_str(), open.2.as_str(), open.3.as_str()),
        ("a2", SEP, OPEN),
        "the open is the new belief"
    );

    let now = db
        .concepts_by_id(&["a".to_owned()], macrame::ReadPlan::new())
        .await
        .unwrap();
    assert_eq!(now.len(), 1);
    let v = &now[0];
    assert_eq!((v.title.as_str(), v.content.as_str()), ("a2", "c2"));
    assert_eq!(
        (v.valid_from.as_deref(), v.valid_to.as_deref()),
        (Some(SEP), Some(OPEN))
    );

    // As-of reads before the cut are the valid axis (D-292): the belief
    // whose interval contains the instant — the closed belief after the
    // close, which is what the plan means by "the old text".
    let before = db
        .reconstruct_as_of(macrame::temporal::AsOf::valid_at(AUG))
        .await
        .unwrap();
    assert_eq!(
        before.concepts["a"].title, "a",
        "as-of reads before the cut return the old title"
    );
    let at_cut = db
        .reconstruct_as_of(macrame::temporal::AsOf::valid_at(SEP))
        .await
        .unwrap();
    assert_eq!(
        at_cut.concepts.get("a").map(|c| c.title.as_str()),
        Some("a2"),
        "at the cut the new belief is live"
    );
    let at_open = db
        .reconstruct_as_of(macrame::temporal::AsOf::valid_at(JAN))
        .await
        .unwrap();
    assert_eq!(
        at_open.concepts["a"].title, "a",
        "the close keeps the old title for its own interval"
    );

    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}

/// Supersede only moves forward in valid time. An open belief starting at the
/// superseding row's start — or after it — is still the single-open case, and
/// the refusal writes nothing.
#[tokio::test]
async fn a_supersede_that_does_not_move_forward_in_valid_time_is_refused() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();

    // At the same start: the start cannot be cut at itself, and D-291's
    // ordinary re-assert already covers what a same-start rewrite wants.
    let err = db
        .upsert_concept_superseding(ConceptUpsert::new("a", "a2").valid_from(JAN))
        .await
        .expect_err("same start is not a forward move");
    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "got {err:?}"
    );

    // Before it: backwards in valid time.
    let err = db
        .upsert_concept_superseding(ConceptUpsert::new("a", "a2").valid_from(PRE))
        .await
        .expect_err("a backwards supersede");
    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "got {err:?}"
    );

    assert_eq!(log_rows(&db, "a").await.len(), 1, "refusals write nothing");

    db.close().await.unwrap();
}

/// Nothing open: the supersede is an ordinary upsert and no close row exists.
#[tokio::test]
async fn a_supersede_with_nothing_open_is_an_ordinary_upsert() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, Some(JUN))).await.unwrap();
    tick().await;
    db.upsert_concept_superseding(ConceptUpsert::new("a", "a2").valid_from(SEP).valid_to(FAR))
        .await
        .unwrap();

    let rows = log_rows(&db, "a").await;
    assert_eq!(
        rows.len(),
        2,
        "no close row without an open belief: {rows:?}"
    );
    assert_eq!(
        (rows[0].1.as_str(), rows[0].3.as_str()),
        ("a", JUN),
        "the first interval keeps its own end"
    );
    assert_eq!(
        (rows[1].2.as_str(), rows[1].3.as_str()),
        (SEP, FAR),
        "the new interval is the old one's continuation"
    );

    db.close().await.unwrap();
}

/// A concept a branch minted is superseded by that branch alone: the close and
/// the open carry the branch's lineage, and the trunk's view is untouched.
#[tokio::test]
async fn a_branch_supersede_of_its_own_concept_leaves_the_trunk_untouched() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();

    upsert(&db, concept("b", JAN, None)).await.unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("c", "c")
            .valid_from(JUN)
            .on_branch(alt.clone()),
    )
    .await
    .unwrap();

    tick().await;
    db.upsert_concept_superseding(
        ConceptUpsert::new("c", "c2")
            .valid_from(SEP)
            .on_branch(alt.clone()),
    )
    .await
    .unwrap();

    // The rows carry the branch.
    for (_, _, _, _, branch) in log_rows(&db, "c").await {
        assert_eq!(branch, "alt");
    }

    // The branch reads its own cut end: the state-rule fold resolves to the
    // branch's open belief.
    let alt_now = db.reconstruct_on(FAR, "alt").await.unwrap();
    assert_eq!(alt_now.concepts.get("c").unwrap().title, "c2");
    let view = db
        .concepts_by_id(&["c".to_owned()], macrame::ReadPlan::new().on(alt.clone()))
        .await
        .unwrap();
    assert_eq!(view[0].title, "c2");
    assert_eq!(view[0].branch_id, "alt");
    assert_eq!(
        (view[0].valid_from.as_deref(), view[0].valid_to.as_deref()),
        (Some(SEP), Some(OPEN))
    );

    // The trunk never held `c` and still sees only itself.
    let trunk = db.reconstruct_on(FAR, "main").await.unwrap();
    assert!(!trunk.concepts.contains_key("c"), "the trunk has no c");
    assert_eq!(trunk.concepts.get("b").unwrap().title, "b");
    assert_eq!(
        log_rows(&db, "b").await.len(),
        1,
        "the trunk's own concept is untouched"
    );

    db.close().await.unwrap();
}

/// An inherited concept belongs to the parent: superseding it on the branch
/// stays the D-214 refusal, and M6's shadow rows are deliberately not built.
#[tokio::test]
async fn a_supersede_of_an_inherited_concept_is_cross_lineage() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;
    let alt = BranchId::new("alt").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();

    upsert(&db, concept("a", JAN, None)).await.unwrap();

    let err = db
        .upsert_concept_superseding(ConceptUpsert::new("a", "a2").valid_from(SEP).on_branch(alt))
        .await
        .expect_err("the branch may not supersede what it inherited");
    assert!(matches!(err, DbError::CrossLineage { .. }), "got {err:?}");

    let rows = log_rows(&db, "a").await;
    assert_eq!(rows.len(), 1, "the refusal writes nothing");
    assert_eq!(rows[0].4, "main", "and nothing landed on the branch");

    db.close().await.unwrap();
}

/// A run of supersedes: each cuts the belief the act before it opened, the
/// stamps pair two-by-two, and the interval ledger verifies clean.
#[tokio::test]
async fn a_run_of_supersedes_keeps_the_interval_ledger_clean() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    tick().await;
    db.upsert_concept_superseding(ConceptUpsert::new("a", "v2").valid_from(SEP))
        .await
        .unwrap();
    tick().await;
    db.upsert_concept_superseding(ConceptUpsert::new("a", "v3").valid_from(FAR))
        .await
        .unwrap();

    let rows = log_rows(&db, "a").await;
    assert_eq!(
        rows.len(),
        5,
        "two closes, two opens, one original: {rows:?}"
    );
    assert_eq!(rows[1].0, rows[2].0, "act one's pair");
    assert_eq!(rows[3].0, rows[4].0, "act two's pair");
    assert_eq!(
        (rows[1].2.as_str(), rows[1].3.as_str()),
        (JAN, SEP),
        "the first close ended the JAN belief at SEP"
    );
    assert_eq!(
        (rows[3].2.as_str(), rows[3].3.as_str()),
        (SEP, FAR),
        "the second close ended the SEP belief at FAR"
    );

    for (at, want) in [
        (JAN, "a"),
        (AUG, "a"),
        (NOV, "v2"),
        (DEC, "v2"),
        (FAR, "v3"),
    ] {
        let state = db
            .reconstruct_as_of(macrame::temporal::AsOf::valid_at(at))
            .await
            .unwrap();
        assert_eq!(
            state.concepts.get("a").map(|c| c.title.as_str()),
            Some(want),
            "as-of-valid at {at}"
        );
    }

    assert!(db.verify_concept_intervals().await.unwrap().is_empty());

    db.close().await.unwrap();
}

/// In the batch the supersede's close is inside the batch's transaction: the
/// close and the open — and the batch's other rows — all carry the batch stamp,
/// and a batch refused for an unrelated row leaves the close unwritten too.
#[tokio::test]
async fn a_batch_supersede_cuts_at_the_batch_stamp() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    upsert(&db, concept("x", JAN, None)).await.unwrap();

    let batch = WriteBatch::new()
        .concept(
            ConceptUpsert::new("a", "a2")
                .valid_from(SEP)
                .supersede(true),
        )
        .edge(macrame::graph::EdgeAssertion::new("a", "x", "LINKS").valid_from(SEP));
    db.write_batch(batch).await.unwrap();

    let rows = log_rows(&db, "a").await;
    assert_eq!(rows.len(), 3, "original, close, open: {rows:?}");
    assert_eq!(rows[1].0, rows[2].0, "the close and the open pair");
    assert_eq!(
        (rows[1].2.as_str(), rows[1].3.as_str()),
        (JAN, SEP),
        "closed at the batch's own start"
    );

    // The batch stamp belongs to the whole act: the edge row the batch also
    // wrote carries it, so the turn's concept, its supersede and its edge are
    // one stamp despite landing in three log contexts.
    let mut edge_rows = db
        .read_conn()
        .query(
            "SELECT MAX(recorded_at) FROM transaction_log WHERE table_name = 'links'",
            (),
        )
        .await
        .unwrap();
    let edge_stamp: Option<String> = edge_rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Option<String>>(0)
        .unwrap_or(None);
    assert_eq!(
        edge_stamp.as_deref(),
        Some(rows[1].0.as_str()),
        "the batch stamp closes and asserts together"
    );

    db.close().await.unwrap();
}

/// A batch refused for an unrelated row must leave the supersede's close
/// unwritten as well — the close lives inside the batch's transaction, not in
/// a side commit of its own.
#[tokio::test]
async fn a_batch_refused_for_an_unrelated_row_leaves_the_close_unwritten() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("a", JAN, None)).await.unwrap();
    upsert(&db, concept("z", JAN, None)).await.unwrap();

    let err = db
        .write_batch(
            WriteBatch::new()
                .concept(
                    ConceptUpsert::new("a", "a2")
                        .valid_from(SEP)
                        .supersede(true),
                )
                .concept(ConceptUpsert::new("z", "z2").valid_from(SEP)),
        )
        .await
        .expect_err("z would open a second interval");
    assert!(
        matches!(err, DbError::ConceptSingleOpen { .. }),
        "got {err:?}"
    );

    assert_eq!(
        log_rows(&db, "a").await.len(),
        1,
        "the supersede's close never made it out of the transaction"
    );
    assert_eq!(log_rows(&db, "z").await.len(), 1, "z is untouched");

    db.close().await.unwrap();
}

/// The plan's probe (§4.1, M1b, [D-312]): one supersede call reads — through
/// every reader — the same as the two-call cut it replaces, minus the instant
/// with no open belief, which the two-call sequence's own recorded axis still
/// carries.
///
/// Fixture 1: the task's belief closed and opened in one write, one stamp.
/// Fixture 2: the same final intervals through two calls on two stamps — the
/// close (a re-assert of the open start with an end), then the open.
///
/// Equal across the fixtures: the valid axis (`reconstruct_as_of`) at four
/// instants, for both concepts; `concepts_by_id` at the present (title,
/// interval, content); its valid-window filter; the traversal current and at an
/// instant; the branch fold inherited at the recorded cut. Different, named:
/// fixture 2's recorded axis between its two stamps holds a closed belief and
/// nothing open — the crash window. Fixture 1 has no such instant.
#[tokio::test]
async fn a_supersede_reads_like_the_two_call_sequence_through_every_reader() {
    struct Fixture {
        db: Database,
    }

    async fn one_write(harness: &TestHarness) -> Fixture {
        let db = open_db(harness).await;
        upsert(&db, concept("a", JAN, None)).await.unwrap();
        upsert(&db, concept("b", JAN, None)).await.unwrap();
        upsert(&db, concept("x", JAN, None)).await.unwrap();
        tick().await;
        db.assert_edge(macrame::graph::EdgeAssertion::new("a", "x", "LINKS").valid_from(JAN))
            .await
            .unwrap();
        tick().await;
        db.upsert_concept_superseding(ConceptUpsert::new("a", "a2").content("c2").valid_from(SEP))
            .await
            .unwrap();
        Fixture { db }
    }

    async fn two_calls(harness: &TestHarness) -> Fixture {
        let db = open_db(harness).await;
        upsert(&db, concept("a", JAN, None)).await.unwrap();
        upsert(&db, concept("b", JAN, None)).await.unwrap();
        upsert(&db, concept("x", JAN, None)).await.unwrap();
        tick().await;
        db.assert_edge(macrame::graph::EdgeAssertion::new("a", "x", "LINKS").valid_from(JAN))
            .await
            .unwrap();
        // The close: re-assert the open start with the cut as its end (the
        // spelling D-291's era used, Doctrine III).
        upsert(&db, concept("a", JAN, Some(SEP))).await.unwrap();
        // The open.
        upsert(
            &db,
            ConceptUpsert::new("a", "a2").content("c2").valid_from(SEP),
        )
        .await
        .unwrap();
        Fixture { db }
    }

    let f1 = one_write(&TestHarness::new()).await;
    let f2 = two_calls(&TestHarness::new()).await;

    // (a) The valid axis, for both concepts.
    for (at, want_a) in [(JAN, "a"), (AUG, "a"), (NOV, "a2"), (DEC, "a2")] {
        let s1 = f1
            .db
            .reconstruct_as_of(macrame::temporal::AsOf::valid_at(at))
            .await
            .unwrap();
        let s2 = f2
            .db
            .reconstruct_as_of(macrame::temporal::AsOf::valid_at(at))
            .await
            .unwrap();
        assert_eq!(
            s1.concepts.get("a").map(|c| c.title.as_str()),
            Some(want_a),
            "valid {at}: fixture 1"
        );
        assert_eq!(
            s2.concepts.get("a").map(|c| c.title.as_str()),
            Some(want_a),
            "valid {at}: fixture 2"
        );
        assert_eq!(
            s1.concepts.get("b").map(|c| c.title.as_str()),
            s2.concepts.get("b").map(|c| c.title.as_str()),
            "valid {at}: the context concept agrees"
        );
    }

    // (b) `concepts_by_id` at the present: same title, same interval, same
    // content on both fixtures.
    let v1 = f1
        .db
        .concepts_by_id(&["a".to_owned()], macrame::ReadPlan::new())
        .await
        .unwrap();
    let v2 = f2
        .db
        .concepts_by_id(&["a".to_owned()], macrame::ReadPlan::new())
        .await
        .unwrap();
    assert_eq!(v1.len(), 1);
    assert_eq!(v2.len(), 1);
    assert_eq!(v1[0].title, v2[0].title);
    assert_eq!(
        (v1[0].valid_from.as_deref(), v1[0].valid_to.as_deref()),
        (v2[0].valid_from.as_deref(), v2[0].valid_to.as_deref())
    );
    assert_eq!(v1[0].valid_to.as_deref(), Some(OPEN));
    assert_eq!(v1[0].content, v2[0].content);

    // (c) The traversal — ids and content are equal current and at an instant
    // (infer/AT_TIME naming is the builder's `.as_of_valid`).
    use macrame::graph::TraversalBuilder;
    let t1 = TraversalBuilder::new("a")
        .execute_ids(f1.db.read_conn(), FAR)
        .await
        .unwrap();
    let t2 = TraversalBuilder::new("a")
        .execute_ids(f2.db.read_conn(), FAR)
        .await
        .unwrap();
    assert_eq!(t1, vec!["a", "x"], "a -> x, both ids");
    assert_eq!(t1, t2, "the traversal's topology agrees");
    let t1 = TraversalBuilder::new("a")
        .as_of_valid(AUG)
        .execute_ids(f1.db.read_conn(), FAR)
        .await
        .unwrap();
    let t2 = TraversalBuilder::new("a")
        .as_of_valid(AUG)
        .execute_ids(f2.db.read_conn(), FAR)
        .await
        .unwrap();
    assert_eq!(t1, t2, "the traversal at an instant agrees");

    // (d) The branched fold inherited at each fixture's own recorded cut
    // (§3.2's rider): each branch starts at its ledger's end and reads the
    // same final belief.
    for f in [&f1, &f2] {
        let end =
            f.db.read_conn()
                .query("SELECT MAX(recorded_at) FROM transaction_log", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<Option<String>>(0)
                .unwrap_or(None)
                .unwrap();
        let alt = BranchId::new("alt").unwrap();
        f.db.fork_at(alt, BranchId::main(), &end).await.unwrap();
        let state = f.db.reconstruct_on(FAR, "alt").await.unwrap();
        assert_eq!(state.concepts.get("a").unwrap().title, "a2");
        assert_eq!(state.concepts.get("b").unwrap().title, "b");
    }

    // (e) The difference, named: fixture 2's recorded axis between its two
    // stamps holds only the closed belief — the window. Fixture 1's log has no
    // instant between "nothing yet" and "the act".
    let stamps = log_rows(&f2.db, "a")
        .await
        .iter()
        .map(|r| r.0.clone())
        .collect::<Vec<_>>();
    assert_eq!(stamps.len(), 3, "{stamps:?}");
    assert!(
        stamps[0] < stamps[1],
        "the pre-act row precedes the close: {stamps:?}"
    );
    assert_eq!(
        stamps.len(),
        3,
        "three rows, three stamps: the act is not one"
    );
    assert_ne!(stamps[1], stamps[2], "the two calls carry two stamps");

    f1.db.close().await.unwrap();
    f2.db.close().await.unwrap();
}
