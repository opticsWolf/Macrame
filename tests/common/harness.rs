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
