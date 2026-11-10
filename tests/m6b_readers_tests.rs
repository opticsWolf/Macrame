//! M6-b's content-reader tests (0.22.0, [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.3, §5.4 cost 1; the P-2 census's content rows).
//!
//! M6-a made the write actor own the overlay (`concept_versions` rows on the
//! writer's lineage, cuts stopping the fall-through); M6-b makes the readers
//! resolve by the law the writes created, corrected by the post-merge review
//! of that same landing. In one sentence: **a read that names a lineage
//! walks the path nearest first** — the first overlay row of `id` covering
//! the instant answers (a version wins over a cut where both cover), a
//! covering cut answers *absent* and stops the walk, and only where nothing
//! of the overlay decides does the projection answer as it does today
//! (D-310's fold at the per-lineage caps) — **and a read that names no
//! lineage walks nothing**: the whole-ledger folds (`reconstruct`,
//! `reconstruct_as_of`) are D-310's creating-lineage row verbatim, because a
//! recency rule over branches is no rule at all (*recorded_at* is a total
//! order only within a branch, the Road's §15.2 cost 4).
//!
//! The stories are the amendment's §12 names:
//!
//! - [`a_branch_supersede_shows_the_old_text_before_the_cut_and_its_own_after`]
//!   (§12.1 — per reader: `concepts_by_id`, traversal `CURRENT` and `AT_TIME`,
//!   the subgraph loader, and the transaction-time bound);
//! - [`a_windowed_takeover_hides_at_current_and_shows_on_its_window`] (the
//!   amendment's own sentence: superseded text shows from the cut until its
//!   version ends, and *absent* thereafter);
//! - [`a_branch_retire_stops_the_read_past_the_cut`] (§12.5's read side);
//! - [`a_later_ancestor_edit_after_the_fork_does_not_change_the_branch_reads`]
//!   (§12.6, monotonicity);
//! - [`a_branch_supersede_leaves_the_mint_lineage_standing_correctly`]
//!   (§12.2 — branch rows are branch rows; the trunk's reads never move),
//!   carried into the folds: [`a_lineage_less_fold_answers_the_mint_s_law
//!   _before_and_after_a_branch_write`] and [`a_windowed_takeover_folds_to
//!   _the_mint_s_law_and_the_walk_to_the_window`] — a whole-ledger fold
//!   names no lineage and resolves nothing (the post-merge review's finding
//!   3, the Road's [§15.2 cost 4](../docs/Macrame%20Update%20Plan%20v0.22.0.md));
//! - the two "today's law holds where the overlay says nothing" gates:
//!   a forked store before any version row (§12.13's forked twin) and the
//!   never-forked database itself;
//! - the traversal's vector-filter candidates resolve through the same law
//!   (P-2 content row 8): the trunk's post-fork retirement must not hide what
//!   the branch's own rows answer, and the branch's cut must drop what it
//!   resolves absent.
//!
//! Everything here is public-API level. The row-level truths of the overlay
//! belong to the rung file (`m6a_rung_tests.rs`), the resolution law's
//! exhaustive shape to P-8 (`m6_resolution_tests.rs`), and the write actor's
//! routing to `m6a_routing_tests.rs`.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const T5: &str = "2026-01-10T00:00:00.000000Z";
const AFTER: &str = "2026-01-11T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const OLD: &str = "the trunk's belief";
const NEW: &str = "the branch's belief";

const BYTES: usize = 10 * 1024 * 1024;

/// The trunk minted `x` (from `TS`, open unless `ends` says otherwise), an
/// edge reaches it from `a`, and `b0` forked after the mint. Tests write on
/// views.
async fn forked_view(harness: &TestHarness, ends: Option<&str>) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("x", OLD)
            .valid_from(TS)
            .valid_to(ends.unwrap_or(FOREVER)),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("a", "Hub")
            .valid_from(TS)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.write_bulk_atomic(vec![EdgeAssertion::new("a", "x", "LINKS")
        .valid_from(TS)
        .valid_to(FOREVER)])
        .await
        .unwrap();
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    db
}

