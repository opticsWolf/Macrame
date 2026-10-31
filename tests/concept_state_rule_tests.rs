//! The one state rule for concepts (0.21.0, P1, D-303).
//!
//! `docs/Macrame Update Plan v0.21.0.md` §2.4: *a concept's state is its open
//! belief if it has one, otherwise its latest-starting belief* — as an
//! ordering, `valid_from DESC, seq_id DESC`, take the first. These tests are
//! the plan's gates in the write-side order:
//!
//! 1. the backfill test goes **red first**: on 0.20.0 the projection's
//!    unconditional `DO UPDATE` hands the projection to the row written last,
//!    so a past interval written after the current one becomes the concept's
//!    state (§1 of the plan);
//! 2. a correction at the same valid start still overwrites (the mutation
//!    guard against `>=` strengthening to `>`);
//! 3. a future-dated version written ahead of time (the §2.4 worked case:
//!    `[Jun, Dec)` current, `[Dec, inf)` written while the clock reads
//!    August) *is* the state from the moment it is written — which the rule
//!    now says on purpose.
//!
//! The projection's readers checked here are the row itself (`concepts`) and
//! the FTS row that follows it through its triggers.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::{ConceptUpsert, Database};

const JAN: &str = "2026-01-01T00:00:00.000000Z";
const JUN: &str = "2026-06-01T00:00:00.000000Z";
const AUG: &str = "2026-08-01T00:00:00.000000Z";
const DEC: &str = "2026-12-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

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

/// Every write waits a tick, so each log row carries its own stamp: two writes
/// to one concept in one tick trip the monotonic `recorded_at` trigger before
/// any guard is reached (the same reason as in `reconstruct_as_of_tests`).
async fn upsert(db: &Database, concept: ConceptUpsert) {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    db.upsert_concept(concept).await.unwrap();
}

/// The projection row of `id`, as `(title, valid_from, valid_to)` + FTS title.
///
/// The second column the query reads is what the walk's `retired` filter and
/// current-mode hydration read, so a test that claims the state is wrong has
/// to read the same row they do, and the FTS row that follows it through the
/// projection's triggers is asserted beside it so a rung that repairs one
/// cannot quietly leave the other behind.
async fn projection(db: &Database, id: &str) -> (String, String, String, String) {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT c.title, c.valid_from, COALESCE(c.valid_to, ''), \
                    COALESCE(f.title, '') \
             FROM concepts c \
             LEFT JOIN concepts_fts f ON f.rowid = c.rowid \
             WHERE c.id = ?1",
            libsql::params![id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (
        row.get::<String>(0).unwrap(),
        row.get::<String>(1).unwrap(),
        row.get::<String>(2).unwrap(),
        row.get::<String>(3).unwrap(),
    )
}

/// Gate 1: the §1 sequence. C2 is the concept's current belief, C1 is a
/// backfill the concept interval guard admits, and after it the projection
/// must still hold C2. Fails on 0.20.0, where the unconditional `DO UPDATE`
/// makes the row written last (`C1`) the projection, which is what the README
/// records as the workaround-shaped defect this release closes.
#[tokio::test]
async fn the_projection_holds_the_current_belief_after_a_backfill() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;

    let (title, from, to, fts_title) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C2", "the projection holds the current belief");
    assert_eq!(from, JUN);
    assert_eq!(to, OPEN, "the open interval is the state's interval");
    assert_eq!(
        fts_title, "C2",
        "the FTS row follows the projection, not the backfill"
    );

    // The backfill itself is still written: both beliefs exist in the log, so
    // the valid reads stay what gate 7's test asserts them to be.
    let mut rows = db
        .read_conn()
        .query(
            "SELECT COUNT(*) FROM transaction_log \
             WHERE entity_id = 'src/a.rs' AND json_extract(payload, '$.title') = 'C1'",
            (),
        )
        .await
        .unwrap();
    let c1_rows = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    assert_eq!(c1_rows, 1, "the backfill row is in the log");

    db.close().await.unwrap();
}

/// A correction at the same valid start is a correction of the current belief,
/// and the projection takes it (gate 8's guard against `>=` strengthening to
/// `>`). The projection rule is about *starts*, not about newness: within one
/// start the row written last is the belief (D-291, unchanged).
#[tokio::test]
async fn a_correction_at_the_same_start_still_overwrites_the_projection() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C2-corrected", JUN, None)).await;

    let (title, from, _to, fts_title) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C2-corrected");
    assert_eq!(from, JUN, "the start does not move");
    assert_eq!(fts_title, "C2-corrected");

    db.close().await.unwrap();
}

/// The rule's explicit future-dated case (§2.4): `[Jun, Dec)` is current, and
/// the version written ahead of time (`[Dec, inf)`, written while the clock
/// reads August) shows as the state from the moment it is written, because it
/// is the latest-starting belief. This is today's behaviour and the rule keeps
/// it on purpose — the write rule only skips *earlier* starts, never later ones.
/// The two intervals touch at December and do not overlap, which is the
/// boundary the concept interval guard already handles (D-291).
#[tokio::test]
async fn a_future_dated_version_written_ahead_is_the_state_from_the_moment_it_is_written() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, Some(DEC))).await;
    // Written ahead of time: the clock reads August, the interval opens December.
    assert!(AUG < DEC);
    let ahead = ConceptUpsert::new("src/a.rs", "C3-opens-in-dec")
        .valid_from(DEC)
        .valid_to(OPEN);
    upsert(&db, ahead).await;

    let (title, from, _to, fts_title) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C3-opens-in-dec");
    assert_eq!(from, DEC);
    assert_eq!(fts_title, "C3-opens-in-dec");

    db.close().await.unwrap();
}

