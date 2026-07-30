//! `archive_windowed` — N sessions must mean the same thing as one (T1.1, D-080).
//!
//! The latency argument for windowing is measured in `examples/archive_window_diag.rs`
//! and is not what this file is for. What is here is the correctness obligation
//! that comes with splitting an operation D-012 requires to be atomic: the
//! *sequence* is not atomic, only each session is, so the burden is to show that
//! the sequence's **result** is indistinguishable from the single session's.
//!
//! Everything is driven by a `FakeClock`. The windows are bounds on transaction
//! time, so a test that let the wall clock supply `recorded_at` would be asking
//! for "one window per hour" against stamps that all land in the same
//! microsecond — it would pass, and it would be testing nothing.

mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::integrity::audit_current;
use macrame::{ConceptUpsert, Database, DbError};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

const HOUR: Duration = Duration::from_secs(3_600);

/// Four generations of the same 12 interval keys, one hour of transaction time
/// apart, so three generations are superseded and archivable and they are spread
/// across four distinct windows.
async fn seed(db: &Database, harness: &TestHarness) {
    let ids: Vec<String> = (0..13).map(|i| format!("c{i:03}")).collect();
    db.write_concepts(
        ids.iter()
            .map(|id| ConceptUpsert::new(id, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();

    for generation in 0..4 {
        let batch: Vec<_> = (0..12)
            .map(|k| {
                EdgeAssertion::new(&ids[k], &ids[k + 1], "LINKS")
                    .valid_from(EPOCH)
                    .valid_to(OPEN)
                    .weight(1.0 + generation as f64)
            })
            .collect();
        db.bulk_import(batch).await.unwrap();
        harness.advance(HOUR);
    }
}

/// Everything a caller can observe after an archive, as one comparable value.
async fn observable(db: &Database) -> (Vec<(String, String, f64)>, i64, Option<i64>) {
    let conn = db.read_conn();

    let mut current = Vec::new();
    let mut rows = conn
        .query(
            "SELECT source_id, target_id, weight FROM links_current \
             ORDER BY source_id, target_id",
            (),
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        current.push((row.get(0).unwrap(), row.get(1).unwrap(), row.get(2).unwrap()));
    }

    let hot_links: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();

    let horizon: Option<i64> = conn
        .query("SELECT MIN(seq_id) FROM transaction_log", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .and_then(|r| r.get(0).ok());

    (current, hot_links, horizon)
}

// ---------------------------------------------------------------------------
// The obligation windowing takes on
// ---------------------------------------------------------------------------

/// Nine one-hour sessions leave the database where one session would.
///
/// This is the load-bearing test of T1.1. D-012's atomicity requirement is *per
/// session* — copy-then-delete must not be split — and N sessions satisfy that
/// exactly as one does. What N sessions could still get wrong is the **result**:
/// a boundary computed from the wrong end, an off-by-one that skips a window, or
/// a final boundary that overshoots `cutoff` and archives rows the caller
/// excluded. All three would show up here and nowhere else.
#[tokio::test]
async fn windowing_leaves_the_same_database_a_single_session_would() {
    let one = {
        let harness = TestHarness::new();
        let db = harness.db_with_fake_clock().await;
        seed(&db, &harness).await;
        let cutoff = harness.clock.peek();

        let report = db.archive(&cutoff).await.unwrap();
        let state = observable(&db).await;
        assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
        db.close().await.unwrap();
        (report.links_archived, state)
    };

    let many = {
        let harness = TestHarness::new();
        let db = harness.db_with_fake_clock().await;
        seed(&db, &harness).await;
        let cutoff = harness.clock.peek();

        let reports = db.archive_windowed(&cutoff, HOUR).await.unwrap();
        assert!(
            reports.len() > 1,
            "the fixture spans four hours of transaction time but produced \
             {} session(s) — the boundaries are not being derived from the data",
            reports.len()
        );

        let state = observable(&db).await;
        assert_eq!(
            audit_current(db.read_conn()).await.unwrap(),
            0,
            "a windowed run induced drift in links_current"
        );
        db.close().await.unwrap();
        (reports.iter().map(|r| r.links_archived).sum::<usize>(), state)
    };

    assert_eq!(
        one.0, many.0,
        "the two runs archived different numbers of links"
    );
    assert_eq!(one.1, many.1, "the two runs left different databases");
    assert!(one.0 > 0, "the fixture archived nothing, so it proves nothing");
}

/// A window wider than the whole span is one session, and it is `archive()`.
#[tokio::test]
async fn a_window_wider_than_the_history_collapses_to_one_session() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;
    let cutoff = harness.clock.peek();

    let reports = db
        .archive_windowed(&cutoff, Duration::from_secs(86_400 * 365))
        .await
        .unwrap();

    assert_eq!(reports.len(), 1);
    db.close().await.unwrap();
}

/// An empty hot file still gets a session, so the horizon row is still written.
///
/// The tempting shortcut is to return an empty `Vec` and do nothing. That would
/// make `archive_windowed` and `archive` observably different on an empty
/// database — one writes an `archive_horizon` row and the other does not — and
/// the horizon is what a pre-horizon `reconstruct` uses to tell "archived" from
/// "never existed".
#[tokio::test]
async fn an_empty_database_still_gets_one_session_and_a_horizon_row() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    let reports = db
        .archive_windowed("2026-01-01T00:00:00.000000Z", HOUR)
        .await
        .unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].links_archived, 0);
    assert!(
        db.archive_path().exists(),
        "no cold file, so no horizon row was written"
    );
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Windows the caller cannot have meant
// ---------------------------------------------------------------------------

/// A zero window is refused rather than treated as "one session".
#[tokio::test]
async fn a_zero_window_is_refused() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;
    let cutoff = harness.clock.peek();

    let err = db
        .archive_windowed(&cutoff, Duration::ZERO)
        .await
        .expect_err("a zero-length window never advances");
    assert!(matches!(err, DbError::ArchiveWindow { .. }), "got {err:?}");

    db.close().await.unwrap();
}

/// A window so narrow it implies millions of sessions is refused, not clamped.
///
/// Clamping would archive over boundaries the caller did not choose, and they
/// would have no way to see that it happened. The error names the session count
/// and the limit, because "too narrow" is not actionable without both.
#[tokio::test]
async fn a_window_needing_more_than_the_session_limit_is_refused() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, &harness).await;
    let cutoff = harness.clock.peek();

    // Four hours of history in one-millisecond windows: 14.4 million sessions.
    let err = db
        .archive_windowed(&cutoff, Duration::from_millis(1))
        .await
        .expect_err("14 million archive sessions is not what anyone meant");

    match err {
        DbError::ArchiveWindow { ref reason, .. } => {
            assert!(
                reason.contains(&macrame::connection::MAX_ARCHIVE_SESSIONS.to_string()),
                "the refusal must quote the limit it is enforcing: {reason}"
            );
        }
        other => panic!("got {other:?}"),
    }

    // Refused, not partially applied: nothing ran.
    assert!(
        !db.archive_path().exists(),
        "a refused window still opened a cold file"
    );

    db.close().await.unwrap();
}
