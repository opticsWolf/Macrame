use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

use macrame::util::FakeClock;
use macrame::Database;

/// A temp directory, a database path, and a clock that can be injected.
///
/// **`clock` used to be constructed here and injected nowhere** — defect K, open
/// since 0.5.2, and the source of the `field is never read` warning on every
/// build of every test binary. `Database::open_with_clock` (D-062) is what it was
/// waiting for; [`TestHarness::db_with_fake_clock`] is the path that uses it.
pub struct TestHarness {
    /// Held, not read: dropping it deletes the directory `db_path` points into,
    /// so the harness must outlive the database. Named rather than `_temp_dir`
    /// because a test that wants a second file in the same directory needs it.
    #[allow(dead_code)]
    pub temp_dir: TempDir,
    pub db_path: std::path::PathBuf,
    pub clock: Arc<FakeClock>,
}

impl Default for TestHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl TestHarness {
    pub fn new() -> Self {
        Self::starting_at(SystemTime::UNIX_EPOCH)
    }

    /// A harness whose fake clock starts at `initial`.
    ///
    /// The epoch is the default because it is unmistakable in a failure message:
    /// a `1970-…` stamp in an assertion is obviously the fake, where a plausible
    /// recent date could be either clock.
    pub fn starting_at(initial: SystemTime) -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path().join("test_macrame.db");
        let clock = Arc::new(FakeClock::new(initial));
        Self {
            temp_dir,
            db_path,
            clock,
        }
    }

    /// Open the harness database driven by [`Self::clock`].
    ///
    /// No snapshot cadence: a background task taking its own stamps would make
    /// the sequence a function of timing, which is the property injecting a
    /// clock exists to remove.
    #[allow(dead_code)] // not every test binary needs a driven clock
    pub async fn db_with_fake_clock(&self) -> Database {
        Database::open_with_clock(&self.db_path, None, Arc::clone(&self.clock) as _)
            .await
            .expect("failed to open database with the fake clock")
    }

    /// Move the fake clock forward.
    #[allow(dead_code)]
    pub fn advance(&self, d: Duration) {
        self.clock.advance(d);
    }
}

// -- The state-rule side table (0.21.0, D-303) --------------------------------
//
// `concept_disorder` arrives with the v24 rung (plan §3.3). Until that rung
// lands, the tests that must behave like post-rung ledgers provision it
// themselves — same shape, same key, same never-clears semantics. When the
// rung lands these helpers become truth-neutral (the rung's own DDL and the
// write path's inserts take over) and they stay for the pre-v24 fallback
// test, which is the one place a ledger without the table is built on
// purpose.

/// The one-off local write handle the marks DDL and inserts go through.
///
/// `db.read_conn()` is read-only, and every other write queueues through the
/// crate's write actor; a fixture's DDL is neither. This is also the handle
/// discipline the shape-5 probe paid for: local libsql keeps per-connection
/// caches, so the test opens the fixture handle only for its own write and
/// drops it before any read runs.
#[allow(dead_code)]
async fn marks_conn(path: &std::path::Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

/// Provision the side table with the rung's shape: `(entity_id, branch_id)`,
/// one row per pair a skipped backfill was ever written on.
#[allow(dead_code)]
pub async fn ensure_marks_table(db: &Database) {
    marks_conn(db.path())
        .await
        .execute(
            "CREATE TABLE IF NOT EXISTS concept_disorder (             entity_id TEXT NOT NULL, branch_id TEXT NOT NULL,              PRIMARY KEY (entity_id, branch_id))",
            (),
        )
        .await
        .unwrap();
}

/// Add a mark, as the skip handler (post-rung) and the rung's fill (for
/// history) will: idempotent, and never removing anything.
#[allow(dead_code)]
pub async fn mark_disordered(db: &Database, entity: &str, branch: &str) {
    ensure_marks_table(db).await;
    marks_conn(db.path())
        .await
        .execute(
            "INSERT OR IGNORE INTO concept_disorder (entity_id, branch_id)              VALUES (?1, ?2)",
            (entity, branch),
        )
        .await
        .unwrap();
}

/// Every pair the side table holds, stable-ordered for assertions.
#[allow(dead_code)]
pub async fn marks(db: &Database) -> Vec<(String, String)> {
    let mut rows = marks_conn(db.path())
        .await
        .query(
            "SELECT entity_id, branch_id FROM concept_disorder              ORDER BY entity_id, branch_id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    out
}

/// The marks table removed: features what a pre-v24 ledger is, and the state
/// the missing-table fallback in the read side exists for.
#[allow(dead_code)]
pub async fn drop_marks_table(db: &Database) {
    marks_conn(db.path())
        .await
        .execute("DROP TABLE IF EXISTS concept_disorder", ())
        .await
        .unwrap();
}
