//! The abandonment arm: forgetting a lineage (0.14.13, W12.13, D-230).
//!
//! §15.4 asks for a branch-aware `archive` on the ground that "an abandoned
//! branch's rows are a contiguous archivable set by construction, which is the
//! cheapest archive predicate in the crate". The predicate is indeed the
//! cheapest — `branch_id = :branch` — and **contiguous by construction is false
//! in both of its senses**, which is what shaped everything here:
//!
//! * not closed under `concepts(id)`, because a concept is keyed by identity
//!   across the whole ledger ([D-214]) and a trunk or sibling edge may name one
//!   minted on the branch. That is a refusal, not a repair;
//! * not a prefix of `transaction_log`, whose rows for one lineage are
//!   scattered through the sequence exactly as `LOG_ARCHIVABLE`'s are.
//!
//! So the arm is all-or-nothing: links, `links_current`, the log **and** the
//! `branches` row move together, or nothing does. The last of those is why v13
//! exists, and it is what makes a hot fold that omits the lineage *correct
//! rather than silently short* — afterwards the name is unknown, and a read
//! naming it is refused instead of quietly answered with missing rows.
//!
//! [D-214]: ../docs/architecture/s13-decision-register.md#d-214

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::integrity::audit_current;
use macrame::{BranchId, ConceptUpsert, Database};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const T1: &str = "1970-01-02T00:00:00.000000Z";
const STEP: Duration = Duration::from_secs(3_600);

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// `a → b → c` on the trunk, with the clock moved on so a fork point lands
/// after every write.
async fn seed(h: &TestHarness) -> Database {
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
    db
}

/// What one lineage can still reach from `a`, in order.
async fn reached(db: &Database, branch: Option<&str>) -> Vec<String> {
    let mut b = TraversalBuilder::new("a");
    if let Some(n) = branch {
        b = b.on_branch(id(n));
    }
    let mut v = b.execute_ids(db.read_conn(), EPOCH).await.unwrap();
    v.sort();
    v
}

/// How many rows a hot table still holds for one lineage.
async fn hot_rows(db: &Database, table: &str, branch: &str) -> i64 {
    db.read_conn()
        .query(
            &format!("SELECT COUNT(*) FROM {table} WHERE branch_id = ?1"),
            libsql::params![branch],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// One scalar from the cold file, opened in its own right.
///
/// Read through a second connection rather than through the `cold` ATTACH,
/// because the session detaches unconditionally and a test that reached in
/// through the database's own handle would be measuring the ATTACH rather than
/// the file.
async fn cold_scalar(db: &Database, sql: &str) -> Option<String> {
    let conn = libsql::Builder::new_local(db.archive_path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .and_then(|r| r.get::<String>(0).ok())
}

// ---------------------------------------------------------------------------
// what the arm does
// ---------------------------------------------------------------------------

/// The whole lineage leaves, and the ledger it forked from is untouched.
///
/// The four tables are asserted separately rather than through one count,
/// because the design's content is that they move *together*: a version of this
/// that took `links` and left the log would pass a total-row assertion and
/// leave `reconstruct(now)` disagreeing with `links_current` about what is
/// currently believed.
#[tokio::test]
async fn an_abandoned_lineage_leaves_and_the_trunk_does_not_notice() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.write_concepts(vec![ConceptUpsert::new("d", "n")
        .valid_from(EPOCH)
        .on_branch(id("alt"))])
        .await
        .unwrap();
    db.assert_edge(
        EdgeAssertion::new("c", "d", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    assert_eq!(reached(&db, Some("alt")).await, ["a", "b", "c", "d"]);
    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);

    db.archive_branch(id("alt")).await.unwrap();

    assert_eq!(
        reached(&db, None).await,
        ["a", "b", "c"],
        "the trunk never named the branch and must not have lost anything to it"
    );

    for table in ["links", "concepts", "transaction_log", "links_current"] {
        assert_eq!(
            hot_rows(&db, table, "alt").await,
            0,
            "{table} still holds rows for a lineage the ledger has forgotten"
        );
    }
    assert_eq!(
        hot_rows(&db, "branches", "alt").await,
        0,
        "the lineage record is the row that makes the omission correct rather \
         than short: while it is there, a fold that skips the branch is a fold \
         that lost rows"
    );

    let names: Vec<String> = db
        .branches()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id.to_string())
        .collect();
    assert_eq!(names, ["main"]);
}

/// After the lineage is gone, naming it is a refusal and not a short answer.
///
/// This is the whole justification for moving the `branches` row, stated as a
/// test. An arm that took the rows and left the lineage record would leave this
/// read succeeding and returning `["a", "b", "c"]` — the trunk's answer, silently
/// substituted for a branch's, with nothing to tell the caller that everything
/// the branch believed had been deleted.
#[tokio::test]
async fn a_read_naming_a_forgotten_lineage_is_refused() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.archive_branch(id("alt")).await.unwrap();

    let err = TraversalBuilder::new("a")
        .on_branch(id("alt"))
        .execute_ids(db.read_conn(), EPOCH)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DbError::UnknownBranch(b) if b == "alt"),
        "expected UnknownBranch naming the lineage, got {err:?}"
    );
}

