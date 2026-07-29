use std::sync::Arc;
use std::time::SystemTime;
use tempfile::TempDir;
use macrame::util::clock::FakeClock;

pub struct TestHarness {
    pub temp_dir: TempDir,
    pub db_path: std::path::PathBuf,
    pub clock: Arc<FakeClock>,
}

impl TestHarness {
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path().join("test_macrame.db");
        let clock = Arc::new(FakeClock::new(SystemTime::UNIX_EPOCH));
        Self {
            temp_dir,
            db_path,
            clock,
        }
    }
}
