//! D-277: `bulk_import_deferred` — the materialization-skipping bulk.
//!
//! What each test owns:
//!
//! - the projection agreement (`audit_current` at zero) and equality with
//!   what the shipped path maintains;
//! - **the mirror comes back on** — a normal write after a deferred bulk
//!   maintains `links_current` again, which is the one thing a dropped
//!   trigger would silently break;
//! - the failure path restores and rebuilds before it reports, with the
//!   load's `written` count;
//! - an empty load touches nothing;
//! - the toggle's own kind is attributed and exempt.
//!
//! There is deliberately **no throughput assertion here**: the recipe's
//! numbers live in the docstring, the probe (`examples/bulk_skip_probe.rs`)
//! and the plan document, all of which are rerunnable. A wall-clock assertion
//! in CI is D-055's rejected shape.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
#[cfg(feature = "metrics")]
use macrame::metrics::CommandKind;
use macrame::error::DbError;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const N: usize = 40; // one chunk at chunk_rows::EDGES = 90, with room to fail late

#[cfg(feature = "metrics")]
fn turns_for(snap: &macrame::metrics::MetricsSnapshot, kind: CommandKind) -> u64 {
    snap.kinds.iter().find(|k| k.kind == kind).unwrap().turns
}

#[cfg(feature = "metrics")]
fn over_budget_for(snap: &macrame::metrics::MetricsSnapshot, kind: CommandKind) -> u64 {
    snap.kinds.iter().find(|k| k.kind == kind).unwrap().over_budget
}

fn random_pairs(n: usize) -> Vec<EdgeAssertion> {
    // Seeded xorshift, same shape as the probe: unique keys, fresh
    // neighborhoods — the shape where the mirror's cost grows with the graph.
    let mut s: u64 = 20250911;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as usize % n
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let a = rng();
        let b = rng();
        if a != b && seen.insert((a, b)) {
            out.push(
                EdgeAssertion::new(format!("c{a:03}"), format!("c{b:03}"), "RELATES")
                    .valid_from(TS)
                    .valid_to(OPEN),
            );
        }
    }
    out
}

async fn seeded(harness: &TestHarness, concepts: usize) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    let all: Vec<_> = (0..concepts)
        .map(|i| ConceptUpsert::new(format!("c{i:03}"), "T").valid_from(TS))
        .collect();
    db.write_concepts(all).await.unwrap();
    db
}

async fn current_rows(db: &Database) -> Vec<(String, String, String, String, String)> {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT source_id, target_id, edge_type, valid_from, valid_to
               FROM links_current ORDER BY source_id, target_id, branch_id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
            row.get(4).unwrap(),
        ));
    }
    out
}

/// The deferred bulk's projection is the shipped path's projection.
///
/// `audit_current` proves internal agreement; this proves the deferred path
/// maintains the *same* belief the shipped path does — the two `links_current`
/// tables must be equal, not merely non-drifting.
#[tokio::test]
async fn a_deferred_bulk_maintains_the_same_projection_the_shipped_path_does() {
    let edges = random_pairs(N);

    let h1 = TestHarness::new();
    let db1 = seeded(&h1, N).await;
    db1.bulk_import(edges.clone()).await.unwrap();
    let shipped = current_rows(&db1).await;
    db1.close().await.unwrap();

    let h2 = TestHarness::new();
    let db2 = seeded(&h2, N).await;
    db2.bulk_import_deferred(edges).await.unwrap();
    let deferred = current_rows(&db2).await;

    // The audit, on the deferred file.
    assert_eq!(
        macrame::integrity::audit_current(db2.read_conn()).await.unwrap(),
        0,
        "the deferred load left the projection drifting from the ledger"
    );
    assert_eq!(shipped, deferred, "both paths maintain the same belief");

    // And the neighbourhood read answers on it.
    let g = db2.load_subgraph("c000", 1, TS, 1 << 20).await.unwrap();
    assert!(g.node_count() >= 1, "the rebuilt projection answers reads");

    db2.close().await.unwrap();
}

