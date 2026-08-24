//! `Tuning` is the constructor surface, and its default is not a trap (W5.1, D-155).
//!
//! The three older constructors delegate to `open_tuned`, so the thing worth
//! testing is not that each one still opens a database — every other test file
//! does that — but the two places where a consolidation can go quietly wrong:
//!
//! 1. **The default must mean what `open()` means.** `Tuning` derives `Default`
//!    so that a new knob is additive, which puts a `Tuning::default()` in the
//!    hands of every caller who wrote `..Default::default()`. If that default
//!    disabled the snapshot cadence — which is exactly what
//!    `Option<SnapshotCadence>` would have given it, since `None` there means
//!    *off* — then `open_tuned(path, Tuning::default())` and `open(path)` would
//!    read as synonyms while one of them stopped writing anchors.
//! 2. **`Disabled` must still be reachable**, or the tri-state has only
//!    replaced one trap with a missing capability.
//!
//! Both are asserted through the snapshot directory, because that is the
//! observable the cadence produces and the one a caller would notice missing.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;
use std::sync::Arc;

const T1: &str = "2026-01-01T00:00:00.000000Z";

/// A cadence that fires on any growth at all, so a short test can observe it.
fn eager() -> SnapshotCadence {
    SnapshotCadence {
        every_entries: 1,
        poll_interval: std::time::Duration::from_millis(5),
    }
}

async fn write_one(db: &Database) {
    db.write_concepts(vec![ConceptUpsert::new("a", "A").valid_from(T1)])
        .await
        .unwrap();
}

/// Snapshot files present after `close()`, which always writes a final anchor.
fn anchors(harness: &TestHarness) -> usize {
    std::fs::read_dir(harness.db_path.with_file_name(format!(
        "{}_snapshots",
        harness.db_path.file_stem().unwrap().to_str().unwrap()
    )))
    .map(|d| d.count())
    .unwrap_or(0)
}

/// **`Tuning::default()` runs the cadence, because `open()` runs the cadence.**
///
/// The assertion is on `CadencePolicy::default()` rather than on file counts,
/// because the cadence is a background task on a timer and a count is a race.
/// What is being guarded is the mapping — that the default is `Default` and not
/// `Disabled` — and that is a value, not a schedule.
#[test]
fn the_default_tuning_asks_for_the_default_cadence() {
    let tuning = Tuning::default();
    assert_eq!(
        tuning.cadence,
        CadencePolicy::Default,
        "Tuning::default() must mean what Database::open() means. If this is \
         Disabled, every caller who wrote `..Default::default()` has silently \
         stopped writing snapshot anchors."
    );
    assert!(
        tuning.clock.is_none(),
        "the default clock is the SystemClock, chosen by absence"
    );
}

/// **`Disabled` is reachable and does what `open_with_cadence(path, None)` did.**
#[tokio::test]
async fn a_disabled_cadence_leaves_the_snapshot_directory_to_close() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    write_one(&db).await;
    // No cadence task exists to race with, so this is a statement about the
    // steady state rather than a sleep-and-hope.
    assert_eq!(anchors(&harness), 0, "nothing but close() writes an anchor");
    db.close().await.unwrap();
    assert_eq!(anchors(&harness), 1, "close() still writes the final one");
}

/// **An explicit cadence is honoured**, which is the third arm of the tri-state.
#[tokio::test]
async fn an_explicit_cadence_anchors_without_being_closed() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Every(eager()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for _ in 0..3 {
        write_one(&db).await;
    }
    // Poll rather than sleep once: the cadence is a timer and the bound is
    // generous, so this fails on a real regression rather than on a slow box.
    let mut seen = 0;
    for _ in 0..100 {
        seen = anchors(&harness);
        if seen > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(seen > 0, "an eager cadence wrote no anchor in two seconds");
    db.close().await.unwrap();
}

/// **The injected clock arrives through `Tuning` as it does through
/// `open_with_clock`**, including the floor.
///
/// Asserted on a stamp rather than on identity, because the delegation could
/// compile perfectly while dropping the `Option` on the floor.
#[tokio::test]
async fn a_clock_injected_through_tuning_stamps_the_ledger() {
    let harness = TestHarness::new();
    let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
    let clock = Arc::new(FakeClock::new(t0));
    let expected = clock.peek();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            // Every field named, deliberately. D-155 says an exhaustive
            // literal breaks when a field is added, with a compile error at the
            // call site rather than a behaviour change — and W5.3 added
            // `wal_autocheckpoint` one release later, W5.4 the two cache sizes
            // the release after that, and W7.4 `future_stamps` — breaking
            // exactly here and nowhere else, three times. Left exhaustive so it
            // keeps demonstrating that.
            clock: Some(clock),
            wal_autocheckpoint: WalCheckpointPolicy::Default,
            writer_cache_size: None,
            reader_cache_size: None,
            future_stamps: FutureStampPolicy::Default,
        },
    )
    .await
    .unwrap();
    write_one(&db).await;
    let mut rows = db
        .read_conn()
        .query("SELECT recorded_at FROM concepts WHERE id = 'a'", ())
        .await
        .unwrap();
    let stamp: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        stamp, expected,
        "the injected clock did not reach the actor through Tuning"
    );
    db.close().await.unwrap();
}

