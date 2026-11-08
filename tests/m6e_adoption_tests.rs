//! M6-e: D-315's adoption — W3's widening (0.22.0, [Amendment 2]
//! (../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md) §6,
//! the review's Q3 acceptance with P-7 as the gate).
//!
//! The rule, word for word from the amendment: *a lineage may write a version
//! of an id whose mint it cannot see, as an ordinary first version in
//! `concept_versions` on its own lineage, at its own stamps. Nothing is
//! copied from the minting lineage. Nothing on any lineage is closed,
//! superseded or cut.* The encyclopedia of the act, and the shapes that stand
//! beside it:
//!
//! - the vista (P-7, accepted gated): the mint is on the trunk, recorded
//!   after the writer's fork cutoff — the M6-a refusal with `CrossLineage`
//!   widens into the adoption act ([D-315]);
//! - the mint on a sibling lineage the writer's path never meets — the same
//!   widening (the review's F3: "R2 becomes its own decision, D-315
//!   adoption");
//! - the root adopting a branch-born id (the trunk side of the same story);
//! - a further write on an adopted id routes down the **standard overlay
//!   arms** — the route reads the writer's own version rows first, so the
//!   second write is a W2 takeover of a lineage's own belief, cut-free where
//!   the close suffices (D-312's own law);
//! - the refusal that keeps the rule honest: a fresh adoption cannot also be
//!   a retirement — nothing is cut, and the overlay's retirement is a cut or
//!   a close, both of which name a belief that was visible ([`DbError::
//!   AdoptedRetirement`]);
//! - the refusal that stays: a write that reaches `concepts` directly — an
//!   outside writer (D-074's class) — still hits `trg_concepts_cross_lineage`,
//!   because adoption routes at the writer, not the row (§13 Q4).

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;
use macrame::WriteBatch;

const T0: &str = "2026-01-01T00:00:00.000000Z";
/// vista's fork instant: `late` is minted after this on the trunk.
const F0: &str = "2026-01-05T00:00:00.000000Z";
/// The trunk's late mint's window start (recorded after `F0`).
const T_LATE: &str = "2026-02-01T00:00:00.000000Z";
/// The adopted first belief's window start.
const T4: &str = "2026-03-01T00:00:00.000000Z";
/// The second belief's window start (the adopted id's own takeover).
const T6: &str = "2026-04-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

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

/// The vista store: the trunk mints `early` at `T0`, forks `vista` at `F0`,
/// then mints `late` at `T_LATE` — recorded after the fork cutoff, so vista's
/// resolve of `late` is: nothing was visible, nothing is visible ([D-314]'s
/// law), and only D-315's adoption names the first belief the writer asserts.
async fn vista_store(harness: &TestHarness) -> std::sync::Arc<Database> {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("early", "the trunk's early belief")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.fork_at(BranchId::new("vista").unwrap(), BranchId::main(), F0)
        .await
        .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk's late mint")
            .valid_from(T_LATE)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db
}

/// A fresh adoption: the vista adopts the trunk's late-minted id, as
/// [D-315]'s rule names the act — one version row on vista's own lineage, at
/// vista's own stamps; nothing is copied from the minting lineage; nothing
/// anywhere is closed, superseded or cut; the mint's provenance stays the
/// mint's. The reads resolve: vista's walk never sees the foreign row, so
/// pre-adoption instants answer absent and the adopted window answers the
/// writer's own belief.
#[tokio::test]
async fn a_fork_adopts_an_id_minted_after_its_fork() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    let log_before = scalar(&db, "SELECT COUNT(*) FROM transaction_log").await;
    let main_log_before = scalar(
        &db,
        "SELECT COUNT(*) FROM transaction_log WHERE branch_id = 'main'",
    )
    .await;

    let view = db.view(BranchId::new("vista").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .content("asserted from the merge tree, not copied")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("D-315: the vista adopts the id whose mint it cannot see");

    // One version row on the writer's own lineage, the asserted window.
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' \
                   AND valid_from = '{T4}' AND valid_to = '{FOREVER}'"
            )
        )
        .await,
        1,
        "the adoption is an ordinary first version in concept_versions"
    );
    // ... and nothing is cut: no cut row anywhere for the pair.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0,
        "nothing is cut: the adoption's act is one row"
    );
    // Nothing on any lineage is closed: the mint's rows are the log's, the
    // vista's windows are vista's own; the act's log delta is exactly the one
    // version row, on the vista side.
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM transaction_log").await,
        log_before + 1,
        "the act carried exactly one log row"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM transaction_log WHERE branch_id = 'main'"
        )
        .await,
        main_log_before,
        "the minting lineage wrote nothing"
    );
    // Nothing is copied: the version row's text is the writer's own.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'late' AND branch_id = 'vista' \
               AND title LIKE '%adopted%'"
        )
        .await,
        1
    );
    // The provenance stays the mint's ([D-214]'s one row per id).
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concepts WHERE id = 'late' AND branch_id = 'main'"
        )
        .await,
        1,
        "the id's provenance row still names the minting lineage"
    );
}

