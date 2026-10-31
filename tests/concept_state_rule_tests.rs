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

// ---------------------------------------------------------------------------
// The read side (D-303, §2.2 and §3.6; gates 8 and 11 named by its mutations)
// ---------------------------------------------------------------------------
//
// The rule is one rule for every recorded read, which is what makes it
// D-303 instead of a projection fix. The folds run unchanged and then merge
// the rule rows the marks table names — the shape-5 pair. Each test names
// the mutation it dies under: a mark left behind (the fold keeps a seq
// winner the rule window replaces), a correction run for an absent pair
// (fold equals seq fold where nothing is marked), a heal that must not
// clear the mark, and gate 11: past reads never change.

use harness::marks as mark_list;
use harness::{drop_marks_table, ensure_marks_table, mark_disordered};
use macrame::graph::{AttributeMode, TraversalBuilder};
use macrame::temporal::{hydrate_attributes, reconstruct, AsOf};
use macrame::BranchId;

/// A "now" beyond every stamp the fixtures can write: the clock reads a real
/// date, so a recorded bound that far out keeps everything.
const FAR: &str = "2030-01-01T00:00:00.000000Z";

/// The rule row is the greatest `(valid_from, seq_id)` of the pair: on a
/// marked backfill, the fold's own winner (the row written last, the
/// backfill) is replaced by the open belief — the row the projection already
/// holds at write time. The unmarked neighbor must be untouched, and a mark
/// naming a pair with no log rows must be harmless: that is gate 8's
/// *correction run for an absent pair*.
#[tokio::test]
async fn the_hot_fold_answers_the_rule_for_its_marked_pairs() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    upsert(&db, concept("src/neighbor.rs", "N", JAN, None)).await;

    mark_disordered(&db, "src/a.rs", "main").await;
    // A mark with no log rows: the correction must answer nothing for it.
    mark_disordered(&db, "src/ghost.rs", "main").await;

    let state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(
        state.concepts["src/a.rs"].title, "C2",
        "the marked pair answers with the rule row, not the seq winner"
    );
    assert_eq!(
        state.concepts["src/neighbor.rs"].title, "N",
        "the unmarked neighbor reads the way it always did"
    );
    assert!(
        !state.concepts.contains_key("src/ghost.rs"),
        "a mark with no log rows folds to nothing, like its ledger"
    );

    db.close().await.unwrap();
}

/// A ledger without the marks table folds plainly: the pre-v24 file every
/// current ledger is, and the state the fallback exists for. The probe reads
/// the missing-table error and answers "nothing is owed", so the read is
/// byte-for-byte the 0.20.0 behaviour rather than an error.
#[tokio::test]
async fn a_ledger_without_the_marks_table_folds_unchanged() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    // Provisioned then dropped: the test proves the *dropped* state, notably
    // not a ledger that never created one.
    drop_marks_table(&db).await;

    let state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(
        state.concepts["src/a.rs"].title, "C1",
        "the plain fold is the last writer, which is what this ledger is"
    );

    db.close().await.unwrap();
}

/// An empty marks table changes nothing either way — gate 8's healthy-ledger
/// equality: with nothing marked, the fold and the rule window agree,
/// because a never-marked concept satisfies the rule without correction
/// (§3.6's induction).
#[tokio::test]
async fn an_empty_marks_table_folds_without_the_correction() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "first", JAN, Some(JUN))).await;
    upsert(&db, concept("src/a.rs", "second", JUN, None)).await;
    ensure_marks_table(&db).await;

    let state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(
        state.concepts["src/a.rs"].title, "second",
        "the greatest start is also the last write here, so the rules agree"
    );

    db.close().await.unwrap();
}

/// The same rule the recorded fold answers is what hydration answers, in one
/// call: `AttributeMode::AtTime` folds the same log per id, so a marked
/// backfill must not put the backfill's title on the node at a recorded
/// instant.
#[tokio::test]
async fn at_time_hydration_answers_the_rule_for_marked_pairs() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    let attrs = hydrate_attributes(
        &db.read_conn(),
        &["src/a.rs".to_string()],
        &AsOf::recorded_at(FAR),
        AttributeMode::AtTime,
    )
    .await
    .unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].title, "C2", "the rule row, not the backfill");

    db.close().await.unwrap();
}

/// The recorded walk's window reads the same rule: the SQL folded in
/// `concepts_at_instant_sql` decides which reached concepts stay live, so a
/// retired backfill (retired = 1, last written) must not drop the concept the
/// rule keeps live. This is the case the plain window cannot answer and the
/// marked window exists for.
#[tokio::test]
async fn a_retired_backfill_does_not_drop_the_concept_from_a_recorded_walk() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    // The backfill carries its own retired flag: a historical interval that
    // was closed. The projection (already the rule) keeps the concept live;
    // the plain fold's window, folding the last written row, would drop it.
    let backfill = concept("src/a.rs", "C1", JAN, Some(JUN)).retired(true);
    upsert(&db, backfill).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    let mut walk = TraversalBuilder::new("src/a.rs")
        .as_of_recorded(FAR)
        .execute_ids(db.read_conn(), "2030-01-02T00:00:00.000000Z")
        .await
        .unwrap();
    walk.sort();
    assert_eq!(walk, vec!["src/a.rs".to_string()]);

    db.close().await.unwrap();
}

