//! The async→sync boundary (P1, D-095).
//!
//! Every interesting method on [`macrame::Database`] is `async`, and Python is
//! not. This module is the whole of the translation: one process-wide tokio
//! runtime, and one rule about the GIL.
//!
//! # Why one runtime, and why it is never dropped
//!
//! A runtime per `Database` would mean N thread pools for N handles, and it
//! would put the sharpest failure mode in reach: `tokio::runtime::Runtime`'s
//! `Drop` panics when it runs inside a runtime context, and a runtime owned by
//! a Python object is dropped wherever the garbage collector decides. A
//! `OnceLock` in a static is never dropped, so that question does not arise.
//!
//! The cost is that the thread pool outlives the last `Database`. For an
//! embedded database in a Python process that is the right trade — the pool is
//! idle threads, and the alternative trades them for a shutdown ordering
//! problem that has no good answer.
//!
//! # The GIL rule
//!
//! **Every call that reaches the engine goes through [`block_on`], and
//! [`block_on`] releases the GIL.** Without that, one thread inside a traversal
//! stops the entire interpreter, which for an embedded database is the
//! difference between a library and a global lock. `Python::detach` — pyo3
//! 0.29's name for what was `allow_threads` — requires its closure to be
//! `Ungil`, satisfied by `Send`, which is why `macrame::Database` being
//! `Send + Sync` is the fact this whole design rests on. It is asserted at
//! compile time in [`assert_bounds`] so that a future field on `Database`
//! breaks here, with an explanation, rather than inside a `#[pyclass]` macro
//! expansion.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use pyo3::prelude::*;
use tokio::runtime::Runtime;

use crate::errors::MacrameError;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Set from `os.register_at_fork` on platforms that have it. See [`block_on`].
static FORKED: AtomicBool = AtomicBool::new(false);

/// The process-wide runtime, built on first use.
///
/// Multi-thread because `Database` spawns the write actor and the snapshot
/// cadence as tasks; a current-thread runtime would deadlock the first time
/// `block_on` waited on either. The time driver is not optional either — the
/// cadence loop is a `tokio::time::sleep` in a `select!`, and a runtime without
/// it panics rather than failing quietly.
///
/// Worker count is left at tokio's default (one per core). The write path is
/// serialised through a single actor regardless, so this is not a throughput
/// knob; it is only what keeps a blocking `block_on` from starving the actor it
/// is waiting on.
pub(crate) fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("macrame-py")
            .build()
            .expect("failed to start the tokio runtime for the Macrame bindings")
    })
}

/// Marks this process as a fork child, poisoning every later call.
///
/// Registered from `__init__.py` through `os.register_at_fork`, which exists
/// only on POSIX. On Linux `multiprocessing` still defaults to `fork`, and a
/// child inherits [`RUNTIME`] as a struct whose worker threads did not come
/// with it — the first `block_on` there waits forever on a pool that does not
/// exist.
///
/// **This does not make forking work; it makes it fail loudly.** A hang with no
/// output is the worst available outcome and it is what happens without this.
/// The supported answer is the `spawn` start method, which is already the
/// default on Windows and macOS.
#[pyfunction]
pub(crate) fn _mark_forked() {
    FORKED.store(true, Ordering::SeqCst);
}

/// Run `fut` to completion on the shared runtime, with the GIL released.
///
/// `T: Send` and `F: Send` are what `detach` requires; both hold for every
/// future in this crate because `Database: Sync`, so `&Database` is `Send`.
pub(crate) fn block_on<F, T>(py: Python<'_>, fut: F) -> PyResult<T>
where
    F: Future<Output = T> + Send,
    T: Send,
{
    check_not_forked()?;
    Ok(py.detach(|| runtime().block_on(fut)))
}

/// The fork guard, split out because callers that take a lock before touching
/// the runtime need to check it inside their own `detach` block.
pub(crate) fn check_not_forked() -> PyResult<()> {
    if FORKED.load(Ordering::Relaxed) {
        return Err(MacrameError::new_err(
            "this process is a fork() child, and the Macrame runtime did not \
             survive the fork — its worker threads exist only in the parent, so \
             this call would hang rather than fail. Use the 'spawn' start \
             method (multiprocessing.set_start_method('spawn'), already the \
             default on Windows and macOS), or open the database after forking \
             rather than before.",
        ));
    }
    Ok(())
}

/// Blocks for `seconds` through the real [`block_on`] path. **Test hook.**
///
/// P1's acceptance is that the GIL is released for the duration of a database
/// call, and that has to be demonstrated rather than asserted. At P1 there is
/// no operation slow enough to demonstrate it with — `open()` on a fresh file
/// is a few milliseconds — so this provides one, and it earns its place by
/// going through exactly the same [`block_on`] as every real method. If this
/// releases the GIL, they do; if it stops releasing it, they have too.
///
/// Underscore-prefixed and absent from `macrame.__all__`.
#[pyfunction]
pub(crate) fn _block_for_testing(py: Python<'_>, seconds: f64) -> PyResult<()> {
    block_on(py, async move {
        tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
    })
}

/// The bounds the design depends on, checked by the compiler.
///
/// `#[pyclass]` requires `Send`, and `allow_threads` requires `Ungil` — which
/// for stable pyo3 means `Send`. Without both, the only build that compiles is
/// `#[pyclass(unsendable)]`, which pins the object to the thread that created
/// it and makes `allow_threads` unavailable, i.e. a different and much worse
/// binding. A future field on `Database` that is not `Sync` should fail here.
#[allow(dead_code)]
fn assert_bounds() {
    fn require<T: Send + Sync>() {}
    require::<macrame::Database>();
}
