//! A concept can outlive its lineage, and the refusal says so (0.15.11, W15.1,
//! review C-3, [D-230], [D-253]).
//!
//! Two arms built two waves apart meet here. [`Database::archive_branch`] takes
//! a lineage's `branches` row into the cold file — that row is what makes a hot
//! fold which omits the lineage *correct rather than short*, which is the whole
//! of D-230. [`Database::rehydrate`] moves a concept back, carrying the
//! `branch_id` it was minted on, and `concepts.branch_id` references
//! `branches(branch_id)` with foreign keys on.
//!
//! So the two are in contradiction for exactly one input: a concept minted on a
//! lineage that has since been forgotten. Measured before this release, that
//! input produced
//!
//! ```text
//! kind    = Engine
//! display = engine: SQLite failure: `FOREIGN KEY constraint failed`
//! ```
//!
//! which names neither the concept the caller asked for, nor the lineage that
//! is missing, nor what to do about it — and attributes the refusal to
//! `concepts`, the table being written, rather than to `branches`, the table
//! with the missing row.
//!
//! **These tests are about the refusal being usable, not about it existing.**
//! An assertion that a rehydrate fails would have passed before the release.
//! Each one below pins something the foreign key could not say: which concept,
//! which lineage, that the kind is `Branch` and catchable as one, that a batch
//! leaves nothing written, that an intact lineage is unaffected, and that the
//! remedy the message names actually works.
//!
//! [D-230]: ../docs/architecture/s13-decision-register.md#d-230
//! [D-253]: ../docs/architecture/s13-decision-register.md#d-253

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::error::{DbError, ErrorKind};
use macrame::{BranchId, ConceptUpsert, Database};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-02-01T00:00:00.000000Z";
/// In the future, for [`concept_archive_tests`]'s reason: `recorded_at` is
/// crate-stamped, so a past cutoff archives nothing whatever the valid-time
/// columns say, and every assertion here would hold over the empty set.
const CUTOFF: &str = "2099-01-01T00:00:00.000000Z";

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// A retired, expired, unreferenced concept — the concept arm's only archivable
/// shape — on whichever lineage is named.
fn archivable(concept: &str, branch: Option<&str>) -> ConceptUpsert {
    let u = ConceptUpsert::new(concept, "Title")
        .content(format!("body of {concept}"))
        .valid_from(T0)
        .valid_to(T1)
        .retired(true);
    match branch {
        Some(b) => u.on_branch(id(b)),
        None => u,
    }
}

