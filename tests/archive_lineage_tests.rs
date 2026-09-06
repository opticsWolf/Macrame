//! The archive predicates and the lineage the folds already had (0.14.12, W12.12, D-229).
//!
//! v12 gave `links_current` a primary key ending in `branch_id` and gave the four
//! folds in `temporal::replay` a partition ending in `branch_id`. It did not give
//! either archive predicate one, because both spell their own SQL and neither is
//! built from the fold — the same shape as
//! [D-227](../docs/architecture/s13-decision-register.md#d-227), one layer down.
//!
//! A link's `entity_id` is `source|target|type|valid_from` and carries no lineage,
//! deliberately: re-keying it would have split every edge's history in two at the
//! rung. So "a later assertion for the same interval key" matched **across**
//! lineages, and one branch writing at an ancestor's key made the ancestor's own
//! open, current row archivable.
//!
//! **The reason this file exists rather than four assertions appended somewhere.**
//! `audit_current` returns 0 across the defect. `links_current` is honestly
//! re-derived from a `links` table that has been wrongly pruned, so the projection
//! *is* the image of the ledger and Doctrine VI's check has nothing to say. The
//! only way to see it is to ask what a lineage can still reach, before and after.

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::graph::{EdgeAssertion, TraversalBuilder};
use macrame::integrity::audit_current;
use macrame::{BranchId, ConceptUpsert, Database};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const T1: &str = "1970-01-02T00:00:00.000000Z";
const T2: &str = "1970-01-03T00:00:00.000000Z";
const LATE: &str = "2999-01-01T00:00:00.000000Z";
const STEP: Duration = Duration::from_secs(3_600);

/// `a → b → c` on the trunk, with the clock moved on so the fork point is after
/// every write and a cutoff of `LATE` is after everything.
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

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// What one lineage can still reach from `a` at `ts`, in order.
///
/// The instant is a parameter because a retirement's whole content is which side
/// of `valid_to` the reader stands on: at `EPOCH` a branch that retired an edge
/// over `[EPOCH, T1)` still sees it, and the case would pass while measuring
/// nothing.
async fn reached_at(db: &Database, branch: Option<&str>, ts: &str) -> Vec<String> {
    let mut b = TraversalBuilder::new("a");
    if let Some(n) = branch {
        b = b.on_branch(id(n));
    }
    let mut v = b.execute_ids(db.read_conn(), ts).await.unwrap();
    v.sort();
    v
}

async fn reached(db: &Database, branch: Option<&str>) -> Vec<String> {
    reached_at(db, branch, EPOCH).await
}

/// Every surviving `links` row at one key, as `"branch valid_to"`.
///
/// A count cannot answer this file's question. A retirement writes a new row and
/// thereby supersedes the lineage's own open one, so an archive after one
/// retirement legitimately takes a row — and "one row went cold" is true both
/// when the right one did and when the shadow did.
async fn rows_at(db: &Database, source: &str, target: &str) -> Vec<String> {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT branch_id || ' ' || valid_to FROM links \
             WHERE source_id = ?1 AND target_id = ?2 ORDER BY branch_id, valid_to",
            libsql::params![source, target],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

/// How many rows each ledger table still holds, as the archive's own footprint.
async fn counts(db: &Database) -> (i64, i64) {
    let conn = db.read_conn();
    let one = |sql: &'static str| {
        let conn = conn.clone();
        async move {
            conn.query(sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap()
        }
    };
    (
        one("SELECT COUNT(*) FROM links").await,
        one("SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'").await,
    )
}

// ───────────────────────────────────────────────────────────────────────────
// The defect, in both directions
// ───────────────────────────────────────────────────────────────────────────

/// A branch asserting at the trunk's edge key must not archive the trunk's row.
///
/// The measurement that named the release. Before the repair the trunk reached
/// `a` alone after one archive, having lost an edge it still currently believed,
/// because a *different lineage* had written at the same interval key.
#[tokio::test]
async fn a_branch_writing_at_the_trunks_key_does_not_archive_the_trunks_belief() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);
    db.archive(LATE).await.unwrap();

    assert_eq!(
        reached(&db, None).await,
        ["a", "b", "c"],
        "the trunk lost an edge it still believed, because a branch disagreed with it"
    );
    assert_eq!(reached(&db, Some("alt")).await, ["a", "b", "c"]);

    db.close().await.unwrap();
}

