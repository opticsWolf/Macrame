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
use std::sync::{Mutex, RwLock};

use pyo3::prelude::*;
use pyo3::types::PyType;

use macrame::prelude::*;

use crate::errors::{closed_error, to_py};
use crate::graph;
use crate::observe;
use crate::rows;
use crate::runtime::{check_not_forked, runtime};
use crate::temporal;
use crate::timestamps::to_canonical;
use crate::types::{PyAnnotation, PyAttributeMode, PyConceptUpsert, PyEdgeAssertion};
use crate::vector;

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

    /// Run `sql` on a fresh diagnostic connection, **one caller at a time**.
    ///
    /// # Why this is serialised when nothing else here is
    ///
    /// Every other method on this handle runs against connections opened once,
    /// at `Database::open`. This path is the exception: `diagnostic_conn()`
    /// performs a *new* `libsql::Builder::…build()` per call
    /// ([D-091](../../../docs/architecture/s13-decision-register.md)), and
    /// `with_db` releases the GIL, so two Python threads calling
    /// `diagnostic_query` reach `build()` concurrently. That is
    /// [R15](../../../README.md)'s shape — the upstream libSQL access violation
    /// on concurrent opens — reachable from ordinary Python with no `unsafe`
    /// and no threading the caller would think twice about. Measured at width
    /// 48: **7 bad runs in 18** without this lock — two `0xC0000005` and five
    /// returned SQLite errors — and **0 in 18** with it
    /// (`tests_py/probes/r15_diagnostic_path.py`).
    ///
    /// The mutex bounds this path to one outstanding open. It changes no
    /// semantics: the connection is still opened and dropped per call, still
    /// read-only, still the caller's own. Two threads that would have opened
    /// simultaneously now queue, and a diagnostic path is where queueing is
    /// cheapest.
    ///
    /// **This mitigates the Python symptom, not R15** — the Rust API has the
    /// same exposure and is documented rather than locked, because a
    /// `Database::diagnostic_conn` that serialised behind a mutex the caller
    /// cannot see would be lying about being "the caller's own". See that
    /// method's rustdoc.
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
    #[staticmethod]
    #[pyo3(signature = (path, *, snapshot_every_entries = Some(10_000), snapshot_poll_seconds = 5.0))]
    fn open(
        py: Python<'_>,
        path: PathBuf,
        snapshot_every_entries: Option<i64>,
        snapshot_poll_seconds: f64,
    ) -> PyResult<Self> {
        let cadence = to_cadence(snapshot_every_entries, snapshot_poll_seconds)?;

        let owned = path.clone();
        let db = crate::runtime::block_on(py, async move {
            Database::open_with_cadence(&owned, cadence).await
        })?
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
        let tuning = macrame::Tuning {
            cadence: match cadence {
                Some(c) => macrame::CadencePolicy::Every(c),
                None => macrame::CadencePolicy::Disabled,
            },
            clock: Some(clock.inner.clone()),
            ..Default::default()
        };

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
    ///
    /// **Where the chunks fall depends on the machine** (0.12.0). The loop
    /// measures each chunk and sizes the next one from it, so the same edges
    /// imported twice can land under a different number of `recorded_at`
    /// stamps. Each chunk is still exactly one transaction under exactly one
    /// stamp, and a reader mid-import still sees a prefix and never half a
    /// chunk — what is not promised is that two identical calls stamp
    /// identically. `write_bulk_atomic` is the escape hatch if you need that.
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
    #[pyo3(signature = (
        start_node, *, max_depth = 2, edge_types = None, min_weight = 0.0,
        as_of = None, now = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn traverse_ids(
        &self,
        py: Python<'_>,
        start_node: &str,
        max_depth: usize,
        edge_types: Option<Vec<String>>,
        min_weight: f64,
        as_of: Option<&Bound<'_, PyAny>>,
        now: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<String>> {
        let as_of = as_of.map(|t| to_canonical(Some(t))).transpose()?;
        let now = self.instant(py, now)?;
        let b = graph::builder(start_node, max_depth, edge_types, min_weight, None, as_of);
        self.with_db(py, move |db| {
            runtime()
                .block_on(b.execute_ids(db.read_conn(), &now))
                .map_err(to_py)
        })
    }

    /// Traverse and hydrate attributes, as a list of `NodeAttributes` (§5.2).
    ///
    /// `attribute_mode` decides *which text* comes back and `as_of` decides
    /// *which topology*. They are independent questions, and setting `as_of`
    /// without stating a mode raises `AttributeModeUnstatedError` rather than
    /// defaulting (D-085): `as_of(t)` with live attributes returns the past's
    /// graph wearing the present's titles — a legitimate thing to want and a
    /// terrible thing to get by accident.
    ///
    /// `AttributeMode.OMIT` is **refused** here, with a message naming
    /// `traverse_ids`. Under that mode there are no attributes to hydrate, so
    /// the Rust method answers with an empty list that no caller can tell apart
    /// from a traversal that reached nothing. See the module docs for why this
    /// is the one place the binding refuses what the library accepts.
    #[pyo3(signature = (
        start_node, *, max_depth = 2, edge_types = None, min_weight = 0.0,
        attribute_mode = None, as_of = None, now = None
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
        as_of: Option<&Bound<'_, PyAny>>,
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
        let as_of = as_of.map(|t| to_canonical(Some(t))).transpose()?;
        let now = self.instant(py, now)?;
        let b = graph::builder(
            start_node,
            max_depth,
            edge_types,
            min_weight,
            attribute_mode,
            as_of,
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
    #[pyo3(signature = (
        start_node, max_hops, byte_budget, *, edge_types = None,
        min_weight = None, now = None, content = false
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
        now: Option<&Bound<'_, PyAny>>,
        content: bool,
    ) -> PyResult<graph::PySubgraph> {
        let now = self.instant(py, now)?;
        let b = graph::builder(
            start_node,
            max_hops,
            edge_types,
            min_weight.unwrap_or(f64::NEG_INFINITY),
            None,
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

    /// Edges under current belief as of `ts`, as tuples.
    ///
    /// Topology only, and unlike `traverse_ids` it is not anchored at a start
    /// node: this is the whole of `links_current` filtered to the instant. On a
    /// large ledger that is a large answer, and there is no budget on it —
    /// `load_subgraph` is the bounded neighbourhood read.
    #[pyo3(signature = (ts = None))]
    fn query_as_of_edges<'py>(
        &self,
        py: Python<'py>,
        ts: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Vec<Bound<'py, pyo3::types::PyTuple>>> {
        let ts = self.instant(py, ts)?;
        let raw = self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::temporal::query_as_of_edges(db.read_conn(), &ts))
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
    fn upsert_embeddings(
        &self,
        py: Python<'_>,
        model: &str,
        rows: &Bound<'_, PyAny>,
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
        self.with_db(py, move |db| {
            runtime()
                .block_on(db.upsert_embeddings(&model, decoded))
                .map_err(to_py)
        })
    }

    /// Nearest `top_k` concepts to `query` by cosine distance (§5.9).
    ///
    /// Goes through the DiskANN index rather than scanning: an
    /// `ORDER BY vector_distance_cos(…)` over the table is linear in the corpus
    /// however small `top_k` is. Results ascend — **smaller score is closer**.
    #[pyo3(signature = (model, query, *, top_k = 10))]
    fn search_vector(
        &self,
        py: Python<'_>,
        model: &str,
        query: &Bound<'_, PyAny>,
        top_k: usize,
    ) -> PyResult<Vec<vector::PyVectorHit>> {
        let model = vector::model_name(model)?;
        let query = crate::types::coerce_embedding(query)?;
        let hits = self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::search_vector(
                    db.read_conn(),
                    &query,
                    &model,
                    top_k,
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
    #[pyo3(signature = (query, *, top_k = 10, raw = false))]
    fn keyword_search(
        &self,
        py: Python<'_>,
        query: &str,
        top_k: usize,
        raw: bool,
    ) -> PyResult<Vec<(String, f64)>> {
        let expr = if raw {
            query.to_string()
        } else {
            macrame::vector::escape_fts5_query(query)
        };
        self.with_db(py, move |db| {
            runtime()
                .block_on(macrame::vector::keyword_search(
                    db.read_conn(),
                    &expr,
                    top_k,
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
    #[pyo3(signature = (model, query_text, query_vector, *, top_k = 10, depth = None, rrf_k = None, raw = false))]
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
    #[pyo3(signature = (
        model, query, start_node, *, max_depth = 2, edge_types = None,
        min_weight = 0.0, top_k = 10, byte_budget = None, probe_cap = None,
        strategy = None, now = None
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
    ) -> PyResult<(Vec<vector::PyVectorHit>, vector::PyCostEstimate)> {
        let model = vector::model_name(model)?;
        let query = crate::types::coerce_embedding(query)?;
        let now = self.instant(py, now)?;
        let traversal = graph::builder(start_node, max_depth, edge_types, min_weight, None, None);

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
    Ok(Some(SnapshotCadence {
        every_entries: n,
        poll_interval: std::time::Duration::from_secs_f64(snapshot_poll_seconds),
    }))
}
