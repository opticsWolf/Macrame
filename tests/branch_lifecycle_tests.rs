//! Creating a lineage, and listing what exists (§15.2, §15.4, W12.7).
//!
//! The read half of branching shipped at 0.14.2 through 0.14.6 and every
//! fixture that exercised it reached its second lineage by **raw SQL**, because
//! no public call could make one. This file is the release where that stops
//! being true, so it is the first branch test that is end to end: `fork` writes
//! the row, `branches` reads it back, and a traversal that names it resolves.
//!
//! # What the assertions here are actually pinning
//!
//! Three things, and only the first is about `fork` returning `Ok`:
//!
//! 1. **A fork is O(1) in rows written** (§17, acceptance 1). One row in
//!    `branches`, and nothing else — no ledger table is read, copied, or
//!    touched. This is the property the whole design rests on: §15.3's option 3
//!    was rejected because it makes a fork O(rows), and the cost of *not*
//!    copying is paid on the read side where it is measured (D-220, D-223).
//!    `a_thousand_forks_write_a_thousand_rows_and_nothing_else` is that test at
//!    the scale the acceptance list names.
//! 2. **The two halves compose.** `a_fork_is_readable_the_moment_it_exists` is
//!    the one test in the crate that goes fork → write on the parent → read on
//!    the child, and it is the first time D-223's cutoff has been reached
//!    through the public API rather than through a hand-written `branches` row.
//!    If the two halves had disagreed about anything — the fork instant's
//!    precision, which column the cutoff is read from — this is where it would
//!    show.
//! 3. **The three refusals a schema cannot make.** A duplicate name, an
//!    unregistered parent, and a fork point before its parent existed. The
//!    first two would fail at the engine anyway and are checked to be *named*;
//!    the third is genuinely unenforceable in SQL, because `CHECK` sees one row
//!    and the invariant spans two.
//!
//! # Why a branch is written to nowhere in this file
//!
//! Because it cannot be. `EdgeAssertion` carries no lineage, so every write in
//! this release lands on the trunk and a fork is a *view* of its parent's
//! history as of an instant. Acceptance 2's "and its own" half is the
//! branch-scoped view, and is not this release. Stated here so the absence
//! reads as a boundary rather than as a gap in the coverage.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::{Branch, BranchId, ConceptUpsert, Database, DbError};
use std::time::Duration;

