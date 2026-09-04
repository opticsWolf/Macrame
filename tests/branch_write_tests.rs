//! Writing on a lineage (§15.4, W12.8, D-225).
//!
//! 0.14.7 shipped `fork()` into a reader that had already resolved lineage for
//! three releases, and said in as many words what it could not do: a caller who
//! forked and then called `assert_edge` got a successful write **on the trunk**.
//! This file is that gap closed. `EdgeAssertion::on_branch` and
//! `ConceptUpsert::on_branch` carry the lineage, `Database::retire_edge_on`
//! shadows an inherited edge, and every write checks the lineage it names
//! before it takes the write lock.
//!
//! # The finding this release turned on
//!
//! The overlap guard (defect AA, D-060) read `links_current` for the edge key
//! with **no lineage predicate at all**, which was exact for as long as every
//! row in the table was `main`'s. The moment a second lineage can write, the
//! same statement is wrong in *both directions at once*: a branch is refused
//! for overlapping its parent's belief that it is entitled to supersede, and
//! the trunk is refused for overlapping a branch's belief it cannot even see.
//!
//! Neither direction is a conservative approximation of the other, so
//! `AND branch_id = ?` is not the repair — that fixes the trunk and leaves the
//! branch checked against only its own rows, free to assert `[10,20)` over an
//! inherited `[5,15)` and put two overlapping intervals into its own view. That
//! is defect AA reintroduced across lineages, and it is exactly the shape
//! `trg_links_single_open`'s v12 comment parked as "§15.4's write-path
//! question": a trigger sees one row and cannot answer it.
//!
//! The answer this file pins is that **what a lineage may not overlap is what
//! that lineage can see** — the read's definition, now the write's. Both
//! directions are tested, and so is the case that separates a real resolution
//! from a filter: a parent that *churns* an edge after the fork, whose pre-fork
//! interval lives only in `transaction_log` and which the guard must still find
//! (`the_guard_sees_a_pre_fork_interval_the_projection_no_longer_holds`).
//!
//! # Why the fixtures fork through the public API
//!
//! `tests/branch_read_tests.rs` cuts its lineages by raw SQL on purpose, so the
//! reader is tested against shapes `fork()` refuses to write. This file is
//! about the writer, so it uses the writer: every branch here is a `fork()`
//! call, and a fixture that could not be built that way would be a fixture
//! about something else.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::{BranchId, ConceptUpsert, Database, DbError};
use std::time::Duration;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const TS2: &str = "2026-02-01T00:00:00.000000Z";
const TS3: &str = "2026-03-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

/// Four concepts on the trunk and nothing else, so each test states its own
/// edges. Concepts are trunk-minted because `links` keys into `concepts` and a
/// branch inherits them.
async fn seed(db: &Database) {
    for id in ["a", "b", "c", "d"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
}

fn edge(source: &str, target: &str, from: &str, to: Option<&str>) -> EdgeAssertion {
    let e = EdgeAssertion::new(source, target, "KNOWS").valid_from(from);
    match to {
        Some(t) => e.valid_to(t),
        None => e,
    }
}

/// Every `(source, target)` this lineage sees at `NOW`.
async fn seen(db: &Database, branch: Option<&BranchId>) -> Vec<(String, String)> {
    let mut out =
        macrame::temporal::query_as_of_edges_on(db.read_conn(), NOW, branch.map(BranchId::as_str))
            .await
            .unwrap()
            .into_iter()
            .map(|(s, t, _, _, _)| (s, t))
            .collect::<Vec<_>>();
    out.sort();
    out
}

/// The weight this lineage believes for `a → b`, or `None` if it sees no such
/// edge. Read through the traversal builder rather than through
/// `query_as_of_edges_on`, which does not return weights.
async fn rows_on(db: &Database, branch: &str) -> i64 {
    db.read_conn()
        .query(
            "SELECT COUNT(*) FROM links WHERE branch_id = ?1",
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

/// A database whose clock is driven, with the trunk seeded and one fork taken
/// after the trunk's writes. Returns the handle and the branch.
async fn forked(h: &TestHarness) -> (Database, BranchId) {
    let db = h.db_with_fake_clock().await;
    seed(&db).await;
    (db, BranchId::new("alt").unwrap())
}

// ---------------------------------------------------------------------------
// The write lands where it says it does
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_edge_asserted_on_a_branch_is_invisible_to_the_trunk() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, None)).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    db.assert_edge(edge("c", "d", TS, None).on_branch(alt.clone()))
        .await
        .unwrap();

    // The branch sees both; the trunk sees only its own. This is the assertion
    // 0.14.7 could not make, because every write landed on the trunk.
    assert_eq!(
        seen(&db, Some(&alt)).await,
        vec![("a".into(), "b".into()), ("c".into(), "d".into())]
    );
    assert_eq!(seen(&db, None).await, vec![("a".into(), "b".into())]);

    assert_eq!(rows_on(&db, "alt").await, 1);
    assert_eq!(rows_on(&db, "main").await, 1);

    db.close().await.unwrap();
}

