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
use crate::rows;
use crate::runtime::{check_not_forked, runtime};
use crate::timestamps::to_canonical;
use crate::types::{PyAnnotation, PyConceptUpsert, PyEdgeAssertion};

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
    #[pyo3(signature = (source, target, edge_type, valid_from, valid_to))]
    fn retire_edge(
        &self,
        py: Python<'_>,
        source: String,
        target: String,
        edge_type: String,
        valid_from: &Bound<'_, PyAny>,
        valid_to: &Bound<'_, PyAny>,
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
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.retire_edge(source, target, edge_type, &from, &to))
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
    fn bulk_import(&self, py: Python<'_>, edges: Vec<PyEdgeAssertion>) -> PyResult<usize> {
        let edges: Vec<EdgeAssertion> = edges.into_iter().map(|e| e.inner).collect();
        self.with_db(py, move |db| {
            runtime().block_on(db.bulk_import(edges)).map_err(to_py)
        })
    }

    /// Upsert many concepts on the background channel, chunked (D-011).
    ///
    /// Every row written here is a **ledger** write: it versions the concept and
    /// lands in `transaction_log`. Derived analytics output does not belong here
    /// — see `write_analytics_annotations` and D-041.
    fn write_concepts(&self, py: Python<'_>, concepts: Vec<PyConceptUpsert>) -> PyResult<usize> {
        let concepts: Vec<ConceptUpsert> = concepts.into_iter().map(|c| c.inner).collect();
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.write_concepts(concepts))
                .map_err(to_py)
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
    fn write_analytics_annotations(
        &self,
        py: Python<'_>,
        annotations: Vec<PyAnnotation>,
    ) -> PyResult<usize> {
        let annotations: Vec<Annotation> = annotations.into_iter().map(|a| a.inner).collect();
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.write_analytics_annotations(annotations))
                .map_err(to_py)
        })
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
    /// The typed read surface is P4.2. This is not it.
    #[pyo3(signature = (sql, params = None))]
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
        let sql = sql.to_string();
        let raw = self.with_db(py, move |db| {
            rows::map_err(runtime().block_on(async {
                let conn = db.diagnostic_conn().await?;
                rows::collect(&conn, &sql, bound).await
            }))
        })?;
        rows::rows_to_py(py, raw)
    }

    /// `EXPLAIN QUERY PLAN` for `sql`, as the detail column only.
    ///
    /// The use T5.1 named first. Separate from `diagnostic_query` because a
    /// plan's shape is not a query's shape, and callers want the detail rather
    /// than three columns of bookkeeping.
    fn explain(&self, py: Python<'_>, sql: &str) -> PyResult<Vec<String>> {
        let sql = format!("EXPLAIN QUERY PLAN {sql}");
        let raw = self.with_db(py, move |db| {
            rows::map_err(runtime().block_on(async {
                let conn = db.diagnostic_conn().await?;
                rows::collect(&conn, &sql, Vec::new()).await
            }))
        })?;
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
