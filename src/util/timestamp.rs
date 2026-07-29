//! Canonical timestamp form for every temporal column (§4.1).
//!
//! Every `valid_from`, `valid_to`, and `recorded_at` in the schema is compared
//! **lexicographically** — by `<=` and `<` in SQL, by `str` ordering in Rust,
//! and by `MAX()` when the clock recovers its floor. Lexicographic ordering
//! agrees with chronological ordering only when every string has the *same
//! shape*. A `Z` suffix alone is not enough:
//!
//! ```text
//! '2026-01-01T00:00:00Z' <= '2026-01-01T00:00:00.000000Z'   -->  FALSE
//! ```
//!
//! because at the first differing byte `'Z'` (0x5A) sorts after `'.'` (0x2E),
//! so the second-precision instant compares as *later* than the identical
//! microsecond-precision instant. A traversal predicated on `valid_from <= :ts`
//! then silently drops every edge — no error, just an empty result.
//!
//! The fix is to admit exactly one width. A timestamp is canonical iff it is
//! exactly [`TIMESTAMP_LEN`] bytes in the form `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
//! [`CANONICAL_TS_GLOB`] enforces that at the storage layer so a non-canonical
//! value cannot be written at all, and [`normalize`] widens the legacy
//! second-precision form at the boundary rather than rejecting it.

use crate::error::{DbError, Result};
use std::time::{Duration, SystemTime};

/// Byte length of the canonical form `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
pub const TIMESTAMP_LEN: usize = 27;

/// The open-interval sentinel, in canonical form.
///
/// Widened from `9999-12-31T23:59:59Z` in 0.5.4: a sentinel that is the one
/// value exempt from the canonical width is a carve-out that reintroduces the
/// very comparison bug the width exists to prevent. `.999999` also makes the
/// sentinel the maximum representable instant, so `ts < OPEN_SENTINEL` holds
/// for every real timestamp — which is what a half-open interval needs.
pub const OPEN_SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

/// GLOB pattern matching exactly the canonical form.
///
/// Used in `CHECK` constraints. GLOB anchors at both ends and supports
/// character classes, so this is a complete shape test — no separate
/// `length()` term is needed.
pub const CANONICAL_TS_GLOB: &str =
    "'[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z'";

/// True iff `s` is exactly `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// Shape only — this does not check that the date is a real calendar date.
/// [`parse`] does that.
pub fn is_canonical(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != TIMESTAMP_LEN {
        return false;
    }
    const DIGITS: [usize; 20] = [
        0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22, 23, 24, 25,
    ];
    const SEPS: [(usize, u8); 7] = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (26, b'Z'),
    ];
    DIGITS.iter().all(|&i| b[i].is_ascii_digit()) && SEPS.iter().all(|&(i, c)| b[i] == c)
}

/// Widen a timestamp to canonical form.
///
/// Accepts the canonical form unchanged and the legacy second-precision form
/// `YYYY-MM-DDTHH:MM:SSZ`, which is widened by appending `.000000`. Anything
/// else — an offset like `+01:00`, a missing `Z`, millisecond precision — is
/// rejected rather than guessed at, because every silent repair here becomes a
/// wrong answer in a temporal query later.
pub fn normalize(s: &str) -> Result<String> {
    if is_canonical(s) {
        return Ok(s.to_string());
    }
    // Legacy second precision: "YYYY-MM-DDTHH:MM:SSZ" (20 bytes).
    if s.len() == 20 && s.ends_with('Z') {
        let widened = format!("{}.000000Z", &s[..19]);
        if is_canonical(&widened) {
            return Ok(widened);
        }
    }
    Err(DbError::ReplayCorrupt {
        seq: 0,
        reason: format!(
            "timestamp {s:?} is not canonical (expected YYYY-MM-DDTHH:MM:SS.ffffffZ)"
        ),
    })
}

/// Days from 1970-01-01 to `y-m-d` (proleptic Gregorian).
///
/// Hinnant's civil-calendar algorithm: shift the year to start in March so the
/// leap day lands at the end, then count whole 400-year eras.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Number of days in `month` of `year` (proleptic Gregorian).
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Strict parser for the canonical form (second precision accepted via
/// [`normalize`]).
///
/// Validates the calendar as well as the shape: `2026-02-30T00:00:00.000000Z`
/// has canonical *shape* but is not a date, and accepting it would let a
/// timestamp exist that no round-trip can reproduce.
pub fn parse(s: &str) -> Result<SystemTime> {
    let canon = normalize(s)?;
    let b = canon.as_bytes();
    let num = |lo: usize, hi: usize| -> i64 {
        canon[lo..hi].parse::<i64>().expect("is_canonical checked digits")
    };
    let (year, month, day) = (num(0, 4), num(5, 7), num(8, 10));
    let (hour, min, sec) = (num(11, 13), num(14, 16), num(17, 19));
    let micros = num(20, 26);
    debug_assert_eq!(b[26], b'Z');

    let bad = |why: &str| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("timestamp {s:?}: {why}"),
    };
    if !(1..=12).contains(&month) {
        return Err(bad("month out of range"));
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(bad("day out of range for month"));
    }
    // Leap seconds are not representable: SystemTime counts SI seconds since
    // the epoch, so :60 has no slot and silently aliasing it to :59 would break
    // the strictly-increasing clock contract.
    if hour > 23 || min > 59 || sec > 59 {
        return Err(bad("time component out of range"));
    }

    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec;
    let offset = Duration::new(secs.unsigned_abs(), micros as u32 * 1_000);
    let t = if secs >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(offset)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(offset)
    };
    t.ok_or_else(|| bad("not representable as a SystemTime on this platform"))
}

