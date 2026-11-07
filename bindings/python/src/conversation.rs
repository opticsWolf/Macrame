//! The 0.22.0 row types: pointer moves, batch reports, concept versions and
//! change-feed rows (D-307…D-311).
//!
//! Landed in the same release as the Rust half — the rule `branch.rs` states
//! for every lineage feature. All four are `frozen`: each is a snapshot of
//! something the ledger already wrote, and a mutable copy would invite the
//! belief that changing it changes anything.

use pyo3::prelude::*;
use pyo3::FromPyObject;

use crate::timestamps::from_canonical;

/// What a pointer move did: `old` target (or `None`), `new` target, and
/// whether anything was written.
#[pyclass(name = "PointerChange", module = "macrame", frozen)]
pub(crate) struct PyPointerChange {
    pub(crate) inner: macrame::PointerChange,
}

#[pymethods]
impl PyPointerChange {
    #[getter]
    fn source(&self) -> &str {
        &self.inner.source
    }
    #[getter]
    fn edge_type(&self) -> &str {
        &self.inner.edge_type
    }
    #[getter]
    fn old(&self) -> Option<&str> {
        self.inner.old.as_deref()
    }
    /// The target open after the move. (`new` in Python; the Rust method is
    /// renamed only because clippy reserves `new` for constructors.)
    #[getter(new)]
    fn new_target(&self) -> &str {
        &self.inner.new
    }
    #[getter]
    fn changed(&self) -> bool {
        self.inner.changed
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.PointerChange {} -{}-> {:?} => {} changed={}>",
            self.inner.source,
            self.inner.edge_type,
            self.inner.old,
            self.inner.new,
            self.inner.changed
        )
    }
}

/// What a `write_batch` wrote, and the one stamp every row carries.
#[pyclass(name = "BatchReport", module = "macrame", frozen)]
pub(crate) struct PyBatchReport {
    pub(crate) inner: macrame::BatchReport,
}

#[pymethods]
impl PyBatchReport {
    /// The transaction time every row of the batch was recorded at.
    #[getter]
    fn recorded_at<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.recorded_at)
    }
    #[getter]
    fn concepts(&self) -> usize {
        self.inner.concepts
    }
    #[getter]
    fn edges(&self) -> usize {
        self.inner.edges
    }
    #[getter]
    fn retirements(&self) -> usize {
        self.inner.retirements
    }
    /// One `PointerChange` per pointer move, in the order given.
    #[getter]
    fn pointers(&self) -> Vec<PyPointerChange> {
        self.inner
            .pointers
            .iter()
            .cloned()
            .map(|inner| PyPointerChange { inner })
            .collect()
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.BatchReport at {} concepts={} edges={} retirements={} pointers={}>",
            self.inner.recorded_at,
            self.inner.concepts,
            self.inner.edges,
            self.inner.retirements,
            self.inner.pointers.len()
        )
    }
}

/// One concept as one lineage sees it, with where and when that version was
/// written.
#[pyclass(name = "ConceptVersion", module = "macrame", frozen)]
pub(crate) struct PyConceptVersion {
    pub(crate) inner: macrame::temporal::ConceptVersion,
}

#[pymethods]
impl PyConceptVersion {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }
    #[getter]
    fn title(&self) -> &str {
        &self.inner.title
    }
    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }
    #[getter]
    fn embedding_model(&self) -> Option<&str> {
        self.inner.embedding_model.as_deref()
    }
    /// App-defined attributes as JSON text, `"{}"` when none.
    #[getter]
    fn extra(&self) -> &str {
        &self.inner.extra
    }
    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.inner.valid_from {
            Some(ts) => from_canonical(py, ts).map(Some),
            None => Ok(None),
        }
    }
    /// `None` for an open interval.
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.inner.valid_to {
            Some(ts) => from_canonical(py, ts).map(Some),
            None => Ok(None),
        }
    }
    /// The lineage that minted the concept.
    #[getter]
    fn branch_id(&self) -> &str {
        &self.inner.branch_id
    }
    /// When this version was recorded.
    #[getter]
    fn recorded_at<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.recorded_at)
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.ConceptVersion id={:?} branch={:?} recorded_at={}>",
            self.inner.id, self.inner.branch_id, self.inner.recorded_at
        )
    }
}

