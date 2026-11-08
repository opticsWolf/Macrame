//! The overlay's law corrections (0.22.0, M6-b continuation, [D-314], the
//! Road's [§15.2](../docs/Macrame%20Update%20Plan%20v0.22.0.md) red line and
//! [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.2–§5.3 as corrected by the post-merge review of `8f20eef..2f8a994`).
//!
//! Five findings, each red first, each traced by hand before it was traced by
//! test:
//!
//! 1. **The fork vista read the wrong stamp.** `route_concept` decided
//!    "invisible at the fork" from `concepts.recorded_at` — the projection's
//!    *last edit* — so a trunk edit after the fork turned a visible id foreign
//!    on a branch that must supersede it, and `effective_state_at` missed the
//!    inherited open belief the same way (a plain write going through without
//!    supersede). The creation stamp and the state at the cap come from the
//!    log, the way D-310's lineage fold reads: the table row when its own
//!    recording is at the cap, the pair's newest log row otherwise ("table
//!    first, else log").
//! 2. **The fold's channels kept one row per `(id, lineage)`.** The tables
//!    key on `(id, branch_id, valid_from)` and `(id, branch_id, cut_at)`; the
//!    delta now keys the same way, because a lineage that supersedes its own
//!    version chain has several windows and all of them stay decisions.
//! 3. **A read with no lineage now ignores the overlay.** The lineage-agnostic
//!    folds (`reconstruct`, `reconstruct_as_of`) answer D-310's law verbatim:
//!    the creating lineage's row. Road §15.2 cost 4: "the rule cannot be 'the
//!    most recent' — recorded_at is a total order only within a branch." A
//!    per-lineage read names its lineage (`reconstruct_on(ts, branch)`,
//!    `concepts_by_id(... .on(branch))`); the whole-ledger reads never moved
//!    and never will. This one is [the reviewer's] own: the amendment listed
//!    `reconstruct` in Story 1's per-reader list without saying which lineage
//!    it reads.
//! 4. **The scan filtered by coverage before picking a pair's newest row.**
//!    The log arms' comment promises "a pair's newest log row is its state
//!    there", and the `WHERE` broke the promise: an older open row survived a
//!    pick that should have answered with the newer closed one. Pick first,
//!    cover after.
//! 5. **A plain write whose interval starts before an inherited open belief
//!    hid it without supersede.** Routing and the D-291-within guard tested
//!    only the instant `valid_from`. The law is interval-shaped: an open
//!    belief *starting inside the new interval* is `ConceptSingleOpen` on the
//!    plain route, and a takeover covering it cuts at the new start (so a
//!    later retirement cannot resurrect the ancestor's text through a
//!    missing cut).
//!
//! Story 6 is the schema's own law (the v27 rung): cuts are immutable —
//! every `UPDATE` refused — and a version row refuses a backwards
//! `recorded_at` and a rewritten start.
//!
//! [the reviewer]: the post-merge review that opened the findings.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const T5: &str = "2026-01-10T00:00:00.000000Z";
const T5_MID: &str = "2026-01-10T12:00:00.000000Z";
const AFTER: &str = "2026-01-11T00:00:00.000000Z";
const V9: &str = "2026-01-20T00:00:00.000000Z";
const V9_AFTER: &str = "2026-01-21T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const OLD: &str = "the trunk's belief";
const NEW: &str = "the branch's belief";

/// Past every act in this file, but canonical: a stamp a post-cap rewrite
/// carries. The read cap falls between the acts and this.
const POKED: &str = "2027-01-01T00:00:00.000000Z";

/// Every write waits a tick, so each act carries its own stamp (the same
/// reason `m6a_routing_tests` sleeps).
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

/// A string stamp the log/table holds, for transaction-time reads.
async fn stamp_of(db: &Database, sql: &str) -> String {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    rows.next()
        .await
        .unwrap()
        .expect("the act's row")
        .get::<String>(0)
        .expect("recorded_at")
}

/// One microsecond earlier than `s` (the same helper the readers file keeps;
/// the act stamps are tick-separated, so the microsecond field is never zero).
fn a_micro_before(s: &str) -> String {
    let (body, micro) = s.split_once('.').expect("canonical stamp");
    let digits = micro.trim_end_matches('Z');
    let n: u64 = digits.parse().expect("microseconds");
    assert!(n > 0, "a stamp ending in .0 cannot lose one microsecond");
    format!("{body}.{:0>6}Z", n - 1)
}

