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
            // `wal_autocheckpoint` one release later, and W5.4 the two cache
            // sizes the release after that — breaking exactly here and nowhere
            // else, twice. Left exhaustive so it keeps demonstrating that.
            clock: Some(clock),
            wal_autocheckpoint: WalCheckpointPolicy::Default,
            writer_cache_size: None,
            reader_cache_size: None,
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