/// The resolution law over the adopted id, on both sides of the act: the
/// writer's walk answers only its own row — before the adopted window's start
/// it answers **absent** (the adopter had no belief), at current belief it
/// answers the writer's belief (never falling through to a mint row whose
/// recording is past the fork cutoff); the minting lineage's own reads are
/// untouched by the adoption.
#[tokio::test]
async fn the_adopted_id_s_reads_resolve_by_the_writer_s_walk_alone() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    let view = db.view(BranchId::new("vista").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    // Current belief on vista: the adopted row. The window's whole span,
    // field-complete.
    for v in ["2026-03-01T00:00:00.000000Z", T6] {
        let rows = db
            .concepts_by_id(
                &["late".to_string()],
                ReadPlan::new()
                    .on(BranchId::new("vista").unwrap())
                    .valid_at(v.to_string()),
            )
            .await
            .unwrap();
        let row = rows
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("vista answers its own belief at {v}"));
        assert!(
            row.title.contains("adopted"),
            "vista answers its own belief at {v}: {:?}",
            row.title
        );
        assert_eq!(
            row.valid_from.as_deref(),
            Some(T4),
            "the version's own start at {v}"
        );
        assert_eq!(row.valid_to.as_deref(), Some(FOREVER));
    }

    // Before the adopted window's start: absent (before the adopting stamp
    // they find nothing — the vista walk's law).
    let answer = db
        .concepts_by_id(
            &["late".to_string()],
            ReadPlan::new()
                .on(BranchId::new("vista").unwrap())
                .valid_at(T_LATE.to_string()),
        )
        .await
        .unwrap();
    assert!(
        answer.is_empty(),
        "pre-adoption valid instants answer absent on the writer: {answer:?}"
    );

    // The minting lineage's reads are untouched: the trunk reads its own mint
    // row at its own window.
    let answer = db
        .concepts_by_id(
            &["late".to_string()],
            ReadPlan::new()
                .on(BranchId::main())
                .valid_at(T_LATE.to_string()),
        )
        .await
        .unwrap();
    let row = answer
        .into_iter()
        .next()
        .expect("the trunk's own mint answers on the trunk");
    assert!(
        row.title.contains("late mint"),
        "the mint stays what it was: {:?}",
        row.title
    );
}