/// A raw connection to the same file, for the one fixture that has to write a
/// row the public API cannot produce. `read_conn` and `diagnostic_conn` are both
/// `query_only`, which is the point of them.
async fn raw(h: &TestHarness) -> libsql::Connection {
    libsql::Builder::new_local(&h.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn count(db: &Database, sql: &str) -> i64 {
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

/// Every table a fork must not touch.
async fn ledger_counts(db: &Database) -> [i64; 4] {
    [
        count(db, "SELECT COUNT(*) FROM links").await,
        count(db, "SELECT COUNT(*) FROM links_current").await,
        count(db, "SELECT COUNT(*) FROM concepts").await,
        count(db, "SELECT COUNT(*) FROM transaction_log").await,
    ]
}

/// Valid from long before any fixture reads, so nothing here turns on valid
/// time — every assertion in this file is about the *transaction*-time axis.
const VALID_FROM: &str = "2020-01-01T00:00:00.000000Z";

async fn nodes(db: &Database, ids: &[&str]) {
    for id in ids {
        db.upsert_concept(ConceptUpsert::new(*id, "N").valid_from(VALID_FROM))
            .await
            .unwrap();
    }
}

async fn edge(db: &Database, source: &str, target: &str) {
    db.assert_edge(EdgeAssertion::new(source, target, "LEADSTO").valid_from(VALID_FROM))
        .await
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The trunk, before anything forks
// ───────────────────────────────────────────────────────────────────────────

/// A database that has never forked still has a lineage, and says so.
///
/// The trunk is not a special case in the schema — it is a row in `branches`
/// with a null parent, seeded by the migration that created the table — and
/// this asserts the listing reports it as one rather than as an empty list that
/// callers have to know means `main`.
#[tokio::test]
async fn a_fresh_ledger_has_exactly_one_lineage_and_it_is_the_trunk() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    let all = db.branches().await.unwrap();
    assert_eq!(all.len(), 1, "{all:?}");
    let trunk = &all[0];
    assert_eq!(trunk.id.as_str(), "main");
    assert!(trunk.id.is_main());
    assert_eq!(trunk.parent, None, "the trunk is nobody's child");
    assert_eq!(
        trunk.forked_at, None,
        "and it was not cut from anything, which is the same fact"
    );
    assert!(!trunk.created_at.is_empty());

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Acceptance 1 — a fork is O(1) in rows written
// ───────────────────────────────────────────────────────────────────────────

/// §17's acceptance 1, at the scale it names.
///
/// The count that matters is not `branches` — a thousand inserts writing a
/// thousand rows is arithmetic. It is the other four, which must be *identical*
/// before and after. A design that copied the parent's projection would move
/// `links_current` here by 1,000 × the fixture, and a design that logged the
/// fork as a ledger act would move `transaction_log`.
#[tokio::test]
async fn a_thousand_forks_write_a_thousand_rows_and_nothing_else() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    nodes(&db, &["a", "b", "c"]).await;
    edge(&db, "a", "b").await;
    edge(&db, "b", "c").await;
    let before = ledger_counts(&db).await;
    assert!(before[0] > 0, "the fixture must not be empty: {before:?}");

    for i in 0..1_000 {
        db.fork(BranchId::new(format!("alt/{i}")).unwrap(), BranchId::main())
            .await
            .unwrap();
    }

    assert_eq!(
        ledger_counts(&db).await,
        before,
        "a fork touched a ledger table; the design's whole cost model is that it does not"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM branches").await, 1_001);

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The two halves, composed through the public API for the first time
// ───────────────────────────────────────────────────────────────────────────

/// Fork, then churn the parent, then read the child (D-223, end to end).
///
/// Every 0.14.6 test of this shape built its `branches` row by hand, so the
/// fork instant was a constant the test chose. Here it is whatever `fork`
/// stamped, which is the thing that has to agree with what the reader compares
/// against — same clock, same precision, same column.
///
/// The trunk keeps `a → b → c`. After the fork it adds `c → d`, so:
///
/// * the trunk reaches `d`, because it wrote it;
/// * the branch does not, because the write is past its cutoff;
/// * and the branch still reaches `c`, which is the half a naive
///   `recorded_at <= cutoff` filter over `links_current` gets wrong for a
///   *churned* key — see `branch_read_tests` for that matrix.
#[tokio::test]
async fn a_fork_is_readable_the_moment_it_exists() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    nodes(&db, &["a", "b", "c", "d"]).await;
    edge(&db, "a", "b").await;
    edge(&db, "b", "c").await;

    h.advance(Duration::from_secs(60));
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    h.advance(Duration::from_secs(60));

    // The trunk moves on after the fork.
    edge(&db, "c", "d").await;

    let now = "2100-01-01T00:00:00.000000Z";
    let trunk = TraversalBuilder::new("a")
        .execute_ids(db.read_conn(), now)
        .await
        .unwrap();
    let branch = TraversalBuilder::new("a")
        // `BranchId` reaches the 0.14.4 read surface with no conversion, which
        // is what `impl From<BranchId> for String` is for.
        .on_branch(alt.id.clone())
        .execute_ids(db.read_conn(), now)
        .await
        .unwrap();

    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(sorted(trunk), ["a", "b", "c", "d"]);
    assert_eq!(
        sorted(branch),
        ["a", "b", "c"],
        "the branch absorbed a write its parent made after the fork point"
    );

    db.close().await.unwrap();
}

/// The fork point the reader uses is the one `fork` recorded.
///
/// Asserted on the row rather than inferred from the traversal above, because
/// the two could agree by accident if both were wrong in the same direction.
#[tokio::test]
async fn the_returned_branch_is_the_row_that_was_written() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    let returned = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();

    let listed: Vec<Branch> = db.branches().await.unwrap();
    let stored = listed.iter().find(|b| b.id.as_str() == "alt").unwrap();
    assert_eq!(&returned, stored, "the handle and the row disagree");

    assert_eq!(returned.parent.as_ref().unwrap().as_str(), "main");
    assert_eq!(
        returned.forked_at.as_deref(),
        Some(returned.created_at.as_str()),
        "this release forks from now, so the two columns hold one instant"
    );

    db.close().await.unwrap();
}

/// Trunk first, then creation order — the shape of the tree over time.
#[tokio::test]
async fn the_listing_is_trunk_first_then_creation_order() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    for name in ["first", "second", "third"] {
        h.advance(Duration::from_secs(1));
        db.fork(BranchId::new(name).unwrap(), BranchId::main())
            .await
            .unwrap();
    }
    // A grandchild, so the order cannot be an artefact of every row sharing a
    // parent.
    h.advance(Duration::from_secs(1));
    db.fork(
        BranchId::new("fourth").unwrap(),
        BranchId::new("second").unwrap(),
    )
    .await
    .unwrap();

    let names: Vec<String> = db
        .branches()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id.to_string())
        .collect();
    assert_eq!(names, ["main", "first", "second", "third", "fourth"]);

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The three refusals
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn forking_from_a_lineage_that_does_not_exist_names_it() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    let err = db
        .fork(
            BranchId::new("alt").unwrap(),
            BranchId::new("ghost").unwrap(),
        )
        .await
        .unwrap_err();
    match err {
        DbError::UnknownBranch(ref what) => assert_eq!(what, "ghost"),
        other => panic!("a foreign-key violation reached the caller: {other}"),
    }
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM branches").await,
        1,
        "the refused fork left a row behind"
    );

    db.close().await.unwrap();
}

