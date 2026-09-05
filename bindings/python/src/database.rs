//! The `Database` handle (P1).
//!
//! P1 exposes the lifecycle and nothing else: open, close, the context manager,
//! and the introspection that needs no ledger work. The write and read surfaces
//! are P4.x. What has to be right *here* is the shape everything else hangs
//! off, and there are three parts to it.
//!
//! # 1. `close()` consumes `self`, and Python cannot
//!
//! [`macrame::Database::close`] takes `self` by value — the type system makes a
//! call-after-close impossible in Rust. Python has no way to express that, so
//! the handle holds an `Option` and `close()` takes it. Every other method then
//! meets a `None` and raises [`crate::errors::MacrameClosedError`], which is
//! the same guarantee enforced one layer later.
//!
//! # 2. `RwLock`, not `&mut self` — and the plan said `&mut self`
//!
//! The obvious shape is a plain `Option<Database>` field with methods taking
//! `&mut self`, which is what this project's binding plan specified. **It is
//! wrong, and the reason is the GIL rule.** A non-`frozen` `#[pyclass]` borrows
//! through a runtime `RefCell`; we then release the GIL for the whole of a
//! database call while that borrow is live. A second Python thread entering any
//! `&mut self` method during that window gets `PyBorrowMutError` — an error
//! about pyo3's internals, raised for what is really an ordinary concurrent
//! call.
//!
//! `#[pyclass(frozen)]` over a `RwLock<Option<…>>` says the intended thing
//! instead: ordinary calls take a **read** lock and run concurrently, which is
//! what the architecture allows (reads are concurrent; writes are serialised by
//! the write actor regardless, one layer down). `close()` takes the **write**
//! lock, so it waits for in-flight calls rather than racing them.
//!
//! # 3. The lock is acquired with the GIL already released
//!
//! Subtle and load-bearing. If `close()` blocked on `inner.write()` while
//! holding the GIL, and another thread held the read lock inside `detach`,
//! neither could proceed: the reader needs the GIL back to finish, and the
//! closer will not release it. So the lock acquisition happens *inside* the
//! `detach` closure, never outside — which is also why
//! [`PyDatabase::with_db`] exists rather than each method taking its own guard.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple, PyType};

use macrame::prelude::*;

use crate::branch;
use crate::errors::{closed_error, to_py, to_py_bulk};
use crate::graph;
use crate::observe;
use crate::plan;
use crate::rows;
use crate::runtime::{check_not_forked, runtime};
use crate::temporal;
use crate::timestamps::{from_canonical, to_canonical};
use crate::types::{PyAnnotation, PyAttributeMode, PyConceptUpsert, PyEdgeAssertion};
use crate::vector;

/// A flag another thread can raise to stop a running bulk write (0.13.8, W7.6).
///
/// `bulk_import`, `write_concepts`, `upsert_embeddings` and
/// `write_analytics_annotations` hold the GIL released for their whole run, so
/// the thread that called one cannot cancel it — some *other* thread has to,
/// which is exactly what this is for. `cancel()` needs no lock beyond an atomic
/// store and is safe to call from a signal handler, a UI callback, or a
/// watchdog thread.
///
/// ```python
/// token = macrame.CancelToken()
/// threading.Timer(30.0, token.cancel).start()
/// try:
///     db.bulk_import(edges, cancel=token)
/// except macrame.BulkCancelledError as e:
///     print(f"stopped with {e.written} rows committed")
/// ```
///
/// The stop happens at a chunk boundary. Nothing rolls back, and the rows that
/// committed stay committed — the same boundary these four methods already
/// have when a chunk fails.
#[pyclass(name = "CancelToken", module = "macrame", frozen)]
// No `Clone`: the *token* is cheap to clone and does so internally, but a
// cloneable `#[pyclass]` opts into a by-value `FromPyObject`, and a token
// copied on the way into a call would be a token whose `cancel()` reached
// nothing. Taken as `PyRef` everywhere instead.
#[derive(Default)]
pub struct PyCancelToken {
    pub(crate) inner: CancelToken,
}

#[pymethods]
impl PyCancelToken {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// Ask the bulk write holding this token to stop at its next chunk
    /// boundary. Idempotent; a token never un-cancels.
    fn cancel(&self) {
        self.inner.cancel();
    }

    /// Whether `cancel()` has been called.
    #[getter]
    fn cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn __repr__(&self) -> String {
        format!("CancelToken(cancelled={})", self.inner.is_cancelled())
    }
}

/// Assemble a [`BulkControl`] from the two optional keyword arguments the four
/// chunked methods take (0.13.8, W7.6).
///
/// # The callback runs with the GIL re-acquired, on the ledger's thread
///
/// `with_db` detaches for the whole call, so a progress callback has to attach
/// again — every chunk boundary therefore costs one GIL acquire, and the import
/// is stalled for as long as the Python callable runs. That is the deal, it is
/// the same deal any progress callback makes, and it is why the docstrings say
/// to update a counter rather than write a file.
///
/// **A callback that raises stops the write.** The exception is captured, the
/// token is cancelled so the loop stops at the next boundary, and it is
/// re-raised in place of the `BulkCancelledError` that would otherwise be
/// reported. Swallowing it was the alternative, and a progress bar whose
/// failure silently becomes "the import finished" is worse than one that stops
/// the import.
fn bulk_control(
    progress: Option<Py<PyAny>>,
    cancel: Option<PyRef<'_, PyCancelToken>>,
    raised: &Arc<Mutex<Option<PyErr>>>,
) -> (BulkControl, CancelToken) {
    // Always a real token, even when the caller passed none: the progress
    // callback needs something to trip when it raises.
    let token = cancel.map(|t| t.inner.clone()).unwrap_or_default();
    let mut control = BulkControl::new().cancel_with(token.clone());

    if let Some(callback) = progress {
        let raised = Arc::clone(raised);
        let token = token.clone();
        control = control.on_progress(move |p| {
            Python::attach(|py| {
                let payload = PyDict::new(py);
                let built = payload
                    .set_item("written", p.written)
                    .and_then(|()| payload.set_item("total", p.total))
                    .and_then(|()| payload.set_item("rows", p.rows))
                    .and_then(|()| payload.set_item("held_ms", p.held.as_secs_f64() * 1000.0))
                    .and_then(|()| callback.call1(py, (payload,)).map(|_| ()));
                if let Err(e) = built {
                    *raised.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    token.cancel();
                }
            });
        });
    }

    (control, token)
}

/// Turn a bulk outcome into a `PyResult`, preferring an exception the progress
/// callback raised over the cancellation it caused.
fn bulk_result(
    outcome: macrame::BulkResult<usize>,
    raised: &Arc<Mutex<Option<PyErr>>>,
) -> PyResult<usize> {
    let callback_error = raised.lock().unwrap_or_else(|e| e.into_inner()).take();
    match outcome {
        Ok(n) => match callback_error {
            // The last chunk committed before the cancellation could be seen,
            // so the write genuinely succeeded -- but the caller's callback
            // still blew up and they must hear about it.
            Some(e) => Err(e),
            None => Ok(n),
        },
        Err(interrupted) => Err(match callback_error {
            Some(e) => {
                let _ = Python::attach(|py| e.value(py).setattr("written", interrupted.written));
                e
            }
            None => to_py_bulk(interrupted),
        }),
    }
}

/// A Macrame ledger handle.
#[pyclass(name = "Database", module = "macrame", frozen)]
pub(crate) struct PyDatabase {
    /// `None` once closed. See the module docs for why this is a `RwLock` and
    /// not a `&mut self` field.
    inner: RwLock<Option<Database>>,
    /// Cached so `path` and `__repr__` still answer after `close()`. A closed
    /// handle that cannot say what it was is needlessly unhelpful in a
    /// traceback.
    path: PathBuf,
    /// Serialises the diagnostic path's opens. See [`PyDatabase::diagnostic_rows`].
    diagnostic_open: Mutex<()>,
}

impl PyDatabase {
    /// Run `f` against the live handle with the GIL released.
    ///
    /// The single choke point for every call that touches the ledger. Three
    /// things happen here and they must happen in this order: the GIL is
    /// released, *then* the lock is taken (see module docs, part 3), then the
    /// closed check.
    fn with_db<F, T>(&self, py: Python<'_>, f: F) -> PyResult<T>
    where
        F: FnOnce(&Database) -> PyResult<T> + Send,
        T: Send,
    {
        py.detach(|| {
            check_not_forked()?;
            // A panic inside one call must not brick the handle for every
            // later one: poisoning here would turn a single failed traversal
            // into a permanently unusable database with a confusing message.
            // The data behind the lock is an `Option<Database>`, whose
            // invariants a panic cannot break.
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let db = guard.as_ref().ok_or_else(closed_error)?;
            f(db)
        })
    }

    /// Run `sql` on the diagnostic connection, **one caller at a time**.
    ///
    /// # Why this is serialised when nothing else here is
    ///
    /// Until 0.15.14 this said: `diagnostic_conn()` performs a new
    /// `libsql::Builder::…build()` per call, `with_db` releases the GIL, so two
    /// Python threads reach `build()` concurrently, and that is
    /// [R15](../../../README.md)'s shape — the upstream libSQL access violation
    /// on concurrent opens — reachable from ordinary Python with no `unsafe`.
    /// Measured at width 48: **7 bad runs in 18** without this lock.
    ///
    /// The conclusion held and the mechanism did not. `build()` costs 0.10 µs
    /// and opens nothing; `connect()` is the open, and it is what two threads
    /// were reaching concurrently (W15.4,
    /// [D-256](../../../docs/architecture/s13-decision-register.md#d-256),
    /// `examples/diagnostic_conn_probe.rs`). Since 0.15.14 there is **one
    /// read-only connection per `Database`**, minted on first use, so this path
    /// opens nothing after that first call and the unlocked arm measures
    /// **0 bad runs in 30** where it measured 3 before.
    ///
    /// # So why keep the lock
    ///
    /// Because it is no longer paying for a measurement, it is paying for a
    /// margin, and the margin is against an unfixed upstream memory-safety bug.
    /// What it now bounds is concurrent *use* of one shared connection — which
    /// SQLite serialises internally anyway, so the lock costs a diagnostic path
    /// nothing it was not already paying. Removing it would trade nothing
    /// measurable for less distance from `0xC0000005`, on the one surface a
    /// caller reaches for when they already doubt the typed answer.
    ///
    /// It does change one thing it did not change before: `diagnostic_conn()`
    /// no longer hands back the caller's own connection, so per-connection
    /// state persists between `diagnostic_query` calls on the same handle. Since
    /// 0.15.15 that is bounded — an open transaction is rolled back and a
    /// connection carrying temp objects or an `ATTACH` is replaced, on the way
    /// in to every call — with one documented residue: a
    /// pragma the crate does not itself set is still inherited by the next
    /// caller. See `Database::diagnostic_conn`'s rustdoc for the table, and
    /// [D-257](../../../docs/architecture/s13-decision-register.md#d-257) for
    /// what it cost to find out that a leaked `BEGIN` also made
    /// `Database.checkpoint()` a no-op.
    ///
    /// `std::sync::Mutex`, not `tokio`'s: the critical section is a
    /// `block_on`, not an await point, so there is no future to hold the guard
    /// across. Poisoning is ignored for the same reason as in `with_db` — the
    /// guarded data is `()`.
    ///
    /// # Lock order
    ///
    /// Taken *inside* `with_db`, so: GIL released, then `inner.read()`, then
    /// this. Nothing acquires them the other way round, and `close()` waits on
    /// `inner.write()` behind a diagnostic call rather than deadlocking with
    /// it.
    fn diagnostic_rows(
        &self,
        py: Python<'_>,
        sql: String,
        bound: Vec<libsql::Value>,
    ) -> PyResult<rows::RawRows> {
        self.with_db(py, move |db| {
            let _one_open_at_a_time = self
                .diagnostic_open
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            rows::map_err(runtime().block_on(async {
                let conn = db.diagnostic_conn().await?;
                // No scrub on the way out, and it was written before it was
                // measured (0.15.15, W15.5, D-257). The argument for one was
                // that a caller who sends `BEGIN` and never calls again pins a
                // WAL snapshot until the handle drops. Deleting the call left
                // the whole Python suite green, and the reason is that the
                // sequence does not exist: a bare `BEGIN` pins nothing — the
                // snapshot is taken by the first *read* inside the transaction
                // — and any statement that would take it arrives through this
                // method, whose entry scrub has already rolled the transaction
                // back. `Database::scrub_diagnostic_conn` stays for the Rust
                // caller who holds a clone across both, which is a sequence
                // that does exist.
                rows::collect(&conn, &sql, bound).await
            }))
        })
    }

    /// The instant a read should be taken at: what the caller said, else the
    /// handle's clock.
    ///
    /// Every read method takes `now` and every one of them defaults it the same
    /// way. Reading the clock through the handle rather than calling
    /// `SystemClock` directly matters for the same reason `open_with_clock`
    /// exists: a handle opened against an injected clock must answer "now" with
    /// *its* now, or a fixture's reads drift away from its writes.
    fn instant(&self, py: Python<'_>, now: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        match now {
            Some(t) => to_canonical(Some(t)),
            None => self.with_db(py, |db| Ok(db.clock().now())),
        }
    }
}

