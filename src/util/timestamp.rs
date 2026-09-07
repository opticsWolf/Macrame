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
    Err(DbError::InvalidTimestamp {
        value: s.to_string(),
        reason: "expected YYYY-MM-DDTHH:MM:SS.ffffffZ".to_string(),
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
        canon[lo..hi]
            .parse::<i64>()
            .expect("is_canonical checked digits")
    };
    let (year, month, day) = (num(0, 4), num(5, 7), num(8, 10));
    let (hour, min, sec) = (num(11, 13), num(14, 16), num(17, 19));
    let micros = num(20, 26);
    debug_assert_eq!(b[26], b'Z');

    let bad = |why: &str| DbError::InvalidTimestamp {
        value: s.to_string(),
        reason: why.to_string(),
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

    // **One signed quantity, split into sign and magnitude exactly once**
    // ([D-270], 0.15.27). This used to build `Duration::new(secs.unsigned_abs(),
    // micros)` and then add or subtract it according to the sign of `secs`,
    // which is right above the epoch and wrong below it: the microseconds
    // always run *forward* from the second, so on the pre-epoch side that
    // subtracted them as well. `1969-12-31T23:59:59.999999Z` — one microsecond
    // before the epoch — came back as `1969-12-31T23:59:58.000001Z`, out by
    // very nearly two seconds, with no error.
    let total_micros = i128::from(secs) * 1_000_000 + i128::from(micros);
    let magnitude = u64::try_from(total_micros.unsigned_abs())
        .map(Duration::from_micros)
        .map_err(|_| bad("not representable as a SystemTime on this platform"))?;

    // `SystemTime` is a `timespec` on Unix and a `FILETIME` on Windows, whose
    // own epoch is 1601-01-01 — so this refusal is genuinely per-platform and
    // says so. It is the reason a pre-epoch defect can hide from a Windows
    // developer box and surface on the Linux fuzz runner.
    let t = if total_micros >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(magnitude)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(magnitude)
    };
    t.ok_or_else(|| bad("not representable as a SystemTime on this platform"))
}

/// Earliest instant the canonical form can express: `0000-01-01T00:00:00.000000Z`.
///
/// The bound is the four-digit year, not the epoch. See [`format`].
const MIN_MICROS: i128 = -62_167_219_200_000_000;

/// Latest instant the canonical form can express: [`OPEN_SENTINEL`].
const MAX_MICROS: i128 = 253_402_300_799_999_999;