/// Every write waits a tick, so each act carries its own stamp (the same
/// reason `m6a_routing_tests` sleeps).
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

/// A scalar read against the store's read connection.
async fn scalar(db: &Database, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    v
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

/// One microsecond earlier than `s`. The act stamps are tick-separated, so
/// the microsecond field is never zero when this runs.
fn a_micro_before(s: &str) -> String {
    let (body, micro) = s.split_once('.').expect("canonical stamp");
    let digits = micro.trim_end_matches('Z');
    let n: u64 = digits.parse().expect("microseconds");
    assert!(n > 0, "a stamp ending in .0 cannot lose one microsecond");
    format!("{body}.{:0>6}Z", n - 1)
}

/// One text, or `None` where the id resolved absent.
async fn text_at(
    db: &std::sync::Arc<Database>,
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

/// The traversal's texts over `a`'s neighbourhood, read at `valid` when named
/// (`AT_TIME`) and at current belief otherwise.
async fn traversal_texts(
    db: &std::sync::Arc<Database>,
    b: BranchId,
    valid: Option<&str>,
    now: &str,
) -> Vec<String> {
    let mut walk = TraversalBuilder::new("a").max_depth(1).on_branch(b.clone());
    if let Some(v) = valid {
        walk = walk.attribute_mode(AttributeMode::AtTime).as_of_valid(v);
    }
    let conn = db.read_conn();
    walk.execute(conn, now)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.title)
        .collect()
}

// ---------------------------------------------------------------------------
// §12.1 — the analogue the amendment opens with, on every reader
// ---------------------------------------------------------------------------

/// The branch superseded the trunk's open `x` at `T5`. Every content reader on
/// `b0` shows the trunk's text before `T5` and the branch's from `T5`, and on
/// the transaction-time axis the same two answers either side of the act's
/// stamp.
#[tokio::test]
async fn a_branch_supersede_shows_the_old_text_before_the_cut_and_its_own_after() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
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

    let r_takeover = stamp_of(
        &db,
        "SELECT MAX(recorded_at) FROM concept_versions WHERE id = 'x'",
    )
    .await;
    let r_pre = a_micro_before(&r_takeover);
    assert_ne!(r_pre, r_takeover, "one microsecond before the act");

    // The valid axis (§12.1's before-the-cut and its-own-after). At the cut's
    // own instant the branch's version is what covers ([T5, ∞) is half-open).
    assert_eq!(
        text_at(&db, b0.clone(), Some(TS), None).await.as_deref(),
        Some(OLD)
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(T5), None).await.as_deref(),
        Some(NEW)
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(AFTER), None).await.as_deref(),
        Some(NEW)
    );

    // The transaction-time axis: before the act the branch believed the
    // trunk's text; the act is its own.
    assert_eq!(
        text_at(&db, b0.clone(), None, Some(r_pre.as_str()))
            .await
            .as_deref(),
        Some(OLD)
    );
    assert_eq!(
        text_at(&db, b0.clone(), None, Some(r_takeover.as_str()))
            .await
            .as_deref(),
        Some(NEW)
    );

    // Traversal: `a` reaches `x`; current belief shows the branch's text, and
    // the same at the instant after the cut.
    let currents = traversal_texts(&db, b0.clone(), None, &r_takeover).await;
    assert!(
        currents.iter().any(|t| t == NEW),
        "current traversal shows the branch's text: {currents:?}"
    );
    let at_time = traversal_texts(&db, b0.clone(), Some(AFTER), &r_takeover).await;
    assert!(
        at_time.iter().any(|t| t == NEW),
        "AT_TIME after the cut shows the branch's text: {at_time:?}"
    );

    // The subgraph loader, whose nodes carry the resolved text. The sugar
    // has a builder to carry the branch, which is the whole of the
    // lineage-view question here.
    let loader_walk = TraversalBuilder::new("a")
        .max_depth(1)
        .on_branch(b0.clone());
    let g = db
        .load_subgraph_with(&loader_walk, &r_takeover, BYTES)
        .await
        .unwrap();
    let n: Vec<(&str, &macrame::graph::NodeData)> = g.nodes().collect();
    let (_, x) = n.iter().find(|(id, _)| *id == "x").expect("x reached");
    assert_eq!(x.title(), NEW, "the loader carries the branch's text");
}