#[tokio::test]
async fn a_branch_supersedes_an_inherited_edge_by_writing_beside_it() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, None).weight(1.0))
        .await
        .unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // Same key, different belief. `links_current`'s primary key ends in
    // `branch_id`, so this adds a row rather than replacing one.
    db.assert_edge(edge("a", "b", TS, None).weight(9.0).on_branch(alt.clone()))
        .await
        .unwrap();

    let two: i64 = db
        .read_conn()
        .query(
            "SELECT COUNT(*) FROM links_current WHERE source_id='a' AND target_id='b'",
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
    assert_eq!(two, 2, "the parent's row must survive the branch's write");

    // And the resolution prefers the nearer lineage.
    let g = db
        .load_subgraph_with(
            &TraversalBuilder::new("a").on_branch(alt.clone()),
            NOW,
            1_000,
        )
        .await
        .unwrap();
    let w = g
        .out_edges("a")
        .iter()
        .find(|e| e.node(&g) == "b")
        .map(|e| e.weight())
        .unwrap();
    assert_eq!(w, 9.0);

    let trunk = db
        .load_subgraph_with(&TraversalBuilder::new("a"), NOW, 1_000)
        .await
        .unwrap();
    assert_eq!(
        trunk
            .out_edges("a")
            .iter()
            .find(|e| e.node(&trunk) == "b")
            .unwrap()
            .weight(),
        1.0,
        "the trunk must not learn what the branch believes"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Shadow retirement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retiring_an_inherited_edge_shadows_it_and_leaves_the_parent_alone() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, None)).await.unwrap();
    db.assert_edge(edge("b", "c", TS, None)).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    db.retire_edge_on("a", "b", "KNOWS", TS, TS3, alt.clone())
        .await
        .unwrap();

    // Gone from the branch's view at an instant after the closure...
    assert_eq!(seen(&db, Some(&alt)).await, vec![("b".into(), "c".into())]);
    // ...and untouched in the parent's.
    assert_eq!(
        seen(&db, None).await,
        vec![("a".into(), "b".into()), ("b".into(), "c".into())]
    );

    // The row that closed it carries the branch's id, and the parent's row is
    // still open. Closing the parent's row in place is the write this design
    // exists to make unrepresentable.
    assert_eq!(rows_on(&db, "alt").await, 1);
    let parent_open: i64 = db
        .read_conn()
        .query(
            "SELECT COUNT(*) FROM links_current WHERE branch_id='main' \
             AND source_id='a' AND valid_to = '9999-12-31T23:59:59.999999Z'",
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
    assert_eq!(parent_open, 1);

    db.close().await.unwrap();
}

#[tokio::test]
async fn retiring_what_a_lineage_cannot_see_is_not_found() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));
    // Written on the trunk *after* the fork, so `alt` is not entitled to it.
    db.assert_edge(edge("a", "b", TS, None)).await.unwrap();

    let err = db
        .retire_edge_on("a", "b", "KNOWS", TS, TS3, alt.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::NotFound(_)), "got {err:?}");

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The overlap guard, in both directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_branch_may_not_overlap_an_interval_it_inherited() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, Some(TS3))).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // `[TS2, NOW)` overlaps the inherited `[TS, TS3)`. The branch can see that
    // interval, so it may not assert across it — it must shadow it first.
    let err = db
        .assert_edge(edge("a", "b", TS2, Some(NOW)).on_branch(alt.clone()))
        .await
        .unwrap_err();
    let DbError::OverlappingInterval { overlap } = err else {
        panic!("expected an overlap, got {err:?}");
    };
    assert_eq!(overlap.existing_from, TS);
    assert_eq!(overlap.existing_to, TS3);

    db.close().await.unwrap();
}

#[tokio::test]
async fn the_trunk_is_not_refused_for_overlapping_what_a_branch_believes() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.assert_edge(edge("a", "b", TS, Some(TS3)).on_branch(alt.clone()))
        .await
        .unwrap();

    // The unfiltered guard would have found the branch's row and refused this.
    // The trunk cannot see it, so there is nothing to overlap.
    db.assert_edge(edge("a", "b", TS2, Some(NOW)))
        .await
        .expect("the trunk must not be refused for a belief it cannot see");

    assert_eq!(rows_on(&db, "main").await, 1);
    assert_eq!(rows_on(&db, "alt").await, 1);

    db.close().await.unwrap();
}

