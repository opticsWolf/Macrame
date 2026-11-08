//! M6-a's write-actor routing tests (0.22.0, [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.2, revision 3).
//!
//! W1 is yesterday — the id minted here writes exactly as it always did.
//! These tests are about what changed: an **inherited** id (W2) writes to the
//! overlay, supersede writes its cut and its version at one stamp, plain
//! writes never supersede by accident, `retired` on an inherited id retires
//! the inheritance (a cut, not a row that mutates a distant projection), and
//! **W3** stays [`DbError::CrossLineage`]: the mint off the writer's path,
//! and the mint on the path but recorded after the writer's fork cutoff
//! (P-7's vista), are both refusals.
//!
//! Every assertion is public-API level (a `BranchView` write is the only way
//! in), because W2 is an answer about *what the write actor decided*, not
//! about the SQL it drove. The overlay's row-level truths belong to the rung
//! file (`m6a_rung_tests.rs`); the resolution law's belongs to P-8
//! (`m6_resolution_tests.rs`).

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::{BranchId, WriteBatch};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T4: &str = "2026-01-10T00:00:00.000000Z";
const T5: &str = "2026-01-11T00:00:00.000000Z";
const T6: &str = "2026-01-12T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// The trunk holds `{id}` (open from `T0`) before anything forks, and
/// `b0` forks after the mint. Tests write on views.
async fn forked_store(harness: &TestHarness, id: &str) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new(id, "the trunk's belief")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    db
}

/// Every write waits a tick, so each act carries its own stamp (the same
/// reason `concept_state_rule_tests` sleeps).
async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

/// A scalar read against the store's read connection.
async fn scalar(db: &Database, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    v
}

/// Assert the takeover's two rows landed: one version row and one cut row,
/// both at the act's single stamp, the cut starting where the version does.
async fn assert_takeover(db: &Database, at: &str, branch: &str) {
    let versions = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'c' AND branch_id = '{branch}' AND valid_from = '{at}'"
        ),
    )
    .await;
    assert_eq!(versions, 1, "the superseding version row is in the overlay");

    let cuts = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM concept_cuts \
             WHERE id = 'c' AND branch_id = '{branch}' AND cut_at = '{at}'"
        ),
    )
    .await;
    assert_eq!(cuts, 1, "the cut is its own row at the same instant");

    let stamps = scalar(
        db,
        &format!(
            "SELECT COUNT(DISTINCT recorded_at) FROM \
             (SELECT recorded_at FROM concept_versions WHERE id = 'c' AND branch_id = '{branch}' \
              UNION ALL SELECT recorded_at FROM concept_cuts WHERE id = 'c' AND branch_id = '{branch}')"
        ),
    )
    .await;
    assert_eq!(
        stamps, 1,
        "the supersede's two rows carry the act's one stamp"
    );
}

// ---------------------------------------------------------------------------
// W2 supersede: the takeover writes two rows at one stamp
// ---------------------------------------------------------------------------

/// The scenario the resurrection fix is about: the branch supersedes the
/// trunk's open belief. The act lands a cut plus a version under one stamp;
/// the ancestor's projection row stays a distant lineage's row (I1); and the
/// ledger keeps one mirror row per overlay table.
#[tokio::test]
async fn an_inherited_supersede_writes_cut_and_version_at_one_stamp() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;
    let view = db.view(BranchId::new("b0").unwrap());
    let trunk_before = scalar(
        &db,
        "SELECT COUNT(*) FROM concepts WHERE id = 'c' AND branch_id = 'main'",
    )
    .await;
    let log_before = scalar(&db, "SELECT COUNT(*) FROM transaction_log").await;

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("c", "b0's belief")
            .supersede(true)
            .valid_from(T4),
    )
    .await
    .unwrap();

    let trunk_after = scalar(
        &db,
        "SELECT COUNT(*) FROM concepts WHERE id = 'c' AND branch_id = 'main'",
    )
    .await;
    assert_eq!(trunk_before, trunk_after, "no distant row moved");

    assert_takeover(&db, T4, "b0").await;

    // The ledger mirrors the act's two rows — one per overlay table. (The
    // act writes nothing else: the trunk's row is untouched, so the only
    // concept/overlay log rows after the act are these two.)
    let total = scalar(&db, "SELECT COUNT(*) FROM transaction_log").await;
    assert_eq!(
        total,
        log_before + 2,
        "one concept_versions and one concept_cuts log row added"
    );
    let logged = scalar(
        &db,
        "SELECT COUNT(*) FROM transaction_log \
         WHERE table_name IN ('concept_versions', 'concept_cuts') AND entity_id = 'c'",
    )
    .await;
    assert_eq!(logged, 2, "the overlay's two rows are the whole act");
}

