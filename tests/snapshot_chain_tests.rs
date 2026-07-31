//! The snapshot chain is cross-checked against a genesis fold (T5.3, D-092).
//!
//! `write_final` composes onto the previous snapshot, so snapshot *n* descends
//! from snapshot *n−1* and nothing in the chain ever folds the whole log. An
//! error at any link propagates forward indefinitely and every read agrees with
//! it, because every read descends from it.
//!
//! **The load-bearing test here is
//! [`a_tampered_snapshot_is_caught`]**, not the agreement one. A checker that has
//! only ever been seen to pass is exactly the shape this project keeps finding
//! defects in — D-030's `audit_current` reduced to a constant zero and certified
//! any corruption as clean; D-071's FTS `'integrity-check'` reports healthy on
//! an emptied index. So the check is run against a snapshot that is deliberately
//! and specifically wrong, and it has to say so.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;
use macrame::temporal::{reconstruct, save_snapshot, MaterializedState};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
// After every `recorded_at` the real clock will produce during the test. The
// handle uses `SystemClock`, not the harness fake, so a 2026 instant would
// *predate* the log and `reconstruct` would refuse it as needing an archive.
const LATER: &str = "2099-01-01T00:00:00.000000Z";

/// A database with enough history that composition has something to compose.
async fn seeded(harness: &TestHarness) -> Database {
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();

    db.write_concepts(
        (0..20)
            .map(|i| {
                ConceptUpsert::new(format!("c{i:03}"), format!("Concept {i}"))
                    .content(format!("body {i}"))
                    .valid_from(TS)
            })
            .collect(),
    )
    .await
    .unwrap();

    db.bulk_import(
        (1..20)
            .map(|i| {
                EdgeAssertion::new("c000", format!("c{i:03}"), "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
            })
            .collect(),
    )
    .await
    .unwrap();

    db
}

/// With no snapshots on disk, both sides fold from genesis and agree.
///
/// The trivial case, and worth pinning because it is the one that says the
/// comparison itself introduces no difference — if this fails, any divergence
/// the other tests report would be an artifact of the comparator rather than of
/// the chain.
#[tokio::test]
async fn an_empty_snapshot_directory_agrees_with_itself() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    let check = db.verify_snapshot_chain(LATER).await.unwrap();
    assert!(!check.diverged(), "{check}");
    assert_eq!(check.composed_concepts, check.folded_concepts);
    assert_eq!(check.composed_edges, check.folded_edges);
    assert!(check.folded_concepts >= 20);

    db.close().await.unwrap();
}

/// An honestly written snapshot agrees with a genesis fold.
///
/// This is the property the chain is supposed to have, exercised through the
/// real writer rather than a hand-built state.
#[tokio::test]
async fn an_honest_snapshot_agrees_with_a_genesis_fold() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    // Write an anchor, then add history past it, so the composed answer is a
    // snapshot *plus a delta* rather than a snapshot alone. Without the second
    // round the composed path returns the snapshot verbatim and the anchored
    // fold never runs.
    macrame::temporal::write_final(db.read_conn(), db.snapshots_dir(), LATER, None)
        .await
        .unwrap();

    db.write_concepts(vec![ConceptUpsert::new("c000", "Renamed")
        .content("new body")
        .valid_from(TS)])
        .await
        .unwrap();
    db.write_concepts(vec![ConceptUpsert::new("c999", "Late")
        .content("late body")
        .valid_from(TS)])
        .await
        .unwrap();

    let check = db.verify_snapshot_chain(LATER).await.unwrap();
    assert!(!check.diverged(), "{check}");
    // And the composed path really did use the snapshot: it anchors at the file
    // it started from plus its delta, which is not the same as folding from
    // nothing. If these were equal the test would be passing because
    // composition never happened.
    assert!(
        check.composed_anchor > 0,
        "the composed answer did not use a snapshot, so this test proved nothing: {check}"
    );

    db.close().await.unwrap();
}