#[tokio::test]
async fn a_branch_may_overlap_a_sibling_it_shares_no_ancestry_with() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    let other = BranchId::new("other").unwrap();
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(other.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    db.assert_edge(edge("a", "b", TS, Some(TS3)).on_branch(alt.clone()))
        .await
        .unwrap();
    // `other` is not a descendant of `alt`, so `alt`'s belief is not in its
    // ancestry and cannot be overlapped.
    db.assert_edge(edge("a", "b", TS2, Some(NOW)).on_branch(other.clone()))
        .await
        .expect("siblings share no visibility");

    db.close().await.unwrap();
}

#[tokio::test]
async fn a_branch_may_still_re_assert_at_the_same_valid_from() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, Some(TS3))).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // Re-assertion at the same `valid_from` is Doctrine III's ordinary case and
    // is settled by the key and the single-open trigger, not by this guard —
    // `valid_from <> ?4` excludes it in the resolved statement exactly as it
    // does in the trunk one. This is also how a branch supersedes.
    db.assert_edge(edge("a", "b", TS, Some(NOW)).on_branch(alt.clone()))
        .await
        .expect("same valid_from is re-assertion, not overlap");

    db.close().await.unwrap();
}

/// The case that separates a resolution from a filter, on the **write** side.
///
/// The parent asserts, the branch forks, and then the parent *churns the same
/// key*. `trg_links_current_sync`'s `DO UPDATE` carries `recorded_at` forward,
/// so the projection no longer holds the pre-fork interval at all — its only
/// home is `transaction_log`. A guard that read `links_current` with a cutoff
/// filter would find nothing and let the overlap through; the fold arm is what
/// finds it. This is D-223's finding reached from the write side.
#[tokio::test]
async fn the_guard_sees_a_pre_fork_interval_the_projection_no_longer_holds() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("a", "b", TS, Some(TS3))).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // The parent churns the key after the fork. `links_current` now holds the
    // reweighted row with a post-cutoff `recorded_at`; the branch may not see
    // it, and must still see what was there before.
    db.assert_edge(edge("a", "b", TS, Some(TS3)).weight(4.0))
        .await
        .unwrap();

    let err = db
        .assert_edge(edge("a", "b", TS2, Some(NOW)).on_branch(alt.clone()))
        .await
        .unwrap_err();
    let DbError::OverlappingInterval { overlap } = err else {
        panic!("the fold arm did not find the pre-fork interval: {err:?}");
    };
    assert_eq!(overlap.existing_from, TS);

    db.close().await.unwrap();
}

/// The same fixture from the other side: a churned key that is **not** this
/// one is not this one's overlap.
///
/// The guard resolves through [`macrame::graph`]'s lowering since 0.15.8
/// (W13.3, D-250), and on a branch that lowering is four CTEs whose *only*
/// mention of the edge key is inside them — the tail filters on `valid_from`
/// alone. So the narrowing in `churned_cte` and `links_cut_cte` is load-bearing
/// for the answer and not merely for the plan: drop it from the churned set and
/// the fold arm goes and fetches every ancestor's pre-fork interval for every
/// edge in the ledger, all of which reach the tail, any of which can overlap.
///
/// The write above is what makes that visible and this one is what makes it
/// fail. Mutating the narrowing out passes every other test in this file,
/// because every other fixture churns the key it then asserts.
#[tokio::test]
async fn a_pre_fork_interval_of_another_key_is_not_this_keys_overlap() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.assert_edge(edge("c", "d", TS, Some(TS3))).await.unwrap();
    h.advance(Duration::from_secs(60));
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // `c -> d` is churned after the fork, so its pre-fork interval is in the
    // log and reachable by the fold arm — for `c -> d`.
    db.assert_edge(edge("c", "d", TS, Some(TS3)).weight(4.0))
        .await
        .unwrap();

    // `a -> b` has no interval on either lineage. `[TS2, NOW)` overlaps
    // `c -> d`'s pre-fork `[TS, TS3)` and that is nothing to do with it.
    db.assert_edge(edge("a", "b", TS2, Some(NOW)).on_branch(alt.clone()))
        .await
        .expect("another key's pre-fork interval was read as this key's");

    // One row on the branch: the assertion it made, and no shadow of anyone
    // else's key dragged in by the resolution.
    assert_eq!(rows_on(&db, "alt").await, 1);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_batch_contradicts_itself_within_a_lineage_and_not_across_two() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // Same lineage, overlapping: refused before the lock is taken.
    let clash = vec![
        edge("a", "b", TS, Some(NOW)).on_branch(alt.clone()),
        edge("a", "b", TS2, Some(NOW)).on_branch(alt.clone()),
    ];
    assert!(matches!(
        db.write_bulk_atomic(clash).await,
        Err(DbError::OverlappingInterval { .. })
    ));

    // The same two intervals on two lineages are two beliefs, not a
    // contradiction, and the sweep's key now says so.
    let split = vec![
        edge("a", "b", TS, Some(NOW)).on_branch(alt.clone()),
        edge("a", "b", TS2, Some(NOW)),
    ];
    assert_eq!(db.write_bulk_atomic(split).await.unwrap(), 2);
    assert_eq!(rows_on(&db, "alt").await, 1);
    assert_eq!(rows_on(&db, "main").await, 1);

    db.close().await.unwrap();
}