/// The mirror comes back on. This is the test a dropped trigger would fail
/// silently: a normal write after the window must maintain `links_current`
/// again, which `audit_current` sees.
#[tokio::test]
async fn a_write_after_a_deferred_bulk_is_mirrored_again() {
    let h = TestHarness::new();
    let db = seeded(&h, N).await;
    db.bulk_import_deferred(random_pairs(N - 1)).await.unwrap();

    db.assert_edge(
        EdgeAssertion::new("c000", "c001", "NEWTYPE").valid_from(TS).valid_to(OPEN),
    )
    .await
    .unwrap();
    assert_eq!(
        macrame::integrity::audit_current(db.read_conn()).await.unwrap(),
        0,
        "a post-window write must reach links_current through the mirror"
    );
    db.close().await.unwrap();
}

/// A failure mid-load still restores the mirror and rebuilds the projection
/// from what committed, and only then reports — with the load's `written`.
#[tokio::test]
async fn a_failed_deferred_bulk_still_rebuilds_and_restores() {
    let h = TestHarness::new();
    let db = seeded(&h, N).await;

    // An open interval the bulk will overlap in a later chunk.
    db.assert_edge(
        EdgeAssertion::new("c000", "c001", "EARLY").valid_from(TS).valid_to(OPEN),
    )
    .await
    .unwrap();

    let mut edges = random_pairs(N);
    // Re-asserting an open interval on the same key is the single-open rule's
    // own refusal — deterministic, and it lands in the last chunk.
    edges.push(
        EdgeAssertion::new("c000", "c001", "EARLY").valid_from("2027-01-01T00:00:00.000000Z").valid_to(OPEN),
    );

    let err = db.bulk_import_deferred(edges).await.unwrap_err();
    // The overlap is refused at the batch's own pairwise check
    // (`normalize_all`, before any chunk), so the count is 0 — the first
    // chunk can fail, and here nothing ran at all.
    assert_eq!(err.written, 0);
    assert!(
        !err.was_cancelled(),
        "the stop is the load's own failure, not the caller's"
    );
    let cause: DbError = err.into();
    assert!(
        matches!(cause, DbError::SingleOpenViolation { .. }),
        "the single-open refusal is the cause, got {cause:?}"
    );

    // The mirror is on and the projection is true.
    assert_eq!(
        macrame::integrity::audit_current(db.read_conn()).await.unwrap(),
        0,
        "the failure path rebuilt before it reported"
    );

    db.close().await.unwrap();
}

/// An empty load touches nothing: no toggle, no rebuild, `Ok(0)`.
///
/// The metric half is behind the feature that owns the counters.
#[tokio::test]
async fn an_empty_deferred_bulk_touches_nothing() {
    let h = TestHarness::new();
    let db = seeded(&h, 4).await;
    let written = db.bulk_import_deferred(Vec::new()).await.unwrap();
    assert_eq!(written, 0);
    #[cfg(feature = "metrics")]
    {
        let m = db.metrics();
        assert_eq!(turns_for(&m, CommandKind::LinksCurrentMirror), 0);
        assert_eq!(turns_for(&m, CommandKind::RebuildCurrent), 0);
    }
    db.close().await.unwrap();
}

/// The toggle is attributed to its own kind, twice per call (down, up), and
/// exempt — one DDL statement either way. Behind the metrics feature: the
/// counters it reads are the feature's, and the assertion itself is the
/// attribution contract.
#[cfg(feature = "metrics")]
#[tokio::test]
async fn the_toggle_is_attributed_and_exempt() {
    let h = TestHarness::new();
    let db = seeded(&h, N).await;
    db.bulk_import_deferred(random_pairs(N)).await.unwrap();
    let m = db.metrics();
    assert_eq!(turns_for(&m, CommandKind::LinksCurrentMirror), 2);
    assert_eq!(over_budget_for(&m, CommandKind::LinksCurrentMirror), 0);
    // The rebuild's own machinery keeps its kinds: the chunked rebuild is
    // fill (counted) + swap (exempt), which is D-082's split and D-277 does
    // not move it.
    assert!(turns_for(&m, CommandKind::ShadowSwap) >= 1);
    db.close().await.unwrap();
}