// ---------------------------------------------------------------------------
// The amendment's own sentence: a version that has ended hands out absent
// ---------------------------------------------------------------------------

/// A takeover with a window (`[T5, AFTER]` named) is open *nowhere*: the
/// branch's current reads stay absent, and the resolution stops there — it
/// must not fall through to the trunk's open row and show `OLD` again. Within
/// the window the branch's text is what holds; before it the trunk's is.
#[tokio::test]
async fn a_windowed_takeover_hides_at_current_and_shows_on_its_window() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(AFTER),
    )
    .await
    .unwrap();

    // Current belief (the axis's far end): the version is not open, the cut
    // covers, the walk stops. Absent — not `OLD` again.
    assert_eq!(text_at(&db, b0.clone(), None, None).await, None);

    // Along the axis both eras are as they were.
    assert_eq!(
        text_at(&db, b0.clone(), Some(TS), None).await.as_deref(),
        Some(OLD)
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(T5), None).await.as_deref(),
        Some(NEW)
    );
    // At the window's own end the version has just ended: absent.
    assert_eq!(text_at(&db, b0.clone(), Some(AFTER), None).await, None);
}

// ---------------------------------------------------------------------------
// §12.5's read side — the retire is invisible on the reader too
// ---------------------------------------------------------------------------

/// The branch retired the inheritance at `T5` (a bare cut). Before it the
/// trunk's text still shows; from the cut on the id is absent — no reader
/// answers past the stop, and the traversal, whose narrow runs the same law,
/// does not reach the node either.
#[tokio::test]
async fn a_branch_retire_stops_the_read_past_the_cut() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    tick().await;
    view.upsert_concept(ConceptUpsert::new("x", OLD).retired(true).valid_from(T5))
        .await
        .unwrap();

    // Before the cut the trunk's belief is what the branch sees.
    assert_eq!(
        text_at(&db, b0.clone(), Some(TS), None).await.as_deref(),
        Some(OLD)
    );

    // At and past the cut: absent, everywhere.
    assert_eq!(text_at(&db, b0.clone(), Some(T5), None).await, None);
    assert_eq!(text_at(&db, b0.clone(), Some(AFTER), None).await, None);
    assert_eq!(
        text_at(&db, b0.clone(), None, None).await,
        None,
        "current too"
    );

    // The traversal meets the same law: a current walk does not show `x`,
    // and its AT_TIME before the cut shows the trunk's text.
    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_cuts").await;
    let currents = traversal_texts(&db, b0.clone(), None, &now).await;
    assert!(
        !currents.iter().any(|t| t == OLD),
        "a current traversal of a cut id is silent: {currents:?}"
    );
    let back = traversal_texts(&db, b0.clone(), Some(TS), &now).await;
    assert!(
        back.iter().any(|t| t == OLD),
        "before the cut the trunk's text shows: {back:?}"
    );
}

// ---------------------------------------------------------------------------
// §12.6 — the ancestor's later edit stays on the ancestor
// ---------------------------------------------------------------------------

/// The trunk superseded `x` *after* the fork (its own takeover, its own
/// stamp). The branch's reads carry what the branch said; the trunk's carry
/// the edit. Nothing crosses.
#[tokio::test]
async fn a_later_ancestor_edit_after_the_fork_does_not_change_the_branch_reads() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
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
    db.upsert_concept(
        ConceptUpsert::new("x", "the trunk's later text")
            .supersede(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        text_at(&db, b0.clone(), None, None).await.as_deref(),
        Some(NEW)
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(T5), None).await.as_deref(),
        Some(NEW)
    );
    assert_eq!(
        text_at(&db, b0.clone(), Some(AFTER), None).await.as_deref(),
        Some(NEW),
        "the trunk's later start does not reach the branch, whose cut already covers"
    );
    assert_eq!(
        text_at(&db, BranchId::main(), None, None).await.as_deref(),
        Some("the trunk's later text")
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'x'").await,
        1
    );
}