#[pymethods]
impl PyDatabase {
    /// Open a ledger at `path`, running migrations and starting the write actor.
    ///
    /// The snapshot cadence (§5.5) writes an anchor once the transaction log has
    /// grown `snapshot_every_entries` past the last one, checking that distance
    /// every `snapshot_poll_seconds`. Pass `snapshot_every_entries=None` to run
    /// without a cadence at all — the right setting for a short-lived process
    /// that will not accumulate a delta worth bounding, and for tests that
    /// assert on the contents of the snapshot directory.
    ///
    /// **Prefer the context manager.** See [`PyDatabase::close`] for what a
    /// handle that is merely dropped loses.
    ///
    /// # The tuning knobs (0.13.0, W5, W6.4)
    ///
    /// Rust takes these as a `Tuning` struct, which exists so the set can grow
    /// without breaking callers. Python has keyword arguments for that, so they
    /// arrive as keywords here rather than as a value type to import and
    /// construct — the same information, one fewer name.
    ///
    /// **Every one of them defaults to "leave it alone", and none of them
    /// spells that `None`-means-off.** That is [D-155](../../../docs/architecture/s13-decision-register.md)'s
    /// lesson repeated at this boundary: a default that silently *disables* a
    /// mechanism is the failure mode, and it reaches every caller who did not
    /// know the knob existed.
    ///
    /// - `wal_autocheckpoint`: `None` leaves SQLite's own default (1,000
    ///   pages); `"disabled"` turns automatic checkpointing off, which is only
    ///   correct if you call `checkpoint()` yourself; a positive integer is a
    ///   page threshold. `0` is **refused** rather than read as SQLite's
    ///   "disabled" overload, because a caller who computed a threshold and got
    ///   zero has a bug, and inheriting the overload turns it into an unbounded
    ///   WAL ([D-157](../../../docs/architecture/s13-decision-register.md)).
    /// - `future_stamps`: what to do about a stored `recorded_at` from the
    ///   future (0.13.5, W7.4,
    ///   [D-178](../../../docs/architecture/s13-decision-register.md)). `None`
    ///   refuses beyond a day; a number of seconds sets your own tolerance
    ///   (`0` refuses anything at all ahead of the wall clock); `"allow"`
    ///   opens the file regardless, which is a reading path and not a repair.
    ///   Same shape as `wal_autocheckpoint` and for the same reason: absent
    ///   means *the bound applies*, never *the bound is off*.
    /// - `writer_cache_size` / `reader_cache_size`: SQLite `cache_size` units —
    ///   negative is KiB, positive is pages. Split because the writer is one
    ///   connection and the readers are several
    ///   ([D-158](../../../docs/architecture/s13-decision-register.md)), so one
    ///   number would mean either starving the writer or multiplying the
    ///   readers' footprint.
    #[staticmethod]
    #[pyo3(signature = (
        path,
        *,
        snapshot_every_entries = Some(10_000),
        snapshot_poll_seconds = 5.0,
        wal_autocheckpoint = None,
        writer_cache_size = None,
        reader_cache_size = None,
        future_stamps = None,
    ))]
    // Keyword-only tuning knobs, one per thing that can be tuned: the same
    // reason the other signatures in this file carry this allow.
    #[allow(clippy::too_many_arguments)]
    fn open(
        py: Python<'_>,
        path: PathBuf,
        snapshot_every_entries: Option<i64>,
        snapshot_poll_seconds: f64,
        wal_autocheckpoint: Option<&Bound<'_, PyAny>>,
        writer_cache_size: Option<i32>,
        reader_cache_size: Option<i32>,
        future_stamps: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let cadence = to_cadence(snapshot_every_entries, snapshot_poll_seconds)?;
        // The two cache sizes arrive from Python as `Option<i32>` where the
        // setter takes an `i32`, so they are applied only when present —
        // which is the same thing `..Default::default()` did for them, said
        // once per field instead of once for the struct.
        let mut tuning = macrame::Tuning::default()
            .cadence(match cadence {
                Some(c) => macrame::CadencePolicy::Every(c),
                None => macrame::CadencePolicy::Disabled,
            })
            .wal_autocheckpoint(to_wal_policy(wal_autocheckpoint)?)
            .future_stamps(to_future_stamp_policy(future_stamps)?);
        if let Some(size) = writer_cache_size {
            tuning = tuning.writer_cache_size(size);
        }
        if let Some(size) = reader_cache_size {
            tuning = tuning.reader_cache_size(size);
        }
        let tuning = tuning;

        let owned = path.clone();
        let db =
            crate::runtime::block_on(
                py,
                async move { Database::open_tuned(&owned, tuning).await },
            )?
            .map_err(to_py)?;

        Ok(Self {
            inner: RwLock::new(Some(db)),
            path,
            diagnostic_open: Mutex::new(()),
        })
    }

    /// Open against an injected [`crate::testing::PyFakeClock`] (W6.3).
    ///
    /// **A test hook, and underscore-prefixed for the reason §14.6 gives**: a
    /// clock injected into a production ledger writes a `recorded_at` axis that
    /// no longer records anything. What it takes is a `_FakeClock`, not a
    /// `Clock` implementation — a caller cannot supply their own, so "inject
    /// arbitrary time into a real database" is not reachable from here at all.
    ///
    /// What it buys is the capability the suite did not have: `recorded_at` is
    /// the transaction-time axis, and every Python assertion about it was
    /// previously impossible rather than merely awkward. That is defect K's
    /// shape on the side that never got D-062's fix.
    ///
    /// Stamps are exact only on a **fresh** file. Opening a populated one
    /// raises the clock to the newest stored `recorded_at` before the actor
    /// starts, because the alternative is aborting the first write on
    /// `trg_concepts_monotonic_ra`.
    #[staticmethod]
    #[pyo3(name = "_open_with_clock")]
    #[pyo3(signature = (path, clock, *, snapshot_every_entries = Some(10_000), snapshot_poll_seconds = 5.0))]
    fn open_with_clock(
        py: Python<'_>,
        path: PathBuf,
        clock: PyRef<'_, crate::testing::PyFakeClock>,
        snapshot_every_entries: Option<i64>,
        snapshot_poll_seconds: f64,
    ) -> PyResult<Self> {
        let cadence = to_cadence(snapshot_every_entries, snapshot_poll_seconds)?;
        let tuning = macrame::Tuning::default()
            .cadence(match cadence {
                Some(c) => macrame::CadencePolicy::Every(c),
                None => macrame::CadencePolicy::Disabled,
            })
            .clock(clock.inner.clone());

        let owned = path.clone();
        let db =
            crate::runtime::block_on(
                py,
                async move { Database::open_tuned(&owned, tuning).await },
            )?
            .map_err(to_py)?;

        Ok(Self {
            inner: RwLock::new(Some(db)),
            path,
            diagnostic_open: Mutex::new(()),
        })
    }

    /// Shut down the write actor and write the final snapshot.
    ///
    /// **Idempotent**: closing an already-closed handle is a no-op, not an
    /// error, so `__exit__` after an explicit `close()` is fine.
    ///
    /// Two things are lost by never calling this, and only one of them is
    /// obvious. The final snapshot means the next `reconstruct` folds from an
    /// older anchor — *slower, not wrong*, since a snapshot is derivative state
    /// under Doctrine VI. The second is the write actor's exit status, which no
    /// other method can return: a handle that is dropped cannot tell you its
    /// write path had died.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            check_not_forked()?;
            let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
            match guard.take() {
                Some(db) => runtime().block_on(db.close()).map_err(to_py),
                None => Ok(()),
            }
        })
    }

    /// `with Database.open(p) as db:` — the supported way to use a handle.
    ///
    /// Not sugar. The Rust `Drop` impl notes a missed `close()` at
    /// `tracing::warn!`, which is invisible in any application that has not
    /// configured a subscriber — essentially every Python application. Python's
    /// garbage collector is non-deterministic on top of that, so a handle that
    /// is merely dropped is closed at an unpredictable time, or not at all.
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Closes, and does **not** suppress an exception from the body.
    ///
    /// If `close()` itself fails while another exception is propagating, this
    /// raises and Python chains the original onto `__context__`. That is the
    /// right way round: the write actor's `Result` is only available here, and
    /// swallowing it to preserve the original exception would hide a dead write
    /// path behind an unrelated error.
    #[pyo3(signature = (exc_type = None, exc_value = None, traceback = None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        exc_value: Option<&Bound<'_, PyAny>>,
        traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = (exc_type, exc_value, traceback);
        self.close(py)?;
        Ok(false)
    }

    /// Whether [`PyDatabase::close`] has run.
    #[getter]
    fn is_closed(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
    }

    /// The database file this handle opened, as a `pathlib.Path`.
    ///
    /// Answers after `close()` too — see the field's comment.
    #[getter]
    fn path<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        to_pathlib(py, &self.path)
    }

    /// The cold-storage file, derived by convention: `foo.db` → `foo_archive.db`.
    #[getter]
    fn archive_path<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let p = self.with_db(py, |db| Ok(db.archive_path().to_path_buf()))?;
        to_pathlib(py, &p)
    }

    /// The snapshot directory, derived by convention: `foo.db` → `foo_snapshots/`.
    #[getter]
    fn snapshots_dir<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let p = self.with_db(py, |db| Ok(db.snapshots_dir().to_path_buf()))?;
        to_pathlib(py, &p)
    }

    /// The migration level this file is at.
    #[getter]
    fn schema_version(&self, py: Python<'_>) -> PyResult<u32> {
        self.with_db(py, |db| Ok(db.schema_version()))
    }

    // -- write surface (P4.1) ------------------------------------------------
    //
    // Every one of these crosses the Write Actor's channel, and §5.1.8 / D-028
    // says what that means: **awaiting a write waits on a Rust channel, not in
    // SQLite**, so `busy_timeout` does not bound it. During an in-flight
    // `rebuild_current` or `archive` the caller stalls for that transaction's
    // duration.
    //
    // The Python consequence belongs where a caller will look for it: running
    // one of these on a thread and giving up on it does **not** cancel it. The
    // command stays queued and commits when the actor reaches it. There is no
    // cancellation at this boundary, and implying one would be worse than saying
    // so.
    //
    // Values were validated in their constructors (P3), so nothing here can fail
    // on a malformed edge type or timestamp — that already happened, at the line
    // that built the value.

    /// Assert an edge. Doctrine III: a new row, never an update.
    ///
    /// Raises `SingleOpenViolationError` if the relationship already has an open
    /// interval — retire it first — and `OverlappingIntervalError` if the
    /// asserted interval collides with one already recorded.
    fn assert_edge(&self, py: Python<'_>, edge: PyEdgeAssertion) -> PyResult<()> {
        let edge = edge.inner;
        self.with_db(py, move |db| {
            runtime().block_on(db.assert_edge(edge)).map_err(to_py)
        })
    }

    /// Close an open interval by asserting its replacement (Doctrine III).
    ///
    /// `valid_to` is when the relationship stopped being true, and it may not be
    /// `None`: `None` means *open*, and retiring something to an open end is not
    /// a retirement. Refused here rather than passed down, because the ledger
    /// would otherwise answer with a single-open violation about a row the
    /// caller did not think they were writing.
    /// # `branch=` here, two methods in Rust
    ///
    /// The Rust surface splits this into `retire_edge` and `retire_edge_on`,
    /// because a sixth positional `Option<BranchId>` would make every existing
    /// call site read as though it had made a lineage decision it never made.
    /// Python has keyword arguments with defaults, so the split buys nothing
    /// here and would cost a second name to keep in step. Retiring on a branch
    /// is **shadow retirement**: the branch writes its own row at the
    /// ancestor's key and the ancestor's row is untouched.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (source, target, edge_type, valid_from, valid_to, *, branch = None))]
    fn retire_edge(
        &self,
        py: Python<'_>,
        source: String,
        target: String,
        edge_type: String,
        valid_from: &Bound<'_, PyAny>,
        valid_to: &Bound<'_, PyAny>,
        branch: Option<String>,
    ) -> PyResult<()> {
        if valid_to.is_none() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "retire_edge needs an instant for valid_to; None means an open \
                 interval, and retiring an edge to an open end is not a \
                 retirement. Pass the moment the relationship stopped being true.",
            ));
        }
        let from = to_canonical(Some(valid_from))?;
        let to = to_canonical(Some(valid_to))?;
        let branch = branch
            .map(|name| crate::branch::branch_id(&name))
            .transpose()?;
        self.with_db(py, move |db| {
            runtime()
                .block_on(async {
                    match branch {
                        Some(b) => {
                            db.retire_edge_on(source, target, edge_type, &from, &to, b)
                                .await
                        }
                        None => db.retire_edge(source, target, edge_type, &from, &to).await,
                    }
                })
                .map_err(to_py)
        })
    }

    /// Insert or update a concept.
    fn upsert_concept(&self, py: Python<'_>, concept: PyConceptUpsert) -> PyResult<()> {
        let concept = concept.inner;
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.upsert_concept(concept))
                .map_err(to_py)
        })
    }

    /// Assert many edges in **one transaction under one stamp** (D-014).
    ///
    /// The batch is one act, so it cannot be chunked — splitting it is the thing
    /// this method exists not to do. That makes the actor's hold a function of
    /// `len(edges)`, and **every other writer in the process waits that long**.
    /// Measured on libSQL 0.9.30 (T1.3, D-081): 500 rows ~34 ms, 2,000 ~155 ms,
    /// 10,000 ~1.0 s, 20,000 ~2.6 s.
    ///
    /// Call `macrame.estimate_bulk_hold(edges)` *before* this to find out which
    /// of those you are in.
    ///
    /// A caller who needs the latency bound and not the atomicity wants
    /// `bulk_import`, which is the same write chunked and explicitly not atomic
    /// overall (D-011).
    fn write_bulk_atomic(&self, py: Python<'_>, edges: Vec<PyEdgeAssertion>) -> PyResult<usize> {
        let edges: Vec<EdgeAssertion> = edges.into_iter().map(|e| e.inner).collect();
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.write_bulk_atomic(edges))
                .map_err(to_py)
        })
    }

    /// Import edges on the background channel, chunked (D-011).
    ///
    /// **Atomic per chunk, not overall**: a failure partway leaves earlier
    /// chunks committed. That is the trade for a bounded hold — use
    /// `write_bulk_atomic` when the batch must be all-or-nothing.
    ///
    /// **Where the chunks fall depends on the machine** (0.12.0). The loop
    /// measures each chunk and sizes the next one from it, so the same edges
    /// imported twice can land under a different number of `recorded_at`
    /// stamps. Each chunk is still exactly one transaction under exactly one
    /// stamp, and a reader mid-import still sees a prefix and never half a
    /// chunk — what is not promised is that two identical calls stamp
    /// identically. `write_bulk_atomic` is the escape hatch if you need that.
    ///
    /// **On failure, the exception carries `written`** (0.13.8, W7.6): the
    /// number of rows the chunks before the stop committed, which are still
    /// committed. The exception *class* is still chosen by what went wrong, so
    /// `except NotFoundError` catches the same thing it always did.
    ///
    /// `progress`, if given, is called after every chunk with one dict:
    /// `written`, `total`, `rows`, `held_ms`. It runs on the ledger's thread
    /// with the GIL re-acquired, so it is on the critical path — update a
    /// counter, do not write a file. An exception raised there stops the
    /// import and is what propagates.
    ///
    /// `cancel` takes a `CancelToken`. This call holds the GIL released for its
    /// whole run, so the cancelling thread has to be a different one.
    #[pyo3(signature = (edges, *, progress = None, cancel = None))]
    fn bulk_import(
        &self,
        py: Python<'_>,
        edges: Vec<PyEdgeAssertion>,
        progress: Option<Py<PyAny>>,
        cancel: Option<PyRef<'_, PyCancelToken>>,
    ) -> PyResult<usize> {
        let edges: Vec<EdgeAssertion> = edges.into_iter().map(|e| e.inner).collect();
        let raised = Arc::new(Mutex::new(None));
        let (control, _token) = bulk_control(progress, cancel, &raised);
        self.with_db(py, move |db| {
            bulk_result(
                runtime().block_on(db.bulk_import_with(edges, control)),
                &raised,
            )
        })
    }

    /// Upsert many concepts on the background channel, chunked (D-011).
    ///
    /// Every row written here is a **ledger** write: it versions the concept and
    /// lands in `transaction_log`. Derived analytics output does not belong here
    /// — see `write_analytics_annotations` and D-041.
    ///
    /// **On failure, the exception carries `written`** (0.13.8, W7.6): the
    /// number of rows the chunks before the stop committed, which are still
    /// committed. The exception *class* is still chosen by what went wrong, so
    /// `except NotFoundError` catches the same thing it always did.
    ///
    /// `progress`, if given, is called after every chunk with one dict:
    /// `written`, `total`, `rows`, `held_ms`. It runs on the ledger's thread
    /// with the GIL re-acquired, so it is on the critical path — update a
    /// counter, do not write a file. An exception raised there stops the
    /// import and is what propagates.
    ///
    /// `cancel` takes a `CancelToken`. This call holds the GIL released for its
    /// whole run, so the cancelling thread has to be a different one.
    #[pyo3(signature = (concepts, *, progress = None, cancel = None))]
    fn write_concepts(
        &self,
        py: Python<'_>,
        concepts: Vec<PyConceptUpsert>,
        progress: Option<Py<PyAny>>,
        cancel: Option<PyRef<'_, PyCancelToken>>,
    ) -> PyResult<usize> {
        let concepts: Vec<ConceptUpsert> = concepts.into_iter().map(|c| c.inner).collect();
        let raised = Arc::new(Mutex::new(None));
        let (control, _token) = bulk_control(progress, cancel, &raised);
        self.with_db(py, move |db| {
            bulk_result(
                runtime().block_on(db.write_concepts_with(concepts, control)),
                &raised,
            )
        })
    }

    /// Write derived analytics results, chunked (§5.4, D-041).
    ///
    /// Rows go to `analytics_annotations`, which carries **no log trigger**:
    /// nothing written here reaches `transaction_log` and nothing here versions
    /// a concept. Rerunning an algorithm replaces the previous pass rather than
    /// recording that the world changed.
    ///
    /// That is the whole of D-041, and it is why `Annotation` is a separate type
    /// from `ConceptUpsert`. Writing one as the other overwrote concept content
    /// with labels and recorded every analytics rerun as a fresh version of the
    /// world.
    ///
    /// **On failure, the exception carries `written`** (0.13.8, W7.6): the
    /// number of rows the chunks before the stop committed, which are still
    /// committed. The exception *class* is still chosen by what went wrong.
    ///
    /// `progress` is called after every chunk with one dict — `written`,
    /// `total`, `rows`, `held_ms` — on the ledger's thread with the GIL
    /// re-acquired, so it is on the critical path. An exception raised there
    /// stops the write and is what propagates. `cancel` takes a `CancelToken`,
    /// which some *other* thread must trip: this call runs with the GIL
    /// released.
    #[pyo3(signature = (annotations, *, progress = None, cancel = None))]
    fn write_analytics_annotations(
        &self,
        py: Python<'_>,
        annotations: Vec<PyAnnotation>,
        progress: Option<Py<PyAny>>,
        cancel: Option<PyRef<'_, PyCancelToken>>,
    ) -> PyResult<usize> {
        let annotations: Vec<Annotation> = annotations.into_iter().map(|a| a.inner).collect();
        let raised = Arc::new(Mutex::new(None));
        let (control, _token) = bulk_control(progress, cancel, &raised);
        self.with_db(py, move |db| {
            bulk_result(
                runtime().block_on(db.write_analytics_annotations_with(annotations, control)),
                &raised,
            )
        })
    }

    // -- read surface (P4.2) --------------------------------------------------
    //
    // These do **not** cross the write actor's channel. They run on the shared
    // read connection, concurrently with each other and with writes, which is
    // why `with_db` takes a read lock and why none of them can stall behind a
    // rebuild the way §5.1.8 describes for the write path.
    //
    // `now` defaults to the handle's clock. It is exposed because the crate
    // exposes it: `now_ts` is the caller's present, and a caller replaying a
    // fixture needs to say what "now" means for it.

    /// Node ids reachable from `start_node`, in id order (§5.2).
    ///
    /// Topology only. No attribute mode is involved, so this can never raise
    /// `AttributeModeUnstatedError` — topology at an instant is unambiguous, and
    /// it is only the *pairing* with live attributes that needed a decision.
    ///
    /// This is the method to use with `AttributeMode.OMIT`'s intent: it says
    /// what it found, where `traverse` under that mode could not.
    ///
    /// `as_of_valid` bounds the edges by their own validity; `as_of_recorded`
    /// folds `transaction_log` and reads the topology the ledger *held* at that
    /// instant. Setting both asks the bitemporal question. See `traverse` and
    /// the Rust `TraversalBuilder::as_of_valid` for why one parameter became two
    /// in 0.13.2 (W7.1, D-174).
    ///
    /// `as_of_recorded` raises `RecordedInstantUnreachableError` when the hot log
    /// has been archived below the instant asked for; `reconstruct` takes the
    /// archive path and answers the same question.
    ///
    /// `branch` reads one lineage's belief instead of the trunk's (0.14.4,
    /// D-220): the edges on the path from it to the root, one per edge key,
    /// from the nearest branch holding it — so a branch that corrected or
    /// retired an inherited edge is seen to have done so. Unset is the trunk.
    /// A lineage that is not registered raises `UnknownBranchError` naming it,
    /// rather than quietly answering for the trunk. Until `fork()` lands there
    /// is no way to create a second lineage from Python, which is why this
    /// parameter arrives with the read rather than after the write.
    /// `limit` bounds the **walk** rather than the list it produces (0.15.10,
    /// W13.5). It stops the recursion, so the answer is the nodes nearest
    /// `start_node` and at most `limit` of them — fewer is possible and does
    /// not mean the graph was smaller, because one node can enter the walk at
    /// two depths and a retired concept is dropped after the walk has paid for
    /// it. Use `traverse_ids_explained` when that difference matters; this
    /// method cannot report it and does not pretend to.
    #[pyo3(signature = (
        start_node, *, max_depth = 2, edge_types = None, min_weight = 0.0,
        as_of_valid = None, as_of_recorded = None, branch = None, limit = None,
        now = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn traverse_ids(
        &self,
        py: Python<'_>,
        start_node: &str,
        max_depth: usize,
        edge_types: Option<Vec<String>>,
        min_weight: f64,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        as_of_recorded: Option<&Bound<'_, PyAny>>,
        branch: Option<String>,
        limit: Option<usize>,
        now: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<String>> {
        Ok(self
            .traverse_ids_explained(
                py,
                start_node,
                max_depth,
                edge_types,
                min_weight,
                as_of_valid,
                as_of_recorded,
                branch,
                limit,
                now,
            )?
            .0)
    }

    /// `traverse_ids`, plus whether `limit` cut the walk short (0.15.10, W13.5).
    ///
    /// Returns `(ids, truncated)`. `truncated` is `False` for every traversal
    /// that set no `limit`, and for one that set a ceiling the walk never
    /// reached; it is `True` when the walk stopped because it ran out of
    /// budget, so more of the graph satisfies the traversal than came back.
    ///
    /// **A `bool` rather than a class**, for the reason `CostEstimate` reports
    /// `candidates_capped` the same way: the underlying `WalkOutcome` has two
    /// states and no payload, and a class would make a caller unwrap it to
    /// reach a question they wanted to write an `if` on.
    ///
    /// The answer is exact rather than inferred. `len(ids) == limit` is not the
    /// same question — the walk's rows and the ids that survive its projection
    /// are different counts — so a walk cut at 10 rows can return 8 ids, and
    /// guessing from the length would call that a complete answer.
    #[pyo3(signature = (
        start_node, *, max_depth = 2, edge_types = None, min_weight = 0.0,
        as_of_valid = None, as_of_recorded = None, branch = None, limit = None,
        now = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn traverse_ids_explained(
        &self,
        py: Python<'_>,
        start_node: &str,
        max_depth: usize,
        edge_types: Option<Vec<String>>,
        min_weight: f64,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        as_of_recorded: Option<&Bound<'_, PyAny>>,
        branch: Option<String>,
        limit: Option<usize>,
        now: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Vec<String>, bool)> {
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        let as_of_recorded = as_of_recorded.map(|t| to_canonical(Some(t))).transpose()?;
        let now = self.instant(py, now)?;
        let b = graph::builder(
            start_node,
            max_depth,
            edge_types,
            min_weight,
            None,
            as_of_valid,
            as_of_recorded,
            branch,
            limit,
        );
        let (ids, outcome) = self.with_db(py, move |db| {
            runtime()
                .block_on(b.execute_ids_explained(db.read_conn(), &now))
                .map_err(to_py)
        })?;
        Ok((ids, outcome.hit_limit()))
    }

    /// Traverse and hydrate attributes, as a list of `NodeAttributes` (§5.2).
    ///
    /// `attribute_mode` decides *which text* comes back and the two `as_of_*`
    /// parameters decide *which topology*. They are independent questions, and
    /// setting either instant without stating a mode raises
    /// `AttributeModeUnstatedError` rather than defaulting (D-085): a historical
    /// topology with live attributes returns the past's graph wearing the
    /// present's titles — a legitimate thing to want and a terrible thing to get
    /// by accident.
    ///
    /// **`as_of` became `as_of_valid` and `as_of_recorded` in 0.13.2 (W7.1,
    /// D-174).** The old single parameter reached `links.valid_from`/`valid_to`
    /// on the valid-time axis and `transaction_log.recorded_at` on the
    /// transaction-time axis, so one keyword asked two questions. `as_of_valid`
    /// is *what was true*; `as_of_recorded` is *what we believed*; setting both
    /// asks what we believed then about what was true then, which no surface in
    /// the crate could express before.
    ///
    ///
    /// `branch` reads one lineage's belief instead of the trunk's (0.14.4,
    /// D-220): the edges on the path from it to the root, one per edge key,
    /// from the nearest branch holding it — so a branch that corrected or
    /// retired an inherited edge is seen to have done so. Unset is the trunk.
    /// A lineage that is not registered raises `UnknownBranchError` naming it,
    /// rather than quietly answering for the trunk. Until `fork()` lands there
    /// is no way to create a second lineage from Python, which is why this
    /// parameter arrives with the read rather than after the write.
    /// `AttributeMode.OMIT` is **refused** here, with a message naming
    /// `traverse_ids`. Under that mode there are no attributes to hydrate, so
    /// the Rust method answers with an empty list that no caller can tell apart
    /// from a traversal that reached nothing. See the module docs for why this
    /// is the one place the binding refuses what the library accepts.
    ///
    /// `limit` bounds the walk exactly as it does on `traverse_ids`, and this
    /// method cannot say whether it bit — a list of hydrated nodes has no room
    /// for the answer. `traverse_ids_explained` is where that is asked; the
    /// keyword is offered here anyway because refusing it would leave the one
    /// surface a caller reaches for topology *and* attributes unable to bound
    /// its own cost.
    #[pyo3(signature = (
        start_node, *, max_depth = 2, edge_types = None, min_weight = 0.0,
        attribute_mode = None, as_of_valid = None, as_of_recorded = None,
        branch = None, limit = None, now = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn traverse(
        &self,
        py: Python<'_>,
        start_node: &str,
        max_depth: usize,
        edge_types: Option<Vec<String>>,
        min_weight: f64,
        attribute_mode: Option<PyAttributeMode>,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        as_of_recorded: Option<&Bound<'_, PyAny>>,
        branch: Option<String>,
        limit: Option<usize>,
        now: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<graph::PyNodeAttributes>> {
        if attribute_mode == Some(PyAttributeMode::Omit) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "traverse() cannot hydrate under AttributeMode.OMIT: there are no \
                 attributes to return, so the answer would be an empty list that \
                 you could not tell apart from a traversal that reached nothing. \
                 Use traverse_ids(), which returns the ids OMIT is for.",
            ));
        }
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        let as_of_recorded = as_of_recorded.map(|t| to_canonical(Some(t))).transpose()?;
        let now = self.instant(py, now)?;
        let b = graph::builder(
            start_node,
            max_depth,
            edge_types,
            min_weight,
            attribute_mode,
            as_of_valid,
            as_of_recorded,
            branch,
            limit,
        );
        let hydrated = self.with_db(py, move |db| {
            runtime()
                .block_on(b.execute(db.read_conn(), &now))
                .map_err(to_py)
        })?;
        Ok(hydrated
            .into_iter()
            .map(|inner| graph::PyNodeAttributes { inner })
            .collect())
    }

    /// Load the neighbourhood of `start_node` as a `Subgraph` (§5.4, D-073).
    ///
    /// `byte_budget` has **no default, because the crate has none**. It bounds
    /// the payload this call may materialise and raises `SubgraphTooLargeError`
    /// rather than allocating past it; how much memory this process may spend on
    /// one graph is not a question a binding can answer.
    ///
    /// # `min_weight` unstated is not `0.0`
    ///
    /// Leave it unset and negative-weight edges reach the guard, raising
    /// `NegativeEdgeWeightError` — which is what you want before running
    /// `dijkstra`, unsound over them. State a floor and edges below it are
    /// *filtered*, not refused, because a caller who states one has asked to
    /// exclude them. That is the difference between the crate's `load_subgraph`
    /// and `load_subgraph_with`, preserved rather than flattened.
    ///
    /// `edge_types` and `min_weight` bound the walk **and** the returned
    /// adjacency. Filtering only the walk would hand a caller who asked for
    /// `CITES` a graph reached via `CITES` and populated with `KNOWS` as well.
    ///
    /// # `content` is off, and asking for it is what spends the budget
    ///
    /// `NodeData.content` is `None` unless `content=True` (0.8.0, D-116). No
    /// algorithm on this graph reads it — `dijkstra`, `astar`, `scc`, `k_core`,
    /// `louvain` and `modularity` touch topology and weight only — and at
    /// realistic document sizes the text is most of the payload, so the default
    /// was spending `byte_budget` on bytes nothing would look at. A caller who
    /// wants the text asks, and pays.
    ///
    /// **`False` here matches the crate's default rather than softening it.** A
    /// binding that disagreed with the layer below about a default would be a
    /// second source of truth, which is the thing the opaque `Subgraph` handle
    /// exists to avoid.
    ///
    /// `traverse` is unaffected: it hydrates by `attribute_mode`, which asks a
    /// different question — *which* text, not *whether* (D-102).
    ///
    /// **The instants are honoured here as of 0.13.2 (W7.1, F-35).** They could
    /// not be reached from this binding at all before, and the Rust loader
    /// ignored them when they were set on a builder passed to it — a historical
    /// subgraph load silently returned the present. Both halves are fixed
    /// together because they were one bug.
    #[pyo3(signature = (
        start_node, max_hops, byte_budget, *, edge_types = None,
        min_weight = None, as_of_valid = None, as_of_recorded = None,
        branch = None, now = None, content = false
    ))]
    #[allow(clippy::too_many_arguments)]
    fn load_subgraph(
        &self,
        py: Python<'_>,
        start_node: &str,
        max_hops: usize,
        byte_budget: usize,
        edge_types: Option<Vec<String>>,
        min_weight: Option<f64>,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        as_of_recorded: Option<&Bound<'_, PyAny>>,
        branch: Option<String>,
        now: Option<&Bound<'_, PyAny>>,
        content: bool,
    ) -> PyResult<graph::PySubgraph> {
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        let as_of_recorded = as_of_recorded.map(|t| to_canonical(Some(t))).transpose()?;
        let now = self.instant(py, now)?;
        let b = graph::builder(
            start_node,
            max_hops,
            edge_types,
            min_weight.unwrap_or(f64::NEG_INFINITY),
            None,
            as_of_valid,
            as_of_recorded,
            branch,
            // No keyword, deliberately. A subgraph's bound is `byte_budget`,
            // which *refuses* with `SubgraphTooLargeError` rather than
            // truncating, and a `Subgraph` has nowhere to record that its walk
            // was cut short — so a second, weaker ceiling here would return a
            // sample that looks like a neighbourhood.
            None,
        )
        .content(content);
        let inner = self.with_db(py, move |db| {
            runtime()
                .block_on(db.load_subgraph_with(&b, &now, byte_budget))
                .map_err(to_py)
        })?;
        Ok(graph::PySubgraph { inner })
    }

    // -- temporal surface (P4.3) ---------------------------------------------

    /// The world as believed at `ts` (§5.5).
    ///
    /// Folds from the newest snapshot at or before `ts` and replays the log
    /// forward, reading cold storage when the instant predates the archive
    /// horizon. A snapshot is derivative state (Doctrine VI), so a missing one
    /// makes this slower and never wrong.
    fn reconstruct(
        &self,
        py: Python<'_>,
        ts: &Bound<'_, PyAny>,
    ) -> PyResult<temporal::PyMaterializedState> {
        let ts = to_canonical(Some(ts))?;
        let inner = self.with_db(py, move |db| {
            runtime().block_on(db.reconstruct(&ts)).map_err(to_py)
        })?;
        Ok(temporal::PyMaterializedState { inner })
    }

    /// The world at `ts` **as one lineage saw it** (0.15.17, D-259).
    ///
    /// `reconstruct` answers about the ledger and returns every lineage's
    /// belief; this answers about `branch`. Each ancestor is bounded at its own
    /// fork point and each edge key is taken from the nearest lineage holding
    /// it — the resolution the traversals and `edges()` read through, applied
    /// to a fold.
    ///
    /// `concepts` is narrowed but **not resolved**: a lineage outside the
    /// ancestry contributes nothing and an ancestor's post-cutoff writes are cut,
    /// but where two *visible* lineages both wrote a concept the winner is the
    /// later log row rather than the nearer lineage. A folded concept row carries
    /// no branch, so there is no nearest one left to pick. Only `edges` gets the
    /// distance rule.
    ///
    /// On an **unforked** database this is `reconstruct` — same path, same
    /// snapshots. On a forked one it cannot use snapshots at all: a snapshot
    /// has no `recorded_at` left in it for a fork point to be compared
    /// against, so this folds from genesis and costs about 3 ms flat whatever
    /// the fork depth. Against `reconstruct` that is ~1.15x where no snapshots
    /// are configured and ~4x where they are — the absolute cost is the same
    /// either way, it is `reconstruct` that gets faster.
    ///
    /// Raises `UnknownBranchError` naming the lineage when it is not
    /// registered, rather than quietly answering for the trunk.
    fn reconstruct_on(
        &self,
        py: Python<'_>,
        ts: &Bound<'_, PyAny>,
        branch: &str,
    ) -> PyResult<temporal::PyMaterializedState> {
        let ts = to_canonical(Some(ts))?;
        let branch = branch.to_string();
        let inner = self.with_db(py, move |db| {
            runtime()
                .block_on(db.reconstruct_on(&ts, &branch))
                .map_err(to_py)
        })?;
        Ok(temporal::PyMaterializedState { inner })
    }

    /// `branch`'s ancestry, nearest first: `[(branch_id, dist, cutoff)]`.
    ///
    /// `dist` is steps from the reader, so the first entry is `branch` itself
    /// at `0`. `cutoff` is the instant past which that ancestor's writes are
    /// not visible here — `None` for the reader, and for an ancestor it is the
    /// **earliest** fork point on the path down to it, not the nearest.
    ///
    /// This is what `reconstruct_on` resolves against, published so the rule is
    /// inspectable rather than only obeyed: a caller who wants to know *why* an
    /// edge is or is not in a lineage's view can read the ancestry that decided
    /// it.
    ///
    /// Raises `UnknownBranchError` for a lineage that is not registered.
    fn ancestry<'py>(&self, py: Python<'py>, branch: &str) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        let name = branch.to_string();
        let anc = self.with_db(py, move |db| {
            runtime().block_on(db.ancestry(&name)).map_err(to_py)
        })?;
        anc.iter()
            .map(|a| {
                PyTuple::new(
                    py,
                    [
                        a.branch_id.clone().into_pyobject(py)?.into_any(),
                        a.dist.into_pyobject(py)?.into_any(),
                        match &a.cutoff {
                            Some(c) => from_canonical(py, c)?,
                            None => py.None().into_bound(py),
                        },
                    ],
                )
            })
            .collect()
    }

    /// Every edge one `ReadPlan` names, as the ledger held them (0.15.9,
    /// W13.4, D-251).
    ///
    /// Six-tuples — `(source, target, edge_type, valid_from, valid_to, branch)`
    /// — where `query_as_of_edges` returns five. The sixth is the lineage that
    /// holds the belief, which on a forked ledger is the difference between
    /// knowing that an edge is visible and knowing whose it is.
    ///
    /// # What this can ask that `query_as_of_edges` cannot
    ///
    /// A transaction-time instant. `query_as_of_edges` takes a valid instant
    /// and a lineage and has no third argument; before this release, *"which
    /// edges did we believe existed, as of March, as they stood in January"*
    /// meant walking from a start node the question does not have, or folding
    /// the whole log with `reconstruct` and filtering the result. `plan` is
    /// where the third qualifier finally fits.
    ///
    /// Topology only, no start node and no budget: on a large ledger this is a
    /// large list, and `load_subgraph` is the bounded neighbourhood read.
    ///
    /// # Raises
    ///
    /// `UnknownBranchError` naming an unregistered lineage — refused rather
    /// than answered for the trunk. `RecordedInstantUnreachableError` when
    /// `plan.recorded` is below what the hot log still covers; `reconstruct`
    /// is what answers there.
    fn edges<'py>(
        &self,
        py: Python<'py>,
        plan: &plan::PyReadPlan,
    ) -> PyResult<Vec<Bound<'py, pyo3::types::PyTuple>>> {
        let inner = plan.inner.clone();
        let beliefs = self.with_db(py, move |db| {
            runtime().block_on(db.edges(inner)).map_err(to_py)
        })?;
        beliefs
            .iter()
            .map(|b| temporal::belief_to_py(py, b))
            .collect()
    }

    /// Edges under current belief as of `ts`, as tuples.
    ///
    /// Topology only, and unlike `traverse_ids` it is not anchored at a start
    /// node: this is the whole of `links_current` filtered to the instant. On a
    /// large ledger that is a large answer, and there is no budget on it —
    /// `load_subgraph` is the bounded neighbourhood read.
    ///
    /// # `branch=`, four releases after the other read surfaces got it
    ///
    /// 0.14.4 bound `branch=` on the four traversal entry points, and this
    /// reader was the fifth surface that took a lineage in Rust and did not get
    /// one here. It went unnoticed because it is the read that does not go
    /// through `graph::builder` — the same reason the fork-point cutoff did not
    /// reach the Rust side of it until 0.14.10 ([D-227]). Closing the two
    /// together is deliberate: a repair Python cannot observe is a repair
    /// nobody here can test.
    ///
    /// The name is passed through rather than validated, exactly as the
    /// traversal entry points pass theirs, so an unregistered lineage raises
    /// `UnknownBranchError` naming it and the two surfaces refuse alike.
    ///
    /// [D-227]: ../../docs/architecture/s13-decision-register.md#d-227
    #[pyo3(signature = (ts = None, *, branch = None))]
    fn query_as_of_edges<'py>(
        &self,
        py: Python<'py>,
        ts: Option<&Bound<'py, PyAny>>,
        branch: Option<String>,
    ) -> PyResult<Vec<Bound<'py, pyo3::types::PyTuple>>> {
        let ts = self.instant(py, ts)?;
        let raw = self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::temporal::query_as_of_edges_on(
                    db.read_conn(),
                    &ts,
                    branch.as_deref(),
                ))
                .map_err(to_py)
        })?;
        raw.iter().map(|e| temporal::edge_to_py(py, e)).collect()
    }

    /// Move everything strictly before `cutoff` to cold storage (§5.6).
    ///
    /// **One session, one hold.** This is a high-priority command and its
    /// duration is a function of how much is being moved; every other writer
    /// waits. `archive_windowed` is the same work in bounded sessions.
    ///
    /// Refuses to archive past the point where a reconstruct would lose
    /// information — `ArchiveViolationError` — rather than deleting history it
    /// cannot rebuild.
    fn archive(
        &self,
        py: Python<'_>,
        cutoff: &Bound<'_, PyAny>,
    ) -> PyResult<temporal::PyArchiveReport> {
        let cutoff = to_canonical(Some(cutoff))?;
        let inner = self.with_db(py, move |db| {
            runtime().block_on(db.archive(&cutoff)).map_err(to_py)
        })?;
        Ok(temporal::PyArchiveReport { inner })
    }

    /// Forget a lineage, moving its whole ledger to cold storage (0.14.13,
    /// §15.4, D-230).
    ///
    /// The abandonment arm. `archive()` is indexed by time, so reclaiming an
    /// abandoned branch's recent history through it means archiving the trunk's
    /// recent history too. This is indexed by lineage instead.
    ///
    /// **Everything the lineage holds moves in one transaction** — its edges,
    /// its concepts, its log entries and its `branches` row — and afterwards the
    /// name is unknown: every read and write naming it raises
    /// `UnknownBranchError`. That refusal is the point rather than a side
    /// effect. An arm that took the rows and left the lineage registered would
    /// answer those reads with the parent's view, silently, and nothing would
    /// say that everything the branch believed had been deleted.
    ///
    /// Refused for the trunk, for a branch with descendants, and for a branch
    /// whose concepts a hot edge on another lineage still names —
    /// `BranchNotArchivableError` with a `reason`. An unregistered name raises
    /// `UnknownBranchError`.
    ///
    /// A write, so it queues through the actor and waits out any transaction in
    /// flight — a channel wait `busy_timeout` does not bound.
    fn archive_branch(&self, py: Python<'_>, branch: &str) -> PyResult<temporal::PyArchiveReport> {
        let branch = branch::branch_id(branch)?;
        let inner = self.with_db(py, move |db| {
            runtime().block_on(db.archive_branch(branch)).map_err(to_py)
        })?;
        Ok(temporal::PyArchiveReport { inner })
    }

    /// Bring named concepts back out of cold storage (0.9.0, C3, D-131).
    ///
    /// The counterpart of `archive`, and a **physical move back**: it mints no
    /// transaction-time facts, so `reconstruct` at any instant answers exactly as
    /// it did before the concept was archived and exactly as it does after. A
    /// concept reacquires its old identity rather than arriving as a new
    /// assertion, because the alternative would make the ledger say it was
    /// *learned* at rehydration time.
    ///
    /// Ids absent from the cold file are **skipped, not refused**. The list a
    /// caller has usually came from an earlier cold-side query and being
    /// partially stale is the normal case; `RehydrateReport.concepts_rehydrated`
    /// is how many actually moved.
    ///
    /// # Latency
    ///
    /// A write, so it queues through the actor like any other and waits out any
    /// transaction in flight (§5.1.8) — a channel wait in Rust, which
    /// `busy_timeout` does not bound. The operation itself costs about 3.7 ms
    /// plus 74 µs per concept, rising above a thousand concepts in one call
    /// because the search index dominates there (§9, D-132). It is one
    /// transaction with no window boundaries, deliberately: a rehydration cannot
    /// half-happen.
    ///
    /// `BranchArchivedError` when a named concept was minted on a lineage
    /// `archive_branch()` has since forgotten (0.15.11, W15.1). Nothing is
    /// written when it refuses, ids ahead of the refused one included; `fork()`
    /// re-registers the lineage and the call then succeeds.
    fn rehydrate(&self, py: Python<'_>, ids: Vec<String>) -> PyResult<temporal::PyRehydrateReport> {
        let inner = self.with_db(py, move |db| {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            runtime().block_on(db.rehydrate(&refs)).map_err(to_py)
        })?;
        Ok(temporal::PyRehydrateReport { inner })
    }

    /// Archive up to `cutoff` in windows, returning one report per session
    /// (D-080).
    ///
    /// Each window is its own transaction, so the actor returns to its loop
    /// between them and other writers interleave. T1.1 measured the longest hold
    /// falling 3,326 → 768 ms this way.
    ///
    /// `window_seconds` is **refused, not clamped**, when it does not advance or
    /// when it implies more than 4,096 sessions: rounding a narrow window up
    /// would archive over boundaries the caller did not choose, and the caller
    /// cannot see it happen. That arrives as `ArchiveWindowError`.
    ///
    /// A `timedelta` is accepted as well as a number of seconds.
    fn archive_windowed(
        &self,
        py: Python<'_>,
        cutoff: &Bound<'_, PyAny>,
        window: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<temporal::PyArchiveReport>> {
        let cutoff = to_canonical(Some(cutoff))?;
        let window = to_duration(window)?;
        let reports = self.with_db(py, move |db| {
            runtime()
                .block_on(db.archive_windowed(&cutoff, window))
                .map_err(to_py)
        })?;
        Ok(reports
            .into_iter()
            .map(|inner| temporal::PyArchiveReport { inner })
            .collect())
    }

    /// Check snapshot composition against an independent fold from genesis
    /// (D-092).
    ///
    /// Reports; does not repair. See `ChainCheck` for why, and for why its two
    /// anchor fields must not be compared with each other.
    ///
    /// This folds the **whole log**, which is the one thing normal operation
    /// never does — that is the point, and it is also the cost. It is a
    /// diagnostic, not a health check to run per request.
    fn verify_snapshot_chain(
        &self,
        py: Python<'_>,
        ts: &Bound<'_, PyAny>,
    ) -> PyResult<temporal::PyChainCheck> {
        let ts = to_canonical(Some(ts))?;
        let inner = self.with_db(py, move |db| {
            runtime()
                .block_on(db.verify_snapshot_chain(&ts))
                .map_err(to_py)
        })?;
        Ok(temporal::PyChainCheck { inner })
    }

    // -- lineage surface (W12.7) ---------------------------------------------

    /// Cut a new lineage from an existing one, and return it.
    ///
    /// A fork is **O(1) in rows written**: one row in `branches`, and nothing
    /// else. A branch inherits its parent's history by resolution at read
    /// rather than by owning a copy of it, so forking a thousand times leaves
    /// every ledger table byte-identical.
    ///
    /// The fork point is *now*. The new lineage sees its parent's history up to
    /// this instant and nothing the parent records after it, which is what
    /// `branch=` on the traversal entry points reads.
    ///
    /// ```python
    /// alt = db.fork("turn/17/alt/1", "main")
    /// seen = db.walk("socrates", branch=alt.id)
    /// ```
    ///
    /// # What this lineage can and cannot do yet
    ///
    /// It can be **read**: every traversal entry point takes `branch=`. It
    /// cannot yet be **written** — no write takes a lineage, so `assert_edge`
    /// after a `fork` lands on the trunk and says nothing about it. A fork is
    /// currently a *view* of its parent's history as of an instant. Said here
    /// because the gap is invisible from the signatures.
    ///
    /// # Raises
    ///
    /// - `UnknownBranchError` when `frm` is not registered.
    /// - `BranchExistsError` when `name` is taken, including `"main"`.
    /// - `InvalidBranchIdError` when `name` is not an acceptable name — which
    ///   is a wider rule than model names: what it refuses is empty, over 128
    ///   characters, control characters, and leading or trailing whitespace.
    /// - `ForkPrecedesParentError` when the clock would place the fork point
    ///   before the parent's own.
    #[pyo3(signature = (name, frm = "main"))]
    fn fork(&self, py: Python<'_>, name: &str, frm: &str) -> PyResult<branch::PyBranch> {
        let name = branch::branch_id(name)?;
        let parent = branch::branch_id(frm)?;
        let inner = self.with_db(py, move |db| {
            runtime().block_on(db.fork(name, parent)).map_err(to_py)
        })?;
        Ok(branch::PyBranch { inner })
    }

    /// Every lineage the ledger knows about, trunk first then creation order.
    ///
    /// A database that has never forked returns exactly one `Branch`: the
    /// trunk, whose `parent` and `forked_at` are both `None`.
    ///
    /// Read rather than queued behind the write actor, so listing branches does
    /// not wait on a bulk import. `branches` is append-only, so the only way
    /// this can be stale is by missing a branch created after it was taken.
    fn branches(&self, py: Python<'_>) -> PyResult<Vec<branch::PyBranch>> {
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.branches())
                .map(|all| {
                    all.into_iter()
                        .map(|inner| branch::PyBranch { inner })
                        .collect()
                })
                .map_err(to_py)
        })
    }

    /// What lineage `a` believes that lineage `b` does not (§15.4, W12.11).
    ///
    /// A belief-level difference, not a provenance filter: a row comes back
    /// when `a` holds an edge at a key `b` does not hold at all, or holds the
    /// same key over a different interval or at a different weight. Its
    /// `branch_id` may therefore name a lineage that is *neither* argument —
    /// two siblings disagree through rows their common ancestor wrote.
    ///
    /// **There is no `ts`.** A retirement is a divergence about an instant
    /// having passed, and any instant filter drops it from `a`'s side, so a
    /// diff taken as-of an instant would silently answer a different question.
    /// Filter the result if that is what you want.
    ///
    /// Ordered by edge key. Both names are validated, so an unregistered
    /// lineage raises `UnknownBranchError` naming it. Properties are not
    /// compared, because no read in this library returns them.
    fn diff(&self, py: Python<'_>, a: &str, b: &str) -> PyResult<Vec<branch::PyDivergence>> {
        let a = branch::branch_id(a)?;
        let b = branch::branch_id(b)?;
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.diff(&a, &b))
                .map(|rows| {
                    rows.into_iter()
                        .map(|inner| branch::PyDivergence { inner })
                        .collect()
                })
                .map_err(to_py)
        })
    }

    // -- vector surface (P4.4) ------------------------------------------------

    /// Create a model's embedding table and DiskANN index (§5.9, D-048).
    ///
    /// Idempotent at the same dimension; registering the same name at a
    /// *different* one raises `DimMismatchError` rather than migrating, because
    /// the stored vectors cannot be reinterpreted at another width.
    fn register_model(&self, py: Python<'_>, model: &str, dim: usize) -> PyResult<()> {
        let model = vector::model_name(model)?;
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.register_model(&model, dim))
                .map_err(to_py)
        })
    }

    /// Every model registered in this database, in name order (W6.2).
    ///
    /// Read from `sqlite_master` rather than from a registry this crate keeps,
    /// so it cannot drift from what actually exists (D-037). libSQL's own
    /// `*_shadow` tables are filtered out, and a table matching the naming
    /// pattern but not the naming *rule* is skipped rather than returned under
    /// a name `register_model` would refuse to take back.
    ///
    /// The write side has been here since P4.4 and the read side had not, so a
    /// Python caller could register a model and not enumerate what was
    /// registered — which made "is this model already set up?" a question
    /// answerable only by registering it again and reading the exception.
    fn registered_models(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::registered_models(db.read_conn()))
                .map(|models| models.into_iter().map(|m| m.to_string()).collect())
                .map_err(to_py)
        })
    }

    /// The dimension `model`'s table declares (W6.2).
    ///
    /// `F32_BLOB(768)` in the column type *is* the declaration — this reads it
    /// back with `PRAGMA table_info` rather than consulting a cache, so the
    /// number a caller sizes a vector against and the number storage enforces
    /// are the same number (D-037).
    ///
    /// Raises `ModelNotRegisteredError` for a model with no table. That is the
    /// distinction against `registered_models()`: membership is a list, and
    /// this is the fact you need before allocating.
    fn declared_dimension(&self, py: Python<'_>, model: &str) -> PyResult<usize> {
        let model = vector::model_name(model)?;
        self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::declared_dimension(db.read_conn(), &model))
                .map_err(to_py)
        })
    }

    /// Store embeddings, chunked (D-011).
    ///
    /// `rows` is a sequence of `(concept_id, embedding)`. Each embedding may be
    /// a sequence of floats or **packed little-endian float32 `bytes`** —
    /// `arr.astype("<f4").tobytes()` — which is the fast path, measured at
    /// 60.8 µs against 94.9 µs for the same numpy array as a sequence.
    ///
    /// A vector whose length is not the model's declared dimension raises
    /// `DimMismatchError`, naming both.
    ///
    /// **On failure, the exception carries `written`** (0.13.8, W7.6): the
    /// number of rows the chunks before the stop committed, which are still
    /// committed. The exception *class* is still chosen by what went wrong.
    ///
    /// `progress` is called after every chunk with one dict — `written`,
    /// `total`, `rows`, `held_ms` — on the ledger's thread with the GIL
    /// re-acquired, so it is on the critical path. An exception raised there
    /// stops the write and is what propagates. `cancel` takes a `CancelToken`,
    /// which some *other* thread must trip: this call runs with the GIL
    /// released.
    #[pyo3(signature = (model, rows, *, progress = None, cancel = None))]
    fn upsert_embeddings(
        &self,
        py: Python<'_>,
        model: &str,
        rows: &Bound<'_, PyAny>,
        progress: Option<Py<PyAny>>,
        cancel: Option<PyRef<'_, PyCancelToken>>,
    ) -> PyResult<usize> {
        let model = vector::model_name(model)?;
        let mut decoded: Vec<(String, Vec<f32>)> = Vec::new();
        for item in rows.try_iter()? {
            let item = item?;
            let (id, embedding): (String, Bound<'_, PyAny>) = item.extract().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "upsert_embeddings takes a sequence of (concept_id, embedding) pairs",
                )
            })?;
            decoded.push((id, crate::types::coerce_embedding(&embedding)?));
        }
        let raised = Arc::new(Mutex::new(None));
        let (control, _token) = bulk_control(progress, cancel, &raised);
        self.with_db(py, move |db| {
            bulk_result(
                runtime().block_on(db.upsert_embeddings_with(&model, decoded, control)),
                &raised,
            )
        })
    }

    /// Nearest `top_k` concepts to `query` by cosine distance (§5.9).
    ///
    /// Goes through the DiskANN index rather than scanning: an
    /// `ORDER BY vector_distance_cos(…)` over the table is linear in the corpus
    /// however small `top_k` is. Results ascend — **smaller score is closer**.
    #[pyo3(signature = (model, query, *, top_k = 10, as_of_valid = None, half_life = None))]
    fn search_vector(
        &self,
        py: Python<'_>,
        model: &str,
        query: &Bound<'_, PyAny>,
        top_k: usize,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        half_life: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<vector::PyVectorHit>> {
        let model = vector::model_name(model)?;
        let query = crate::types::coerce_embedding(query)?;
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        let half_life = crate::timestamps::to_duration(half_life)?;
        let hits = self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::search_vector(
                    db.read_conn(),
                    &query,
                    &model,
                    top_k,
                    as_of_valid.as_deref(),
                    half_life,
                ))
                .map_err(to_py)
        })?;
        Ok(hits.into_iter().map(Into::into).collect())
    }

    /// Full-text search over concept text, as `(concept_id, rank)` (§5.9).
    ///
    /// **`rank` is bm25, which FTS5 returns negative**, with magnitude growing
    /// with relevance — so the list is ascending and best-first, and sorting it
    /// descending puts the worst match on top. Retired concepts are excluded.
    ///
    /// `query` goes to FTS5 as a MATCH expression. Punctuation in an untrusted
    /// string is an FTS5 syntax error rather than a literal, so it is escaped
    /// here unless `raw` is set — which is the same choice `hybrid_search`
    /// makes, for the same reason.
    #[pyo3(signature = (query, *, top_k = 10, raw = false, as_of_valid = None, half_life = None))]
    #[allow(clippy::too_many_arguments)]
    fn keyword_search(
        &self,
        py: Python<'_>,
        query: &str,
        top_k: usize,
        raw: bool,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        half_life: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<(String, f64)>> {
        let expr = if raw {
            query.to_string()
        } else {
            macrame::vector::escape_fts5_query(query)
        };
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        let half_life = crate::timestamps::to_duration(half_life)?;
        self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::keyword_search(
                    db.read_conn(),
                    &expr,
                    top_k,
                    as_of_valid.as_deref(),
                    half_life,
                ))
                .map_err(to_py)
        })
    }

    /// Vector and keyword arms, fused by reciprocal rank (§5.9).
    ///
    /// Results **descend** — larger fused score is better — which is the
    /// opposite of `search_vector`. Each hit carries `vector_rank` and
    /// `keyword_rank`, either of which may be `None`; a concept both arms found
    /// is a different kind of hit from one only the keyword arm found, and the
    /// fused score alone cannot say which.
    ///
    /// `depth` is how deep each arm goes before fusing, defaulting to
    /// `max(5 * top_k, 50)`. An unregistered model raises rather than degrading
    /// to keyword-only: a caller who named a model that does not exist asked a
    /// question this cannot answer.
    #[pyo3(signature = (model, query_text, query_vector, *, top_k = 10, depth = None, rrf_k = None, raw = false, as_of_valid = None, half_life = None))]
    #[allow(clippy::too_many_arguments)]
    fn hybrid_search(
        &self,
        py: Python<'_>,
        model: &str,
        query_text: &str,
        query_vector: &Bound<'_, PyAny>,
        top_k: usize,
        depth: Option<usize>,
        rrf_k: Option<usize>,
        raw: bool,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        half_life: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<vector::PyVectorHit>> {
        let model = vector::model_name(model)?;
        let query_vector = crate::types::coerce_embedding(query_vector)?;
        let mut search = macrame::vector::HybridSearch::new(model, query_text, query_vector)
            .top_k(top_k)
            .raw_match(raw);
        if let Some(d) = depth {
            search = search.depth(d);
        }
        if let Some(k) = rrf_k {
            search = search.rrf_k(k);
        }
        if let Some(t) = as_of_valid {
            search = search.as_of_valid(to_canonical(Some(t))?);
        }
        if let Some(h) = crate::timestamps::to_duration(half_life)? {
            search = search.half_life(h);
        }
        let hits = self.with_db(py, move |db| {
            runtime()
                .block_on(search.execute(db.read_conn()))
                .map_err(to_py)
        })?;
        Ok(hits.into_iter().map(Into::into).collect())
    }

    /// Vector search restricted to a traversal's neighbourhood (§5.3, D-007).
    ///
    /// Returns `(hits, plan)`. The plan comes back rather than only being
    /// logged, because D-007's requirement is empirical tuning and that needs
    /// the estimate next to the outcome.
    ///
    /// The planner prices both strategies in bytes and takes the cheaper.
    /// `strategy` forces one, bypassing it — for tests and diagnosis, not for
    /// production code, which should not be second-guessing a measurement it
    /// can read.
    ///
    /// `as_of_valid` bounds the traversal **and** the ranking, because it is one
    /// instant rather than two (0.13.19, W9.4, D-192). A past neighbourhood
    /// scored against the present corpus is the defect F-32 describes, so it is
    /// not offered as a setting.
    #[pyo3(signature = (
        model, query, start_node, *, max_depth = 2, edge_types = None,
        min_weight = 0.0, top_k = 10, byte_budget = None, probe_cap = None,
        strategy = None, now = None, as_of_valid = None, branch = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn search_filtered(
        &self,
        py: Python<'_>,
        model: &str,
        query: &Bound<'_, PyAny>,
        start_node: &str,
        max_depth: usize,
        edge_types: Option<Vec<String>>,
        min_weight: f64,
        top_k: usize,
        byte_budget: Option<usize>,
        probe_cap: Option<usize>,
        strategy: Option<vector::PyFilterStrategy>,
        now: Option<&Bound<'_, PyAny>>,
        as_of_valid: Option<&Bound<'_, PyAny>>,
        branch: Option<String>,
    ) -> PyResult<(Vec<vector::PyVectorHit>, vector::PyCostEstimate)> {
        let model = vector::model_name(model)?;
        let query = crate::types::coerce_embedding(query)?;
        let now = self.instant(py, now)?;
        let as_of_valid = as_of_valid.map(|t| to_canonical(Some(t))).transpose()?;
        // One instant, reaching the walk and the ranking together: the whole
        // point of W9.4 is that those two cannot disagree (D-192).
        let traversal = graph::builder(
            start_node,
            max_depth,
            edge_types,
            min_weight,
            None,
            as_of_valid,
            None,
            branch,
            // `probe_cap` is this search's ceiling and reaches the same walk
            // through `FilteredVectorSearch`. A `limit=` keyword beside it
            // would be two spellings of one knob in one signature.
            None,
        );

        let mut search =
            macrame::graph::FilteredVectorSearch::new(model, query, traversal).top_k(top_k);
        if let Some(b) = byte_budget {
            search = search.byte_budget(b);
        }
        if let Some(c) = probe_cap {
            search = search.probe_cap(c);
        }
        if let Some(s) = strategy {
            search = search.strategy(s.into());
        }

        let (hits, estimate) = self.with_db(py, move |db| {
            runtime()
                .block_on(search.execute_explained(db.read_conn(), &now))
                .map_err(to_py)
        })?;
        Ok((
            hits.into_iter().map(Into::into).collect(),
            vector::PyCostEstimate::new(estimate),
        ))
    }

    // -- maintenance (W6.4) ---------------------------------------------------

    /// Refresh the query planner's statistics (D-149).
    ///
    /// Runs `ANALYZE`, which writes `sqlite_stat1`. Before 0.12.4 nothing in
    /// the crate ever did, so the planner costed every query against SQLite's
    /// built-in guesses — an estimate that depends on how many columns a query
    /// binds rather than on what the table holds.
    ///
    /// Low priority and bounded: `PRAGMA analysis_limit` caps the rows examined
    /// per index, so the hold scales with the index count and not with the size
    /// of `links_current`.
    ///
    /// Call it after a bulk import, or after anything that changes a table's
    /// shape by an order of magnitude. Prefer `optimize()` for routine upkeep —
    /// this does the work unconditionally.
    fn analyze(&self, py: Python<'_>) -> PyResult<()> {
        self.with_db(py, |db| runtime().block_on(db.analyze()).map_err(to_py))
    }

    /// Re-analyse only what has gone stale (D-149).
    ///
    /// `PRAGMA optimize`: a no-op on an idle database and the full cost of
    /// `analyze()` on one that has changed completely. That property is what
    /// makes it safe to call on a schedule. `close()` already runs it, so a
    /// process that opens, works and closes keeps its statistics current
    /// without anybody arranging it.
    fn optimize(&self, py: Python<'_>) -> PyResult<()> {
        self.with_db(py, |db| runtime().block_on(db.optimize()).map_err(to_py))
    }

    /// Move WAL frames back into the main database file (D-156).
    ///
    /// Runs a `FULL` pass for the frame counts and then a `TRUNCATE` to reset
    /// the WAL, and reports what happened — a checkpoint that did nothing and
    /// one that reclaimed a 400 MB WAL are otherwise indistinguishable, and the
    /// difference is the reason a caller asked.
    ///
    /// **Read `busy` before treating the file as self-contained.** A busy
    /// checkpoint gave up waiting for a reader and may have moved only some
    /// frames, which matters exactly when this is being called before copying
    /// the database somewhere.
    ///
    /// High priority: a caller asking for a checkpoint is asking for it *now*,
    /// usually at the end of a bulk load, and queueing it behind the background
    /// work it was meant to follow inverts the intent.
    fn checkpoint(&self, py: Python<'_>) -> PyResult<observe::PyCheckpointReport> {
        let inner = self.with_db(py, |db| runtime().block_on(db.checkpoint()).map_err(to_py))?;
        Ok(observe::PyCheckpointReport { inner })
    }

    // -- integrity (P4.5) -----------------------------------------------------

    /// How many rows `links_current` disagrees with `links` about (§5.8).
    ///
    /// `0` in steady state. `links_current` is derivative under Doctrine VI, so
    /// drift is a repairable inconsistency in a cache rather than damage to the
    /// ledger — the assertions are still what they were.
    ///
    /// A read, not a command: this does not touch the write actor.
    fn audit_current(&self, py: Python<'_>) -> PyResult<usize> {
        self.with_db(py, |db| {
            runtime()
                .block_on(macrame::integrity::audit_current(db.read_conn()))
                .map_err(to_py)
        })
    }

    /// Reproject `links_current` from `links` in **one transaction**.
    ///
    /// The whole repair is a single hold, so every other writer waits for it —
    /// §5.8's table budgets ~5 s at 1M edges. That is the cost of atomicity
    /// here, and `rebuild_current_chunked` is the same repair without it.
    fn rebuild_current(&self, py: Python<'_>) -> PyResult<observe::PyRebuildReport> {
        let inner = self.with_db(py, |db| {
            runtime().block_on(db.rebuild_current()).map_err(to_py)
        })?;
        Ok(observe::PyRebuildReport { inner })
    }

    /// Reproject `links_current` via a shadow table, in chunks (D-082).
    ///
    /// Builds a copy across many short transactions and swaps it in under one,
    /// so `links_current` is never partially populated and no traversal can
    /// observe a half-built graph. Longer in wall-clock, made of
    /// `chunk_budget_ms()`-sized holds instead of one long one.
    ///
    /// An `archive` landing mid-rebuild raises `RebuildInterruptedError`, which
    /// means the repair **did not run** — `links_current` is untouched, whatever
    /// was true of it before is still true, and the action is to retry. That is
    /// a different thing from `RebuildFailedError`, which means the repair ran
    /// and did not repair.
    fn rebuild_current_chunked(&self, py: Python<'_>) -> PyResult<observe::PyRebuildReport> {
        let inner = self.with_db(py, |db| {
            runtime()
                .block_on(db.rebuild_current_chunked())
                .map_err(to_py)
        })?;
        Ok(observe::PyRebuildReport { inner })
    }

    /// Rebuild the `concepts_fts` index from `concepts` (D-051).
    fn rebuild_fts(&self, py: Python<'_>) -> PyResult<()> {
        self.with_db(py, |db| runtime().block_on(db.rebuild_fts()).map_err(to_py))
    }

    // -- introspection (P4.6) -------------------------------------------------

    /// What the write actor has held the write connection for (§5.10, D-079).
    ///
    /// The wheel is built with the `metrics` feature on (D-093), so this always
    /// answers with real counters — unlike a default Rust build, where the type
    /// is zero-sized and the numbers are zero.
    ///
    /// Start with `.violations()`: it is the question `chunk_budget_ms()` exists
    /// to make askable, and an empty list is the good answer.
    fn metrics(&self, py: Python<'_>) -> PyResult<observe::PyMetricsSnapshot> {
        let inner = self.with_db(py, |db| Ok(db.metrics()))?;
        Ok(observe::PyMetricsSnapshot { inner })
    }

    // -- diagnostic reads (pulled forward from P4.6) --------------------------

    /// Run a read-only query on a connection belonging to this call.
    ///
    /// Opens the file `SQLITE_OPEN_READ_ONLY` — an OS-level boundary, not a
    /// reversible `PRAGMA` — runs `sql`, and drops the connection. Returns a list
    /// of tuples, with values as they are stored: a timestamp column comes back
    /// as the canonical string, not a `datetime`, because this is the diagnostic
    /// path and its job is to show what is actually there.
    ///
    /// For ordinary reads use the typed surface — `traverse`, `load_subgraph`,
    /// `reconstruct`, the search methods — which coerces and validates. This one
    /// is for looking at the file when a typed answer is the thing in doubt.
    ///
    /// **Calls on this path serialise across threads.** Each one opens a
    /// connection, and concurrent opens are R15's shape, so `diagnostic_query`
    /// and `explain` take a mutex the rest of the surface does not. Reads on
    /// the typed surface stay concurrent. See `PyDatabase::diagnostic_rows`.
    #[pyo3(signature = (sql, params = None))]
    /// # This is not a safe place for a string from somewhere else
    ///
    /// Two reasons, and neither is about writes — the connection is opened
    /// `SQLITE_OPEN_READ_ONLY` and cannot modify the ledger. `ATTACH` names any
    /// file the process can open, so this is a read window onto the filesystem
    /// rather than onto this database. And `PRAGMA hard_heap_limit = 1` leaves
    /// the **whole process** unable to use SQLite at all — every later write,
    /// read, `checkpoint()` and `close()`, and opening any other database,
    /// fails with `out of memory` until the process restarts (0.15.16,
    /// [D-258](../../../docs/architecture/s13-decision-register.md#d-258),
    /// `tests_py/probes/diagnostic_global_pragmas.py`).
    ///
    /// Both are properties of arbitrary SQL rather than of this binding, and
    /// neither is affected by the connection hygiene D-257 added. Fine for a
    /// developer at a REPL; not fine for a debug console that runs what a user
    /// typed.
    ///
    /// # One statement per call
    ///
    /// Only the **first** statement of `sql` runs; the rest are discarded
    /// without an error. `"SELECT 1; SELECT 2"` returns the rows of `SELECT 1`
    /// and nothing else — libSQL prepares one statement and this method steps
    /// that one. Worth knowing before pasting a script in, and worth knowing
    /// because of what the discarded half can leave behind: `"BEGIN; SELECT 1"`
    /// executes the `BEGIN` and nothing else, which is one of the ways the
    /// connection ends up carrying a transaction nobody meant to open (0.15.15,
    /// W15.5,
    /// [D-257](../../../docs/architecture/s13-decision-register.md#d-257)).
    /// That one is scrubbed rather than merely documented — see
    /// [`PyDatabase::diagnostic_rows`].
    fn diagnostic_query<'py>(
        &self,
        py: Python<'py>,
        sql: &str,
        params: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Vec<Bound<'py, pyo3::types::PyTuple>>> {
        let bound: Vec<libsql::Value> = match params {
            None => Vec::new(),
            Some(seq) => seq
                .try_iter()?
                .map(|item| rows::py_to_value(&item?))
                .collect::<PyResult<_>>()?,
        };
        let raw = self.diagnostic_rows(py, sql.to_string(), bound)?;
        rows::rows_to_py(py, raw)
    }

    /// `EXPLAIN QUERY PLAN` for `sql`, as the detail column only.
    ///
    /// The use T5.1 named first. Separate from `diagnostic_query` because a
    /// plan's shape is not a query's shape, and callers want the detail rather
    /// than three columns of bookkeeping.
    fn explain(&self, py: Python<'_>, sql: &str) -> PyResult<Vec<String>> {
        let raw = self.diagnostic_rows(py, format!("EXPLAIN QUERY PLAN {sql}"), Vec::new())?;
        Ok(raw
            .into_iter()
            .filter_map(|cells| match cells.last() {
                Some(libsql::Value::Text(detail)) => Some(detail.clone()),
                _ => None,
            })
            .collect())
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.Database path={:?}{}>",
            self.path,
            if self.is_closed() { " closed" } else { "" }
        )
    }
}