#[tokio::test]
async fn a_bulk_import_lands_each_edge_on_the_lineage_it_names() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    let edges = vec![
        edge("a", "b", TS, None).on_branch(alt.clone()),
        edge("b", "c", TS, None).on_branch(alt.clone()),
        edge("c", "d", TS, None),
    ];
    assert_eq!(db.bulk_import(edges).await.unwrap(), 3);

    assert_eq!(rows_on(&db, "alt").await, 2);
    assert_eq!(rows_on(&db, "main").await, 1);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// A lineage that does not exist
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_write_refuses_an_unregistered_lineage_by_name() {
    let h = TestHarness::new();
    let (db, _) = forked(&h).await;
    let ghost = BranchId::new("ghost").unwrap();

    let unknown = |e: DbError| match e {
        DbError::UnknownBranch(b) => b,
        other => panic!("expected UnknownBranch, got {other:?}"),
    };

    // Four entry points, one refusal. Left to the foreign key these would have
    // been an unqualified "FOREIGN KEY constraint failed" out of a rolled-back
    // transaction, naming neither the column nor the branch.
    assert_eq!(
        unknown(
            db.assert_edge(edge("a", "b", TS, None).on_branch(ghost.clone()))
                .await
                .unwrap_err()
        ),
        "ghost"
    );
    assert_eq!(
        unknown(
            db.retire_edge_on("a", "b", "KNOWS", TS, TS3, ghost.clone())
                .await
                .unwrap_err()
        ),
        "ghost"
    );
    assert_eq!(
        unknown(
            db.upsert_concept(
                ConceptUpsert::new("z", "Z")
                    .valid_from(TS)
                    .on_branch(ghost.clone())
            )
            .await
            .unwrap_err()
        ),
        "ghost"
    );
    assert_eq!(
        unknown(
            db.write_bulk_atomic(vec![edge("a", "b", TS, None).on_branch(ghost.clone())])
                .await
                .unwrap_err()
        ),
        "ghost"
    );

    // And nothing was written by any of them.
    assert_eq!(rows_on(&db, "main").await, 0);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Concepts, whose rule is the schema's rather than this crate's
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_branch_mints_a_new_concept_and_may_not_restate_an_inherited_one() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // Minting is fine: the trunk has no row for this id.
    db.upsert_concept(
        ConceptUpsert::new("mine", "Mine")
            .valid_from(TS)
            .on_branch(alt.clone()),
    )
    .await
    .unwrap();

    // Restating one it inherited is not. `trg_concepts_cross_lineage` has been
    // in the schema since v12; until 0.14.8 no caller could reach it, and
    // `classify` had no arm for it — so it would have surfaced as `Engine`.
    let err = db
        .upsert_concept(
            ConceptUpsert::new("a", "Renamed")
                .valid_from(TS)
                .on_branch(alt.clone()),
        )
        .await
        .unwrap_err();
    let DbError::CrossLineage {
        id,
        held_by,
        attempted,
    } = err
    else {
        panic!("expected CrossLineage, got {err:?}");
    };
    assert_eq!(
        (id.as_str(), held_by.as_str(), attempted.as_str()),
        ("a", "main", "alt")
    );

    db.close().await.unwrap();
}

#[tokio::test]
async fn re_upserting_a_trunk_concept_from_the_trunk_is_still_a_plain_update() {
    let h = TestHarness::new();
    let (db, alt) = forked(&h).await;
    db.fork(alt.clone(), BranchId::main()).await.unwrap();
    h.advance(Duration::from_secs(60));

    // `branch_id` is deliberately absent from the `DO UPDATE` list: listing it
    // would make every re-upsert an abort from `trg_concepts_branch_immutable`
    // rather than the no-op it is.
    db.upsert_concept(ConceptUpsert::new("a", "Renamed").valid_from(TS2))
        .await
        .unwrap();

    let title: String = db
        .read_conn()
        .query("SELECT title FROM concepts WHERE id='a'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(title, "Renamed");

    db.close().await.unwrap();
}
