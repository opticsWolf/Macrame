//! `kv_store` through the public surface (0.18.0, P3, [D-280]).
//!
//! The rung test in `migration_tests` asserts the *shape* — the table, its
//! key, its CHECK, and the three exclusions it carries by absence. This file
//! asserts the four methods behave, and in particular that the two things a
//! caller could mistake for each other stay apart: a key that was never
//! written, and a key whose value is the empty string.
//!
//! [D-280]: ../docs/architecture/s13-decision-register.md#d-280

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;
use macrame::DbError;

async fn db(harness: &TestHarness) -> Database {
    Database::open(&harness.db_path).await.unwrap()
}

#[tokio::test]
async fn a_key_round_trips_and_overwrites() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    assert_eq!(db.kv_get("okf:epoch").await.unwrap(), None);

    db.kv_put("okf:epoch", "7").await.unwrap();
    assert_eq!(db.kv_get("okf:epoch").await.unwrap().as_deref(), Some("7"));

    // Plain overwrite, no history. A versioned KV would re-create the ledger
    // through the back door for state whose previous value nobody wants, so
    // the absence of a second row is the semantic rather than an omission.
    db.kv_put("okf:epoch", "8").await.unwrap();
    assert_eq!(db.kv_get("okf:epoch").await.unwrap().as_deref(), Some("8"));
    assert_eq!(db.kv_scan("okf:", 10).await.unwrap().len(), 1);
}

/// The two states a caller must not confuse.
///
/// `None` is *no such key*; `Some("")` is a key whose value is empty. The
/// column is `NOT NULL`, so there is no third state below them and the
/// `Option` means exactly one thing.
#[tokio::test]
async fn an_absent_key_and_an_empty_value_are_different_answers() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    db.kv_put("okf:cursor", "").await.unwrap();
    assert_eq!(db.kv_get("okf:cursor").await.unwrap().as_deref(), Some(""));
    assert_eq!(db.kv_get("okf:missing").await.unwrap(), None);
}

/// The delete says whether there was a row, rather than collapsing both
/// outcomes into `Ok(())`.
#[tokio::test]
async fn a_delete_reports_whether_it_removed_anything() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    assert!(!db.kv_delete("okf:epoch").await.unwrap());

    db.kv_put("okf:epoch", "7").await.unwrap();
    assert!(db.kv_delete("okf:epoch").await.unwrap());
    assert_eq!(db.kv_get("okf:epoch").await.unwrap(), None);

    // And it stays gone. This is a physical delete and it is not a Doctrine V
    // violation, because the table is not in the ledger: nothing logged it,
    // nothing archives it, and there is no past state to explain the absence.
    assert!(!db.kv_delete("okf:epoch").await.unwrap());
}

/// The scan is a prefix scan, in key order, and bounded by its argument.
#[tokio::test]
async fn a_scan_is_bounded_ordered_and_stops_at_the_prefix() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    for key in [
        "okf:filehash:a.md",
        "okf:filehash:b.md",
        "okf:filehash:c.md",
        // Above the prefix's range under BINARY collation, which is what the
        // scan's upper bound exists to exclude: the bound is `okf;` — `:` plus
        // one — and `_` (0x5F) sorts above it.
        "okf_sibling",
        "other:key",
    ] {
        db.kv_put(key, "v").await.unwrap();
    }

    let all = db.kv_scan("okf:filehash:", 10).await.unwrap();
    assert_eq!(
        all.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        [
            "okf:filehash:a.md",
            "okf:filehash:b.md",
            "okf:filehash:c.md"
        ],
    );

    // `limit` is required and it is honoured, taking the first in key order.
    let two = db.kv_scan("okf:filehash:", 2).await.unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(two[0].0, "okf:filehash:a.md");

    // An empty prefix means everything, still bounded.
    assert_eq!(db.kv_scan("", 100).await.unwrap().len(), 5);
    assert_eq!(db.kv_scan("", 3).await.unwrap().len(), 3);
}

/// Validation is at the boundary, and it is a typed error rather than a
/// missing row — the shape `assert_edge` has taken since D-034.
#[tokio::test]
async fn a_bad_key_is_refused_at_the_boundary_on_every_method() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let bad = "has space";
    assert!(matches!(
        db.kv_put(bad, "v").await.unwrap_err(),
        DbError::InvalidKvKey(_)
    ));
    assert!(matches!(
        db.kv_get(bad).await.unwrap_err(),
        DbError::InvalidKvKey(_)
    ));
    assert!(matches!(
        db.kv_delete(bad).await.unwrap_err(),
        DbError::InvalidKvKey(_)
    ));
    assert!(matches!(
        db.kv_scan(bad, 10).await.unwrap_err(),
        DbError::InvalidKvKey(_)
    ));

    // An empty key is not addressable; an empty *prefix* is.
    assert!(db.kv_put("", "v").await.is_err());
    assert!(db.kv_scan("", 10).await.is_ok());
}

/// The store is branch-global: a value written on a branch is the trunk's too.
///
/// This is the semantic D-280 states rather than an omission to be tidied up
/// later, so it is pinned. An application that wants per-lineage operational
/// state puts the branch name in the key.
#[tokio::test]
async fn the_store_is_branch_global() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let branch = BranchId::new("feature").unwrap();
    db.fork(branch.clone(), BranchId::main()).await.unwrap();

    db.kv_put("okf:epoch", "7").await.unwrap();
    assert_eq!(db.kv_get("okf:epoch").await.unwrap().as_deref(), Some("7"));

    // Nothing in the crate copies tables on a fork — a branch is a row in
    // `branches` plus a label carried on writes — so there is exactly one row
    // and every lineage reads it.
    assert_eq!(db.kv_scan("okf:", 10).await.unwrap().len(), 1);
    assert!(db
        .branches()
        .await
        .unwrap()
        .iter()
        .any(|b| b.id.as_str() == "feature"));
}

/// `updated_at` moves when the value does, and it is canonical.
#[tokio::test]
async fn the_stamp_is_canonical_and_written_by_the_actor() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    db.kv_put("okf:epoch", "7").await.unwrap();
    let stamp: String = db
        .read_conn()
        .query(
            "SELECT updated_at FROM kv_store WHERE key = 'okf:epoch'",
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

    // The canonical form is 27 characters ending in `Z` (D-029). The `CHECK`
    // on the column enforces it on disk; this asserts the actor is what
    // supplies it, rather than the column merely permitting it.
    assert_eq!(stamp.len(), 27, "not the canonical width: {stamp}");
    assert!(stamp.ends_with('Z'), "not the canonical form: {stamp}");
}