// ---------------------------------------------------------------------------
// §12.2 — the trunk's reads never move because a branch wrote
// ---------------------------------------------------------------------------

/// The branch's takeover wrote two rows on its own lineage. The trunk's
/// projection stays one row, its reads carry `OLD` on every axis, and the
/// ledger's concept-fold (`reconstruct`) carries no branch text.
#[tokio::test]
async fn a_branch_supersede_leaves_the_mint_lineage_standing_correctly() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let view = db.view(BranchId::new("b0").unwrap());

    let before = text_at(&db, BranchId::main(), None, None).await;
    let rows_before = scalar(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'x'").await;

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        text_at(&db, BranchId::main(), None, None).await,
        before,
        "the trunk's current read is unchanged"
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'x'").await,
        rows_before
    );
    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_versions").await;
    // "Byte identical" names the mint lineage's own fold: its ancestry is
    // itself alone, so nothing the branch wrote can reach it, and the
    // reconstruction answers the mint's own row everywhere.
    let state = db.reconstruct_on(&now, "main").await.unwrap();
    let entry = state.concepts.get("x").expect("the mint's projection");
    assert_eq!(
        entry.title, OLD,
        "the mint lineage's reconstruction holds the mint's own row"
    );
}

// ---------------------------------------------------------------------------
// The folds (§12.1's per-reader list: `reconstruct`, `reconstruct_as_of` and
// the ancestor fold `reconstruct_on`)
// ---------------------------------------------------------------------------

/// **A whole-ledger fold answers the mint's law, before and after a branch
/// write.** The overlay's rows are per-lineage states, and a fold with no
/// lineage to walk resolves nothing: D-310's answer — the creating lineage's
/// row — is the answer at every instant the row covers, both axes. Road
/// [§15.2 cost 4](../docs/Macrame%20Update%20Plan%20v0.22.0.md) rules the
/// alternative out by name ("the rule cannot be 'the most recent' —
/// `recorded_at` is a total order only within a branch"); the per-reader list
/// of §12.1 names [`reconstruct_on`] and the `concepts_by_id` plan, not the
/// whole-ledger folds. (This test pinned the recency reading when M6-b first
/// landed and was corrected with the post-merge review's finding 3 same
/// day.)
///
/// [`reconstruct_on`]: crate::temporal::reconstruct_on
#[tokio::test]
async fn a_lineage_less_fold_answers_the_mint_s_law_before_and_after_a_branch_write() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_versions").await;
    let r_pre = a_micro_before(&now);

    // Before the act: not yet in the ledger, the mint's row is the answer.
    let pre = db.reconstruct(&r_pre).await.unwrap();
    assert_eq!(pre.concepts.get("x").expect("x pre").title, OLD);

    // After the act: the branch's row is a per-lineage state; the
    // whole-ledger fold's answer did not move.
    let post = db.reconstruct(&now).await.unwrap();
    assert_eq!(
        post.concepts.get("x").expect("x post").title,
        OLD,
        "the whole-ledger fold keeps the mint's row: a read with no lineage resolves nothing"
    );

    // The valid axis: the same law at every instant the mint's row covers.
    let pre_valid = macrame::temporal::AsOf::bitemporal(TS, now.clone());
    let post_valid = macrame::temporal::AsOf::bitemporal(T5, now.clone());
    let pre_state = db.reconstruct_as_of(pre_valid).await.unwrap();
    assert_eq!(pre_state.concepts.get("x").expect("x pre valid").title, OLD);
    let post_state = db.reconstruct_as_of(post_valid).await.unwrap();
    assert_eq!(
        post_state.concepts.get("x").expect("x post valid").title,
        OLD,
        "the valid-axis fold keeps the mint's row"
    );

    // The walked read is the one that answers the branch: same store, named
    // lineage.
    let state = db.reconstruct_on(&now, "b0").await.unwrap();
    assert_eq!(
        state.concepts.get("x").expect("x on b0").title,
        NEW,
        "the per-lineage fold is the one that walks"
    );
}

