//! Ceilings imposed by the engine rather than chosen by this crate.
//!
//! The distinction matters and is the reason this module is separate from the
//! tuning constants in [`crate::connection::chunk_rows`]. Those are *choices* —
//! measured, revisable, and traded against latency. What is here is a limit
//! SQLite imposes, which no amount of measurement will move.

/// How many ids go into one `IN (…)` list when hydrating attributes (T3.1).
///
/// **Not a latency budget.** These are reads, and [`crate::CHUNK_BUDGET`] bounds
/// how long the *writer* holds the lock. This is a bind-variable ceiling:
/// `SQLITE_MAX_VARIABLE_NUMBER` is 999 on a stock build, and hydrating a
/// budget-sized subgraph in one statement would otherwise fail at the driver
/// with an error that says nothing about node count.
///
/// 400 leaves room for the other bound parameters a hydrate query carries
/// (timestamps, mode discriminators) without arithmetic at every call site that
/// would have to be re-checked whenever one of those queries gained a parameter.
///
/// # Why this lives in `util` (T3.1)
///
/// It was defined twice — in `graph::subgraph` and in `temporal::as_of` — both
/// `400`, with the reasoning written out at one of them and the other carrying
/// `// See as_of::HYDRATE_CHUNK`. That is a working arrangement right up until
/// someone tunes one of them, at which point two constants that must be equal
/// silently are not, and the symptom is a driver error on one code path and not
/// the other. The cross-reference comment is evidence the duplication was known
/// and was being managed by convention; conventions are what a shared constant
/// replaces.
pub const HYDRATE_CHUNK: usize = 400;

/// The ceiling [`HYDRATE_CHUNK`] exists to stay under, on a stock SQLite build.
pub const SQLITE_MAX_VARIABLE_NUMBER: usize = 999;

/// A **compile-time** check that the chunk leaves room under the ceiling.
///
/// A `#[test]` was the first form and clippy was right to object: both sides are
/// constants, so the assertion has a constant value and belongs where it cannot
/// be skipped. A const block fails the *build* rather than a test run, which
/// matters because the failure mode it guards is someone tuning `HYDRATE_CHUNK`
/// upward — plausibly in a release build, plausibly without running the suite.
///
/// The margin is doubled rather than exact, because the reason the value is 400
/// and not 990 is that a hydrate query carries other bound parameters too. A
/// query that gains one should not be the thing that discovers the ceiling.
const _: () = assert!(
    HYDRATE_CHUNK * 2 < SQLITE_MAX_VARIABLE_NUMBER,
    "HYDRATE_CHUNK leaves no margin under the stock bind-variable ceiling"
);