/// One microsecond after `s` — the read cap between the last act and the
/// poke, derived from the ledger rather than a literal, because the
/// harnesses stamp acts with the wall clock and no literal is safe under it.
fn a_micro_after(s: &str) -> String {
    let (body, micro) = s.split_once('.').expect("canonical stamp");
    let digits = micro.trim_end_matches('Z');
    let n: u64 = digits
        .parse::<u64>()
        .expect("microseconds")
        .checked_add(1)
        .expect("microseconds do not roll over inside a canonical day");
    format!("{body}.{:0>6}Z", n)
}

/// The latest act stamp the ledger carries below a cap far past everything
/// (the poke writes [`POKED`], the only row under that ceiling), plus one
/// microsecond: the read cap after the acts and before the poke.
async fn cap_after_acts(db: &Database) -> String {
    let last = stamp_of(
        db,
        "SELECT MAX(recorded_at) FROM transaction_log \
         WHERE recorded_at < '3000-01-01T00:00:00.000000Z'",
    )
    .await;
    a_micro_after(&last)
}

/// One text, or `None` where the id resolved absent.
async fn text_at(
    db: &Database,
    b: BranchId,
    valid: Option<&str>,
    recorded: Option<&str>,
) -> Option<String> {
    let mut plan = ReadPlan::new().on(b.clone());
    if let Some(v) = valid {
        plan = plan.valid_at(v);
    }
    if let Some(r) = recorded {
        plan = plan.recorded_at(r);
    }
    let rows = db.concepts_by_id(&["x".to_string()], plan).await.unwrap();
    rows.into_iter().next().map(|c| c.title)
}

/// The trunk minted `x` (`from` .. `to`, `OLD`), and `b0` forked after the
/// mint with two ticks between every act.
async fn mint_then_fork(harness: &TestHarness, from: &str, to: &str) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(ConceptUpsert::new("x", OLD).valid_from(from).valid_to(to))
        .await
        .unwrap();
    tick().await;
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    tick().await;
    db
}

/// A raw second connection to the store the tests opened, for the pokes the
/// fold must survive: the state `concept_versions`/`concept_cuts` hold is read
/// literally, and a poke that moves a row's `recorded_at` represents the
/// post-cap rewrite its case is about (the same way the replay tests punch
/// archive-shaped holes with a second connection).
async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Finding 1: the fork vista reads the LOG, not the projection's recorded_at
// ---------------------------------------------------------------------------

/// A trunk edit after the fork leaves the id visible at the fork: the
/// branch's supersede is a takeover, not a cross-lineage refusal. The
/// projection's `recorded_at` is the *edit's* stamp, not the creation's, and
/// the vista must read the creation from the log (the first `concepts` row
/// this lineage ever wrote for the id).
#[tokio::test]
async fn the_p7_vista_reads_the_creation_stamp_from_the_log() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    // The trunk edits its row after the fork: same start, new content, a
    // recorded_at the projection now carries that is past the fork cutoff.
    tick().await;
    db.upsert_concept(ConceptUpsert::new("x", "the trunk, edited").valid_from(TS))
        .await
        .unwrap();
    tick().await;

    // The branch supersedes what it inherited: the takeover route.
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .expect("the branch's own row is a takeover over a belief it saw at the fork");

    // And each line reads its own truth.
    assert_eq!(
        text_at(&db, b0, None, None).await.as_deref(),
        Some(NEW),
        "the branch reads its own row"
    );
    assert_eq!(
        text_at(&db, BranchId::main(), None, None).await.as_deref(),
        Some("the trunk, edited"),
        "the trunk reads its edited row"
    );
}