/// Notes a handle that was garbage-collected instead of closed.
///
/// The Rust side already warns through `tracing`, and that is the problem: a
/// Python process almost never has a `tracing` subscriber, so the warning goes
/// nowhere. `ResourceWarning` is the established Python signal for exactly this
/// — it is what an unclosed file object raises — and it is visible to `-W
/// default`, to `pytest`, and to anything that captures warnings.
///
/// The inner handle is dropped **inside a runtime context**. `Database` holds
/// tokio `JoinHandle`s and a `watch::Sender`; dropping those with no runtime
/// entered is the kind of thing that panics in a destructor, which in Python
/// means an unraisable exception during collection.
impl Drop for PyDatabase {
    fn drop(&mut self) {
        let taken = match self.inner.get_mut() {
            Ok(slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(db) = taken else { return };

        {
            let _guard = runtime().enter();
            drop(db);
        }

        // `tp_dealloc` runs with the GIL held, so this re-acquire is cheap. The
        // result is discarded on purpose: with `-W error` a warning *raises*,
        // and an exception escaping a destructor is worse than a lost warning.
        Python::attach(|py| {
            let _ = PyErr::warn(
                py,
                py.get_type::<pyo3::exceptions::PyResourceWarning>()
                    .as_any(),
                std::ffi::CString::new(format!(
                    "macrame.Database for {:?} was garbage-collected without close(): \
                     the final snapshot was not written, so the next reconstruct folds \
                     from an older anchor, and the write actor's exit status was never \
                     checked. Use `with Database.open(...) as db:`.",
                    self.path
                ))
                .unwrap_or_else(|_| c"macrame.Database was collected without close()".into())
                .as_c_str(),
                1,
            );
        });
    }
}

/// `timedelta` or a number of seconds → `Duration`.
///
/// Both are accepted because both are natural: a caller who has a window in
/// hand has a `timedelta`, and a caller typing one at a REPL types `86400`.
/// Negative and non-finite values are refused here rather than saturating to
/// zero, which is what `Duration::from_secs_f64` would do — and a zero window
/// reaches `ArchiveWindowError` with a message about session counts, which is
/// a true statement about the wrong problem.
///
/// **A negative `timedelta` used to arrive as a `TypeError`** (0.12.20, W6.3).
/// `Duration` cannot represent one, so the extraction fails and the fallback
/// then fails to read a `timedelta` as a float — reporting "expected a
/// datetime.timedelta" to a caller holding one. The instance check below is
/// what makes the sign the complaint, matching the float arm beside it.
pub(crate) fn to_duration(obj: &Bound<'_, PyAny>) -> PyResult<std::time::Duration> {
    if obj.is_instance_of::<pyo3::types::PyDelta>() {
        // Zero is refused here as well as below. `Duration` *can* hold it, so
        // it would otherwise pass on this arm and be refused on the float one,
        // which is one rule with two answers depending on how it was typed.
        return match obj.extract::<std::time::Duration>() {
            Ok(d) if !d.is_zero() => Ok(d),
            _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "expected a positive duration, got {}",
                obj.repr()?
            ))),
        };
    }
    let secs: f64 = obj.extract().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(
            "expected a datetime.timedelta or a number of seconds",
        )
    })?;
    if !(secs.is_finite() && secs > 0.0) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "expected a positive, finite duration, got {secs} seconds"
        )));
    }
    Ok(std::time::Duration::from_secs_f64(secs))
}

