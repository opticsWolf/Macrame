use std::sync::Mutex;
use std::time::SystemTime;
use crate::error::Result;
use crate::util::timestamp;

/// Trait defining the clock interface for timestamp generation.
/// CONTRACT: successive calls return strictly increasing values,
/// even across application restarts and NTP corrections.
pub trait Clock: Send + Sync {
    /// Returns the current timestamp as ISO-8601 UTC string (e.g. "YYYY-MM-DDTHH:MM:SS.ffffffZ").
    fn now(&self) -> String;
}

/// Production system clock maintaining a monotonic timestamp floor.
pub struct SystemClock {
    last_issued: Mutex<SystemTime>,
}

impl SystemClock {
    pub async fn new(conn: &libsql::Connection) -> Result<Self> {
        let max_ts: Option<String> = conn
            .query(
                "SELECT MAX(recorded_at) FROM (
                     SELECT MAX(recorded_at) AS recorded_at FROM concepts
                     UNION ALL
                     SELECT MAX(recorded_at) AS recorded_at FROM links
                 )",
                (),
            )
            .await?
            .next()
            .await?
            .and_then(|row| row.get(0).ok());

        let floor = match max_ts {
            Some(ts) => parse_iso8601_utc(&ts).unwrap_or_else(|e| {
                tracing::warn!(
                    "SystemClock: failed to parse MAX(recorded_at)={:?}: {}; falling back to wall clock",
                    ts, e
                );
                SystemTime::now()
            }),
            None => SystemTime::now(),
        };

        Ok(Self {
            last_issued: Mutex::new(floor),
        })
    }
}

impl Clock for SystemClock {
    fn now(&self) -> String {
        let mut guard = self.last_issued.lock().unwrap();
        let wall = SystemTime::now();
        let next = if wall > *guard {
            wall
        } else {
            *guard + std::time::Duration::from_micros(1)
        };
        *guard = next;
        format_iso8601_utc(next)
    }
}

/// Fake clock for deterministic unit and scenario testing.
pub struct FakeClock {
    current: Mutex<SystemTime>,
}

impl FakeClock {
    pub fn new(initial: SystemTime) -> Self {
        Self {
            current: Mutex::new(initial),
        }
    }

    pub fn advance(&self, duration: std::time::Duration) {
        let mut guard = self.current.lock().unwrap();
        *guard += duration;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> String {
        let mut guard = self.current.lock().unwrap();
        let res = format_iso8601_utc(*guard);
        *guard += std::time::Duration::from_micros(1);
        res
    }
}

/// Strict parser for the canonical timestamp form (§4.1), accepting the legacy
/// second-precision form for rows written by older crate versions.
///
/// Both this and [`format_iso8601_utc`] delegate to [`crate::util::timestamp`],
/// which owns the canonical form. They remain here as the names §5.1.1 uses.
pub fn parse_iso8601_utc(s: &str) -> Result<SystemTime> {
    timestamp::parse(s)
}

/// Format a `SystemTime` in the canonical form `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
pub fn format_iso8601_utc(st: SystemTime) -> String {
    timestamp::format(st)
}
