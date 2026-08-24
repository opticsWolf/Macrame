use crate::error::{DbError, Result};
use crate::util::timestamp;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// How far ahead of the wall clock a stored `recorded_at` may be before
/// [`recorded_at_floor`] refuses it. Twenty-four hours.
///
/// A whole day is deliberately generous. The condition being caught is a stamp
/// that is *wrong* — a skewed machine, a bad import, a fixture that escaped —
/// and those are typically years out, not hours. What a tight bound would catch
/// instead is a timezone-confused host or a database carried across a daylight
/// boundary by a tool that mishandled it, and refusing to open on that is worse
/// than the disease. The check exists to stop a stamp no clock could have
/// issued from becoming permanent, not to police minutes.
pub const DEFAULT_FUTURE_STAMP_TOLERANCE: Duration = Duration::from_secs(24 * 60 * 60);

/// What [`crate::Database::open_tuned`] does about a `recorded_at` in the future
/// (0.13.5, W7.4, [D-178]).
///
/// Shaped like [`crate::WalCheckpointPolicy`] and for
/// [D-155](../../docs/architecture/s13-decision-register.md)'s reason: this
/// guards an invariant, so "leave it alone" must not be spelt with an `Option`
/// whose `None` turns it off for every caller who never heard of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FutureStampPolicy {
    /// Refuse beyond [`DEFAULT_FUTURE_STAMP_TOLERANCE`].
    #[default]
    Default,
    /// Refuse beyond a tolerance of your own. `Duration::ZERO` refuses any
    /// stamp at all ahead of the wall clock, which is the strictest form and is
    /// only reasonable where the host's time is known good.
    Tolerance(Duration),
    /// Open regardless, and take the floor from whatever is stored.
    ///
    /// **The repair path, and it is not a repair.** It exists because a
    /// database this check refuses cannot otherwise be reached by the crate
    /// that refuses it, and inspecting a file requires opening it. Every write
    /// made under this policy inherits the poisoned floor, so use it to read
    /// and to plan, not to carry on.
    Allow,
}

impl FutureStampPolicy {
    /// `None` means no bound is applied.
    fn tolerance(self) -> Option<Duration> {
        match self {
            Self::Default => Some(DEFAULT_FUTURE_STAMP_TOLERANCE),
            Self::Tolerance(d) => Some(d),
            Self::Allow => None,
        }
    }
}

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
///
/// # A stamp from the future is refused rather than absorbed (0.13.5, W7.4, §3.4)
///
/// The floor is `MAX(recorded_at)` and the clock is raised to it, so a single
/// row stamped in 2087 — a skewed host, a bad import, a fixture that escaped —
/// becomes this process's floor, and every stamp it then issues is in 2087 too.
/// Those rows are written, so the next open reads the same floor back. **The
/// damage is permanent and it spreads**, which is what separates this from an
/// ordinary bad value.
///
/// It is caught here rather than at the write, because here is where a stamp
/// the crate could not have issued becomes one the crate does issue. `policy`
/// decides how far ahead is too far; see [`FutureStampPolicy`].
///
/// A corrupt stamp is still a `warn!` and no floor, unchanged
/// ([D-027](../../docs/architecture/s13-decision-register.md)) — an unparseable
/// value cannot be inherited, so it cannot spread, and refusing to open on one
/// would be the harsher answer to the smaller problem.
pub(crate) async fn recorded_at_floor(
    conn: &libsql::Connection,
    policy: FutureStampPolicy,
) -> Result<Option<SystemTime>> {
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
            Ok(t) => {
                if let Some(tolerance) = policy.tolerance() {
                    let limit = SystemTime::now() + tolerance;
                    if t > limit {
                        return Err(DbError::FutureRecordedAt {
                            stamp: ts,
                            limit: format_iso8601_utc(limit),
                        });
                    }
                }
                Some(t)
            }
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
    pub async fn new(conn: &libsql::Connection, policy: FutureStampPolicy) -> Result<Self> {
        // Wall clock when the database is empty or its stamp will not parse:
        // there is nothing to be behind, and a corrupt stored timestamp must not
        // become this process's floor (D-027). A stamp from the *future* is a
        // different case and `recorded_at_floor` refuses it (W7.4).
        let floor = recorded_at_floor(conn, policy)
            .await?
            .unwrap_or_else(SystemTime::now);
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