/// And the other way round: the trunk writing after a fork must not archive the
/// branch's own row.
///
/// Both directions matter and only one of them is the motivating story. The
/// predicate compares `recorded_at`, so whichever lineage wrote *second* was the
/// one that pruned the other — which means an abandoned branch could delete the
/// trunk's history simply by having been written to last.
#[tokio::test]
async fn the_trunk_writing_after_a_fork_does_not_archive_the_branchs_belief() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    // The trunk moves last, so *its* row is the newer one at this key.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(3.0),
    )
    .await
    .unwrap();
    h.advance(STEP);

    db.archive(LATE).await.unwrap();

    assert_eq!(
        reached(&db, Some("alt")).await,
        ["a", "b", "c"],
        "the branch lost an edge because the trunk wrote at the same key after it forked"
    );
    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);

    db.close().await.unwrap();
}

/// Archiving a branch's closed shadow row must not resurrect what it retired.
///
/// The second defect, and a different one: not supersession matching across
/// lineages but "a closed interval is history", which is true of a lineage
/// holding the only row at its key and false of a shadow. A branch retires an
/// inherited edge by writing its **own** closed row at the ancestor's key — the
/// only cross-lineage retirement Doctrine III permits. Archive that row and the
/// ancestor's open row wins the resolution again.
///
/// Measured before the repair: the branch reached `c` at `T2` after the archive,
/// having stopped believing it before the archive. A maintenance operation that
/// mints no assertions restored a belief the ledger had superseded.
#[tokio::test]
async fn archiving_a_shadow_row_does_not_resurrect_what_the_branch_retired() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.retire_edge_on("b", "c", "LEADSTO", EPOCH, T1, alt.id.clone())
        .await
        .unwrap();
    h.advance(STEP);

    assert_eq!(reached_at(&db, Some("alt"), T2).await, ["a", "b"]);
    assert_eq!(reached_at(&db, None, T2).await, ["a", "b", "c"]);

    db.archive(LATE).await.unwrap();

    assert_eq!(
        reached_at(&db, Some("alt"), T2).await,
        ["a", "b"],
        "the archive un-retired an edge -- a belief resurrected by an operation \
         that asserts nothing"
    );
    assert_eq!(
        reached_at(&db, None, T2).await,
        ["a", "b", "c"],
        "and the ancestor's own row was never the branch's to affect"
    );

    db.close().await.unwrap();
}

/// The ancestor's row is held back too, and that is the conservative half.
///
/// Strictly, only a row whose *ancestor* still holds the key must stay; the arm
/// stands down for **any** second lineage at the key, in both directions. That
/// is deliberate — ancestry would mean resolving the chain for every branch in
/// an operation that takes no branch parameter, and "still holds it after this
/// session" is self-referential. Leaving rows hot costs bytes and is never
/// wrong. This pins the cost so it is a measured choice rather than a surprise.
#[tokio::test]
async fn a_key_two_lineages_hold_keeps_its_closed_intervals_hot() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    // Both lineages close the same key. Neither row may go cold while the other
    // is hot, even though the trunk's would be archivable on its own.
    db.retire_edge_on("b", "c", "LEADSTO", EPOCH, T1, alt.id.clone())
        .await
        .unwrap();
    h.advance(STEP);
    db.retire_edge("b", "c", "LEADSTO", EPOCH, T1)
        .await
        .unwrap();
    h.advance(STEP);

    let report = db.archive(LATE).await.unwrap();

    // **Nothing goes cold, and this number moved at 0.15.26** ([D-269]). It read
    // 1 until then: retiring on the trunk superseded the trunk's own open row,
    // and the supersession arm took it. That arm now asks *when* as well as
    // whose, and `alt` forked between the open row and the retirement — so the
    // open row is the one `alt`'s cutoff still points at, and taking it would
    // have changed what a branch believes. The assertion below is the reason
    // the number is allowed to move: the answers are what they were.
    //
    // [D-269]: ../docs/architecture/s13-decision-register.md#d-269
    assert_eq!(report.links_archived, 0);
    assert_eq!(
        rows_at(&db, "b", "c").await,
        [
            format!("alt {T1}"),
            format!("main {T1}"),
            format!("main {OPEN}")
        ],
        "both closed rows stay hot: two lineages hold the key, so the \
         closed-interval arm stands down for both. The trunk's open row is the \
         third and is held for the other reason ([D-269]): `alt` forked after it \
         and reads it still"
    );
    assert_eq!(reached_at(&db, None, T2).await, ["a", "b"]);
    assert_eq!(reached_at(&db, Some("alt"), T2).await, ["a", "b"]);
    // At an instant inside the retired interval both lineages still believe the
    // edge, which is the reading the held row is being held for.
    assert_eq!(reached_at(&db, Some("alt"), EPOCH).await, ["a", "b", "c"]);

    db.close().await.unwrap();
}