/// A branch's reconstruction walks its own ancestry nearest first. The
/// branch's own overlay row answers before any ancestor's row; where the
/// branch said nothing and the mint holder holds the id, the mint's row is
/// what the fold carries; and a deeper fork inherits the nearer lineage's
/// row the way the walk reads it.
#[tokio::test]
async fn a_branch_reconstruction_resolves_the_walk() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
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
    db.fork(BranchId::new("b1").unwrap(), b0.clone())
        .await
        .unwrap();

    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_versions").await;

    // The branch's own fold: the walk reaches its own version row first.
    let on_b0 = db.reconstruct_on(&now, "b0").await.unwrap();
    assert_eq!(on_b0.concepts.get("x").expect("x on b0").title, NEW);

    // A deeper fork: `b1` inherits `b0`'s overlay row by the same walk.
    let on_b1 = db.reconstruct_on(&now, "b1").await.unwrap();
    assert_eq!(
        on_b1.concepts.get("x").expect("x on b1").title,
        NEW,
        "b1 inherits b0's overlay answer"
    );

    // Where nothing of the overlay decides, the fold is today's: the mint's
    // row. `a` was never versioned.
    assert_eq!(on_b0.concepts.get("a").expect("a on b0").title, "Hub");
}

/// The branch's cut (the retire read side, §12.5) reaches the folds: the
/// branch's reconstruction answers absent — and where a snapshot-base or an
/// ancestor's row still stands in the composed map, the overlay's cut must
/// remove it, not merely decline to add it. The mint-holder's own fold keeps
/// the id as it ever was.
#[tokio::test]
async fn a_branch_reconstruction_stops_at_the_branch_s_cut() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(ConceptUpsert::new("x", OLD).retired(true).valid_from(T5))
        .await
        .unwrap();

    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_cuts").await;
    let on_b0 = db.reconstruct_on(&now, "b0").await.unwrap();
    assert_eq!(
        on_b0.concepts.get("x"),
        None,
        "the overlay's cut removes the fold's row"
    );
    let on_main = db.reconstruct_on(&now, "main").await.unwrap();
    assert_eq!(
        on_main
            .concepts
            .get("x")
            .expect("the mint-holder's own fold")
            .title,
        OLD,
        "the mint-holder's own fold is untouched"
    );
}