/// `t` on the trunk and `d` on `alt`, both cold, and `alt` forgotten.
///
/// The two concepts reach the cold file by **different arms** on purpose: `t`
/// through [`Database::archive`], whose lineage is still registered, and `d`
/// through [`Database::archive_branch`], whose lineage is not. That is what
/// makes `t` a control rather than a second copy of `d` — the difference
/// between them is the lineage and nothing else.
async fn forgotten(h: &TestHarness) -> Database {
    let db = Database::open(&h.db_path).await.unwrap();
    db.upsert_concept(archivable("t", None)).await.unwrap();
    db.fork(id("alt"), BranchId::main()).await.unwrap();
    // Live, open and unretired, so the concept arm below will not take it: `d`
    // must reach the cold file through the *branch* arm or the two halves of
    // this fixture are the same half twice.
    db.upsert_concept(
        ConceptUpsert::new("d", "Title")
            .content("body of d")
            .valid_from(T0)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(
        report.concepts_archived, 1,
        "the trunk concept must reach the cold file through the concept arm, \
         or the control below proves nothing"
    );
    db.archive_branch(id("alt")).await.unwrap();
    db
}

/// Is the concept in the hot table?
async fn hot(db: &Database, concept: &str) -> bool {
    let n: i64 = db
        .read_conn()
        .query(
            "SELECT COUNT(*) FROM concepts WHERE id = ?1",
            libsql::params![concept],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    n == 1
}

// ---------------------------------------------------------------------------
// what the refusal says
// ---------------------------------------------------------------------------

/// The whole point: both names, in a variant a caller can match on.
#[tokio::test]
async fn a_rehydrate_of_a_forgotten_lineage_names_the_concept_and_the_lineage() {
    let h = TestHarness::new();
    let db = forgotten(&h).await;

    let err = db.rehydrate(&["d"]).await.unwrap_err();

    match &err {
        DbError::BranchArchived { branch, concept } => {
            assert_eq!(branch, "alt");
            assert_eq!(concept, "d");
        }
        other => panic!("expected BranchArchived, got {other:?}"),
    }

    let msg = err.to_string();
    for needle in ["d", "alt"] {
        assert!(
            msg.contains(needle),
            "the message must name {needle}, which the foreign key's did not: {msg}"
        );
    }
}

/// The kind, which is the half a caller filters on rather than reads.
///
/// `Engine` is what this was, and it is the kind a caller treats as "the
/// database is unwell" — retried, alerted on, escalated. This refusal is none
/// of those: the ledger is exactly as healthy as it was a moment ago and the
/// caller asked for something it cannot have. Asserted against `Engine` by name
/// rather than only for `Branch`, because the regression worth catching is the
/// return of the old classification, not the absence of the new one.
#[tokio::test]
async fn the_refusal_is_a_branch_error_and_not_an_engine_fault() {
    let h = TestHarness::new();
    let db = forgotten(&h).await;

    let err = db.rehydrate(&["d"]).await.unwrap_err();

    assert_eq!(err.kind(), ErrorKind::Branch);
    assert_ne!(
        err.kind(),
        ErrorKind::Engine,
        "a caller filtering engine faults must not see a lineage it forgot on \
         purpose: {err}"
    );
}

// ---------------------------------------------------------------------------
// what it does not do
// ---------------------------------------------------------------------------

/// A cold concept whose lineage is intact is untouched by any of this.
///
/// The over-refusal guard. A check written against "the concept is cold" rather
/// than "its lineage is gone" passes every test above and fails this one.
#[tokio::test]
async fn a_cold_concept_on_a_living_lineage_still_rehydrates() {
    let h = TestHarness::new();
    let db = forgotten(&h).await;

    let report = db.rehydrate(&["t"]).await.unwrap();

    assert_eq!(report.concepts_rehydrated, 1);
    assert!(hot(&db, "t").await);
}

/// The refusal takes the batch with it, and leaves nothing behind.
///
/// `t` is rehydratable and is asked for **first**, so a version of this that
/// refused per-id and carried on would have moved it. The transaction is what
/// makes that untrue, and this is the test that would notice its removal.
#[tokio::test]
async fn a_refused_batch_writes_nothing_at_all() {
    let h = TestHarness::new();
    let db = forgotten(&h).await;

    let err = db.rehydrate(&["t", "d"]).await.unwrap_err();
    assert!(matches!(err, DbError::BranchArchived { .. }), "{err:?}");

    assert!(
        !hot(&db, "t").await,
        "the id ahead of the refusal was written and kept, so the refusal is \
         not the all-or-nothing the transaction promises"
    );
}

/// The concept named is the first one the *caller* asked about.
///
/// Two forgotten lineages, asked for in both orders. Without this, an
/// implementation free to name whichever row the engine reached first would
/// pass everything above while reporting a different concept on each run — and
/// the message would be describing the ledger's iteration order rather than the
/// caller's request.
#[tokio::test]
async fn the_refusal_names_the_first_id_the_caller_asked_for() {
    let h = TestHarness::new();
    let db = Database::open(&h.db_path).await.unwrap();

    for lineage in ["one", "two"] {
        db.fork(id(lineage), BranchId::main()).await.unwrap();
    }
    db.upsert_concept(archivable("d1", Some("one")))
        .await
        .unwrap();
    db.upsert_concept(archivable("d2", Some("two")))
        .await
        .unwrap();
    db.archive_branch(id("one")).await.unwrap();
    db.archive_branch(id("two")).await.unwrap();

    for (ids, expected) in [(["d1", "d2"], "d1"), (["d2", "d1"], "d2")] {
        match db.rehydrate(&ids).await.unwrap_err() {
            DbError::BranchArchived { concept, .. } => assert_eq!(
                concept, expected,
                "asked for {ids:?}, so the refusal must name {expected}"
            ),
            other => panic!("expected BranchArchived, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// the remedy the message names
// ---------------------------------------------------------------------------

/// Re-register the lineage, ask again, and the concept comes home.
///
/// This is the test that makes the message honest. "Re-register the lineage
/// before rehydrating the concept" is advice, and advice in an error message is
/// a claim about the crate that nothing else here checks: every assertion above
/// would hold just as well if the refusal were a dead end.
///
/// It also shows what the refusal is protecting. `fork` is a deliberate act
/// with a stated parent and fork point — the lineage comes back on terms the
/// caller chose. Reinstating the `branches` row inside the rehydrate would have
/// produced the same successful move with nobody having decided anything, which
/// is what [D-230] means by an archived lineage being forgotten.
#[tokio::test]
async fn re_registering_the_lineage_makes_the_rehydrate_succeed() {
    let h = TestHarness::new();
    let db = forgotten(&h).await;

    db.rehydrate(&["d"]).await.unwrap_err();

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    let report = db.rehydrate(&["d"]).await.unwrap();

    assert_eq!(report.concepts_rehydrated, 1);
    assert!(hot(&db, "d").await);

    let branch: String = db
        .read_conn()
        .query(
            "SELECT branch_id FROM concepts WHERE id = ?1",
            libsql::params!["d"],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        branch, "alt",
        "the concept must come back on the lineage it was minted on, not on \
         the trunk: a rehydrate that quietly re-homed it would satisfy the \
         foreign key by changing the row's meaning"
    );
}