/// The inherited state at the fork cutoff comes from the log too: a trunk
/// supersede *after* the fork leaves the pre-fork open belief in the log, and
/// the branch's plain write at an instant inside that belief is refused — the
/// projection's rows are all recorded after the cutoff now, but the *belief*
/// the branch inherited is the one the cutoff holds.
#[tokio::test]
async fn the_inherited_state_at_a_cutoff_comes_from_the_log() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    // The trunk supersedes its own row after the fork: [TS, AFTER) closed,
    // [AFTER, ∞) opened, both recorded after b0's fork cutoff.
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("x", "the trunk's second belief")
            .supersede(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;

    // The branch's plain write at T5 swims inside the belief the cutoff
    // holds: [TS, ∞) as of the fork. Supersede is required.
    let refused = view
        .upsert_concept(ConceptUpsert::new("x", "the branch's belief").valid_from(T5))
        .await;
    assert!(
        matches!(
            refused,
            Err(macrame::error::DbError::ConceptSingleOpen { .. })
        ),
        "a plain write inside an inherited open belief is ConceptSingleOpen, got {refused:?}"
    );

    // With supersede it is the takeover, and each line reads its own truth.
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    assert_eq!(
        text_at(&db, b0, None, None).await.as_deref(),
        Some(NEW),
        "the branch reads its own row after the takeover"
    );
    assert_eq!(
        text_at(&db, BranchId::main(), Some(AFTER), None)
            .await
            .as_deref(),
        Some("the trunk's second belief"),
        "the trunk reads its superseded row"
    );
}

// ---------------------------------------------------------------------------
// Finding 5: the interval-shaped guard on the branch's route
// ---------------------------------------------------------------------------

/// A plain write whose interval starts *before* an inherited open belief is
/// refused: nothing covers its start, but the ancestor's belief starts inside
/// the write's interval, and accepting it would let a later retirement
/// resurrect the ancestor's text (the no-resurrection red line, arriving
/// through the plain route this time).
#[tokio::test]
async fn a_plain_write_that_starts_inside_an_inherited_open_belief_is_refused() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    // [T5, ∞) says nothing covers T5 — the ancestor's [TS, ∞) starts inside
    // the interval. The plain route is refused.
    let refused = view
        .upsert_concept(ConceptUpsert::new("x", "the branch's belief").valid_from(T5))
        .await;
    assert!(
        matches!(
            refused,
            Err(macrame::error::DbError::ConceptSingleOpen { .. })
        ),
        "the interval-shaped guard refuses the plain write, got {refused:?}"
    );

    // With supersede the takeover cuts what it takes over, and the
    // retirement that follows cannot resurrect the trunk's text: the cut the
    // takeover wrote covers every instant the branch's own row ever stopped
    // covering.
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .retired(true)
            .valid_from(V9)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    assert_eq!(
        text_at(&db, b0, Some(V9_AFTER), None).await,
        None,
        "past the retirement the branch reads absent, not the trunk's text again"
    );
}

/// The same law on the takeover route: a supersede at an instant an inherited
/// belief does not cover must still cut it, because the belief *starts*
/// inside the superseding interval and would be resurrected by the
/// retirement's own close.
#[tokio::test]
async fn a_supersede_cut_covers_a_belief_that_starts_within_its_interval() {
    // The trunk minted late: [AFTER, ∞). The branch supersedes at T5, before
    // the mint's start — nothing covers T5 on the path.
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, AFTER, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .expect(
        "the takeover route itself is not refused; the ancestor's start is inside the interval",
    );
    tick().await;
    // The retirement closes the branch's own row — and the takeover's cut is
    // what keeps the ancestor from coming back.
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .retired(true)
            .valid_from(V9)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    assert_eq!(
        text_at(&db, b0, Some(V9_AFTER), None).await,
        None,
        "past the retirement the branch reads absent: the takeover's cut covers, the ancestor does not resurrect"
    );
}

// ---------------------------------------------------------------------------
// Finding 3: a read with no lineage answers the mint's law
// ---------------------------------------------------------------------------

