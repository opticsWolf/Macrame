//! One lineage's handle on the ledger (§15.4, W12.9, D-226).
//!
//! `BranchView` is the last piece of §15.4's first bullet and the smallest:
//! every operation on it exists on `Database` already and takes a lineage
//! there, so **the type buys ergonomics and no capability**. That is what this
//! file has to pin — not that a branched write works, which
//! `branch_write_tests.rs` pins, but that going through the view produces the
//! *same* rows as naming the branch by hand.
//!
//! # The two properties that are not merely delegation
//!
//! **It cannot close the database.** `Database::close` takes `self` by value
//! and the view holds an `Arc`, so the restriction is structural rather than
//! documented — a caller who forks a view, reads it and drops it is not one
//! call away from stopping the actor everyone else is using. That is the
//! argument D-203 made when `Database: Clone` was declined, and it is why the
//! view is a separate type rather than a `Database` with a field added. A test
//! cannot assert that a call does not compile, so what is asserted here is the
//! half that runs: the handle outlives every view of it, and closing it is
//! still the owner's to do.
//!
//! **It refuses a foreign lineage rather than relabelling it.** An assertion
//! naming no branch is stamped; one naming a *different* branch is
//! `BranchMismatch`. The failure that motivates it is holding two views and
//! passing one's assertion to the other, which nothing in the type system
//! prevents — both views have the same methods and the call site reads
//! correctly.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::{BranchId, ConceptUpsert, Database, DbError};
use std::sync::Arc;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const TS2: &str = "2026-02-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

async fn seeded(h: &TestHarness) -> Arc<Database> {
    let db = h.db_with_fake_clock().await;
    for id in ["a", "b", "c", "d"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
    Arc::new(db)
}

fn edge(source: &str, target: &str) -> EdgeAssertion {
    EdgeAssertion::new(source, target, "KNOWS").valid_from(TS)
}

/// Rows physically carrying `branch`, which is what "the write landed here"
/// means before any resolution runs.
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

// ---------------------------------------------------------------------------
// The view is the branch, at every door
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_write_through_a_view_lands_on_its_lineage() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(alt.id);

    view.assert_edge(edge("a", "b")).await.unwrap();
    view.write_bulk_atomic(vec![edge("b", "c")]).await.unwrap();
    view.bulk_import(vec![edge("c", "d")]).await.unwrap();
    view.upsert_concept(ConceptUpsert::new("mine", "Mine").valid_from(TS))
        .await
        .unwrap();
    view.write_concepts(vec![ConceptUpsert::new("mine2", "M2").valid_from(TS)])
        .await
        .unwrap();

    assert_eq!(rows_on(&db, "alt").await, 3);
    assert_eq!(rows_on(&db, "main").await, 0);

    let minted: Vec<String> = {
        let mut rows = db
            .read_conn()
            .query(
                "SELECT id FROM concepts WHERE branch_id = 'alt' ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(r.get::<String>(0).unwrap());
        }
        out
    };
    assert_eq!(minted, ["mine", "mine2"]);
}

#[tokio::test]
async fn a_view_reads_what_naming_the_branch_by_hand_reads() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    db.assert_edge(edge("a", "b")).await.unwrap();
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(alt.id.clone());
    view.assert_edge(edge("b", "c")).await.unwrap();

    // Inherited plus its own, through the view's seeded builder …
    let through_view = view
        .traversal("a")
        .max_depth(5)
        .execute_ids(view.read_conn(), NOW)
        .await
        .unwrap();
    // … and through the long form the view exists to save.
    let by_hand = macrame::graph::TraversalBuilder::new("a")
        .max_depth(5)
        .on_branch(alt.id.as_str())
        .execute_ids(db.read_conn(), NOW)
        .await
        .unwrap();
    assert_eq!(through_view, by_hand);
    assert_eq!(through_view, ["a", "b", "c"]);

    let view_edges = view.query_as_of_edges(NOW).await.unwrap();
    let hand_edges =
        macrame::temporal::query_as_of_edges_on(db.read_conn(), NOW, Some(alt.id.as_str()))
            .await
            .unwrap();
    assert_eq!(view_edges, hand_edges);
    assert_eq!(view_edges.len(), 2);

    let g = view.load_subgraph("a", 5, NOW, 1_000_000).await.unwrap();
    assert_eq!(g.node_count(), 3);
}

#[tokio::test]
async fn a_view_of_the_trunk_is_the_trunk() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    db.fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let trunk = db.view(BranchId::main());

    trunk.assert_edge(edge("a", "b")).await.unwrap();
    assert_eq!(rows_on(&db, "main").await, 1);
    assert_eq!(rows_on(&db, "alt").await, 0);
    assert_eq!(trunk.query_as_of_edges(NOW).await.unwrap().len(), 1);
}