/// `PathBuf` → `pathlib.Path`.
///
/// pyo3 renders a `PathBuf` as `str`, which is correct and unhelpful: every
/// caller who wants to do anything with the value converts it back. Doing it
/// here means the type is right at the boundary.
fn to_pathlib<'py>(py: Python<'py>, path: &std::path::Path) -> PyResult<Bound<'py, PyAny>> {
    let pathlib = py.import("pathlib")?;
    let cls: Bound<'py, PyType> = pathlib.getattr("Path")?.cast_into()?;
    cls.call1((path,))
}

/// `snapshot_every_entries` / `snapshot_poll_seconds` → a [`SnapshotCadence`].
///
/// Shared by `open` and `_open_with_clock` rather than duplicated: the two
/// refusals below are the whole of the validation, and a second copy of them is
/// a second chance to drift.
///
/// Both are refusals rather than clamps. A zero or negative threshold would
/// anchor on every poll, which is not what any caller means by it, and a silent
/// repair here becomes a mystery about snapshot volume later.
fn to_cadence(
    snapshot_every_entries: Option<i64>,
    snapshot_poll_seconds: f64,
) -> PyResult<Option<SnapshotCadence>> {
    let Some(n) = snapshot_every_entries else {
        return Ok(None);
    };
    if n <= 0 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "snapshot_every_entries must be positive, got {n}. \
             Pass None to run without a snapshot cadence."
        )));
    }
    if !(snapshot_poll_seconds.is_finite() && snapshot_poll_seconds > 0.0) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "snapshot_poll_seconds must be a positive, finite number, \
             got {snapshot_poll_seconds}"
        )));
    }
    Ok(Some(
        SnapshotCadence::default()
            .every_entries(n)
            .poll_interval(std::time::Duration::from_secs_f64(snapshot_poll_seconds)),
    ))
}

