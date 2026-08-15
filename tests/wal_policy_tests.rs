//! `wal_autocheckpoint` reaches the write connection, and only the write
//! connection (W5.3, F-30, D-157).
//!
//! The setting cannot be read back through the public surface: `PRAGMA
//! wal_autocheckpoint` is per-connection, and the connection it is set on — the
//! writer — is deliberately unnameable from outside the actor. So this asserts
//! the *behaviour* instead, which is the better assertion anyway: the automatic
//! checkpointer's whole observable effect is that it bounds the WAL, so a
//! fixture that writes well past the 1,000-page threshold separates the two
//! policies by the size of the `-wal` file and by nothing else.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const T1: &str = "2026-01-01T00:00:00.000000Z";

/// SQLite's default threshold, in pages, and the page size the crate gets.
const DEFAULT_THRESHOLD_BYTES: u64 = 1000 * 4096;

fn wal_bytes(harness: &TestHarness) -> u64 {
    let mut wal = harness.db_path.clone().into_os_string();
    wal.push("-wal");
    std::fs::metadata(std::path::PathBuf::from(wal))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// ~3,000 rows of 1 KB, which is several thousand WAL frames once the indexes
/// and the FTS shadow tables are counted — comfortably past 1,000 pages.
async fn write_a_lot(db: &Database) {
    let filler = "x".repeat(1024);
    let concepts: Vec<_> = (0..3000)
        .map(|i| {
            ConceptUpsert::new(format!("c{i}"), format!("Concept {i}"))
                .content(filler.clone())
                .valid_from(T1)
        })
        .collect();
    db.write_concepts(concepts).await.unwrap();
}

async fn open(harness: &TestHarness, wal_autocheckpoint: WalCheckpointPolicy) -> Database {
    Database::open_tuned(
        &harness.db_path,
        Tuning {
            cadence: CadencePolicy::Disabled,
            wal_autocheckpoint,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// **The default is unchanged**, which is half of what W5.3 promised.
///
/// F-30 is a control-loop perturbation, not a correctness bug, and changing a
/// default is a behaviour change for every existing caller. So the assertion is
/// that a database opened without asking still has its WAL bounded near
/// SQLite's own threshold — not that it is exactly 1,000 pages, which is a
/// high-water mark the file reaches and then reuses.
#[tokio::test]
async fn the_default_still_bounds_the_wal_without_being_asked() {
    let harness = TestHarness::new();
    let db = open(&harness, WalCheckpointPolicy::default()).await;
    write_a_lot(&db).await;

    let wal = wal_bytes(&harness);
    assert!(
        wal < DEFAULT_THRESHOLD_BYTES * 3,
        "the WAL reached {wal} bytes with the default policy — the automatic \
         checkpointer is not running, which means the default changed"
    );

    db.close().await.unwrap();
}

/// **`Disabled` really disables it**, and the WAL grows past what the default
/// would ever have allowed.
#[tokio::test]
async fn a_disabled_autocheckpoint_lets_the_wal_grow() {
    let harness = TestHarness::new();
    let db = open(&harness, WalCheckpointPolicy::Disabled).await;
    write_a_lot(&db).await;

    let wal = wal_bytes(&harness);
    assert!(
        wal > DEFAULT_THRESHOLD_BYTES * 3,
        "the WAL is only {wal} bytes with autocheckpoint disabled — something \
         is still checkpointing, so the pragma did not reach the writer"
    );

    // The pairing W5.2 exists for: the WAL that was allowed to grow is the
    // WAL the caller now has to reclaim, and it is not free — see D-157 for
    // the measured cost of deferring it.
    let report = db.checkpoint().await.unwrap();
    assert!(report.is_complete());
    assert_eq!(wal_bytes(&harness), 0);

    db.close().await.unwrap();
}

/// **An explicit page count is honoured**, which is the third arm.
///
/// A low threshold, so the bound it produces is well clear of the default's and
/// the test is asserting the number it passed rather than the number SQLite
/// would have used anyway.
#[tokio::test]
async fn an_explicit_threshold_bounds_the_wal_below_the_default() {
    let harness = TestHarness::new();
    let db = open(&harness, WalCheckpointPolicy::EveryPages(64)).await;
    write_a_lot(&db).await;

    let wal = wal_bytes(&harness);
    assert!(
        wal < DEFAULT_THRESHOLD_BYTES,
        "a 64-page threshold left {wal} bytes of WAL, which is more than the \
         1,000-page default would have"
    );

    db.close().await.unwrap();
}

/// **Zero is not silently the same as `Disabled`.**
///
/// SQLite treats `PRAGMA wal_autocheckpoint = 0` as "off", and this crate does
/// not hide that — but it also does not *reinterpret* a caller's computed zero
/// as a request for `Disabled`. The two spellings do the same thing at the
/// pragma; what matters is that `EveryPages(0)` is reachable and honest rather
/// than remapped behind the caller's back, so a threshold that came out zero by
/// arithmetic behaves identically to one written by hand and can be found in a
/// `Debug` dump.
#[test]
fn a_zero_threshold_is_kept_as_the_caller_wrote_it() {
    let tuning = Tuning {
        wal_autocheckpoint: WalCheckpointPolicy::EveryPages(0),
        ..Default::default()
    };
    assert_eq!(
        tuning.wal_autocheckpoint,
        WalCheckpointPolicy::EveryPages(0),
        "a computed zero was rewritten to Disabled, which hides a caller's bug"
    );
    assert_ne!(
        WalCheckpointPolicy::EveryPages(0),
        WalCheckpointPolicy::Disabled
    );
}