/// One ledger write, as the change feed reports it.
#[pyclass(name = "Change", module = "macrame", frozen)]
pub(crate) struct PyChange {
    pub(crate) inner: macrame::temporal::Change,
}

#[pymethods]
impl PyChange {
    /// The cursor a consumer keeps.
    #[getter]
    fn seq_id(&self) -> i64 {
        self.inner.seq_id
    }
    #[getter]
    fn branch_id(&self) -> &str {
        &self.inner.branch_id
    }
    /// `"concepts"` or `"links"`.
    #[getter]
    fn table(&self) -> &str {
        &self.inner.table
    }
    /// The concept id, or the edge key `source|target|type|valid_from`.
    #[getter]
    fn entity_id(&self) -> &str {
        &self.inner.entity_id
    }
    /// `"I"` or `"U"`.
    #[getter]
    fn operation(&self) -> &str {
        &self.inner.operation
    }
    #[getter]
    fn recorded_at<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.recorded_at)
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.Change #{} {} {} {:?} on {}>",
            self.inner.seq_id,
            self.inner.operation,
            self.inner.table,
            self.inner.entity_id,
            self.inner.branch_id
        )
    }
}

/// The no-check value a conditional pointer move passes by omission
/// (0.22.0, M3, D-313).
///
/// Python cannot distinguish "keyword omitted" from "keyword given as
/// `None`" at the Rust layer, and the two must mean different things here:
/// `expect=None` is claim-if-free and `expect=UNSET` checks nothing. The
/// value is a one-member class instance so the two spellings stay distinct;
/// the module publishes it as `macrame.UNSET`.
#[pyclass(name = "Unset", module = "macrame", frozen, eq)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyUnset;

#[pymethods]
impl PyUnset {
    fn __repr__(&self) -> &str {
        "macrame.UNSET"
    }
}

/// The claim a `set_pointer(..., expect=..)` makes, as the binding parses
/// it: `Unset` is "no claim at all" (the keyword left out or passed the
/// `UNSET` instance), `ClaimFree` is the draft's `expect=None` — claim
/// there is no open target — and `Target(t)` requires `t` to be open.
/// Hand-parsed rather than derived because two of the arms are unit
/// variants and the parser has to tell the UNSET instance apart from
/// `None`, which no derive can do.
#[derive(Clone, Debug, Default)]
pub(crate) enum Expect {
    /// `macrame.UNSET` — no check, the default.
    #[default]
    Unset,
    /// The draft's `expect=None`: claim-if-free.
    ClaimFree,
    /// `expect="t9"`: require `t9` to be the open target.
    Target(String),
}

impl FromPyObject<'_, '_> for Expect {
    type Error = PyErr;

    fn extract(ob: Borrowed<'_, '_, PyAny>) -> Result<Self, PyErr> {
        if ob.is_none() {
            // The draft's `expect=None`: claim-if-free.
            return Ok(Expect::ClaimFree);
        }
        if ob.is_instance_of::<PyUnset>() {
            // `macrame.UNSET` — the default's own instance: no check.
            return Ok(Expect::Unset);
        }
        // Anything else must be a target the claim requires to be open.
        let target: String = ob.extract()?;
        Ok(Expect::Target(target))
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyUnset>()?;
    m.add_class::<PyPointerChange>()?;
    m.add_class::<PyBatchReport>()?;
    m.add_class::<PyConceptVersion>()?;
    m.add_class::<PyChange>()?;
    m.add("MAX_BATCH_ROWS", macrame::MAX_BATCH_ROWS)?;
    m.add("UNSET", Py::new(m.py(), PyUnset {})?.into_any())?;
    Ok(())
}