/// The jump the rung has to make is shown in one test rather than its
/// results in three: unmarked history folds last-writer-wins (the §1
/// defect, `bounded now to the two rows the fixture wrote`), the projection
/// answers the rule, and the mark is what makes the two agree. When the rung
/// lands and fills marks from history, the first assertion inverts on its
/// own fixture (marked) and the middle one keeps standing.
#[tokio::test]
async fn the_projection_and_the_fold_agree_only_when_the_mark_holds() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;

    // No mark: the divergence, named on purpose.
    let state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(state.concepts["src/a.rs"].title, "C1");
    let (title, from, to, fts_title) = projection(&db, "src/a.rs").await;
    assert_eq!(title, "C2");
    assert_eq!(from, JUN);
    assert_eq!(to, OPEN);
    assert_eq!(fts_title, "C2");

    // The mark lands and the fold agrees with the projection.
    mark_disordered(&db, "src/a.rs", "main").await;
    let marked = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(marked.concepts["src/a.rs"].title, "C2");
    let (title2, from2, to2, fts2) = projection(&db, "src/a.rs").await;
    assert_eq!(
        (title2, from2, to2, fts2),
        (title, from, to, fts_title),
        "the mark is what makes the fold equal the projection"
    );

    db.close().await.unwrap();
}

/// The lineage fold's correction stays inside its reader's ancestry: the
/// correction joins `lineage` the way the fold itself does, so a pair marked
/// on a branch this reader cannot see leaks nothing, and a pair marked on an
/// inherited branch is corrected within the cutoffs the fork point set.
#[tokio::test]
async fn the_lineage_fold_corrects_within_the_branches_its_reader_sees() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    let alt = BranchId::new("alt").unwrap();
    db.fork(alt, BranchId::main()).await.unwrap();

    // A mark whose pair's rows are not in this reader's ancestry must drop
    // rather than surface: the mark alone cannot conjure a row the fold's
    // own window did not carry.
    mark_disordered(&db, "src/other.rs", "alt").await;

    let state = db.reconstruct_on(FAR, "alt").await.unwrap();
    assert_eq!(
        state.concepts["src/a.rs"].title, "C2",
        "the inherited pair is corrected inside the reader's lineages"
    );
    assert!(
        !state.concepts.contains_key("src/other.rs"),
        "a mark beyond the reader's ancestry folds to nothing"
    );

    db.close().await.unwrap();
}

/// Gate 11: past reads never change. The read at the open row's stamp fixes
/// the winners; the backfill is recorded *after* that stamp, so the read must
/// not change; and a healing write (recorded later still) must not change it
/// either. If the heal cleared the mark, the third read would fall to the seq
/// winner — exactly the mutation gate 11 exists to catch — so the mark's
/// persistence is asserted beside the read's stability.
#[tokio::test]
async fn past_reads_never_change_around_a_backfill_and_its_heal() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, None)).await;
    let open_stamp: String = db
        .read_conn()
        .query(
            "SELECT recorded_at FROM transaction_log \
             WHERE entity_id = 'src/a.rs' AND json_extract(payload, '$.title') = 'C2'",
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

    let title_of = |state: &macrame::temporal::MaterializedState| -> Option<String> {
        state.concepts.get("src/a.rs").map(|a| a.title.clone())
    };

    // The backfill is recorded strictly later (the monotonic trigger), so it
    // sits beyond this bound.
    upsert(&db, concept("src/a.rs", "C1", JAN, Some(JUN))).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    // Read at the stamped instant: only pre-backfill rows can answer.
    let before = reconstruct(&db.read_conn(), &open_stamp, None, None)
        .await
        .unwrap();

    // The healing write: a correction at Jun (same start), recorded after
    // the bound. Marks never clear; the rule row within this bound stays the
    // open row rather than the heal.
    upsert(&db, concept("src/a.rs", "C4-heals", JUN, None)).await;

    let after_heal = reconstruct(&db.read_conn(), &open_stamp, None, None)
        .await
        .unwrap();
    assert_eq!(
        title_of(&before),
        title_of(&after_heal),
        "a healing write does not change a past read bound before it"
    );
    assert_eq!(
        mark_list(&db).await,
        vec![("src/a.rs".to_string(), "main".to_string())],
        "the mark survives the heal: marks never clear (§3.6)"
    );

    // At the present the rule answers the healing write: same start, so it
    // is the correction and it is the state.
    let now_state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert_eq!(now_state.concepts["src/a.rs"].title, "C4-heals");

    db.close().await.unwrap();
}

/// A retired rule row is the state: the fold removes the concept rather than
/// resists. Rows from the fold behave identically to fold rows, which is
/// what makes the merge a merge and not a special case.
#[tokio::test]
async fn the_rule_row_retires_the_concept_from_the_fold() {
    let harness = TestHarness::new();
    let db = open_db(&harness).await;

    upsert(&db, concept("src/a.rs", "C2", JUN, Some(DEC))).await;
    upsert(&db, concept("src/a.rs", "C3-dec", DEC, None)).await;
    let retire = concept("src/a.rs", "C3-dec", DEC, None).retired(true);
    upsert(&db, retire).await;
    mark_disordered(&db, "src/a.rs", "main").await;

    let state = reconstruct(&db.read_conn(), FAR, None, None).await.unwrap();
    assert!(
        !state.concepts.contains_key("src/a.rs"),
        "a retired row from the correction removes the concept like a fold row would"
    );

    db.close().await.unwrap();
}