/// A key only one lineage holds still sends its closed intervals cold — and
/// **"one lineage holds it" has to be true of the whole key**, not of one row
/// of it ([D-269], 0.15.26).
///
/// The arm the repair narrows is the one the cold file exists for, so the
/// narrowing has to stop where shadowing stops. That was D-229's sentence and
/// it was half of one. Shadowing is not the only thing that pins a row: a fork
/// point is a `recorded_at` a descendant reads its ancestors through, so a
/// branch goes on believing the row that was newest when it forked. This
/// fixture used to close the seed's own `a → b` — a key with a pre-fork row on
/// it — and assert both later rows cold, which took `alt`'s inheritance with
/// them and is the defect D-269 fixes.
///
/// So the key here is one that **did not exist when `alt` forked**. `a → c` is
/// written and retired entirely after the fork, `alt` never writes at it and
/// never inherited anything at it, and the archive behaves exactly as D-229
/// said it should: both rows cold.
///
/// [D-269]: ../docs/architecture/s13-decision-register.md#d-269
#[tokio::test]
async fn a_key_one_lineage_holds_still_archives_its_closed_interval() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    // Born after the fork: `alt` inherits nothing at this key, so nothing of
    // `alt`'s resolves to either row below.
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    h.advance(STEP);

    // The branch writes at a *different* key, so `a -> c` is the trunk's alone.
    db.assert_edge(
        EdgeAssertion::new("b", "c", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    db.retire_edge("a", "c", "LEADSTO", EPOCH, T1)
        .await
        .unwrap();
    h.advance(STEP);

    let report = db.archive(LATE).await.unwrap();

    // Two rows, and both belong in the cold file: the open row the retirement
    // superseded, and the closed row the retirement wrote. Nobody's shadow and
    // nobody's inheritance.
    assert_eq!(report.links_archived, 2);
    assert_eq!(
        rows_at(&db, "a", "c").await,
        [] as [String; 0],
        "the trunk's closed `a -> c` is nobody's shadow and belongs in the cold file"
    );
    assert_eq!(
        rows_at(&db, "b", "c").await.len(),
        2,
        "and the key two lineages do hold is untouched by this session"
    );

    db.close().await.unwrap();
}

/// **A fork pins the row its cutoff points at, with nobody writing on it.**
///
/// The case `lineage_property_tests.rs` found on its first run against a clean
/// tree, minimised by hand ([D-269]). Every operation is on the trunk: `alt`
/// forks, and then `a → b` is restated — the same key, the same interval, a new
/// row, which is what an idempotent importer writes on every run. The old row is
/// superseded *on its own lineage*, so D-229's clause had nothing to say, and
/// one ordinary `archive` took it.
///
/// It is the only row `alt` can resolve `a → b` to. The trunk kept reaching `b`
/// and the branch stopped, and no branch wrote anything anywhere in the history.
/// That is what separates this from D-229, which needed two lineages disagreeing
/// at a shared key: this needs one lineage agreeing with an older version of
/// itself while somebody else is pinned to it.
///
/// [D-269]: ../docs/architecture/s13-decision-register.md#d-269
#[tokio::test]
async fn a_fork_keeps_believing_the_row_the_trunk_restated_over() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let before = reached_at(&db, None, T2).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    // Idempotent restatement: same key, same interval, later `recorded_at`.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    h.advance(STEP);

    assert_eq!(reached_at(&db, Some("alt"), T2).await, before);
    let report = db.archive(LATE).await.unwrap();

    assert_eq!(
        reached_at(&db, Some("alt"), T2).await,
        before,
        "the branch believes what it believed; an archive is a move, not a \
         retirement (report: {report:?})"
    );
    assert_eq!(
        reached_at(&db, None, T2).await,
        before,
        "and the trunk, which was never at risk, is the control"
    );

    db.close().await.unwrap();
}

