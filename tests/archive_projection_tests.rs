//! The archive's repair of `links_current` is keyed, and keyed must mean the
//! same thing as rebuilt (W14.1, [D-245], review C-1).
//!
//! Until 0.15.3 every archive session that deleted a link ran the **whole**
//! latest-belief projection again, inside its `BEGIN IMMEDIATE`, on the write
//! connection: `DELETE FROM links_current` plus an `INSERT … SELECT` over all
//! of `links`. D-077 measured that at 318 ms for 40,000 rows, and `Archive` is
//! budget-exempt, so no counter ever flagged it. The hold grew with the ledger
//! while the work that justified it — the archived rows — is a small and
//! shrinking fraction of it.
//!
//! The repair is now confined to the keys the session disturbed. That is a
//! claim about *equality*, not about speed, and this file is the equality.
//! Everything here is written so that a repair which is merely plausible fails:
//!
//! * A key whose last belief is archived must **leave** the projection. A
//!   compensation that deletes nothing gets this wrong, and it is the case a
//!   hand-written `DELETE … WHERE valid_to <= :cutoff` got wrong for real —
//!   see `archive_session`'s comment on Doctrine II.
//! * A key that merely shed a superseded row must **keep** its surviving
//!   belief, at the surviving weight. A compensation that deletes and forgets
//!   to re-derive gets this wrong.
//! * A key the session never touched must be **byte-identical** afterwards,
//!   which the whole-table comparison covers rather than the audit — an audit
//!   only asks whether `links_current` matches the projection of `links`.
//!
//! What no test in this file can see is a key set that is too **wide**. Such a
//! repair re-derives untouched partitions to the rows they already held, so it
//! is correct, passes everything here, and costs exactly what the full rebuild
//! cost — the defect this release exists to remove, wearing the fix's name.
//! That one is pinned where it is visible, on the key set itself:
//! `archive::tests::the_collected_keys_are_only_the_ones_the_delete_disturbs`.
//!
//! [D-245]: ../docs/architecture/s13-decision-register.md#d-245

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::integrity::{audit_current, rebuild_current};
use macrame::{ConceptUpsert, Database};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
/// Closed half an hour after `EPOCH`, which is before the cutoff every test
/// here passes and after the `valid_from` every row here carries. Kept on the
/// fake clock's own 1970 scale for the reason the harness gives: a stamp that
/// could be today's is a stamp nobody reads twice.
const CLOSED: &str = "1970-01-01T00:30:00.000000Z";
const HOUR: Duration = Duration::from_secs(3_600);

/// Every row of `links_current`, in a form two databases can be compared by.
async fn projection(
    conn: &libsql::Connection,
) -> Vec<(String, String, String, String, f64, String)> {
    let mut out = Vec::new();
    let mut rows = conn
        .query(
            "SELECT source_id, target_id, edge_type, valid_to, weight, branch_id \
             FROM links_current ORDER BY source_id, target_id, edge_type, valid_from, branch_id",
            (),
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
            row.get(4).unwrap(),
            row.get(5).unwrap(),
        ));
    }
    out
}

