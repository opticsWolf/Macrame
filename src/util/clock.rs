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

    /// Raise this clock's floor to `floor`, if it is currently behind it.
    ///
    /// **The contract above is what makes this part of the trait rather than an
    /// implementation detail of [`SystemClock`].** "Strictly increasing across
    /// restarts" is not a property a clock can hold on its own — it depends on
    /// what the database already contains, which the clock cannot see. Every
    /// implementation therefore needs a way to be told, and
    /// [`crate::Database::open_with_clock`] tells it exactly once, at open.
    ///
    /// Without this, injecting a [`FakeClock`] was not merely awkward but
    /// unusable on any database with rows in it: a fake starting at the epoch
    /// issues stamps below every existing `recorded_at` and the first concept
    /// write aborts on `trg_concepts_monotonic_ra`. That is the reason defect K
    /// stalled for three releases, and it is a missing trait method rather than
    /// a missing constructor argument.
    fn raise_floor(&self, floor: SystemTime);
}

/// The newest `recorded_at` in the ledger tables, or `None` on an empty database.
///
/// One definition of "the floor", used by [`SystemClock::new`] and by
/// `open_with_clock`, rather than the query existing twice and being kept in
/// step by hand.
pub(crate) async fn recorded_at_floor(conn: &libsql::Connection) -> Result<Option<SystemTime>> {
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

    Ok(match max_ts {
        Some(ts) => match parse_iso8601_utc(&ts) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(
                    "clock: failed to parse MAX(recorded_at)={:?}: {}; no floor applied",
                    ts,
                    e
                );
                None
            }
        },
        None => None,
    })
}

/// Production system clock maintaining a monotonic timestamp floor.
pub struct SystemClock {
    last_issued: Mutex<SystemTime>,
}

impl SystemClock {
    pub async fn new(conn: &libsql::Connection) -> Result<Self> {
        // Wall clock when the database is empty or its stamp will not parse:
        // there is nothing to be behind, and a corrupt stored timestamp must not
        // become this process's floor (D-027).
        let floor = recorded_at_floor(conn).await?.unwrap_or_else(SystemTime::now);
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

    fn raise_floor(&self, floor: SystemTime) {
        let mut guard = self.last_issued.lock().unwrap();
        if floor > *guard {
            *guard = floor;
        }
    }
}

/// Fake clock for deterministic unit and scenario testing.
///
/// Inject with [`crate::Database::open_with_clock`]. On a **fresh** database the
/// stamps are exactly what the caller sets, which is the point: a bitemporal
/// test that has to accommodate wall-clock `recorded_at` values cannot assert on
/// them, so most of the interesting divergence properties were previously
/// written against `valid_time` alone or against a hand-driven connection.
///
/// On a database that **already holds rows**, `open_with_clock` raises this
/// clock to their newest `recorded_at` before the actor starts — see
/// [`Clock::raise_floor`]. That is not optional and it does cost determinism:
/// reopening a populated database with a fake starting at the epoch would
/// otherwise abort the first concept write on `trg_concepts_monotonic_ra`. Tests
/// wanting exact stamps should start from an empty file.
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

    /// The stamp this clock would issue next, without issuing it.
    pub fn peek(&self) -> String {
        format_iso8601_utc(*self.current.lock().unwrap())
    }
}

impl Clock for FakeClock {
    fn now(&self) -> String {
        let mut guard = self.current.lock().unwrap();
        let res = format_iso8601_utc(*guard);
        *guard += std::time::Duration::from_micros(1);
        res
    }

    fn raise_floor(&self, floor: SystemTime) {
        let mut guard = self.current.lock().unwrap();
        if floor > *guard {
            // One microsecond past, not equal to: `recorded_at` must be
            // *strictly* increasing, and issuing the floor itself would collide
            // with the row it was read from.
            *guard = floor + std::time::Duration::from_micros(1);
        }
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
