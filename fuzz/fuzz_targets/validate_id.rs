//! `validate_id` against arbitrary text (0.15.24, W16.3, D-266, review A-5).
//!
//! The last check between a caller's string and a key the transaction log will
//! carry forever. The reserved character it looks for is the log's own entity-key
//! delimiter, so an id carrying one is not invalid in the abstract — it is
//! *ambiguous three layers down*, in a table that is append-only and therefore
//! cannot be repaired after the fact ([D-061]).
//!
//! One function per target rather than one target for both identifier checks:
//! libFuzzer scores its corpus against the coverage a target reaches, and two
//! unrelated parsers behind one entry point share a budget while each keeps
//! inputs the other cannot use.
//!
//! # What is asserted
//!
//! Never panicking is the first property and the plain one. The second is about
//! **acceptance**, which is the risky direction: a refusal that is too strict is
//! an error message a caller can read, and one that is too loose is a key nobody
//! can rewrite. So an accepted id must be non-empty and must carry no character
//! from `RESERVED_ID_CHARS` — read from the crate rather than spelled `'|'` here,
//! so that widening the list cannot leave this target quietly checking the old
//! one ([D-030]'s failure class: two literals that must agree).
//!
//! [D-030]: ../../docs/architecture/s13-decision-register.md#d-030
//! [D-061]: ../../docs/architecture/s13-decision-register.md#d-061

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::util::{validate_id, RESERVED_ID_CHARS};

fuzz_target!(|data: &[u8]| {
    // Non-UTF-8 never reaches this function: it takes `&str`, and the bytes are
    // refused one layer up. Skipping says where the boundary is rather than
    // inventing a lossy conversion no real caller performs.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    if validate_id(s).is_ok() {
        assert!(!s.is_empty(), "validate_id accepted the empty identifier");
        assert!(
            !s.chars().any(|c| RESERVED_ID_CHARS.contains(&c)),
            "validate_id accepted {s:?}, which carries one of {RESERVED_ID_CHARS:?} \
             -- the characters that delimit the transaction-log entity key"
        );
    }
});
