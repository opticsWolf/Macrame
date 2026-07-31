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
use std::sync::RwLock;

use pyo3::prelude::*;
use pyo3::types::PyType;

use macrame::prelude::*;

use crate::errors::{closed_error, to_py};
use crate::runtime::{check_not_forked, runtime};

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
    #[staticmethod]
    #[pyo3(signature = (path, *, snapshot_every_entries = Some(10_000), snapshot_poll_seconds = 5.0))]
    fn open(
        py: Python<'_>,
        path: PathBuf,
        snapshot_every_entries: Option<i64>,
        snapshot_poll_seconds: f64,
    ) -> PyResult<Self> {
        let cadence = match snapshot_every_entries {
            None => None,
            Some(n) => {
                // Refused rather than clamped. A zero or negative threshold
                // would anchor on every poll, which is not what any caller
                // means by it, and a silent repair here becomes a mystery about
                // snapshot volume later.
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
                Some(SnapshotCadence {
                    every_entries: n,
                    poll_interval: std::time::Duration::from_secs_f64(snapshot_poll_seconds),
                })
            }
        };

        let owned = path.clone();
        let db = crate::runtime::block_on(py, async move {
            Database::open_with_cadence(&owned, cadence).await
        })?
        .map_err(to_py)?;

        Ok(Self {
            inner: RwLock::new(Some(db)),
            path,
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
                py.get_type::<pyo3::exceptions::PyResourceWarning>().as_any(),
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