/// §12.6's fold twin: the ancestor's post-fork supersede is cut from the
/// branch's fold at the fork point, so the branch's reconstruction carries
/// what the branch said and the mint-holder's carries the ancestor's edit.
#[tokio::test]
async fn an_ancestor_s_post_fork_edit_stays_out_of_the_branch_s_reconstruction() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let view = db.view(BranchId::new("b0").unwrap());

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
    db.upsert_concept(
        ConceptUpsert::new("x", "the trunk's later text")
            .supersede(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM transaction_log").await;
    let on_b0 = db.reconstruct_on(&now, "b0").await.unwrap();
    assert_eq!(
        on_b0.concepts.get("x").expect("x on b0").title,
        NEW,
        "the ancestor's post-fork edit is cut from the branch's fold"
    );
    let on_main = db.reconstruct_on(&now, "main").await.unwrap();
    assert_eq!(
        on_main.concepts.get("x").expect("x on main").title,
        "the trunk's later text",
        "the mint-holder's own fold carries its edit"
    );
}

/// The windowed takeover, read from **both** ends of the resolution's
/// division: a whole-ledger fold (`reconstruct`, `reconstruct_as_of`) names
/// no lineage, so it answers the mint's row at every instant it covers — the
/// present, inside the window, past it — while the per-lineage reads answer
/// the window where it runs and drop the id past it (the pair's cut covers).
/// The whole-ledger claims are the story this file's closing gates already
/// carry for the branch's current reads; this one pins the fold-side half of
/// the same statement (the post-merge review's finding 3). The window's own
/// read shape is the story above.
#[tokio::test]
async fn a_windowed_takeover_folds_to_the_mint_s_law_and_the_walk_to_the_window() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(AFTER),
    )
    .await
    .unwrap();

    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM concept_versions").await;
    // The whole-ledger fold, on the recorded axis: the mint's law verbatim.
    let recorded_now = db.reconstruct(&now).await.unwrap();
    assert_eq!(
        recorded_now.concepts.get("x").expect("x now").title,
        OLD,
        "the windowed pair is the branch's state; the lineage-less fold answers the mint"
    );
    // The valid axis, whole-ledger: the same law inside and past the window.
    let in_window = db
        .reconstruct_as_of(macrame::temporal::AsOf::bitemporal(T5, now.clone()))
        .await
        .unwrap();
    assert_eq!(
        in_window.concepts.get("x").expect("x in window").title,
        OLD,
        "the creating lineage's row covers the instant the fold asks about"
    );
    let past_window = db
        .reconstruct_as_of(macrame::temporal::AsOf::bitemporal(AFTER, now.clone()))
        .await
        .unwrap();
    assert_eq!(
        past_window.concepts.get("x").expect("x past window").title,
        OLD,
        "past the window the fold still answers the mint: the overlay's cut is the branch's own decision"
    );

    // The per-lineage fold: the walk answers the window on its own time.
    let on_b0_in = db.reconstruct_on(&now, "b0").await.unwrap();
    assert_eq!(
        on_b0_in.concepts.get("x"),
        None,
        "on the branch's own line the window has ended and the cut covers (the far end)"
    );
    // And intra-window on the walked read (the valid-axis one is
    // `concepts_by_id`'s story above).
    let plan = ReadPlan::new()
        .on(BranchId::new("b0").unwrap())
        .valid_at(T5);
    let rows = db.concepts_by_id(&["x".to_string()], plan).await.unwrap();
    assert_eq!(
        rows.into_iter().next().map(|c| c.title),
        Some(NEW.to_string()),
        "the branch's window shows on its own interior"
    );
}

// ---------------------------------------------------------------------------
// Where the overlay says nothing, today's law runs
// ---------------------------------------------------------------------------

/// **A forked database that has never versioned a concept reads exactly as it
/// did before the overlay existed.** The takeover route is what writes the
/// first version row; until something does, the branch's fold is D-310's: the
/// trunk's supersede after the fork is invisible, the branch carries the
/// trunk's pre-fork row, and none of the readers consult the overlay.
#[tokio::test]
async fn a_forked_store_before_any_version_row_still_reads_today_s_fold() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let b0 = BranchId::new("b0").unwrap();

    tick().await;
    // A trunk supersede after the fork: invisible to the branch by today's
    // fold, visible as its own on the trunk.
    db.upsert_concept(
        ConceptUpsert::new("x", "the trunk's later text")
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concept_versions").await,
        0
    );
    assert_eq!(
        text_at(&db, b0.clone(), None, None).await.as_deref(),
        Some(OLD)
    );
    let now = stamp_of(&db, "SELECT MAX(recorded_at) FROM transaction_log").await;
    let currents = traversal_texts(&db, b0.clone(), None, &now).await;
    assert!(currents.iter().any(|t| t == OLD), "{currents:?}");
}

/// **A never-forked database runs every content reader unchanged** (§12
/// story 13, the byte-identical gate). A trunk supersede has always been the
/// projection channel's own move: one row per id, replaced in place.
#[tokio::test]
async fn a_never_forked_database_reads_every_current_surface_unchanged() {
    let harness = TestHarness::new();
    let db = std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("x", OLD)
            .valid_from(TS)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("x", "the trunk's superseded text")
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let rows = db
        .concepts_by_id(&["x".to_string()], ReadPlan::new())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "the trunk's superseded text");
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'x'").await,
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concept_versions").await,
        0
    );
}