/// Render a [`SystemTime`] in canonical form.
///
/// Saturates at the epoch for pre-1970 inputs: the schema has no use for them,
/// and the alternative — a negative-year string — would not be canonical.
pub fn format(st: SystemTime) -> String {
    let d = st
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let micros = d.subsec_micros();

    // Floor division: rem_euclid keeps the time-of-day positive.
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, dd) = civil_from_days(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        y,
        m,
        dd,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        micros
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_is_canonical_and_maximal() {
        assert!(is_canonical(OPEN_SENTINEL));
        assert_eq!(OPEN_SENTINEL.len(), TIMESTAMP_LEN);
        // Every real timestamp sorts before the sentinel, which is what makes
        // the half-open interval [valid_from, valid_to) work by string compare.
        assert!("2026-01-01T00:00:00.000000Z" < OPEN_SENTINEL);
        assert!("9999-12-31T23:59:59.999998Z" < OPEN_SENTINEL);
    }

    #[test]
    fn canonical_form_orders_lexicographically() {
        // This is the invariant the whole module exists to guarantee, and the
        // exact comparison that silently failed before canonicalisation.
        let a = normalize("2026-01-01T00:00:00Z").unwrap();
        let b = "2026-01-01T00:00:00.000000Z";
        assert_eq!(a, b);
        assert!(a.as_str() <= b);

        let mut stamps = [
            "2026-01-01T00:00:01.000000Z",
            "2026-01-01T00:00:00.000001Z",
            "2026-01-01T00:00:00.000000Z",
            "2025-12-31T23:59:59.999999Z",
        ];
        stamps.sort_unstable();
        assert_eq!(stamps[0], "2025-12-31T23:59:59.999999Z");
        assert_eq!(stamps[3], "2026-01-01T00:00:01.000000Z");
    }

    #[test]
    fn normalize_widens_seconds_and_rejects_everything_else() {
        assert_eq!(
            normalize("2026-01-01T00:00:00Z").unwrap(),
            "2026-01-01T00:00:00.000000Z"
        );
        assert_eq!(normalize(OPEN_SENTINEL).unwrap(), OPEN_SENTINEL);

        for bad in [
            "2026-01-01T00:00:00",           // no zone
            "2026-01-01T00:00:00+01:00",     // offset
            "2026-01-01T00:00:00.000Z",      // milliseconds
            "2026-01-01T00:00:00.000000000Z", // nanoseconds
            "2026-01-01 00:00:00.000000Z",   // space separator
            "",
        ] {
            assert!(normalize(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn format_reports_the_real_date() {
        // Regression: format() previously hard-coded a literal date, so every
        // stamp the crate wrote claimed the same day regardless of the clock.
        assert_eq!(
            format(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00.000000Z"
        );
        assert_eq!(
            format(SystemTime::UNIX_EPOCH + Duration::from_secs(86_400)),
            "1970-01-02T00:00:00.000000Z"
        );
        // 2000-03-01: the far side of a leap day in a 400-year leap year.
        assert_eq!(
            format(SystemTime::UNIX_EPOCH + Duration::from_secs(951_868_800)),
            "2000-03-01T00:00:00.000000Z"
        );
    }

    #[test]
    fn parse_format_roundtrip() {
        for s in [
            "1970-01-01T00:00:00.000000Z",
            "2000-02-29T12:34:56.654321Z",
            "2026-07-28T09:15:00.000001Z",
            OPEN_SENTINEL,
        ] {
            assert_eq!(format(parse(s).unwrap()), s, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn parse_validates_the_calendar_not_just_the_shape() {
        for bad in [
            "2026-02-30T00:00:00.000000Z", // February has no 30th
            "2026-13-01T00:00:00.000000Z", // no 13th month
            "2026-00-01T00:00:00.000000Z",
            "2025-02-29T00:00:00.000000Z", // 2025 is not a leap year
            "2026-01-01T24:00:00.000000Z",
            "2026-01-01T00:60:00.000000Z",
            "2026-01-01T00:00:60.000000Z", // leap second, not representable
        ] {
            assert!(parse(bad).is_err(), "should reject {bad:?}");
        }
        // ...but a real leap day parses.
        assert!(parse("2024-02-29T00:00:00.000000Z").is_ok());
    }
}