/// A branch's shadow of an inherited edge leaves with it, and the ancestor's
/// row stays.
///
/// The cross-lineage retirement of [D-229], on the other side of the operation.
/// A branch retires `b → c` by writing its **own** closed row at the trunk's
/// key; forgetting the branch takes that row, and the trunk — which never
/// stopped believing the edge — must still reach `c`.
#[tokio::test]
async fn a_shadow_retirement_leaves_with_the_lineage_that_wrote_it() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.retire_edge_on("b", "c", "LEADSTO", EPOCH, T1, id("alt"))
        .await
        .unwrap();

    db.archive_branch(id("alt")).await.unwrap();

    assert_eq!(
        reached(&db, None).await,
        ["a", "b", "c"],
        "the trunk's own open row was never the branch's to retire, and it is \
         not the branch's to take with it either"
    );
    assert_eq!(hot_rows(&db, "links", "alt").await, 0);
}

/// The lineage record lands in the cold file, with when it was forgotten.
///
/// `upgrade_cold_lineage`'s note predicted this table: a cold row stamped with
/// a branch name that nothing resolves is what falls out of an abandonment arm,
/// and `cold.branches` is what resolves it. So the assertion is not that a row
/// exists but that the cold file can answer *what lineage this cold link
/// belonged to* without the hot database.
#[tokio::test]
async fn the_cold_file_can_still_say_what_lineage_a_row_belonged_to() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    db.archive_branch(id("alt")).await.unwrap();

    let resolved = cold_scalar(
        &db,
        "SELECT b.branch_id || ' from ' || b.parent_id \
         FROM links l JOIN branches b ON b.branch_id = l.branch_id \
         WHERE l.source_id = 'a' AND l.target_id = 'c'",
    )
    .await;
    assert_eq!(
        resolved.as_deref(),
        Some("alt from main"),
        "the cold link must join to a lineage record in the same file"
    );

    let archived_at = cold_scalar(
        &db,
        "SELECT archived_at FROM branches WHERE branch_id = 'alt'",
    )
    .await
    .expect("the lineage record must carry when it was forgotten");
    assert!(
        archived_at.starts_with("1970-"),
        "archived_at is the injected wall clock, not a ledger fact: {archived_at}"
    );
}

/// Doctrine VI holds across the operation.
///
/// Necessary and — as [D-229] established the hard way — nowhere near
/// sufficient. `audit_current` asks whether `links_current` is the image of
/// `links`, and this arm re-derives the projection from what survives, so a
/// version that deleted the wrong rows would pass this too. It is here because
/// a version that deleted the right rows and *forgot* to re-derive would fail
/// it, which is a different mistake and a real one.
#[tokio::test]
async fn forgetting_a_lineage_leaves_no_drift() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    db.archive_branch(id("alt")).await.unwrap();

    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}

// ---------------------------------------------------------------------------
// what the arm refuses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_trunk_is_refused() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    let err = db.archive_branch(BranchId::main()).await.unwrap_err();
    assert!(
        matches!(&err, DbError::BranchNotArchivable { branch, reason }
                 if branch == "main" && reason.contains("trunk")),
        "expected a refusal naming the trunk, got {err:?}"
    );
    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);
}

/// A typo reads the same here as everywhere else.
#[tokio::test]
async fn a_name_that_is_not_registered_is_refused_as_unknown() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    let err = db.archive_branch(id("ghost")).await.unwrap_err();
    assert!(
        matches!(&err, DbError::UnknownBranch(b) if b == "ghost"),
        "expected UnknownBranch rather than a refusal type only this surface \
         raises, got {err:?}"
    );
}