// ---------------------------------------------------------------------------
// P-2 content row 8 — the traversal's vector-filter candidates
// ---------------------------------------------------------------------------

/// A vector-index fixture: the trunk mints a four-node corpus reachable from
/// `a`, embeds it, *then* forks `b0`, whose takeover of `c02` writes the
/// branch's own rows. Everything embedded is minted before the fork, so each
/// lineage's caps see the corpus.
async fn vector_fixture(harness: &TestHarness) -> (std::sync::Arc<Database>, BranchId) {
    async fn embedding(rank: usize) -> Vec<f32> {
        let theta = (rank as f32) * std::f32::consts::PI / 4.0;
        vec![theta.cos(), theta.sin()]
    }
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.write_concepts(vec![
        ConceptUpsert::new("a", "Hub")
            .valid_from(TS)
            .valid_to(FOREVER),
        ConceptUpsert::new("c00", "N")
            .valid_from(TS)
            .valid_to(FOREVER),
        ConceptUpsert::new("c01", "N")
            .valid_from(TS)
            .valid_to(FOREVER),
        ConceptUpsert::new("c02", "N")
            .valid_from(TS)
            .valid_to(FOREVER),
    ])
    .await
    .unwrap();
    db.write_bulk_atomic(vec![
        EdgeAssertion::new("a", "c00", "LINKS")
            .valid_from(TS)
            .valid_to(FOREVER),
        EdgeAssertion::new("a", "c01", "LINKS")
            .valid_from(TS)
            .valid_to(FOREVER),
        EdgeAssertion::new("a", "c02", "LINKS")
            .valid_from(TS)
            .valid_to(FOREVER),
    ])
    .await
    .unwrap();
    let m = ModelName::new("r_v1").unwrap();
    db.register_model(&m, 2).await.unwrap();
    db.upsert_embeddings(
        &m,
        vec![
            ("c00".to_string(), embedding(2).await),
            ("c01".to_string(), embedding(1).await),
            ("c02".to_string(), embedding(0).await),
        ],
    )
    .await
    .unwrap();
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    (db, BranchId::new("b0").unwrap())
}

