//! `BranchId::new` against arbitrary text (0.15.24, W16.3, D-266, review A-5).
//!
//! A lineage name is a key too, and this refusal is unusual in that it rejects
//! names that are merely *unreadable* rather than unrepresentable: control
//! characters, and any leading or trailing whitespace — `trim` and not
//! `trim_ascii`, because a non-breaking space is invisible in every terminal the
//! name will be read in.
//!
//! # What is asserted
//!
//! Never panicking, and then the four clauses of the refusal, checked on the
//! accepting side: an accepted name comes back byte-identical, fits
//! `MAX_BRANCH_ID`, carries no control character, and equals its own trim.
//! Checking acceptance rather than rejection is deliberate — a name wrongly
//! refused is an error a caller reads and works around, while one wrongly
//! accepted becomes a row in `branches` that [`BranchId::from_stored`] will
//! adopt without revalidating, forever.

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::{BranchId, MAX_BRANCH_ID};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(branch) = BranchId::new(s) {
        assert_eq!(
            branch.as_str(),
            s,
            "BranchId::new accepted {s:?} and stored something else"
        );
        assert!(
            !s.is_empty() && s.len() <= MAX_BRANCH_ID,
            "BranchId::new accepted a name of {} bytes, over the {MAX_BRANCH_ID} limit",
            s.len()
        );
        assert!(
            !s.chars().any(char::is_control),
            "BranchId::new accepted {s:?}, which carries a control character"
        );
        assert_eq!(
            s.trim(),
            s,
            "BranchId::new accepted {s:?}, which is not equal to its own trim \
             -- the invisible-whitespace case the refusal exists for"
        );
    }
});
