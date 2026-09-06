//! `timestamp::normalize` and `timestamp::parse` against arbitrary text
//! (0.15.24, W16.3, D-266, review A-5).
//!
//! The first of the four parsers A-5 names, and the one with the most surface:
//! `normalize` widens a legacy second-precision stamp and refuses everything
//! else, and `parse` turns a canonical string into a `SystemTime` through a
//! hand-written civil-calendar conversion. Byte indexing, `&s[..19]`, and
//! arithmetic on numbers taken from the input — three ways to panic on text a
//! caller supplied, on a path every write goes through.
//!
//! Unlike the snapshot targets this needs no `fuzzing` feature door: both
//! functions are public, so this target calls them exactly as a caller does.
//!
//! # What is asserted, beyond "it returned"
//!
//! A parser that never panics but answers wrongly is not much of a win, so the
//! two properties worth having are checked whenever the input is accepted:
//!
//! - **`normalize` really produces the canonical form.** Its whole contract is
//!   that anything it accepts comes back as `YYYY-MM-DDTHH:MM:SS.ffffffZ`, and
//!   `is_canonical` is the same predicate the `CHECK` constraint uses. A widened
//!   stamp that satisfies `normalize` and not the constraint would be refused by
//!   the database three layers below, where the message is about SQL.
//! - **`parse` is stable across a round trip.** Not `format(parse(s)) == s`,
//!   which is a claim about *this* string; the useful one is that re-parsing the
//!   formatted output lands on the same instant, because that is what makes a
//!   stamp survive being written and read back.

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::util::timestamp;

fuzz_target!(|data: &[u8]| {
    // Non-UTF-8 never reaches these functions: they take `&str`, and the bytes
    // are refused one layer up by the binding or by serde. Skipping is honest
    // about where the boundary is rather than inventing a lossy conversion the
    // real caller does not perform.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(widened) = timestamp::normalize(s) {
        assert!(
            timestamp::is_canonical(&widened),
            "normalize accepted {s:?} and produced {widened:?}, which is not canonical"
        );
        assert_eq!(
            timestamp::normalize(&widened).ok().as_deref(),
            Some(widened.as_str()),
            "normalize is not idempotent on its own output {widened:?}"
        );
    }

    if let Ok(instant) = timestamp::parse(s) {
        let formatted = timestamp::format(instant);
        assert!(
            timestamp::is_canonical(&formatted),
            "parse accepted {s:?} and format produced {formatted:?}, which is not canonical"
        );
        assert_eq!(
            timestamp::parse(&formatted).ok(),
            Some(instant),
            "the round trip moved the instant: {s:?} -> {formatted:?}"
        );
    }
});