/// The branch's traversal-candidate vector search: the walk's own rows
/// answer over the mint's late retirement — the candidate set culls exactly
/// like the content readers — AND, since M6-c (the amendment's §5.4 cost
/// 3b), a version-resolved id is scored by **its own pair's vector**, not
/// the mint's: a version whose text changed has a different meaning, and
/// ranking it by the trunk's embedding is the wrong-text defect the cost
/// row names. So the story's two halves:
///
/// - the pair with no vector of its own is **not a vector result** (the
///   trunk's row is dropped because the id resolves elsewhere — the same
///   absence Doctrine VII gives any concept whose embedding has not been
///   derived yet);
/// - once the caller embeds the version (Doctrine VII's caller-driven
///   shape), the pair is a result, scored by that vector.
#[tokio::test]
async fn a_traversal_vector_filter_keeps_what_the_branch_resolves() {
    let harness = TestHarness::new();
    let (db, b0) = vector_fixture(&harness).await;
    let view = db.view(b0.clone());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("c02", "the branch's own")
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    // The trunk retires its own row after the fork: entirely legitimate on
    // the trunk, invisible to the branch. D-312's explicit law, the same one
    // the branch's act runs — the retire is the superseding row here, and the
    // close underneath is a log row, not a projection edit.
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("c02", "N")
            .supersede(true)
            .retired(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let m = ModelName::new("r_v1").unwrap();
    let search = FilteredVectorSearch::new(
        m.clone(),
        vec![1.0, 0.0],
        TraversalBuilder::new("a")
            .max_depth(1)
            .on_branch(b0.clone()),
    )
    .top_k(3)
    .execute(db.read_conn(), TS)
    .await
    .unwrap();
    let ids: Vec<String> = search.iter().map(|r| r.concept_id.clone()).collect();
    assert!(
        !ids.contains(&"c02".to_string()),
        "a version-resolved id without a version vector is not a vector result: {ids:?}"
    );

    // Embed the version. The pair is now a result — and because the query
    // direction is the version's own vector, it ranks first.
    db.upsert_version_embeddings(
        &m,
        vec![(
            "c02".to_string(),
            "b0".to_string(),
            T5.to_string(),
            vec![1.0, 0.0],
        )],
    )
    .await
    .unwrap();
    let search = FilteredVectorSearch::new(
        m,
        vec![1.0, 0.0],
        TraversalBuilder::new("a").max_depth(1).on_branch(b0),
    )
    .top_k(3)
    .execute(db.read_conn(), TS)
    .await
    .unwrap();
    let first = &search[0];
    assert_eq!(
        first.concept_id, "c02",
        "the branch's own c02, scored by its own vector, ranks first"
    );
    // The trunk's late retirement never hid a route: the candidate set
    // resolved through the walk, which is why c02 is scored at all.
    let ids: Vec<String> = search.iter().map(|r| r.concept_id.clone()).collect();
    assert!(
        ids.contains(&"c00".to_string()) || ids.contains(&"c01".to_string()),
        "the rest of the branch's candidates still answer: {ids:?}"
    );
}

/// …and the filter drops `c02` when the branch's own act is a cut: the
/// trunk's row says alive to anyone whose fork preceded the act, but the
/// branch's overlay says absent from `T5`, and the reader law is the
/// branch's.
#[tokio::test]
async fn a_traversal_vector_filter_drops_what_the_branch_s_cut_resolves_as_absent() {
    let harness = TestHarness::new();
    let (db, b0) = vector_fixture(&harness).await;
    let view = db.view(b0.clone());

    tick().await;
    view.upsert_concept(ConceptUpsert::new("c02", "N").retired(true).valid_from(T5))
        .await
        .unwrap();

    let m = ModelName::new("r_v1").unwrap();
    let search = FilteredVectorSearch::new(
        m,
        vec![1.0, 0.0],
        TraversalBuilder::new("a").max_depth(1).on_branch(b0),
    )
    .top_k(3)
    .execute(db.read_conn(), TS)
    .await
    .unwrap();
    let ids: Vec<String> = search.iter().map(|r| r.concept_id.clone()).collect();
    assert!(
        !ids.contains(&"c02".to_string()),
        "the branch's cut decides: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// The scan's per-slot law: extra at 5, the embedding model at 6 (the final
// SELECT's column order). While the scan read each from the other's slot,
// every overlay-resolved row carried the empty extra default — which the
// trunk/writer's resolution (the reader's own accept path) compared and
// believed, churning every bulk's unchanged filter and refusing same-start
// writes as AlreadyRecorded. Pinned public, because every resolution read
// shares the walk.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_resolution_row_reads_its_extra_and_its_embedding_model_from_their_own_slots() {
    let harness = TestHarness::new();
    let db = forked_view(&harness, None).await;
    let b0 = BranchId::new("b0").unwrap();
    let view = db.view(b0.clone());

    tick().await;
    let layer_extra = r#"{"coderadar":{"content_hash":"a-hash"}}"#;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .extra(layer_extra)
            .embedding_model("the-model")
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let resolved = db
        .concepts_by_id(
            &["x".to_owned()],
            ReadPlan::new().on(b0.clone()).valid_at(AFTER.to_string()),
        )
        .await
        .unwrap();
    let v = resolved.first().expect("the branch's version resolves");
    assert_eq!(
        v.extra.as_str(),
        layer_extra,
        "the extra reads the extra column, not the model's slot"
    );
    assert_eq!(
        v.embedding_model.as_deref(),
        Some("the-model"),
        "the model reads the model column, not the extra's slot"
    );
}