/// **The old constructors still map `None` to disabled**, which is the
/// compatibility half of `from_legacy`.
///
/// If the mapping were inverted — `None` becoming `CadencePolicy::Default` —
/// every existing caller of `open_with_cadence(path, None)` would acquire a
/// background task they had explicitly declined, and nothing else in the suite
/// would notice, because an extra anchor is not an error.
#[tokio::test]
async fn the_legacy_none_still_means_disabled() {
    let harness = TestHarness::new();
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();
    write_one(&db).await;
    assert_eq!(
        anchors(&harness),
        0,
        "open_with_cadence(path, None) acquired a cadence it declined"
    );
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// W7.4 / D-178: a `recorded_at` from the future is refused at open.
// ---------------------------------------------------------------------------

/// Seconds since the epoch for a stamp far enough ahead to be unambiguous.
///
/// Chosen absolutely rather than as `now + n`, because the condition being
/// tested is a stored value that no clock on this machine could have produced,
/// and a relative fixture would drift into "plausible" as the tolerance grows.
const A_STAMP_FROM_THE_FUTURE: u64 = 3_000_000_000; // 2065-01-24

/// Write one concept under a clock set to `secs`, then close cleanly.
async fn seed_at(path: &std::path::Path, secs: u64) {
    let clock = Arc::new(FakeClock::new(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
    ));
    let db = Database::open_tuned(
        path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            clock: Some(clock),
            ..Default::default()
        },
    )
    .await
    .expect("seeding must succeed: an empty database has no floor to refuse");
    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T1))
        .await
        .unwrap();
    db.close().await.unwrap();
}

/// Reopening a database stamped in the future is refused, by name (W7.4, D-178).
///
/// The floor is `MAX(recorded_at)` and the clock is raised to it, so absorbing
/// this stamp would make every subsequent write inherit it — into rows the open
/// after that reads back. This is the one bad value in the file that
/// manufactures more of itself, and the refusal is placed where the crate can
/// still tell a stamp it wrote from one it did not.
#[tokio::test]
async fn a_recorded_at_from_the_future_is_refused_at_open() {
    let harness = TestHarness::new();
    seed_at(&harness.db_path, A_STAMP_FROM_THE_FUTURE).await;

    let err = match Database::open(&harness.db_path).await {
        Ok(_) => panic!("a floor from 2065 must not be absorbed"),
        Err(e) => e,
    };

    match &err {
        DbError::FutureRecordedAt { stamp, limit } => {
            assert!(stamp.starts_with("2065-"), "the stored stamp: {stamp}");
            assert!(
                limit.as_str() > "2026-",
                "the limit must be a real timestamp: {limit}"
            );
        }
        other => panic!("expected FutureRecordedAt, got {other:?}"),
    }
    // And it must say how to get in, since the crate that refuses the file is
    // the only thing that can read it.
    // The message names the knob and not the Rust spelling: it crosses to
    // Python verbatim, and a caller there cannot write a `Tuning` literal.
    assert!(err.to_string().contains("future_stamps"), "{err}");
    assert!(err.to_string().contains("allow"), "{err}");
}

/// `Allow` opens it, which is the reading path and not a repair (W7.4, D-178).
///
/// Asserted together with the refusal because an escape hatch nobody has
/// exercised is a claim in a rustdoc: a caller reaches for it exactly once, in
/// the situation where being wrong about it is most expensive.
#[tokio::test]
async fn allow_opens_a_database_the_default_refuses() {
    let harness = TestHarness::new();
    seed_at(&harness.db_path, A_STAMP_FROM_THE_FUTURE).await;

    let db = Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            future_stamps: FutureStampPolicy::Allow,
            ..Default::default()
        },
    )
    .await
    .expect("Allow must waive the bound");

    let n: i64 = db
        .read_conn()
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(n, 1, "the file must be readable, which is the whole point");
    db.close().await.unwrap();
}

/// An ordinary database is unaffected, and that is half the claim (W7.4, D-178).
///
/// A check on `MAX(recorded_at)` that refused anything a real clock produces
/// would be caught by the rest of the suite — but only by the tests that
/// reopen, and most do not. This asserts the negative directly: a database
/// written by the wall clock, closed, and reopened under the default policy.
#[tokio::test]
async fn a_database_stamped_now_reopens_under_the_default() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T1))
        .await
        .unwrap();
    db.close().await.unwrap();

    let db = Database::open(&harness.db_path)
        .await
        .expect("wall-clock stamps are not from the future");
    db.close().await.unwrap();
}

/// `Tolerance(ZERO)` is the strict form and it reaches the same refusal.
///
/// The two ends of the knob, so neither `Default` nor `Allow` can be the only
/// arm anyone has run. `ZERO` refuses any stamp at all ahead of the wall clock,
/// which is why the fixture only has to be *slightly* ahead.
#[tokio::test]
async fn a_zero_tolerance_refuses_a_stamp_that_the_default_would_accept() {
    let harness = TestHarness::new();
    let soon = std::time::SystemTime::now() + std::time::Duration::from_secs(60 * 60);
    let secs = soon
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    seed_at(&harness.db_path, secs).await;

    // An hour ahead is inside the default day-wide tolerance.
    Database::open(&harness.db_path)
        .await
        .expect("an hour is within the default tolerance")
        .close()
        .await
        .unwrap();

    // And outside a zero one.
    let err = Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            future_stamps: FutureStampPolicy::Tolerance(std::time::Duration::ZERO),
            ..Default::default()
        },
    )
    .await;
    let err = match err {
        Ok(_) => panic!("zero tolerance admits nothing ahead of the wall clock"),
        Err(e) => e,
    };
    assert!(matches!(err, DbError::FutureRecordedAt { .. }), "{err:?}");
}