/// `None` / `"disabled"` / a positive page count → a [`WalCheckpointPolicy`].
///
/// Three states, spelled so that the *absent* one means "leave SQLite alone"
/// rather than "turn it off" — see [`PyDatabase::open`] for why that ordering
/// is the whole point of the type (D-155, D-157).
///
/// `0` is refused. SQLite reads it as *disable*, and inheriting that overload
/// would turn a caller's arithmetic mistake into a WAL that grows for the life
/// of the process, reported nowhere.
fn to_wal_policy(obj: Option<&Bound<'_, PyAny>>) -> PyResult<macrame::WalCheckpointPolicy> {
    let Some(obj) = obj else {
        return Ok(macrame::WalCheckpointPolicy::Default);
    };
    if obj.is_none() {
        return Ok(macrame::WalCheckpointPolicy::Default);
    }
    if let Ok(s) = obj.extract::<String>() {
        return match s.as_str() {
            "disabled" => Ok(macrame::WalCheckpointPolicy::Disabled),
            other => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "wal_autocheckpoint accepts None, \"disabled\", or a positive \
                 number of pages; got {other:?}"
            ))),
        };
    }
    let pages: i64 = obj.extract().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(
            "wal_autocheckpoint accepts None, \"disabled\", or a positive number of pages",
        )
    })?;
    if pages <= 0 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "wal_autocheckpoint must be positive, got {pages}. Pass \"disabled\" \
             to turn automatic checkpointing off — and then call checkpoint() \
             yourself, or the WAL grows without bound."
        )));
    }
    let pages = u32::try_from(pages).map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "wal_autocheckpoint must fit in 32 bits, got {pages}"
        ))
    })?;
    Ok(macrame::WalCheckpointPolicy::EveryPages(pages))
}