/// A parent is not abandoned while a child reads through it.
///
/// The same loss D-229 repaired in the time-indexed predicates, reached from
/// the other direction: archiving the parent would delete rows the child still
/// believes, and the child cannot see that it happened.
#[tokio::test]
async fn a_lineage_with_descendants_is_refused() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    db.fork(id("alt_child"), id("alt")).await.unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    let err = db.archive_branch(id("alt")).await.unwrap_err();
    assert!(
        matches!(&err, DbError::BranchNotArchivable { branch, reason }
                 if branch == "alt" && reason.contains("descendants")),
        "expected a refusal naming descendants, got {err:?}"
    );

    assert_eq!(
        reached(&db, Some("alt_child")).await,
        ["a", "b", "c"],
        "a refusal must change nothing: the child still reads through its parent"
    );
    assert_eq!(hot_rows(&db, "links", "alt").await, 1);
    assert_eq!(hot_rows(&db, "branches", "alt").await, 1);
}

/// The road map's "contiguous by construction", refuted as a refusal.
///
/// `concepts` is keyed by identity across the whole ledger (D-214), so a
/// concept minted on a branch can be named by a trunk edge — measured by probe
/// before this arm was written, and both a trunk edge and a sibling's edge
/// succeed. The branch's rows are therefore not closed under `concepts(id)`,
/// and archiving them would leave `links.source_id` pointing at nothing.
///
/// Refused rather than repaired, because there is no repair that is not a lie:
/// leaving the concept hot makes the post-condition conditional, and taking it
/// cold breaks a foreign key the trunk depends on. A lineage other lineages
/// still depend on is not abandoned.
#[tokio::test]
async fn a_lineage_whose_concept_another_lineage_names_is_refused() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.write_concepts(vec![ConceptUpsert::new("d", "n")
        .valid_from(EPOCH)
        .on_branch(id("alt"))])
        .await
        .unwrap();
    // The trunk reaching into the branch's concept: legal, and the whole
    // reason the plan's claim does not hold.
    db.assert_edge(
        EdgeAssertion::new("c", "d", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN),
    )
    .await
    .unwrap();

    let err = db.archive_branch(id("alt")).await.unwrap_err();
    assert!(
        matches!(&err, DbError::BranchNotArchivable { branch, reason }
                 if branch == "alt" && reason.contains('d')),
        "the refusal must name the concept that is holding the lineage here, \
         because that is the thing the caller has to retire: {err:?}"
    );

    assert_eq!(
        reached(&db, None).await,
        ["a", "b", "c", "d"],
        "a refusal must change nothing"
    );
    assert_eq!(hot_rows(&db, "concepts", "alt").await, 1);
}

/// A branch's own edges into its own concepts do not hold it back.
///
/// The companion of the case above, and the one that says the refusal is a
/// dependency test rather than a reference count. Everything here is the
/// branch's, so everything goes.
#[tokio::test]
async fn a_lineage_that_only_names_its_own_concepts_is_archivable() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.write_concepts(vec![
        ConceptUpsert::new("d", "n")
            .valid_from(EPOCH)
            .on_branch(id("alt")),
        ConceptUpsert::new("e", "n")
            .valid_from(EPOCH)
            .on_branch(id("alt")),
    ])
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("d", "e", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    let report = db.archive_branch(id("alt")).await.unwrap();
    assert_eq!(report.links_archived, 1);
    assert_eq!(report.concepts_archived, 2);
    assert!(
        report.log_entries_archived >= 3,
        "two concept inserts and one link assertion, at least: {report:?}"
    );

    assert_eq!(hot_rows(&db, "concepts", "alt").await, 0);
    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);
}

/// The log rows go, and that is not a detail.
///
/// If the links go and the log stays, `reconstruct(now)` folds the log and
/// yields the branch's open edges while `links_current` does not — a genuine
/// disagreement about present belief, inside one file, with nothing to
/// arbitrate it. The count is asserted on the *lineage*, not on the log's size,
/// because a branch's entries are scattered through the sequence rather than
/// forming a suffix.
#[tokio::test]
async fn the_lineages_log_entries_go_with_its_links() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .on_branch(id("alt")),
    )
    .await
    .unwrap();
    // A trunk write *after* the branch's, so the branch's entries sit in the
    // middle of the sequence and a prefix-shaped deletion would take the wrong
    // ones.
    db.assert_edge(
        EdgeAssertion::new("a", "c", "MENTIONS")
            .valid_from(EPOCH)
            .valid_to(OPEN),
    )
    .await
    .unwrap();

    assert!(hot_rows(&db, "transaction_log", "alt").await > 0);
    let trunk_before = hot_rows(&db, "transaction_log", "main").await;

    db.archive_branch(id("alt")).await.unwrap();

    assert_eq!(hot_rows(&db, "transaction_log", "alt").await, 0);
    assert_eq!(
        hot_rows(&db, "transaction_log", "main").await,
        trunk_before,
        "the trunk's entries surround the branch's and must be untouched"
    );
}
