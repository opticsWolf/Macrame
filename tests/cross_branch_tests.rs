//! §17 item 4, and the shape it asks about (0.14.21, W12.21, §15.4, §16).
//!
//! The criterion reads *"cross-branch edges are refused with a named error,
//! with a test"*, and §16 records at 0.14.18 why the literal sentence cannot be
//! built: **an edge carries exactly one `branch_id`**, and concepts are shared
//! vocabulary across the whole ledger ([D-214]) rather than rows a lineage
//! owns. There is no pair of endpoints "on different branches" for an edge to
//! span. What the schema cannot represent needs no runtime refusal, and a test
//! asserting that an unconstructable shape is refused would be
//! [D-030](../docs/architecture/s13-decision-register.md#d-030)'s check that
//! cannot fail.
//!
//! **So this file tests the question the criterion was reaching for rather than
//! the sentence it used.** A caller cannot span two lineages with one edge, but
//! a caller *can* make one lineage's belief depend on another's vocabulary, and
//! that is what "cross-branch" means in the design that shipped. The ledger's
//! answer to it is in two halves, and both are here:
//!
//! * **at assertion, it is allowed** — an edge on any lineage may name a
//!   concept minted on any other, because `concepts.id` is unique ledger-wide
//!   and copy-on-write is the whole economy of an O(1) fork. The trunk arm of
//!   this was measured by probe before `branch_archive_tests` was written and
//!   the sibling arm was measured with it, and *neither was pinned anywhere*.
//!   A design claim with no instrument is the pattern this project keeps
//!   finding in its own gates; the two arms are pinned here;
//! * **at abandonment, it is refused and named** — `BranchNotArchivable`,
//!   carrying the concept id, because the branch's rows are then not closed
//!   under `concepts(id)` and deleting them would leave another lineage's
//!   `links.source_id` pointing at nothing. `branch_archive_tests` covers the
//!   trunk arm of that refusal; the guard's predicate is `l.branch_id <>
//!   :branch`, which is *sibling* language, and no test made a sibling the
//!   other lineage. The one here does.
//!
//! And the refusal names a remedy — *"retire those edges first"* — which is a
//! claim about a path a caller is told to take. The last test takes it.
//!
//! The three refusals a caller can actually reach are tested where they live
//! and are deliberately not copied here: `BranchMismatch` in
//! `branch_view_tests`, `CrossLineage` and the write-side `UnknownBranch` in
//! `branch_write_tests`.
//!
//! [D-214]: ../docs/architecture/s13-decision-register.md#d-214

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::{BranchId, ConceptUpsert, Database};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const CLOSED: &str = "1970-01-01T00:30:00.000000Z";
const STEP: Duration = Duration::from_secs(3_600);

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// `a → b → c` on the trunk, then two siblings forked from it.
///
/// The clock moves before the forks so every trunk write is on the visible side
/// of both fork points — this file is about lineage reach, and a cutoff
/// swallowing the seed would make every assertion here ambiguous.
async fn seeded_with_two_siblings(h: &TestHarness) -> Database {
    let db = h.db_with_fake_clock().await;
    db.write_concepts(
        ["a", "b", "c"]
            .iter()
            .map(|n| ConceptUpsert::new(*n, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();
    db.bulk_import(vec![
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN),
        EdgeAssertion::new("b", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN),
    ])
    .await
    .unwrap();
    h.advance(STEP);
    db.fork(id("alpha"), BranchId::main()).await.unwrap();
    db.fork(id("beta"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    db
}

/// What one lineage can reach from `a`, sorted.
async fn reached(db: &Database, branch: Option<&str>) -> Vec<String> {
    let mut b = TraversalBuilder::new("a");
    if let Some(n) = branch {
        b = b.on_branch(id(n));
    }
    let mut v = b.execute_ids(db.read_conn(), EPOCH).await.unwrap();
    v.sort();
    v
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// `alpha` mints `d`, and `beta` asserts `c → d` on its own lineage.
async fn one_lineage_reaching_into_anothers_vocabulary(db: &Database) {
    db.write_concepts(vec![ConceptUpsert::new("d", "n")
        .valid_from(EPOCH)
        .on_branch(id("alpha"))])
        .await
        .unwrap();
    db.assert_edge(
        EdgeAssertion::new("c", "d", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("beta")),
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// the shape the criterion names, and the one a caller can build
// ---------------------------------------------------------------------------

/// Two lineages in one batch is two rows, not one row spanning two lineages.
///
/// The closest a caller can come to constructing the criterion's shape, written
/// so the *absence* of the shape is measured rather than asserted in prose:
/// `EdgeAssertion::on_branch` takes one `BranchId`, `links.branch_id` holds one
/// value, and a batch naming both siblings lands one row on each. If a spanning
/// edge were ever representable it would have to show up here as a row this
/// query cannot attribute.
#[tokio::test]
async fn a_batch_naming_two_lineages_lands_one_row_on_each() {
    let h = TestHarness::new();
    let db = seeded_with_two_siblings(&h).await;

    db.bulk_import(vec![
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alpha")),
        EdgeAssertion::new("b", "a", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("beta")),
    ])
    .await
    .unwrap();

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM links WHERE branch_id = 'alpha'").await,
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM links WHERE branch_id = 'beta'").await,
        1
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM links WHERE branch_id NOT IN ('main','alpha','beta')"
        )
        .await,
        0,
        "every row belongs to exactly one registered lineage"
    );

    assert_eq!(reached(&db, Some("alpha")).await, ["a", "b", "c"]);
    assert_eq!(reached(&db, Some("beta")).await, ["a", "b", "c"]);
}

/// An edge may name a concept a **sibling** minted, and that is the design.
///
/// The surprising half of [D-214] and the one nothing pinned: `branch_id` on
/// `concepts` is provenance rather than identity, so `beta` asserting about
/// `alpha`'s concept is not a violation to refuse — it is the inheritance that
/// makes a fork O(1), reached sideways. The cost of that generosity is the
/// refusal in the next test, and pinning the acceptance is what makes the
/// refusal legible as a consequence rather than a rule.
#[tokio::test]
async fn an_edge_may_name_a_concept_a_sibling_minted() {
    let h = TestHarness::new();
    let db = seeded_with_two_siblings(&h).await;

    one_lineage_reaching_into_anothers_vocabulary(&db).await;

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM links WHERE branch_id = 'beta' AND target_id = 'd'"
        )
        .await,
        1,
        "the edge lands on the lineage that asserted it"
    );
    assert_eq!(
        reached(&db, Some("beta")).await,
        ["a", "b", "c", "d"],
        "and beta reads through it to a concept it did not mint"
    );
    assert_eq!(
        reached(&db, Some("alpha")).await,
        ["a", "b", "c"],
        "while alpha, which minted the concept, has no edge to reach it by: \
         vocabulary is shared and belief is not"
    );
}

// ---------------------------------------------------------------------------
// where the reach is refused, and by what name
// ---------------------------------------------------------------------------

/// The refusal `branch_archive_tests` reaches through the trunk, reached
/// through a sibling.
///
/// The guard's predicate is `l.branch_id <> :branch`, which says *any other
/// lineage*; the only test of it used the trunk as the other lineage, and the
/// trunk is the one lineage whose rows can never be archived away, so it is the
/// arm least able to distinguish a general predicate from a special case.
#[tokio::test]
async fn a_lineage_whose_concept_a_sibling_names_is_refused_by_name() {
    let h = TestHarness::new();
    let db = seeded_with_two_siblings(&h).await;

    one_lineage_reaching_into_anothers_vocabulary(&db).await;

    let err = db.archive_branch(id("alpha")).await.unwrap_err();
    assert!(
        matches!(&err, DbError::BranchNotArchivable { branch, reason }
                 if branch == "alpha" && reason.contains('d')),
        "the refusal has to name the concept holding the lineage, because that \
         is what the caller must retire: {err:?}"
    );

    assert_eq!(
        reached(&db, Some("beta")).await,
        ["a", "b", "c", "d"],
        "a refusal must change nothing"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concepts WHERE branch_id = 'alpha'"
        )
        .await,
        1
    );
}

/// The remedy the refusal names is a path, and this is it end to end.
///
/// *"Retire those edges first"* is an instruction, and an instruction in an
/// error message is a claim the crate has to be able to keep. The word in it is
/// **hot**: retirement alone does not clear the guard, because `links` is
/// append-only and a shadow retirement adds a row rather than removing one. The
/// sequence that works is retire, then archive past the closed interval, which
/// takes both the superseded row and the closed one cold — and only then is the
/// lineage forgettable.
#[tokio::test]
async fn retiring_the_sibling_edge_and_archiving_it_releases_the_lineage() {
    let h = TestHarness::new();
    let db = seeded_with_two_siblings(&h).await;

    one_lineage_reaching_into_anothers_vocabulary(&db).await;

    db.retire_edge_on("c", "d", "LEADSTO", EPOCH, CLOSED, id("beta"))
        .await
        .unwrap();

    let err = db.archive_branch(id("alpha")).await.unwrap_err();
    assert!(
        matches!(&err, DbError::BranchNotArchivable { .. }),
        "retirement alone leaves the rows hot, so the guard still holds: {err:?}"
    );

    h.advance(STEP);
    db.archive(&h.clock.peek()).await.unwrap();
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM links WHERE branch_id = 'beta' AND target_id = 'd'"
        )
        .await,
        0,
        "the sibling's edge is cold, which is what the guard reads"
    );

    db.archive_branch(id("alpha")).await.unwrap();
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM branches WHERE branch_id = 'alpha'"
        )
        .await,
        0,
        "and the lineage the reach was holding open is now forgettable"
    );
}
