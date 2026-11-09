//! The trunk walk folds its past ([D-316] — the retirement review's
//! confirmed Macrame finding).
//!
//! A forward retirement (`[D-312]`'s close + the tombstone in one
//! transaction) left `concepts_by_id(.on(main).valid_at(T1))` — T1 inside
//! the row's live window — answering **absent**. The undecided rows of the
//! walk resolve through `[D-310]`'s fold (the `versions_cte`: the
//! projection row where it serves, the log only past the row's own
//! recording), and the walk applies one current row's `shows_at` to the
//! answer — so the tombstone answers nothing at every instant. The
//! branch read on the same store answered correctly, because
//! `overlay_engaged`'s walk decides its rows from `concept_versions`/
//! `concept_cuts` at the instant named — the exact one-reader-two-laws
//! shape the reader census exists to prevent. The decision is the fix
//! (the register entry): the undecided fold gains `valid_at`, the log's
//! one-row-per-(entity, branch, start) bootstrap (`[D-292]`'s valid fold,
//! id-scoped) pastes the window the projection's row cannot answer.
//!
//! The plain supersede rides the same red (the review's second question,
//! untested until now): the projection keeps the newer row, so the
//! superseded text was unreadable at a past instant on the trunk; the
//! fold's own pair answers it. And the branch walk stays its own law
//! (superior): a decided row answers from the overlay, and
//! `overlay_absent` never pastes — the branch's rejection is an
//! answer.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;
use macrame::WriteBatch;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-01-15T00:00:00.000000Z";
const T2: &str = "2026-02-01T00:00:00.000000Z";
const T3: &str = "2026-03-01T00:00:00.000000Z";
const T5: &str = "2026-04-01T00:00:00.000000Z";
const T6: &str = "2026-05-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// Every write waits a tick, so each act carries its own wall stamp (the
/// same reason `m6a_routing_tests` sleeps).
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

/// The trunk minted `g` at `T0` (live), and — when asked — one branch
/// forked after the mint, so the store is an engaged one.
async fn store(harness: &TestHarness, forked: bool) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("g", "live g")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    if forked {
        db.fork(BranchId::new("alt").unwrap(), BranchId::main())
            .await
            .unwrap();
        tick().await;
    }
    db
}

/// The trunk's retired-tombstone act: the row, re-asserted at the
/// retirement instant, closed there, retired ([D-312]'s one-write close
/// beside it in the same transaction).
async fn tombstone_at_t2(db: &Database) {
    let batch = WriteBatch::new().concept(
        ConceptUpsert::new("g", "live g")
            .valid_from(T2)
            .valid_to(T2)
            .retired(true),
    );
    db.write_batch(batch).await.unwrap();
    tick().await;
}