/// **A snapshot that is wrong is caught, and the report names what is wrong.**
///
/// The snapshot is rewritten in place with a legitimately serialised, correctly
/// named, correctly anchored file whose *contents* have been altered — a
/// concept's title changed, one concept removed, one edge removed. That is the
/// failure mode this check exists for: not a corrupt file, which zstd or bincode
/// would notice, but a **plausible** one, which nothing does.
#[tokio::test]
async fn a_tampered_snapshot_is_caught() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    macrame::temporal::write_final(db.read_conn(), db.snapshots_dir(), LATER, None)
        .await
        .unwrap();

    // Read the state the chain will descend from, corrupt it, write it back
    // under the same anchor so it stays the snapshot `snapshot_anchor` picks.
    let mut state: MaterializedState = reconstruct(db.read_conn(), LATER, None, None)
        .await
        .unwrap();
    let anchor = state.seq_anchor;
    assert!(anchor > 0);

    state
        .concepts
        .get_mut("c001")
        .expect("c001 must be in the fold")
        .title = "WRONG".to_string();
    state.concepts.remove("c002");
    let dropped = state.edges.pop().expect("there must be edges to drop");
    save_snapshot(db.snapshots_dir(), &state).unwrap();

    let check = db.verify_snapshot_chain(LATER).await.unwrap();

    assert!(
        check.diverged(),
        "a snapshot with a renamed concept, a missing concept and a missing \
         edge was certified as agreeing with the log: {check}"
    );
    assert!(
        check.concept_disagreements.contains(&"c001".to_string()),
        "the altered title was not reported: {check}"
    );
    assert!(
        check.concept_disagreements.contains(&"c002".to_string()),
        "the removed concept was not reported: {check}"
    );
    assert_eq!(
        check.edge_disagreements.len(),
        1,
        "expected exactly the dropped edge: {check}"
    );
    assert!(
        check.edge_disagreements[0].contains(&dropped.0)
            && check.edge_disagreements[0].contains(&dropped.1),
        "the reported edge is not the one dropped ({dropped:?}): {check}"
    );

    // And the message says what to do about it, since a divergence is a wrong
    // cache rather than a corrupt ledger.
    let text = check.to_string();
    assert!(text.contains("DIVERGED"), "{text}");
    assert!(
        text.contains("Doctrine VI") || text.contains("deleting the snapshot"),
        "the report does not say the snapshots are disposable: {text}"
    );

    db.close().await.unwrap();
}

/// Deleting the snapshots is the repair, and the check confirms it.
///
/// Pins the remedy the report names. If this stopped working, the report would
/// be telling callers to do something that does not fix their problem.
#[tokio::test]
async fn deleting_the_snapshots_restores_agreement() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    macrame::temporal::write_final(db.read_conn(), db.snapshots_dir(), LATER, None)
        .await
        .unwrap();
    let mut state: MaterializedState = reconstruct(db.read_conn(), LATER, None, None)
        .await
        .unwrap();
    state.concepts.remove("c003");
    save_snapshot(db.snapshots_dir(), &state).unwrap();
    assert!(db.verify_snapshot_chain(LATER).await.unwrap().diverged());

    for entry in std::fs::read_dir(db.snapshots_dir()).unwrap().flatten() {
        std::fs::remove_file(entry.path()).unwrap();
    }

    let check = db.verify_snapshot_chain(LATER).await.unwrap();
    assert!(
        !check.diverged(),
        "deleting the snapshots did not repair it: {check}"
    );

    db.close().await.unwrap();
}

/// The report is bounded, so a chain that went wrong early is still readable.
#[tokio::test]
async fn the_report_is_bounded() {
    use macrame::temporal::ChainCheck;

    let harness = TestHarness::new();
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();
    let n = ChainCheck::SAMPLE_LIMIT * 3;
    db.write_concepts(
        (0..n)
            .map(|i| {
                ConceptUpsert::new(format!("c{i:04}"), format!("C{i}"))
                    .content("body")
                    .valid_from(TS)
            })
            .collect(),
    )
    .await
    .unwrap();

    macrame::temporal::write_final(db.read_conn(), db.snapshots_dir(), LATER, None)
        .await
        .unwrap();

    // Empty the snapshot's concept map entirely: every concept disagrees.
    let mut state: MaterializedState = reconstruct(db.read_conn(), LATER, None, None)
        .await
        .unwrap();
    state.concepts.clear();
    save_snapshot(db.snapshots_dir(), &state).unwrap();

    let check = db.verify_snapshot_chain(LATER).await.unwrap();
    assert!(check.diverged());
    assert_eq!(check.concept_disagreements.len(), ChainCheck::SAMPLE_LIMIT);
    assert!(check.truncated, "a truncated report must say so: {check}");

    db.close().await.unwrap();
}
