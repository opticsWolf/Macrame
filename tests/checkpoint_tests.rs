//! `checkpoint()` moves the WAL back into the file, and says what it did (W5.2, D-156).
//!
//! The thing worth asserting is not that the pragma runs — that is one line —
//! but that the report is *read from* SQLite rather than invented. A method
//! returning a hardcoded `CheckpointReport::default()` would pass any test that
//! only checks `Ok(())`, and the entire reason this returns a struct instead of
//! `()` is that a checkpoint which did nothing and a checkpoint which reclaimed
//! the whole WAL are otherwise indistinguishable to the caller.
//!
//! So the assertions are on the *file*: the `-wal` sidecar is measured before
//! and after, and the frame count in the report is checked against it changing.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const T1: &str = "2026-01-01T00:00:00.000000Z";

/// Size of the `-wal` sidecar in bytes, or 0 if it is not there.
fn wal_bytes(harness: &TestHarness) -> u64 {
    let mut wal = harness.db_path.clone().into_os_string();
    wal.push("-wal");
    std::fs::metadata(std::path::PathBuf::from(wal))
        .map(|m| m.len())
        .unwrap_or(0)
}

async fn db(harness: &TestHarness) -> Database {
    Database::open_tuned(
        &harness.db_path,
        Tuning::default().cadence(CadencePolicy::Disabled),
    )
    .await
    .unwrap()
}

/// Enough writes to put frames in the WAL without tripping the 1,000-page
/// automatic checkpoint, which would do this test's work for it.
async fn write_some(db: &Database) {
    for i in 0..40 {
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i}"), format!("Concept {i}")).valid_from(T1),
        )
        .await
        .unwrap();
    }
}

/// **A checkpoint truncates the WAL, and reports having done so.**
///
/// Both halves matter. The file check alone would pass for a method that ran
/// the pragma and returned a fabricated report; the report check alone would
/// pass for a method that read the row and never truncated.
#[tokio::test]
async fn a_checkpoint_reclaims_the_wal_and_reports_the_frames_it_moved() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    write_some(&db).await;

    let before = wal_bytes(&harness);
    assert!(before > 0, "the fixture wrote nothing to the WAL");

    let report = db.checkpoint().await.unwrap();

    assert!(
        !report.busy,
        "nothing else holds this database; a busy checkpoint here means the \
         write connection is blocking itself"
    );
    assert!(
        report.checkpointed_frames > 0,
        "the WAL was {before} bytes and the report claims 0 frames moved — \
         the report is not being read from SQLite"
    );
    assert_eq!(
        report.log_frames, 0,
        "TRUNCATE leaves no frames behind; a non-zero count means a weaker \
         mode is being run than the one documented"
    );
    assert!(report.is_complete());
    assert_eq!(
        wal_bytes(&harness),
        0,
        "the report says truncated and the -wal file disagrees"
    );

    db.close().await.unwrap();
}

/// **A checkpoint on a quiet database is a no-op that says so**, rather than an
/// error or a fabricated frame count.
#[tokio::test]
async fn a_second_checkpoint_moves_nothing_and_is_still_ok() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    write_some(&db).await;

    db.checkpoint().await.unwrap();
    let second = db.checkpoint().await.unwrap();

    assert!(second.is_complete());
    assert_eq!(
        second.checkpointed_frames, 0,
        "there was nothing left to move, so a non-zero count is invented"
    );

    db.close().await.unwrap();
}

/// **The ledger survives it.** A checkpoint rewrites where the bytes live, and
/// the one way it could be catastrophic is by losing a commit that was still
/// only in the WAL.
#[tokio::test]
async fn the_rows_written_before_a_checkpoint_are_still_there_after_it() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    write_some(&db).await;
    db.checkpoint().await.unwrap();

    let mut rows = db
        .read_conn()
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 40, "a checkpoint lost committed rows");

    db.close().await.unwrap();
}

/// **It is attributed to its own kind, and it is budget-exempt.**
///
/// The exemption is the half that would fail silently: a checkpoint's hold is a
/// function of accumulated WAL and will exceed 3 ms on any real database, so
/// counting it as a violation would make `violations()` useless for exactly the
/// callers who checkpoint — which is [D-152]'s trap, one wave later.
#[cfg(feature = "metrics")]
#[tokio::test]
async fn a_checkpoint_is_counted_as_a_checkpoint_and_exempt_from_the_budget() {
    use macrame::metrics::{CommandKind, MetricsSnapshot};

    fn turns(snap: &MetricsSnapshot, kind: CommandKind) -> u64 {
        snap.kinds.iter().find(|k| k.kind == kind).unwrap().turns
    }

    assert!(
        CommandKind::Checkpoint.exempt_from_budget(),
        "a checkpoint's hold is not the caller's to bound"
    );

    let harness = TestHarness::new();
    let db = db(&harness).await;
    write_some(&db).await;

    let before = turns(&db.metrics(), CommandKind::Checkpoint);
    db.checkpoint().await.unwrap();
    let after = turns(&db.metrics(), CommandKind::Checkpoint);

    assert_eq!(
        after,
        before + 1,
        "the checkpoint turn was attributed to some other kind"
    );

    db.close().await.unwrap();
}