/// The review's F3: the mint on a sibling lineage — off the writer's path —
/// adopts the same way. And the root adopting a branch-born id ([D-315]'s
/// "R2" — the trunk asserts a branch-born id after the merge) is the same
/// act with the root as writer.
#[tokio::test]
async fn a_sibling_s_mint_adopts_off_the_path_just_the_same() {
    let harness = TestHarness::new();
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());

    db.upsert_concept(
        ConceptUpsert::new("early", "the trunk's early belief")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.fork_at(BranchId::new("b0").unwrap(), BranchId::main(), F0)
        .await
        .unwrap();
    tick().await;
    db.view(BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("sibborn", "a sibling's mint")
                .valid_from(T_LATE)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();

    db.fork_at(BranchId::new("other").unwrap(), BranchId::main(), F0)
        .await
        .unwrap();
    tick().await;
    db.view(BranchId::new("other").unwrap())
        .upsert_concept(
            ConceptUpsert::new("sibborn", "asserted elsewhere")
                .supersede(true)
                .valid_from(T4)
                .valid_to(FOREVER),
        )
        .await
        .expect("D-315: a branch adopts a sibling's mint");
    let row = scalar(
        &db,
        &format!(
            "SELECT COUNT(*) FROM concept_versions \
             WHERE id = 'sibborn' AND branch_id = 'other' AND valid_from = '{T4}'",
        ),
    )
    .await;
    assert_eq!(row, 1, "the sibling's adoption wrote its own first version");
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'sibborn' AND branch_id = 'other'"
        )
        .await,
        0
    );

    // The root side: b0 forks; b0 mints a branch-born id; the trunk asserts
    // its own belief of that id — the R2 story — as an adoption whose row
    // lives in concept_versions on (id, main).
    db.view(BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("branchborn", "the branch's own belief")
                .valid_from(T_LATE)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("branchborn", "the trunk asserts the branch-born id")
            .supersede(true)
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("D-315: the trunk adopts a branch-born id");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'branchborn' AND branch_id = 'main' AND valid_from = '{T4}'",
            )
        )
        .await,
        1,
        "the root's adoption is an overlay row on the trunk's own lineage"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'branchborn' AND branch_id = 'main'"
        )
        .await,
        0,
        "nothing is closed or cut on the branch either"
    );

    // And both sides read what each wrote: the root's walk answers its own
    // overlay row; the branch's walk answers its own mint.
    let answer = db
        .concepts_by_id(
            &["branchborn".to_string()],
            ReadPlan::new()
                .on(BranchId::main())
                .valid_at(T6.to_string()),
        )
        .await
        .unwrap();
    let row = answer
        .into_iter()
        .next()
        .expect("the trunk answers its adoption");
    assert!(
        row.title.contains("the trunk asserts"),
        "the trunk's belief is its own: {:?}",
        row.title
    );
    let answer = db
        .concepts_by_id(
            &["branchborn".to_string()],
            ReadPlan::new()
                .on(BranchId::new("b0").unwrap())
                .valid_at(T6.to_string()),
        )
        .await
        .unwrap();
    let row = answer
        .into_iter()
        .next()
        .expect("the branch keeps its own belief");
    assert!(
        row.title.contains("the branch's own"),
        "the branch's belief is its own: {:?}",
        row.title
    );
}

/// A further write on an adopted id: the route reads the writer's own version
/// rows first, so the second write is the standard overlay machinery — the
/// takeover of a lineage's own belief (`D-312`'s close under its own stamp,
/// no cut), the next-first plain write refused by the single-open law. The
/// adopted id's first write and its supersede land in the same machinery as
/// any versioned id.
#[tokio::test]
async fn a_further_write_on_an_adopted_id_walks_the_standard_overlay_arms() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    let view = db.view(BranchId::new("vista").unwrap());
    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    // The plain refusal, the W2 shape on the writer's own row.
    let err = view
        .upsert_concept(
            ConceptUpsert::new("late", "the single open refuses it")
                .valid_from(T4)
                .valid_to(FOREVER),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::ConceptSingleOpen { .. }),
        "the adopted row is the writer's own belief: {err:?}"
    );

    // The supersede: D-312's own-row law — the close under the same stamp, no
    // cut (the belief came from the writer itself).
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("late", "vista's later belief")
            .supersede(true)
            .valid_from(T6)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();

    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' AND valid_from = '{T6}'"
            )
        )
        .await,
        1,
        "the second belief's own row"
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' \
                   AND valid_from = '{T4}' AND valid_to = '{T6}'"
            )
        )
        .await,
        1,
        "the close that pivoted the adopted row stayed on its own pair"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0,
        "the takeover of a lineage's own belief is a close, never a cut"
    );

    // And the reads: the first window answers the first belief, the second
    // the second.
    let answer = db
        .concepts_by_id(
            &["late".to_string()],
            ReadPlan::new()
                .on(BranchId::new("vista").unwrap())
                .valid_at("2026-03-15T00:00:00.000000Z".to_string()),
        )
        .await
        .unwrap();
    let row = answer.into_iter().next().unwrap();
    assert!(
        row.title.contains("adopted"),
        "the first window's belief: {:?}",
        row.title
    );
    let answer = db
        .concepts_by_id(
            &["late".to_string()],
            ReadPlan::new()
                .on(BranchId::new("vista").unwrap())
                .valid_at(T6.to_string()),
        )
        .await
        .unwrap();
    let row = answer.into_iter().next().unwrap();
    assert!(
        row.title.contains("later"),
        "the second window answers the later belief: {:?}",
        row.title
    );
}