/// The write goes through a batch too: D-308's concept phase routes, and the
/// batch's per-row dispatch does the same pairing the single write does.
#[tokio::test]
async fn a_batch_concept_supersede_routes_like_the_single_write() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;

    tick().await;
    let report = db
        .view(BranchId::new("b0").unwrap())
        .database()
        .write_batch(
            WriteBatch::new().concept(
                ConceptUpsert::new("c", "b0's belief")
                    .supersede(true)
                    .valid_from(T4)
                    .on_branch(BranchId::new("b0").unwrap()),
            ),
        )
        .await
        .unwrap();
    assert!(report.concepts == 1, "the batch carried the one concept");

    assert_takeover(&db, T4, "b0").await;
}

// ---------------------------------------------------------------------------
// W2 plain: never an implicit supersede
// ---------------------------------------------------------------------------

/// The plain write over an open inherited belief is refused with
/// [`DbError::ConceptSingleOpen`] — a lineage never gains an implicit
/// supersede its trunk does not have. Before M6-a this refused as
/// `CrossLineage` (the guard's sentence, not the decision's), which is
/// exactly the difference between "not your id" and "supersede it first".
#[tokio::test]
async fn a_plain_write_over_an_open_inherited_belief_is_refused() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    let err = view
        .upsert_concept(ConceptUpsert::new("c", "b0's belief").valid_from(T4))
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::ConceptSingleOpen { .. }),
        "the refusal is the single-open error, not cross-lineage or engine text: {err:?}"
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM concept_versions WHERE id = 'c'").await,
        0
    );
}

/// The trunk's era for `{id}` closes at `T5` (a supersede with a named end —
/// an ordinary single-lineage act that keeps a window there). A branch forks
/// after that and plain-writes its own era from `T4`: the effective belief at
/// `T4` is the trunk's *closed* row, so this is an ordinary later upsert's
/// shape — a version row and **no cut** — and the overlap with the trunk's
/// `[T0, T5)` is not an error, because D-291 is a within-lineage rule.
#[tokio::test]
async fn a_plain_write_over_a_closed_inherited_belief_writes_a_version_and_no_cut() {
    let harness = TestHarness::new();
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());

    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk's era")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk's era")
            .supersede(true)
            .valid_from(T5),
    )
    .await
    .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk's era finish")
            .valid_from(T5)
            .valid_to(T6),
    )
    .await
    .unwrap();

    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("late", "the branch continues")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'late' AND branch_id = 'b0' AND valid_from = '2026-01-10T00:00:00.000000Z'"
        )
        .await,
        1,
        "the branch's own era opened at T4"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'b0'"
        )
        .await,
        0,
        "a plain write decides nothing about the instants it does not touch"
    );
}

/// `retired` on an inherited id is the branch's retirement of the
/// inheritance: a cut row at the named instant, no version row. From there
/// on the id resolves absent on this lineage whatever its versions do — the
/// law P-8 pinned, arriving from the write side.
#[tokio::test]
async fn a_retired_flag_on_an_inherited_id_writes_the_cut() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("c", "not on my lineage")
            .retired(true)
            .valid_from(T4),
    )
    .await
    .unwrap();

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'c' AND branch_id = 'b0' AND cut_at = '2026-01-10T00:00:00.000000Z'"
        )
        .await,
        1,
        "the branch rejected the inheritance from T4 on"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'c' AND branch_id = 'b0'"
        )
        .await,
        0,
        "a retirement carries a decision, not a belief"
    );
}