/// `None` / `"allow"` / a non-negative number of seconds → a
/// [`macrame::FutureStampPolicy`] (0.13.5, W7.4, D-178).
///
/// Deliberately the same three-state shape as [`to_wal_policy`], including that
/// the *absent* state is the one that leaves the guard on. A `None` meaning
/// "no bound" would switch off a check against a value that spreads, for every
/// caller who never heard of the keyword — D-155's failure mode against a
/// costlier invariant.
///
/// A negative tolerance is refused rather than saturated at zero: it can only
/// come from arithmetic, and arithmetic that produced a negative duration has a
/// sign error the caller should see.
fn to_future_stamp_policy(obj: Option<&Bound<'_, PyAny>>) -> PyResult<macrame::FutureStampPolicy> {
    let Some(obj) = obj else {
        return Ok(macrame::FutureStampPolicy::Default);
    };
    if obj.is_none() {
        return Ok(macrame::FutureStampPolicy::Default);
    }
    if let Ok(s) = obj.extract::<String>() {
        return match s.as_str() {
            "allow" => Ok(macrame::FutureStampPolicy::Allow),
            other => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "future_stamps accepts None, \"allow\", or a tolerance in \
                 seconds; got {other:?}"
            ))),
        };
    }
    let seconds: f64 = obj.extract().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(
            "future_stamps accepts None, \"allow\", or a tolerance in seconds",
        )
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "future_stamps tolerance must be a finite, non-negative number of \
             seconds, got {seconds}. Pass \"allow\" to waive the bound entirely."
        )));
    }
    Ok(macrame::FutureStampPolicy::Tolerance(
        std::time::Duration::from_secs_f64(seconds),
    ))
}