/// The one refusal D-315 adds by its own rule: a *fresh* adoption cannot also
/// be a retirement. Nothing is cut (the rule), yet the overlay's retirement
/// is a cut or a close — both name a belief that was visible, and none is.
/// [`DbError::AdoptedRetirement`] says the caller is asking the adoption act
/// to do a later act's work: adopt first, retire on the adopted id's own
/// machinery after.
#[tokio::test]
async fn a_fresh_adoption_cannot_also_be_a_retirement() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    let view = db.view(BranchId::new("vista").unwrap());
    let err = view
        .upsert_concept(
            ConceptUpsert::new("late", "the caller says dead")
                .retired(true)
                .valid_from(T4)
                .valid_to(FOREVER),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, macrame::DbError::AdoptedRetirement { .. }),
        "the fresh adoption's retirement is its own refusal: {err:?}"
    );
    // And nothing was written by the refusal.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0
    );
}

/// A batch carries the adoption the same way (the concept phase's Foreign arm
/// is the same act), and an in-batch edge to the adopted id follows
/// [`D-308`]'s concepts-before-edges order — P-7's own answer: there is no
/// endpoint-visibility check to out-manoeuvre, so the batch needs no new
/// ordering machinery.
#[tokio::test]
async fn a_batched_adoption_rides_the_concept_phase_and_edges_follow() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    let report = db
        .write_batch(
            WriteBatch::new()
                .concept(
                    ConceptUpsert::new("late", "vista's adopted belief")
                        .valid_from(T4)
                        .valid_to(FOREVER)
                        .on_branch(BranchId::new("vista").unwrap()),
                )
                .edge(
                    EdgeAssertion::new("early", "late", "LINKS")
                        .on_branch(BranchId::new("vista").unwrap())
                        .valid_from(T4)
                        .valid_to(FOREVER),
                ),
        )
        .await
        .unwrap();
    assert!(
        report.concepts == 1 && report.edges == 1,
        "the batch carried one adoption and its edge: {report:?}"
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' AND valid_from = '{T4}'"
            )
        )
        .await,
        1,
        "the batched adoption is the same one row"
    );
}

/// The refusal that stays ([§13 Q4]): a write that reaches `concepts`
/// directly — an outside writer — still hits the trigger's cross-lineage
/// refusal, because D-315 routes at the writer and never at the row.
#[tokio::test]
async fn the_trigger_s_cross_lineage_refusal_survives_d_315() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;

    // vista adopted the id above the ledger this test checks; the trigger's
    // law is about direct rows, so the dot from an outside writer refuses on
    // any minted id (the pair exists in `concepts` under another branch).
    db.view(BranchId::new("vista").unwrap())
        .upsert_concept(
            ConceptUpsert::new("late", "vista's adopted belief")
                .valid_from(T4)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();

    libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .expect("a direct connection to the store's file")
        .execute(
            "INSERT INTO concepts (id, branch_id, valid_from, valid_to, title, content, extra, embedding_model, recorded_at, retired) \
             VALUES ('late', 'vista', '2026-05-01T00:00:00.000000Z', '2026-06-01T00:00:00.000000Z', 'sneaky', '', '{}', NULL, '2026-05-01T00:00:00.000000Z', 0)",
            (),
        )
        .await
        .expect_err("the trigger refuses rows for a minted id under another branch");
}