/// **And a held row must not resurrect the row that closed it** ([D-269]).
///
/// The second shape the generator found, and it exists only because of the
/// first repair. `alt` forks after the seed wrote `a → b` open, so that open row
/// is pinned and stays hot. The trunk then retires the edge, writing a closed
/// row at the same key — and the closed-interval arm was willing to take it,
/// because no *other lineage* holds the key. With the older row still hot, the
/// trunk's resolution falls back to it: the edge the trunk had retired comes
/// back, restored by an operation that mints no assertions.
///
/// That is exactly D-229's resurrection, reached through D-269's own repair,
/// which is why the fix is two clauses and not one. Worth stating plainly: the
/// first clause on its own is a regression, and only a generator that keeps
/// checking after every op would have said so.
///
/// [D-269]: ../docs/architecture/s13-decision-register.md#d-269
#[tokio::test]
async fn the_row_a_fork_pins_does_not_resurrect_the_retirement_over_it() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.retire_edge("a", "b", "LEADSTO", EPOCH, T1)
        .await
        .unwrap();
    h.advance(STEP);

    let before = reached_at(&db, None, T2).await;
    assert_eq!(before, ["a"], "the trunk retired its way out of the graph");

    let report = db.archive(LATE).await.unwrap();

    assert_eq!(
        reached_at(&db, None, T2).await,
        before,
        "the retirement stands: archiving the closed row would let the open row          the fork pinned win again (report: {report:?})"
    );
    assert_eq!(
        reached_at(&db, Some("alt"), T2).await,
        ["a", "b", "c"],
        "and `alt`, which forked before the retirement, never stopped believing it"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// What the repair must not have turned off
// ───────────────────────────────────────────────────────────────────────────

/// Same-lineage supersession still goes cold, on the trunk and on a branch alike.
///
/// A narrowing clause is worth nothing if it narrows to nothing. Each lineage
/// writes twice at one key, so each has exactly one superseded row, and the
/// archive must take both.
#[tokio::test]
async fn a_lineage_still_supersedes_itself() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    for (weight, branch) in [(2.0, None), (3.0, Some(alt.id.clone()))] {
        let mut e = EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(weight);
        if let Some(b) = branch {
            e = e.on_branch(b);
        }
        db.assert_edge(e).await.unwrap();
        h.advance(STEP);
    }
    // A third generation on each, so each lineage supersedes itself once.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(4.0),
    )
    .await
    .unwrap();
    h.advance(STEP);
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(5.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    let (links_before, _) = counts(&db).await;
    let report = db.archive(LATE).await.unwrap();
    let (links_after, _) = counts(&db).await;

    assert!(
        report.links_archived >= 2,
        "each lineage superseded itself once and both should have gone cold, got {}",
        report.links_archived
    );
    assert_eq!(links_before - links_after, report.links_archived as i64);
    assert_eq!(
        reached(&db, None).await,
        ["a", "b", "c"],
        "and current belief is untouched on both"
    );
    assert_eq!(reached(&db, Some("alt")).await, ["a", "b", "c"]);

    db.close().await.unwrap();
}

/// An unbranched database archives exactly what it archived before.
///
/// The clause is `newer.branch_id = links.branch_id`, and every row on a ledger
/// that has never forked carries `'main'`, so it is satisfied by every pair. This
/// says so as a measurement rather than as an argument about defaults.
#[tokio::test]
async fn a_ledger_that_never_forked_archives_what_it_always_did() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    for weight in [2.0, 3.0] {
        db.assert_edge(
            EdgeAssertion::new("a", "b", "LEADSTO")
                .valid_from(EPOCH)
                .valid_to(OPEN)
                .weight(weight),
        )
        .await
        .unwrap();
        h.advance(STEP);
    }

    let report = db.archive(LATE).await.unwrap();

    assert_eq!(
        report.links_archived, 2,
        "two superseded generations, and nothing about lineage in the way"
    );
    assert_eq!(reached(&db, None).await, ["a", "b", "c"]);
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Why nothing caught it
// ───────────────────────────────────────────────────────────────────────────

/// The drift audit is silent across this defect, by construction.
///
/// This is not a gap in `audit_current` and the test is here to say so. Doctrine
/// VI asks whether `links_current` is the image of `links`; the archive deletes
/// from `links` and then re-derives `links_current` from what survives, so the
/// answer is *yes* whether or not the right rows survived. An audit that could
/// see this would have to compare the ledger against something outside itself.
#[tokio::test]
async fn the_drift_audit_cannot_see_a_wrongly_pruned_ledger() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
    db.archive(LATE).await.unwrap();
    assert_eq!(
        audit_current(db.read_conn()).await.unwrap(),
        0,
        "zero before and zero after — the same answer the defect gave"
    );

    db.close().await.unwrap();
}

/// The newest log entry of every fold partition stays hot.
///
/// `LOG_ARCHIVABLE`'s docstring promised "the newest entry per entity", written
/// when the fold partitioned by entity. Since v12 it partitions by
/// `(table_name, entity_id, branch_id)`, so the promise had quietly become one
/// about a coarser unit than the thing it was protecting.
#[tokio::test]
async fn every_lineage_keeps_the_newest_log_entry_for_its_own_key() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(OPEN)
            .weight(2.0)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();
    h.advance(STEP);

    db.archive(LATE).await.unwrap();

    let mut rows = db
        .read_conn()
        .query(
            "SELECT branch_id, COUNT(*) FROM transaction_log \
             WHERE table_name = 'links' AND entity_id = ?1 GROUP BY branch_id ORDER BY branch_id",
            libsql::params![format!("a|b|LEADSTO|{EPOCH}")],
        )
        .await
        .unwrap();
    let mut per_lineage = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        per_lineage.push((r.get::<String>(0).unwrap(), r.get::<i64>(1).unwrap()));
    }

    assert_eq!(
        per_lineage,
        [("alt".to_string(), 1), ("main".to_string(), 1)],
        "one surviving entry per lineage, which is one per fold partition"
    );

    db.close().await.unwrap();
}