// ---------------------------------------------------------------------------
// The second takeover on one lineage: rewrite the close, keep the cut count
// ---------------------------------------------------------------------------

/// After a takeover, the lineage's own version holds the id. Supersede again
/// at a later instant: the previous open version *closes* at the new start
/// (an in-place same-start-style rewrite of the window's end, logged by the
/// update mirror), the new row opens, and the cut count stays one — a cut is
/// written when the inheritance is taken over, not every act.
#[tokio::test]
async fn a_later_supersede_on_the_own_version_closes_it_and_does_not_re_cut() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;
    let view = db.view(BranchId::new("b0").unwrap());

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("c", "first era")
            .supersede(true)
            .valid_from(T4),
    )
    .await
    .unwrap();

    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("c", "second era")
            .supersede(true)
            .valid_from(T6),
    )
    .await
    .unwrap();

    // the previous open version was closed at the second start
    let closed = scalar(
        &db,
        &format!(
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'c' AND branch_id = 'b0' AND valid_from = '{T4}' \
             AND valid_to = '{T6}'"
        ),
    )
    .await;
    assert_eq!(
        closed, 1,
        "the first era's row now ends at the second's start"
    );

    // the new row is open
    let open = scalar(
        &db,
        &format!(
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'c' AND branch_id = 'b0' AND valid_from = '{T6}' AND valid_to = '{FOREVER}'"
        ),
    )
    .await;
    assert_eq!(open, 1, "the second era holds from T6");

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'c' AND branch_id = 'b0'"
        )
        .await,
        1,
        "the takeover cut is written once; a later supersede rewrites rows, not history"
    );

    // forward-only, the same law as on one lineage (D-312 + D-291's refusal)
    let err = view
        .upsert_concept(
            ConceptUpsert::new("c", "backwards")
                .supersede(true)
                .valid_from(T5),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::ConceptSingleOpen { .. }),
        "supersede only moves forward in valid time, on the overlay like on the \
         projection: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// W3: the refusals stay refusals
// ---------------------------------------------------------------------------

/// The mint on a lineage the writer cannot see at all: a sibling.
#[tokio::test]
async fn the_mint_on_a_sibling_lineage_is_refused() {
    let harness = TestHarness::new();
    let db = forked_store(&harness, "c").await;

    db.fork(BranchId::new("sib").unwrap(), BranchId::main())
        .await
        .unwrap();
    tick().await;
    db.view(BranchId::new("sib").unwrap())
        .upsert_concept(
            ConceptUpsert::new("sibborn", "a sibling's mint")
                .valid_from(T0)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();

    let view = db.view(BranchId::new("b0").unwrap());
    tick().await;
    let err = view
        .upsert_concept(
            ConceptUpsert::new("sibborn", "off the path")
                .supersede(true)
                .valid_from(T4),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::CrossLineage { .. }),
        "W3 stays the cross-lineage refusal: {err:?}"
    );
}

/// P-7's vista: the mint is on the trunk but recorded after the writer's fork
/// cutoff, so nothing of it was visible at the fork and nothing is visible
/// now. D-314 alone does not widen this; D-315 (adoption) is the decision
/// that does.
#[tokio::test]
async fn the_mint_after_the_writer_s_fork_is_refused() {
    let harness = TestHarness::new();
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());

    // vista forks before the trunk mints
    db.fork(BranchId::new("vista").unwrap(), BranchId::main())
        .await
        .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk mints after the fork")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    let view = db.view(BranchId::new("vista").unwrap());
    tick().await;
    let err = view
        .upsert_concept(
            ConceptUpsert::new("late", "not visible at the fork")
                .supersede(true)
                .valid_from(T4),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::CrossLineage { .. }),
        "a mint recorded after the fork cutoff is W3: {err:?}"
    );
}