/// A taken name is refused rather than ignored, and the distinction is the test.
///
/// `SEED_MAIN_BRANCH` uses `INSERT OR IGNORE`, which is correct there — the row
/// is identical whoever writes it. Here it would not be: an ignored insert
/// returns a handle to a lineage with a *different* parent and fork point than
/// the caller asked for, and the caller would then read history they never
/// requested and have nothing to notice it by. That is D-069's shape.
#[tokio::test]
async fn a_taken_name_is_refused_rather_than_quietly_returning_the_other_branch() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    db.fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    h.advance(Duration::from_secs(60));

    // Re-forking the same name from a *different* parent: the case where an
    // ignore would be silently wrong rather than merely redundant.
    db.fork(BranchId::new("other").unwrap(), BranchId::main())
        .await
        .unwrap();
    let err = db
        .fork(
            BranchId::new("alt").unwrap(),
            BranchId::new("other").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DbError::BranchExists(ref w) if w == "alt"),
        "{err}"
    );

    let alt = db.branches().await.unwrap();
    let alt = alt.iter().find(|b| b.id.as_str() == "alt").unwrap();
    assert_eq!(
        alt.parent.as_ref().unwrap().as_str(),
        "main",
        "the original row was rewritten, which `branches` triggers should have refused"
    );

    db.close().await.unwrap();
}

/// The trunk is taken from the first migration, and `fork` says so in the same
/// words it uses for any other collision.
#[tokio::test]
async fn the_trunk_cannot_be_forked_into_existence_twice() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    let err = db
        .fork(BranchId::main(), BranchId::main())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DbError::BranchExists(ref w) if w == "main"),
        "{err}"
    );

    db.close().await.unwrap();
}

/// The one refusal no `CHECK` could have made, on the path that reaches it.
///
/// # This is not the invariant the schema comment promised, and that is the
/// finding
///
/// `CREATE_BRANCHES_TABLE` said from v12 that `fork()` would enforce *"the fork
/// point is at or after the parent's **creation**"*. It cannot: `seed_root_branch`
/// stamps `main.created_at` from `SystemTime::now()` during migration, which
/// runs before the database's injected clock exists, so on every harness in
/// this crate the trunk's `created_at` sits decades ahead of every ledger row.
/// Enforcing the promised rule refuses **every** fork in this file. What is
/// comparable is `forked_at`, which `fork` itself issues from the same clock as
/// every `recorded_at`, and the resulting rule — fork points do not decrease
/// down a root path — is the one `ancestry_cte`'s running minimum clamps for.
///
/// Refused rather than tolerated because the consequence is silent: a branch cut
/// before its parent was inherits **nothing whatever** from the parent it names,
/// since every row that parent wrote falls past the child's cutoff. Its
/// `parent_id` and its visible history then say different things — the wrong
/// answer this wave keeps finding, arrived at from a third direction.
///
/// The fixture writes the parent by raw SQL, which is the only way to get a
/// `forked_at` the clock did not issue. That is not a contrived state: the clock
/// floor applied at open is `MAX(recorded_at)` over `concepts` and `links`
/// **only**, so a database whose writes have all been forks has no floor at all,
/// and reopening it with a clock behind those forks issues exactly this stamp.
#[tokio::test]
async fn a_fork_point_before_its_parent_existed_is_refused() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;

    // Legal in every respect the schema can check: `forked_at <= created_at`
    // holds, the parent exists, the stamp is canonical.
    raw(&h)
        .await
        .execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('ahead', 'main', ?1, ?1)",
            libsql::params!["2999-01-01T00:00:00.000000Z"],
        )
        .await
        .unwrap();

    let err = db
        .fork(
            BranchId::new("behind").unwrap(),
            BranchId::new("ahead").unwrap(),
        )
        .await
        .unwrap_err();
    match err {
        DbError::ForkPrecedesParent {
            ref branch,
            ref parent,
            ref parent_forked_at,
            ..
        } => {
            assert_eq!(branch, "behind");
            assert_eq!(parent, "ahead");
            assert_eq!(parent_forked_at, "2999-01-01T00:00:00.000000Z");
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM branches WHERE branch_id = 'behind'"
        )
        .await,
        0
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The name itself
// ───────────────────────────────────────────────────────────────────────────

/// The validation is at the type, so it cannot be reached through `fork` at all.
///
/// Worth an assertion rather than left to the unit tests in `branch.rs`: the
/// claim is that an invalid name is unrepresentable at the call site, and the
/// way to check that is that there is no `fork` overload taking a `&str`.
#[tokio::test]
async fn an_invalid_name_never_reaches_the_database() {
    assert!(BranchId::new("trailing ").is_err());
    assert!(BranchId::new("").is_err());

    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;
    assert_eq!(count(&db, "SELECT COUNT(*) FROM branches").await, 1);
    db.close().await.unwrap();
}