/// Render a [`SystemTime`] in canonical form.
///
/// # The range is `0000-01-01` … `9999-12-31`, and it used to be `1970-…`
///
/// This function's doc comment used to read *"saturates at the epoch for
/// pre-1970 inputs: the schema has no use for them, and the alternative — a
/// negative-year string — would not be canonical."* The second clause is right
/// and the first was wrong about its own schema ([D-270], 0.15.27): a
/// negative year is not canonical, but **`0060-06-22` is** — four digits,
/// [`CANONICAL_TS_GLOB`] accepts it, it sorts correctly against every later
/// stamp, and [`parse`] has always returned a real pre-epoch `SystemTime` for
/// it. So the two halves of this module disagreed about their own range:
/// `parse` accepted from year 0 and `format` answered from 1970, and every
/// instant in between came back as `1970-01-01T00:00:00.000000Z` — the wrong
/// day, silently, with no error anywhere.
///
/// That matters because valid time is the caller's, not the clock's. A
/// bitemporal ledger recording when a fact *was true* has every reason to
/// carry a date before 1970, and the storage layer has always stored and
/// ordered one correctly. Only the `SystemTime` round trip lost it.
///
/// Found by `fuzz_targets/timestamp_parse.rs` on CI run `34060567071`, on the
/// input `"0060-06-22T22:22:24Z"` — which is [D-266]'s point about these four
/// parsers arriving with a defect nobody had reasoned their way to.
///
/// Saturation remains, at the two ends the *form* imposes rather than at the
/// epoch: an instant outside the four-digit year clamps to
/// `0000-01-01T00:00:00.000000Z` or to [`OPEN_SENTINEL`]. `parse` cannot
/// produce one — its input is four digits by construction — so the clamp is
/// reachable only from a `SystemTime` built elsewhere, where returning
/// something canonical beats panicking on a display path.
///
/// [D-266]: ../../docs/architecture/s13-decision-register.md#d-266
/// [D-270]: ../../docs/architecture/s13-decision-register.md#d-270
pub fn format(st: SystemTime) -> String {
    // `duration_since` reports the magnitude and the direction separately, so
    // the sign has to be put back by hand. `unwrap_or_default` is what used to
    // discard it.
    let signed_micros = match st.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_micros()).unwrap_or(i128::MAX),
        Err(before) => i128::try_from(before.duration().as_micros())
            .map_or(i128::MIN, |m| -m),
    }
    .clamp(MIN_MICROS, MAX_MICROS);

    // Floor division throughout: `rem_euclid` keeps the microseconds and the
    // time of day positive on the pre-epoch side, where truncating division
    // would produce a negative time of day and a day off by one.
    let secs = signed_micros.div_euclid(1_000_000) as i64;
    let micros = signed_micros.rem_euclid(1_000_000) as u32;
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
            "2026-01-01T00:00:00",            // no zone
            "2026-01-01T00:00:00+01:00",      // offset
            "2026-01-01T00:00:00.000Z",       // milliseconds
            "2026-01-01T00:00:00.000000000Z", // nanoseconds
            "2026-01-01 00:00:00.000000Z",    // space separator
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

    /// `parse` has a platform floor and `format` does not, so a test that
    /// wants an early instant has to ask rather than assume.
    ///
    /// `SystemTime` is a `timespec` on Unix and a `FILETIME` on Windows, whose
    /// own epoch is **1601-01-01** — so `parse("0060-…")` returns a real
    /// instant on one and `InvalidTimestamp { reason: "not representable as a
    /// SystemTime on this platform" }` on the other. That refusal predates
    /// [D-270] and is correct: it is typed, it names the platform, and it
    /// loses nothing silently. It is also exactly why this defect surfaced on
    /// the Linux fuzz runner and never on the Windows developer box.
    fn parses_here(s: &str) -> Option<SystemTime> {
        match parse(s) {
            Ok(t) => Some(t),
            Err(DbError::InvalidTimestamp { reason, .. })
                if reason.contains("not representable") =>
            {
                None
            }
            Err(e) => panic!("{s} was rejected for the wrong reason: {e}"),
        }
    }

    /// The two bounds of the canonical form are the two bounds of `format`.
    ///
    /// They are written as literals in the source, so this is the arithmetic
    /// that says the literals are the instants they claim to be. Without it a
    /// transposed digit would clamp silently at the wrong century.
    #[test]
    fn the_clamp_sits_exactly_on_the_representable_ends() {
        assert_eq!(
            MIN_MICROS,
            i128::from(days_from_civil(0, 1, 1)) * 86_400 * 1_000_000
        );
        assert_eq!(
            MAX_MICROS,
            (i128::from(days_from_civil(9999, 12, 31)) * 86_400 + 86_399) * 1_000_000
                + 999_999
        );

        // The upper end is post-epoch, so every platform can build it.
        let sentinel = parse(OPEN_SENTINEL).unwrap();
        assert_eq!(format(sentinel), OPEN_SENTINEL);
        assert_eq!(
            format(sentinel + Duration::from_secs(86_400)),
            OPEN_SENTINEL,
            "past the end, still canonical rather than a five-digit year"
        );

        // The lower end needs a pre-year-0 `SystemTime`, which a Windows
        // `FILETIME` cannot hold at all.
        if let Some(zero) = parses_here("0000-01-01T00:00:00.000000Z") {
            assert_eq!(format(zero), "0000-01-01T00:00:00.000000Z");
            assert_eq!(
                format(zero - Duration::from_secs(86_400)),
                "0000-01-01T00:00:00.000000Z",
                "before the start, still canonical rather than a negative year"
            );
        }
    }

    /// **A pre-1970 valid time survives the round trip** ([D-270]).
    ///
    /// `format` used to saturate at the epoch, so every instant `parse`
    /// returned for a date between year 0 and 1970 came back as
    /// `1970-01-01T00:00:00.000000Z`: the wrong day, with no error. Found by
    /// the `timestamp_parse` fuzz target on `"0060-06-22T22:22:24Z"`, which is
    /// the last case below and the only one that needs the platform guard.
    ///
    /// Valid time is the caller's. A ledger recording when a fact *was* true
    /// has every reason to carry a date before 1970, and the storage layer has
    /// always stored and ordered one correctly — only the `SystemTime` round
    /// trip lost it.
    ///
    /// [D-270]: ../../docs/architecture/s13-decision-register.md#d-270
    #[test]
    fn a_pre_epoch_instant_round_trips_instead_of_collapsing_to_1970() {
        // At or after 1601-01-01, so these run everywhere — including the
        // Windows box where the defect was invisible.
        for s in [
            "1601-01-01T00:00:00.000000Z", // the Windows `FILETIME` epoch itself
            "1900-02-28T12:00:00.000000Z", // 1900 is not a leap year
            "1969-12-31T23:59:59.999999Z", // one microsecond before the epoch
            "1969-12-31T23:59:59.000000Z",
            "1970-01-01T00:00:00.000000Z", // the boundary, from the other side
        ] {
            assert_eq!(format(parse(s).unwrap()), s, "roundtrip failed for {s}");
        }

        // The fuzzer's own input, in the legacy second-precision form it
        // arrived as, on the platforms that can hold it.
        if let Some(t) = parses_here("0060-06-22T22:22:24Z") {
            assert_eq!(format(t), "0060-06-22T22:22:24.000000Z");
        }

        // And the ordering the whole module exists for still holds across the
        // epoch, which is what a collapsed stamp would have broken.
        assert!("0060-06-22T22:22:24.000000Z" < "1970-01-01T00:00:00.000000Z");
        assert!("1969-12-31T23:59:59.999999Z" < "1970-01-01T00:00:00.000000Z");
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