/// **The trunk's reads never move because a branch wrote** — the sentence
/// Story 2's heading has always opened with, now pinned to the folds it
/// names. A whole-ledger read (`reconstruct`, `reconstruct_as_of`) carries no
/// lineage, so it consults none of the overlay's per-lineage rows: it
/// answers the creating lineage's row, both at the present and on the valid
/// axis, wherever that row covers — and a branch-only retirement removes
/// nothing from it. The per-lineage reads are the ones that walk; the
/// lineage-less folds are D-310's, byte for byte.
///
/// Road [§15.2](../docs/Macrame%20Update%20Plan%20v0.22.0.md) cost 4 rules
/// the alternative out by name: with two branches reading "the most recent"
/// is not a rule, because `recorded_at` is a total order only within a
/// branch.
#[tokio::test]
async fn a_lineage_less_fold_answers_the_mint_s_law() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    let now = cap_after_acts(&db).await;
    let pre = a_micro_before(&now);

    // The recorded axis: the whole-ledger fold never moved.
    let r_pre = db.reconstruct(&pre).await.unwrap();
    assert_eq!(
        r_pre.concepts.get("x").expect("x pre").title,
        OLD,
        "before the act the mint's row is the whole-ledger answer"
    );
    let r_post = db.reconstruct(&now).await.unwrap();
    assert_eq!(
        r_post.concepts.get("x").expect("x post").title,
        OLD,
        "the branch supersede is a per-lineage row: the whole-ledger fold still answers the mint's row"
    );

    // The valid axis: the same law at every instant the mint's row covers.
    let in_window = db
        .reconstruct_as_of(macrame::temporal::AsOf::bitemporal(T5_MID, now.clone()))
        .await
        .unwrap();
    assert_eq!(
        in_window.concepts.get("x").expect("x in window").title,
        OLD,
        "the whole-ledger valid-axis fold answers the creating lineage's row"
    );

    // A branch-only retirement removes nothing from the whole-ledger folds:
    // the cut is the branch's decision about its own path. A second store,
    // second harness: one harness carries one file.
    let harness2 = TestHarness::new();
    let db2 = mint_then_fork(&harness2, TS, FOREVER).await;
    let view2 = db2.view(b0.clone());
    tick().await;
    view2
        .upsert_concept(
            ConceptUpsert::new("x", NEW)
                .retired(true)
                .valid_from(T5)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
    tick().await;
    let cap = stamp_of(
        &db2,
        "SELECT MAX(recorded_at) FROM transaction_log \
         WHERE recorded_at < '3000-01-01T00:00:00.000000Z'",
    )
    .await;
    let r_retired = db2.reconstruct(&cap).await.unwrap();
    assert!(
        r_retired.concepts.contains_key("x"),
        "a branch-only cut does not retire the id on the lineage-less fold"
    );

    // The per-lineage read is the one that walks: same store, named lineage,
    // the branch's cut answers.
    assert_eq!(
        text_at(&db2, b0, Some(V9), None).await,
        None,
        "the walked read on the branch resolves the retirement absent"
    );
}

// ---------------------------------------------------------------------------
// Finding 2: the channels carry the tables' own keys
// ---------------------------------------------------------------------------

/// A lineage's version chain reads by each version's own window. Two
/// supersedes, then a retirement that closes the second window: the walked
/// valid-axis reads answer window 1 inside window 1, window 2 inside window
/// 2, and absent past the retirement (the cut covers, both windows named).
/// The whole-ledger fold still answers the mint's row everywhere
/// ([finding 3]), and the recorded-axis walk answers the newest window at the
/// present.
#[tokio::test]
async fn the_paths_version_rows_read_by_their_own_windows() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    view.upsert_concept(
        ConceptUpsert::new("x", "the first window")
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", "the second window")
            .supersede(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", "the second window")
            .retired(true)
            .valid_from(V9)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        text_at(&db, b0.clone(), Some(T5_MID), None)
            .await
            .as_deref(),
        Some("the first window"),
        "window 1 covers its own interior"
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(AFTER), None).await.as_deref(),
        Some("the second window"),
        "window 2 covers from its start"
    );
    assert_eq!(
        text_at(&db, b0, Some(V9_AFTER), None).await,
        None,
        "past the retirement both windows are named and the cut covers"
    );
    // The recorded-axis walk, at the present: the newest window.
    let now = cap_after_acts(&db).await;
    let recon = db.reconstruct_on(&now, "b0").await.unwrap();
    assert!(
        !recon.concepts.contains_key("x"),
        "at the present the walk's windows are closed and the cut answers"
    );
    assert_eq!(
        db.reconstruct(&now)
            .await
            .unwrap()
            .concepts
            .get("x")
            .map(|c| c.title.as_str()),
        Some(OLD),
        "the whole-ledger fold answers the mint's row ([finding 3]), untouched by the chain"
    );
}

// ---------------------------------------------------------------------------
// Finding 4: pick a pair's newest row first, then cover
// ---------------------------------------------------------------------------

