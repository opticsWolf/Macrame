//! The `macrame._macrame` extension module.
//!
//! A table of contents and nothing else. Each phase of the bindings plan gets
//! its own module; this file only registers what they export.
//!
//! - [`runtime`] — the async→sync boundary and the GIL rule (P1, D-095)
//! - [`errors`] — the exception hierarchy, 27 variants mapped (P2)
//! - [`database`] — the `Database` handle's lifecycle (P1)
//! - [`timestamps`] — `str` / aware `datetime` in, `datetime` out (P3)
//! - [`types`] — the value types callers construct (P3)
//! - [`testing`] — underscore-prefixed hooks the Python suite drives

mod database;
mod errors;
mod rows;
mod runtime;
mod testing;
mod timestamps;
mod types;

use pyo3::prelude::*;

/// Whether the libSQL engine is actually bound into this extension.
///
/// Taking the address of [`macrame::Database::open`] is what makes this a test
/// rather than a tautology. Reading one of the crate's `const`s would not:
/// constants fold at compile time, so a module that only exposed
/// `CHUNK_BUDGET` would link clean without a byte of libSQL present, and the
/// first real database call would be where we found out. Naming a function
/// that reaches the engine forces its codegen, and with it the amalgamation.
///
/// Retained past P1, when real calls prove the same thing, because it isolates
/// the failure: if this is `True` and a call still fails, the problem is not
/// the link.
#[pyfunction]
fn engine_linked() -> bool {
    // Wrapped in a local `fn` with a concrete parameter type rather than
    // turbofished directly: `Database::open` takes `impl AsRef<Path>`, and
    // `impl Trait` in argument position cannot be named as a generic argument,
    // so `Database::open::<&Path>` does not compile. Monomorphising it here
    // gives a plain function item whose address can be taken.
    async fn open(path: &std::path::Path) -> macrame::Result<macrame::Database> {
        macrame::Database::open(path).await
    }
    // `black_box` rather than a comparison. Two earlier attempts here were
    // wrong in the same way: `open as usize != 0` and `!(open as *const
    // ()).is_null()` both read as runtime checks, and a function pointer is
    // never null, so clippy correctly calls them tautologies. Nothing about
    // this is a runtime question. What is wanted is that the optimiser cannot
    // discard `open` — and with it the engine — as unreachable, which is
    // exactly what `black_box` states.
    //
    // So this returns `true` in every build that exists. The assertion is that
    // the build exists, and it is made by the linker, not by the `bool`.
    std::hint::black_box(open as *const ());
    true
}

/// How long `write_bulk_atomic` will hold the write actor for this batch.
///
/// **T1.3's whole delivery was making that ceiling predictable *before* the
/// call**, so a Python caller who cannot ask is back where 0.5.x was: the only
/// statement of the cost was the word "uncapped" in a doc table.
///
/// The batch is one act under one stamp and cannot be chunked, so this duration
/// is time every other writer in the process spends waiting.
///
/// **It is a shape, not a promise.** Calibrated on libSQL 0.9.30, one machine,
/// 100–20,000 rows, within 5% across that range except below ~500 rows where
/// fixed costs dominate and it over-predicts by 3× — harmless, since nothing
/// that small approaches the warning threshold. It says nothing about disk. It
/// exists to distinguish 30 ms from 18 s, and should not be read closer than
/// that.
///
/// The 7× case it is built for: 20,000 edges spread over distinct relationships
/// against 20,000 corrections to a single relationship's history. A size-only
/// model under-predicts the second by 7×, which is the direction that hurts.
#[pyfunction]
fn estimate_bulk_hold(edges: Vec<types::PyEdgeAssertion>) -> std::time::Duration {
    let edges: Vec<macrame::prelude::EdgeAssertion> =
        edges.into_iter().map(|e| e.inner).collect();
    macrame::prelude::estimated_bulk_hold(&edges)
}

/// The chunk budget the write actor holds itself to, in milliseconds.
///
/// The crate's one cross-cutting number. The observability that makes it
/// meaningful — `Database.metrics()` — arrives in P4.6, on the counters this
/// wheel is built with (D-093).
#[pyfunction]
fn chunk_budget_ms() -> u64 {
    macrame::CHUNK_BUDGET.as_millis() as u64
}

#[pymodule]
fn _macrame(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    errors::register(m)?;
    types::register(m)?;

    m.add_class::<database::PyDatabase>()?;

    m.add_function(wrap_pyfunction!(engine_linked, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_budget_ms, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_bulk_hold, m)?)?;
    // The threshold `write_bulk_atomic` warns above, as a timedelta. Exposed so
    // a caller can compare an estimate against it rather than hard-coding 250ms.
    m.add(
        "BULK_ATOMIC_WARN_HOLD",
        macrame::prelude::BULK_ATOMIC_WARN_HOLD,
    )?;
    m.add_function(wrap_pyfunction!(runtime::_block_for_testing, m)?)?;
    m.add_function(wrap_pyfunction!(runtime::_mark_forked, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_db_error_variants, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_raise_db_error, m)?)?;
    m.add_function(wrap_pyfunction!(timestamps::_coerce_timestamp, m)?)?;
    m.add_function(wrap_pyfunction!(timestamps::_render_timestamp, m)?)?;
    Ok(())
}