/// Four kinds of key, chosen so that each arm of the repair is load-bearing.
///
/// * `a → b` — two open beliefs an hour apart. The older is archivable by
///   `LINKS_ARCHIVABLE`'s first arm (a newer row exists at the key); the newer
///   is open, so the second arm cannot take it. **The key survives at weight
///   2.0**, and it is what a repair that deletes without re-deriving loses.
/// * `a → c` — one closed belief. The second arm takes it and nothing is left.
///   **The key leaves the projection**, and it is what a repair that
///   re-derives without deleting keeps forever.
/// * `a → d` — two closed beliefs. Both arms fire on the same key. **The key
///   leaves**, and it is the one a repair keyed on "rows deleted by the first
///   arm" would half-repair.
/// * `a → e` — one open belief, never superseded. **Untouched**, and the
///   comparison below is over the whole table precisely so that a repair which
///   re-derived this key as well would still have to get it right.
async fn seed(db: &Database, harness: &TestHarness) {
    db.write_concepts(
        ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| ConceptUpsert::new(*id, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();

    db.bulk_import(vec![
        EdgeAssertion::new("a", "b", "LINKS")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(1.0),
        EdgeAssertion::new("a", "c", "LINKS")
            .valid_from(EPOCH)
            .valid_to(CLOSED)
            .weight(1.0),
        EdgeAssertion::new("a", "d", "LINKS")
            .valid_from(EPOCH)
            .valid_to(CLOSED)
            .weight(1.0),
        EdgeAssertion::new("a", "e", "LINKS")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(1.0),
    ])
    .await
    .unwrap();

    harness.advance(HOUR);

    db.bulk_import(vec![
        EdgeAssertion::new("a", "b", "LINKS")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0),
        EdgeAssertion::new("a", "d", "LINKS")
            .valid_from(EPOCH)
            .valid_to(CLOSED)
            .weight(2.0),
    ])
    .await
    .unwrap();

    harness.advance(HOUR);
}

/// The keyed repair leaves the database the full rebuild would have left.
///
/// The comparison is against a rebuild run **on the archived database**, so it
/// is the projection of the same surviving `links` either way and the only
/// variable is which statement produced it. `audit_current` is asserted too,
/// but it is the weaker check: it compares `links_current` against the
/// projection of `links`, which is exactly what a *too wide* key set would
/// still satisfy.
#[tokio::test]
async fn the_keyed_repair_leaves_what_the_full_rebuild_would() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;

    let report = db.archive(&harness.clock.peek()).await.unwrap();
    assert!(
        report.links_archived >= 4,
        "the fixture archived nothing to repair around: {report:?}"
    );

    let after_keyed = projection(db.read_conn()).await;
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
    db.close().await.unwrap();

    // A writable handle of its own: `Database::read_conn` is read-only, and the
    // rebuild is a write. Same file, so it is the same surviving `links`.
    let raw = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    rebuild_current(&raw).await.unwrap();
    let after_full = projection(&raw).await;

    assert_eq!(
        after_keyed, after_full,
        "the keyed repair and the full rebuild disagree about current belief"
    );
}

/// The three outcomes, named row by row.
///
/// The test above would pass if the repair were wrong in a way the rebuild is
/// wrong in too — it compares two derivations of the same rule. This one
/// states the rule.
#[tokio::test]
async fn a_key_whose_last_belief_is_archived_leaves_and_a_superseded_one_stays() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;

    db.archive(&harness.clock.peek()).await.unwrap();

    let rows = projection(db.read_conn()).await;
    let targets: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();

    assert_eq!(
        targets,
        vec!["b", "e"],
        "a → c and a → d had their last belief archived and must be gone; \
         a → b shed a superseded row and must remain: {rows:?}"
    );
    assert_eq!(
        rows[0].4, 2.0,
        "the surviving belief at a → b is the later one, not the archived one"
    );
    assert_eq!(rows[1].4, 1.0, "a → e was never touched");

    db.close().await.unwrap();
}

/// An archive that deletes nothing leaves the projection alone, and says so
/// without running a repair at all.
///
/// [D-080](../docs/architecture/s13-decision-register.md#d-080) skips the
/// repair when the `DELETE` removed nothing. That skip predates the keyed
/// repair and is what made windowing affordable; it is pinned here because the
/// keyed repair makes it look redundant — cheap is not free, and an empty key
/// set still costs two statements per window.
#[tokio::test]
async fn an_archive_with_nothing_to_delete_leaves_the_projection_untouched() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;

    let before = projection(db.read_conn()).await;
    let report = db.archive(EPOCH).await.unwrap();
    assert_eq!(report.links_archived, 0, "the cutoff admitted something");

    assert_eq!(projection(db.read_conn()).await, before);
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);

    db.close().await.unwrap();
}