/// The scan's log arm must pick a pair's newest row *before* asking whether
/// it covers the read's instant — the arm's own comment promises "a pair's
/// newest log row is its state there". The construct: one lineage whose pair
/// opened and later closed, the pair's table row rewritten past the read cap
/// (the poke standing in for the later act that could mint it — a content
/// re-touch — so the state at the cap can only come from the log), and a
/// read after the close.
///
/// As of the cap the lineage believes nothing for instants past the close:
/// its window is named, its cut covers, the id is *absent* — not the
/// open-text an older, open log row would answer if the `WHERE` filtered
/// before the pick.
#[tokio::test]
async fn the_overlay_s_log_arms_pick_a_pair_s_newest_row_first() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    // The pair opens at T5 and closes at AFTER (a retirement of the open
    // state closes its own row; no second version, no other candidate).
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .retired(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    // The pair's table row is pushed past the read cap — the state a later
    // rewrite would carry — so the pair's state at the cap lives only in the
    // log: the open insert and the close.
    let raw = raw_conn(&harness.db_path).await;
    raw.execute(
        "UPDATE concept_versions \
         SET recorded_at = ?1 \
         WHERE id = 'x' AND branch_id = 'b0' AND valid_from = ?2",
        libsql::params![POKED, T5],
    )
    .await
    .unwrap();
    drop(raw);

    // At the cap, after the close: the newest row for the pair is the close
    // — it does not cover V9; the cut does. Absent, not the open text. The
    // cap is derived from the ledger (after the acts, before the poke).
    let cap = cap_after_acts(&db).await;
    assert_eq!(
        text_at(&db, b0.clone(), Some(V9), Some(&cap)).await,
        None,
        "the pair's newest row as of the cap is the close; the read is absent past it"
    );
    // Inside the window the close's row answers (and its window is the named
    // one, not the pre-close open interval).
    let plan = ReadPlan::new().on(b0).valid_at(T5_MID).recorded_at(&cap);
    let rows = db.concepts_by_id(&["x".to_string()], plan).await.unwrap();
    assert_eq!(
        rows.into_iter().next().map(|c| c.valid_to.clone()),
        Some(Some(AFTER.to_string())),
        "the pair's state at the cap carries the close's window"
    );
}

// ---------------------------------------------------------------------------
// Finding 6: the overlay's rows are the law's own rows (the v27 rung)
// ---------------------------------------------------------------------------

/// A cut row is a decision, not a window: no act rewrites it. The ladder's
/// v27 rung refuses every `UPDATE` on `concept_cuts` — a raw edit would make
/// the table disagree with the log silently, and the ledger's rows are the
/// resolution's source of truth.
#[tokio::test]
async fn a_cut_row_refuses_update() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let view = db.view(BranchId::new("b0").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let raw = raw_conn(&harness.db_path).await;
    let refused = raw
        .execute("UPDATE concept_cuts SET cut_at = ?1", libsql::params![V9])
        .await;
    drop(raw);
    assert!(
        refused.is_err(),
        "a cut row refuses every UPDATE: {refused:?}"
    );
}

/// A version row's `recorded_at` moves forward with the acts: the closest
/// thing the ledger has to an order, the state rule reads it as one, and a
/// backwards edit would put an older belief *after* a newer one inside the
/// same pair.
#[tokio::test]
async fn a_version_row_refuses_recorded_at_backwards() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let view = db.view(BranchId::new("b0").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let raw = raw_conn(&harness.db_path).await;
    let refused = raw
        .execute(
            "UPDATE concept_versions SET recorded_at = ?1",
            libsql::params![TS],
        )
        .await;
    drop(raw);
    assert!(
        refused.is_err(),
        "a version row refuses recorded_at going backwards: {refused:?}"
    );
}

/// A version row's start is its identity (`(id, branch_id, valid_from)` — the
/// tables' own keys the fold's channels are re-keyed to): a raw edit that
/// rewrote it would orphan the pair's log rows and split the lineage's
/// windows.
#[tokio::test]
async fn a_version_row_refuses_rewriting_its_start() {
    let harness = TestHarness::new();
    let db = mint_then_fork(&harness, TS, FOREVER).await;
    let view = db.view(BranchId::new("b0").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let raw = raw_conn(&harness.db_path).await;
    let refused = raw
        .execute(
            "UPDATE concept_versions SET valid_from = ?1",
            libsql::params![V9],
        )
        .await;
    drop(raw);
    assert!(
        refused.is_err(),
        "a version row refuses a rewritten start: {refused:?}"
    );
}