#[tokio::test]
async fn retiring_through_a_view_shadows_rather_than_touching_the_parent() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    db.assert_edge(edge("a", "b")).await.unwrap();
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(alt.id);

    view.retire_edge("a", "b", "KNOWS", TS, TS2).await.unwrap();

    assert!(view.query_as_of_edges(NOW).await.unwrap().is_empty());
    assert_eq!(
        macrame::temporal::query_as_of_edges_on(db.read_conn(), NOW, None)
            .await
            .unwrap()
            .len(),
        1
    );
    // The row that closed it is the branch's own; the parent's is untouched.
    assert_eq!(rows_on(&db, "alt").await, 1);
    assert_eq!(rows_on(&db, "main").await, 1);
}

// ---------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_view_refuses_a_write_that_names_another_lineage() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let other = db
        .fork(BranchId::new("other").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(alt.id);
    let foreign = edge("a", "b").on_branch(other.id.clone());

    match view.assert_edge(foreign.clone()).await {
        Err(DbError::BranchMismatch { view, named }) => {
            assert_eq!(view, "alt");
            assert_eq!(named, "other");
        }
        other => panic!("expected BranchMismatch, got {other:?}"),
    }
    assert!(matches!(
        view.write_bulk_atomic(vec![edge("a", "b"), foreign.clone()])
            .await,
        Err(DbError::BranchMismatch { .. })
    ));
    assert!(matches!(
        view.bulk_import(vec![foreign]).await,
        Err(macrame::error::BulkInterrupted {
            written: 0,
            cause: DbError::BranchMismatch { .. },
            ..
        })
    ));
    assert!(matches!(
        view.upsert_concept(
            ConceptUpsert::new("z", "Z")
                .valid_from(TS)
                .on_branch(other.id)
        )
        .await,
        Err(DbError::BranchMismatch { .. })
    ));

    // Nothing was written by any of the four.
    assert_eq!(rows_on(&db, "alt").await, 0);
    assert_eq!(rows_on(&db, "other").await, 0);
}

#[tokio::test]
async fn a_view_accepts_a_write_that_names_its_own_lineage() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();
    let view = db.view(alt.id.clone());

    // Redundant but not wrong: the caller said what the view already says.
    view.assert_edge(edge("a", "b").on_branch(alt.id))
        .await
        .unwrap();
    assert_eq!(rows_on(&db, "alt").await, 1);
}

#[tokio::test]
async fn a_view_of_an_unregistered_lineage_is_refused_at_first_use_by_name() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    db.fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();

    // Construction cannot fail and does no I/O — the check is the operation's.
    let ghost = db.view(BranchId::new("ghost").unwrap());
    assert_eq!(ghost.id().as_str(), "ghost");

    match ghost.assert_edge(edge("a", "b")).await {
        Err(DbError::UnknownBranch(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownBranch, got {other:?}"),
    }
    match ghost
        .traversal("a")
        .execute_ids(ghost.read_conn(), NOW)
        .await
    {
        Err(DbError::UnknownBranch(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownBranch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The lifecycle it deliberately does not have
// ---------------------------------------------------------------------------

#[tokio::test]
async fn views_are_clones_of_one_handle_and_the_owner_still_closes_it() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    let alt = db
        .fork(BranchId::new("alt").unwrap(), BranchId::main())
        .await
        .unwrap();

    let a = db.view(alt.id.clone());
    let b = a.clone();
    let c = db.view(BranchId::main());
    assert_eq!(a.id(), b.id());
    assert_ne!(a.id(), c.id());
    // Same handle underneath, not three of them.
    assert!(Arc::ptr_eq(a.database(), b.database()));
    assert!(Arc::ptr_eq(a.database(), c.database()));

    a.assert_edge(edge("a", "b")).await.unwrap();
    drop((a, b, c));

    // The views are gone and the handle is intact, so the owner can still end
    // it — which is the whole reason the view holds an `Arc` and `close` takes
    // `self`. `try_unwrap` is what a caller with no other clones does.
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("a view outlived its drop"));
    db.close().await.unwrap();
}

#[tokio::test]
async fn the_debug_shape_names_the_lineage_and_the_file() {
    let h = TestHarness::new();
    let db = seeded(&h).await;
    let view = db.view(BranchId::new("alt").unwrap());
    let text = format!("{view:?}");
    assert!(text.contains("alt"), "{text}");
    assert!(text.contains("test_macrame.db"), "{text}");
}
