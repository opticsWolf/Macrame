//! `escape_fts5_query` against arbitrary text (0.15.24, W16.3, D-266, review A-5).
//!
//! **The one of A-5's four that takes text straight from an end user**, and the
//! only one that cannot refuse its input. The other three parse and return
//! `Err`; this one takes whatever is in a search box and must produce an FTS5
//! MATCH expression that is neither a syntax error nor a query the user did not
//! write — `cats not dogs` quietly excluding documents is the failure it exists
//! to prevent, and that is a wrong answer rather than an exception.
//!
//! So its property is a round trip rather than a refusal, and it is the reason
//! this target is worth more than its four lines suggest: a function that always
//! succeeds has no error path for a test to check, so the only evidence it works
//! is a property that holds over every input.
//!
//! # What is asserted
//!
//! - **The output is made of quoted alphanumeric runs and nothing else.** Every
//!   character is alphanumeric, a double quote, or the space between terms. This
//!   is the whole safety claim: no operator, no column filter, no `NEAR`, no
//!   prefix `*` can survive, because none of them is alphanumeric.
//! - **Quotes are balanced and never nested.** An odd count is FTS5's
//!   `SQLITE_ERROR` — the failure a user sees as an exception from a search box.
//! - **Escaping is idempotent.** `escape(escape(s)) == escape(s)`. Not a
//!   decoration: it is what says the output is inside the accepted input set,
//!   which is the closest a target with no SQLite behind it can get to "this
//!   cannot be a syntax error".

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::vector::escape_fts5_query;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    let escaped = escape_fts5_query(s);

    let mut quotes = 0usize;
    for c in escaped.chars() {
        if c == '"' {
            quotes += 1;
        } else {
            assert!(
                c.is_alphanumeric() || c == ' ',
                "escaping {s:?} left {c:?} in {escaped:?}; only quoted \
                 alphanumeric runs may survive"
            );
        }
    }
    assert!(
        quotes % 2 == 0,
        "escaping {s:?} produced {quotes} quotes in {escaped:?}; an unbalanced \
         quote is the SQLITE_ERROR this function exists to prevent"
    );

    assert_eq!(
        escape_fts5_query(&escaped),
        escaped,
        "escaping is not idempotent: {s:?} -> {escaped:?} -> {:?}. The output \
         is therefore outside the input set it claims to be safe for.",
        escape_fts5_query(&escaped)
    );
});