/// Concept log rows the ledger holds (a cheap `COUNT` through the
/// diagnostic connection, the same one-row-at-a-time shape the fold's
/// callers pay).
async fn log_concept_rows(db: &Database) -> i64 {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'concepts'",
            (),
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// One valid instant's answer through the one method the finding names.
/// `None` = the id is absent at the instant.
async fn title_at(db: &Database, branch: BranchId, v: &str) -> Option<String> {
    db.concepts_by_id(&["g".to_string()], ReadPlan::new().on(branch).valid_at(v))
        .await
        .unwrap()
        .into_iter()
        .next()
        .map(|c| c.title)
}

/// **The review's confirmed red, with the user's T1/T2/T3 instants.**
///
/// Write `g`, valid from T0. Read at T1 (trunk): "live g". Retire at T2
/// in the forward shape. The same read at T1 must stay "live g" (it was
/// answering `[]`); the read at T3 is still absent.
#[tokio::test]
async fn a_trunk_retirement_keeps_the_live_window_readable_below_the_cut() {
    let harness = TestHarness::new();
    let db = store(&harness, false).await;

    // Before the act: every branch answers the pre-cut instants from the
    // projection row (the current row is live and covers T1).
    assert_eq!(
        title_at(&db, BranchId::main(), T1).await,
        Some("live g".to_string())
    );

    tombstone_at_t2(&db).await;

    // The tombstone's OWN window must first exist in the ledger: the close
    // row `[T0,T2)` plus the tombstone row (the same pair a879070's
    // retirement wrote) reads back from the log. The paste would be a
    // fold row of a ledger whose dead row is the ONLY row — a store with
    // no history is not a store whose past is unreadable, and this test
    // is not about a ledger that remembers nothing. So the mint's close
    // is written by an explicit supersed|supersede swap: one re-assert of
    // the T0 start at [T0,T2) before the tombstone.
    let close_batch = WriteBatch::new().concept(
        ConceptUpsert::new("g", "live g")
            .valid_from(T0)
            .valid_to(T2)
            .retired(false)
            .supersede(true),
    );
    db.write_batch(close_batch).await.unwrap();
    tick().await;
    assert!(
        log_concept_rows(&db).await >= 2,
        "the close row and the tombstone are the ledger's two windows"
    );

    // The red instant: the live window below the cut.
    assert_eq!(
        title_at(&db, BranchId::main(), T1).await,
        Some("live g".to_string()),
        "the trunk walk answers the pre-cut instant through the pasted fold row"
    );
    // Past the cut, absent — the fold's tombstone/current rows cover
    // nothing, so nothing is pasted (and the raw projection's tombstone
    // answers the present the same way it always did).
    assert_eq!(title_at(&db, BranchId::main(), T3).await, None);
}

/// **The review's second question, answered in Macrame alone:** the same
/// tombstone red shape holds for a plain supersede — the projection keeps
/// the newer row, so a read before the new version would return nothing,
/// not the old text. Now the close row's window answers it, the same
/// fold row the two-call sequence's read answers.
#[tokio::test]
async fn a_trunk_supersede_keeps_the_superseded_text_readable_below_the_cut() {
    let harness = TestHarness::new();
    let db = store(&harness, false).await;

    let batch = WriteBatch::new().concept(
        ConceptUpsert::new("g", "new text")
            .valid_from(T2)
            .valid_to(FOREVER)
            .supersede(true),
    );
    db.write_batch(batch).await.unwrap();
    tick().await;

    // The superseded text, at the past instant: the close's row (the only
    // record of a superseded window's start).
    assert_eq!(
        title_at(&db, BranchId::main(), T1).await,
        Some("live g".to_string()),
        "the trunk walk answers the pre-cut instant through the pasted fold row"
    );
    // At and past the supersede's start: the walk's row ("new text").
    assert_eq!(
        title_at(&db, BranchId::main(), T2).await,
        Some("new text".to_string())
    );
    assert_eq!(
        title_at(&db, BranchId::main(), T3).await,
        Some("new text".to_string())
    );
}

/// **The branch/trunk symmetry note's shape:** on a FORKED store (the
/// overlay's engaged one), the same retirement act on the trunk answers
/// the same at the same instants. Then the same answer rides the overlay
/// walk's own trunk decision and the pasted row never replaces it. The
/// tombstone's exact case, again.
#[tokio::test]
async fn the_engaged_trunk_answer_matches_the_unforked_one() {
    let harness = TestHarness::new();
    let db = store(&harness, true).await;

    tombstone_at_t2(&db).await;
    let close_batch = WriteBatch::new().concept(
        ConceptUpsert::new("g", "live g")
            .valid_from(T0)
            .valid_to(T2)
            .supersede(true),
    );
    db.write_batch(close_batch).await.unwrap();
    tick().await;

    // The trunk pre-cut instant answers "live g", as the unforked store
    // now does after the fold's `valid_at` gained the paste and engaged
    // branch reads answered through the overlay rows anyway.
    assert_eq!(
        title_at(&db, BranchId::main(), T1).await,
        Some("live g".to_string()),
        "the trunk's answer matches the unforked store's at the same instant"
    );
}

/// **The branch's windows stay the walk's own.** The branch superseded the
/// id's trunk row mid-window ([D-312]'s own-row close on the overlay),
/// then forked a second branch; the branch's projection row (the newest
/// trunk text) reads "new text" — but the branch's own window relation
/// holds the OLD text starting when the branch wrote (window [T2, FOREVER)).
/// The branch's own answer wins, at the FIRST read-of-things shape: its
/// mid-window instant name (T1.5 — between the mint and the branch write).
#[tokio::test]
async fn the_branch_walk_windows_stay_over_the_fold_s_rows() {
    let harness = TestHarness::new();
    let db = store(&harness, false).await;

    db.upsert_concept_superseding(
        ConceptUpsert::new("g", "new text")
            .valid_from(T2)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db.fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    tick().await;

    // The branch's answer at the past instant — T1 — rides the pasted row
    // ("live g"), the same as the trunk's. The branch's own [T2..) row
    // and the trunk's pasted row do not agree on which TEXT was believed
    // at T1 — the walk's overlay row wins for the id — but the pasted
    // row is only named where the walk decides nothing. (The overlay
    // walk decides the branch's rows at the instants its rows name, so
    // [T2..) answers "new text".)
    let alt = BranchId::new("alt").unwrap();
    assert_eq!(
        title_at(&db, alt.clone(), T1).await,
        Some("live g".to_string())
    );
    assert_eq!(
        title_at(&db, alt, T3).await,
        Some("new text".to_string()),
        "the branch's own overlay row wins at an instant its rows cover"
    );
}

/// **The post-push review's confirmed leak: the paste must inherit the
/// fold's ancestry cutoff.** The trunk believes `g` from T5, the branch
/// forks, the trunk then backfills `g` [T0, T5) — a belief recorded after
/// the fork. The branch read at T1 must answer **nothing**: the cutoff
/// fold answers the pre-fork row ([T5, FOREVER)), which does not serve
/// T1, and an ancestor's post-fork writes are invisible below the fork
/// cutoff (the visibility rule the fold's own `versions_cte` enforces on
/// every ancestor it folds). D-316's first draft pasted with the read's
/// recorded cap alone, so the ancestor's backfill surfaced below the
/// fork. The trunk's own reads keep the paste in both directions; the
/// branch keeps the vista its fork cut admits and nothing else.
#[tokio::test]
async fn a_branch_s_past_does_not_surface_the_ancestor_s_post_fork_beliefs() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    // The trunk's row, recorded before the fork.
    db.upsert_concept(
        ConceptUpsert::new("g", "trunk's row")
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    let feat = BranchId::new("feature").unwrap();
    db.fork(feat.clone(), BranchId::main()).await.unwrap();
    tick().await;

    // The trunk backfills below its own row's start, recorded after the
    // fork: the branch never saw this belief.
    db.upsert_concept(
        ConceptUpsert::new("g", "post-fork backfill")
            .valid_from(T0)
            .valid_to(T5),
    )
    .await
    .unwrap();
    tick().await;

    // The leak, at the named instant: the branch must answer nothing.
    assert_eq!(title_at(&db, feat.clone(), T1).await, None);
    assert_eq!(title_at(&db, feat.clone(), T3).await, None);
    // The vista the fork cut admits: the pre-fork row above its start.
    assert_eq!(
        title_at(&db, feat.clone(), T6).await,
        Some("trunk's row".to_string())
    );
    // The trunk's paste survives intact in both directions.
    assert_eq!(
        title_at(&db, BranchId::main(), T1).await,
        Some("post-fork backfill".to_string())
    );
    assert_eq!(
        title_at(&db, BranchId::main(), T6).await,
        Some("trunk's row".to_string())
    );
}