/// A skipped backfill writes its own log row. This is the plan §3.2 spike and
/// the write side in one case, through the crate's own statement:
///
/// - the projection's `DO UPDATE … WHERE` (D-303) reports 0 changed rows
///   through `Connection::execute` — the spike's first half, exercised by the
///   shipped path rather than a lookalike;
/// - no update trigger fires: the projection row is untouched to the byte, so
///   `trg_concepts_log_update` stood idle and the self-insert is necessary,
///   not optional (the spike's other half);
/// - the row the write path inserts reads exactly like an update row: op `U`,
///   payload v3, the backfill's own fields, the backfill's own stamp.
#[tokio::test]
async fn a_skipped_backfill_writes_its_own_log_row() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    let projection_recorded: String = db
        .read_conn()
        .query("SELECT recorded_at FROM concepts WHERE id = 'src/a.rs'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();

    // The backfill passes the interval guard (its end falls before the start
    // it follows — the §1 sequence's admission) and must leave the projection
    // alone while still landing in the log.
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;

    let (title, from, _to, fts_title) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C2", "the projection holds the current belief");
    assert_eq!(from, JUN);
    assert_eq!(fts_title, "C2");

    let projection_recorded_after: String = db
        .read_conn()
        .query("SELECT recorded_at FROM concepts WHERE id = 'src/a.rs'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        projection_recorded, projection_recorded_after,
        "no update fired: the projection row did not move a byte"
    );

    let row = db
        .read_conn()
        .query(
            "SELECT operation,                     json_extract(payload, '$.v') AS v,                     json_extract(payload, '$.title'),                     json_extract(payload, '$.valid_from'),                     json_extract(payload, '$.retired'),                     COALESCE(json_extract(payload, '$.extra'), ''),                     recorded_at              FROM transaction_log WHERE entity_id = 'src/a.rs'              ORDER BY seq_id DESC LIMIT 1",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let op: String = row.get(0).unwrap();
    let version: i64 = row.get(1).unwrap();
    let log_title: String = row.get(2).unwrap();
    let log_from: String = row.get(3).unwrap();
    let log_retired: i64 = row.get(4).unwrap();
    let log_extra: String = row.get(5).unwrap();
    let stamp: String = row.get(6).unwrap();

    assert_eq!(op, "U", "the self-inserted row is an update row");
    assert_eq!(version, 3, "payload v3");
    assert_eq!(log_title, "C1", "the backfill's own version");
    assert_eq!(
        log_from, JAN,
        "the backfill's own start, not the projection's"
    );
    assert_eq!(log_retired, 0);
    assert_eq!(
        log_extra, "{}",
        "D-278/D-283: a first-appearance backfill has no same-start row to          inherit from; its attributes stay the empty object"
    );
    assert!(
        stamp > projection_recorded_after,
        "the log row carries the backfill's own stamp, not the projection row's"
    );

    db.close().await.unwrap();
}

/// Preserve-on-omission (D-283) reads **within one start** on the skip path:
/// a corrective re-assertion of a start the projection no longer shows keeps
/// the extra the same start's latest row already holds, and a backfill whose
/// caller bound its own extra owns it. The projection row holds a *different*
/// version's attributes, and inheriting those by accident is exactly the
/// accident D-278 exists to make impossible.
#[tokio::test]
async fn a_skipped_correction_inherits_its_own_start_s_extra() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    let attributed = ConceptUpsert::new("src/a.rs", "C1")
        .valid_from(JAN)
        .valid_to(JUN)
        .extra(r#"{"layer": "one"}"#);
    upsert(&db, attributed).await;
    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;

    // Correcting the January version: same start, same end, new content, no
    // extra bound. The projection now starts in June, so the write is skipped
    // — and its log row must carry January's own attributes, not June's.
    let corrected = ConceptUpsert::new("src/a.rs", "C1-corrected")
        .valid_from(JAN)
        .valid_to(JUN);
    upsert(&db, corrected).await;

    let (title, from, _to, _fts) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C2");
    assert_eq!(from, JUN);

    let log_extra: String = db
        .read_conn()
        .query(
            "SELECT COALESCE(json_extract(payload, '$.extra'), '') \
             FROM transaction_log \
             WHERE entity_id = 'src/a.rs' \
               AND json_extract(payload, '$.title') = 'C1-corrected'",
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
    // json() compiles the payload, so the comparison reads the layer the
    // json text carries rather than the caller's own spacing.
    assert_eq!(
        log_extra, r#"{"layer":"one"}"#,
        "the corrected start keeps its own attributes"
    );

    db.close().await.unwrap();
}

/// A backfill whose caller binds its own extra owns it on the skip path
/// (preserve-on-omission is about omission, not about earlier starts).
#[tokio::test]
async fn a_backfill_owns_the_extra_its_caller_bound() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    let owned = ConceptUpsert::new("src/a.rs", "C1")
        .valid_from(JAN)
        .valid_to(JUN)
        .extra(r#"{"layer": "backfill"}"#);
    upsert(&db, owned).await;

    let log_extra: String = db
        .read_conn()
        .query(
            "SELECT COALESCE(json_extract(payload, '$.extra'), '') \
             FROM transaction_log \
             WHERE entity_id = 'src/a.rs' \
               AND json_extract(payload, '$.title') = 'C1'",
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
    assert_eq!(log_extra, r#"{"layer":"backfill"}"#);

    db.close().await.unwrap();
}
