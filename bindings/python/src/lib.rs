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
//! - [`graph`] — traversals and the opaque `Subgraph` handle (P4.2, D-097)
//! - [`temporal`] — reconstruct, archive, and the chain check (P4.3)
//! - [`observe`] — rebuild reports and actor metrics (P4.5, P4.6)
//! - [`vector`] — embeddings, search, and the filter planner (P4.4)
//! - [`testing`] — underscore-prefixed hooks the Python suite drives

mod database;
mod errors;
mod graph;
mod observe;
mod rows;
mod runtime;
mod temporal;
mod testing;
mod timestamps;
mod types;
mod vector;

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
/// 100–20,000 rows, within 15% from 500 rows up; below that it under-predicts
/// by up to 3×, which is harmless since nothing that small approaches the
/// warning threshold. It says nothing about disk, and should not be read closer
/// than an order of magnitude.
///
/// **It reads `len(edges)` and nothing else, as of 0.13.6 (D-179).** Until then
/// the batch's *shape* was the dominant term — 20,000 corrections to one
/// relationship's history cost 18.1 s against 2.6 s for 20,000 unrelated edges,
/// because the within-batch overlap guard compared every pair. That guard sorts
/// and sweeps now, the two shapes hold for 2.2 s and 1.9 s, and a model still
/// predicting the spread would warn about a batch that is fine.
#[pyfunction]
fn estimate_bulk_hold(edges: Vec<types::PyEdgeAssertion>) -> std::time::Duration {
    let edges: Vec<macrame::prelude::EdgeAssertion> = edges.into_iter().map(|e| e.inner).collect();
    macrame::prelude::estimated_bulk_hold(&edges)
}

/// The chunk budget the write actor holds itself to, in milliseconds.
///
/// The crate's one cross-cutting number. What makes it meaningful rather than
/// an aspiration is `Database.metrics()`, whose counters are real in this wheel
/// because it is built with `--features metrics` unconditionally (D-093):
/// `metrics().violations()` is the list of kinds that exceeded this budget.
///
/// Since 0.12.0 it is also the number the actor **steers on** rather than one a
/// row count approximates: the bulk paths measure each chunk and size the next
/// from it against this budget (D-146). `violations()` is still the honest
/// answer to whether it is being met, and on a populated `links` table it is
/// still missed — by ~0.2 ms at the floor rather than by 3×.
#[pyfunction]
fn chunk_budget_ms() -> u64 {
    macrame::CHUNK_BUDGET.as_millis() as u64
}

#[pymodule]
fn _macrame(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    errors::register(m)?;
    types::register(m)?;
    graph::register(m)?;
    temporal::register(m)?;
    observe::register(m)?;
    vector::register(m)?;

    m.add_class::<database::PyDatabase>()?;
    m.add_class::<database::PyCancelToken>()?;

    // convention (D-068/D-091): `Database::raw()` is deliberately NOT on this
    // list and must not be added. It hands back a write-capable connection the
    // write actor does not own, which dissolves the single-writer property
    // CHUNK_BUDGET's latency argument and the overlap guard both rest on. Its
    // Rust rustdoc is #[doc(hidden)], so a contributor deciding to expose it
    // would be standing *here*, seeing nothing — hence this line and its twin at
    // `connection.rs`'s `raw()` (0.10.0, W4.10).
    //
    // The supported diagnostic path is `diagnostic_query` / `explain`, which
    // open `SQLITE_OPEN_READ_ONLY` per call (D-091) and serialise (D-138).
    //
    // convention (D-165, 0.12.22, W6.5): `Database::shadow_step` is likewise NOT
    // exposed, and unlike `raw()` this one is a judgement rather than an
    // invariant. It is public in Rust and safe there; what does not cross well
    // is its *obligation*. The `epoch` from `ShadowOutcome::Started` must be
    // handed back to `ShadowStep::Swap`, and a caller who loses it defeats the
    // archive interlock and can swap a stale projection over a live one —
    // silently, since the swap succeeds. In Rust that obligation is carried by
    // two types the caller cannot fabricate; across this boundary they would
    // become two more `#[pyclass]`es whose only purpose is to be passed back
    // correctly, which is the obligation restated as a convention rather than
    // enforced.
    //
    // `rebuild_current_chunked` is the loop, it is exposed, and it cannot get
    // the epoch wrong. The seam `shadow_step` exists for — pacing steps against
    // a frame budget, abandoning a long rebuild, provoking the interlock in a
    // test — has no Python caller today. Expose it when one appears, with the
    // epoch as an opaque handle rather than an integer; the reason for waiting
    // is that a type invented for a hypothetical caller is a type nobody can
    // check against a real use.

    m.add_function(wrap_pyfunction!(engine_linked, m)?)?;
    m.add_function(wrap_pyfunction!(chunk_budget_ms, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_bulk_hold, m)?)?;
    // The threshold `write_bulk_atomic` warns above, as a timedelta. Exposed so
    // a caller can compare an estimate against it rather than hard-coding 250ms.
    m.add(
        "BULK_ATOMIC_WARN_HOLD",
        macrame::prelude::BULK_ATOMIC_WARN_HOLD,
    )?;
    // The refusal threshold on `archive_windowed`'s session count. Exposed so a
    // caller computing a window from a span can check it before the call rather
    // than catching `ArchiveWindowError` after it.
    m.add(
        "MAX_ARCHIVE_SESSIONS",
        macrame::connection::MAX_ARCHIVE_SESSIONS,
    )?;
    // The four chunk ceilings. **Ceilings, not sizes** — since 0.12.0 the bulk
    // loops time each chunk and size the next from its measured hold (D-143,
    // D-146), so these are the largest a chunk will ever be and a populated
    // database converges below them. A Python caller dividing a batch by one of
    // these to predict transaction count is reading a 0.11.0 fact.
    m.add("CHUNK_ROWS_EDGES", macrame::connection::chunk_rows::EDGES)?;
    m.add(
        "CHUNK_ROWS_CONCEPTS",
        macrame::connection::chunk_rows::CONCEPTS,
    )?;
    m.add(
        "CHUNK_ROWS_ANNOTATIONS",
        macrame::connection::chunk_rows::ANNOTATIONS,
    )?;
    m.add(
        "CHUNK_ROWS_EMBEDDINGS",
        macrame::connection::chunk_rows::EMBEDDINGS,
    )?;
    m.add_function(wrap_pyfunction!(runtime::_block_for_testing, m)?)?;
    m.add_function(wrap_pyfunction!(runtime::_mark_forked, m)?)?;
    m.add_class::<testing::PyFakeClock>()?;
    m.add_function(wrap_pyfunction!(testing::_db_error_variants, m)?)?;
    m.add_function(wrap_pyfunction!(testing::_raise_db_error, m)?)?;
    m.add_function(wrap_pyfunction!(timestamps::_coerce_timestamp, m)?)?;
    m.add_function(wrap_pyfunction!(timestamps::_render_timestamp, m)?)?;
    Ok(())
}
