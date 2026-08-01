//! The temporal surface (P4.3): reconstruct, archive, and the chain check.
//!
//! # Edges are tuples, and the timestamps inside them are not strings
//!
//! [`macrame::temporal::MaterializedState::edges`] and
//! [`macrame::temporal::query_as_of_edges`] both answer with
//! `(source, target, edge_type, valid_from, valid_to)`. Those stay tuples here
//! — a five-field record with no behaviour is what a tuple is for, and a
//! `#[pyclass]` per shape would be three more names in `dir(macrame)` for no
//! capability.
//!
//! The two timestamps are rendered as aware `datetime`s all the same, with the
//! open sentinel as `None`, because P3's rule is about the boundary rather than
//! about which container the value arrives in. A caller who gets a `datetime`
//! from `EdgeAssertion.valid_to` and a 27-byte string from an edge tuple would
//! have to learn which is which, and the sentinel is exactly the value that
//! punishes guessing.
//!
//! # `ArchiveReport` and `ChainCheck` are classes, and that is not inconsistent
//!
//! Both carry more than five fields, both have fields whose *relationship*
//! needs stating — `composed_anchor` and `folded_anchor` may legitimately differ
//! and must never be compared — and `ChainCheck` has a derived question,
//! `diverged()`, that is the one most callers want. A tuple cannot carry any of
//! that, and a positional index into eleven fields is how a caller compares the
//! two anchors by accident.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use macrame::temporal::{ArchiveReport, ChainCheck, MaterializedState};

use crate::graph::PyNodeAttributes;
use crate::timestamps::from_canonical;

/// `(source, target, edge_type, valid_from, valid_to)`, timestamps rendered.
pub(crate) fn edge_to_py<'py>(
    py: Python<'py>,
    e: &(String, String, String, String, String),
) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(
        py,
        [
            e.0.clone().into_pyobject(py)?.into_any(),
            e.1.clone().into_pyobject(py)?.into_any(),
            e.2.clone().into_pyobject(py)?.into_any(),
            from_canonical(py, &e.3)?,
            from_canonical(py, &e.4)?,
        ],
    )
}

/// The world as believed at an instant (§5.5).
#[pyclass(name = "MaterializedState", module = "macrame", frozen)]
pub(crate) struct PyMaterializedState {
    pub(crate) inner: MaterializedState,
}

#[pymethods]
impl PyMaterializedState {
    /// The newest `transaction_log.seq` this state accounts for.
    ///
    /// **Not a comparison key against another state's anchor** — see
    /// [`PyChainCheck`], where two legitimately differ.
    #[getter]
    fn seq_anchor(&self) -> i64 {
        self.inner.seq_anchor
    }

    /// The instant this state was reconstructed at.
    #[getter]
    fn timestamp<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.timestamp)
    }

    /// `{concept_id: NodeAttributes}`.
    ///
    /// A copy, unlike `Subgraph` — a `MaterializedState` is already a fold of
    /// the whole log up to an instant, so there is no lazy view to preserve and
    /// nothing here that a caller can ask for a piece of.
    #[getter]
    fn concepts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        for (id, attrs) in &self.inner.concepts {
            out.set_item(
                id,
                PyNodeAttributes {
                    inner: attrs.clone(),
                },
            )?;
        }
        Ok(out)
    }

    /// `[(source, target, edge_type, valid_from, valid_to)]`.
    #[getter]
    fn edges<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        self.inner.edges.iter().map(|e| edge_to_py(py, e)).collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.MaterializedState at={} concepts={} edges={}>",
            self.inner.timestamp,
            self.inner.concepts.len(),
            self.inner.edges.len()
        )
    }
}

/// What one archive session moved to cold storage.
#[pyclass(name = "ArchiveReport", module = "macrame", frozen)]
pub(crate) struct PyArchiveReport {
    pub(crate) inner: ArchiveReport,
}

#[pymethods]
impl PyArchiveReport {
    #[getter]
    fn links_archived(&self) -> usize {
        self.inner.links_archived
    }
    #[getter]
    fn log_entries_archived(&self) -> usize {
        self.inner.log_entries_archived
    }
    /// The `seq` below which the log has been moved, or `None` if nothing was.
    #[getter]
    fn horizon(&self) -> Option<i64> {
        self.inner.horizon
    }
    fn __repr__(&self) -> String {
        // `{:?}` on the Option would render `Some(1)` / `None`, which is Rust
        // leaking into a Python repr. The horizon is a Python `int | None` and
        // should read as one.
        format!(
            "<macrame.ArchiveReport links={} log={} horizon={}>",
            self.inner.links_archived,
            self.inner.log_entries_archived,
            match self.inner.horizon {
                Some(h) => h.to_string(),
                None => "None".to_string(),
            }
        )
    }
}

/// Whether snapshot composition agrees with a fold from genesis (D-092).
///
/// Snapshot *n* composes onto snapshot *n-1* and nothing ever folds the whole
/// log, so an error at any link is copied forward and every subsequent read
/// agrees with it — consistently, and wrongly. This folds independently and
/// compares.
///
/// **It reports and does not repair.** Under Doctrine VI a snapshot is
/// derivative: the fix is to delete the snapshot directory, which a caller can
/// do without this, and rewriting the file would destroy the only evidence that
/// composition has a defect.
#[pyclass(name = "ChainCheck", module = "macrame", frozen)]
pub(crate) struct PyChainCheck {
    pub(crate) inner: ChainCheck,
}

#[pymethods]
impl PyChainCheck {
    /// **The question worth asking.** True when the two disagree about anything.
    fn diverged(&self) -> bool {
        self.inner.diverged()
    }

    #[getter]
    fn timestamp<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.timestamp)
    }

    /// Reported for diagnosis, **never compared** against `folded_anchor`.
    ///
    /// The composed answer anchors at the snapshot it started from plus its
    /// delta; the fold anchors at the newest row it saw. They differ in ordinary
    /// operation, so an equality check here reports divergence that is not there
    /// — which is worse than no check, because it is a check.
    #[getter]
    fn composed_anchor(&self) -> i64 {
        self.inner.composed_anchor
    }
    /// See [`PyChainCheck::composed_anchor`].
    #[getter]
    fn folded_anchor(&self) -> i64 {
        self.inner.folded_anchor
    }
    #[getter]
    fn composed_concepts(&self) -> usize {
        self.inner.composed_concepts
    }
    #[getter]
    fn folded_concepts(&self) -> usize {
        self.inner.folded_concepts
    }
    #[getter]
    fn composed_edges(&self) -> usize {
        self.inner.composed_edges
    }
    #[getter]
    fn folded_edges(&self) -> usize {
        self.inner.folded_edges
    }
    /// Concept ids present in one and not the other, or whose attributes differ.
    #[getter]
    fn concept_disagreements(&self) -> Vec<String> {
        self.inner.concept_disagreements.clone()
    }
    /// Edge keys present in one and not the other.
    #[getter]
    fn edge_disagreements(&self) -> Vec<String> {
        self.inner.edge_disagreements.clone()
    }
    /// True when either list hit `SAMPLE_LIMIT` — so the lists are a sample and
    /// their length is not the count.
    #[getter]
    fn truncated(&self) -> bool {
        self.inner.truncated
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.ChainCheck at={} diverged={}>",
            self.inner.timestamp,
            // Rust prints `false`; Python spells it `False`.
            if self.inner.diverged() {
                "True"
            } else {
                "False"
            }
        )
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMaterializedState>()?;
    m.add_class::<PyArchiveReport>()?;
    m.add_class::<PyChainCheck>()?;
    // The cap on each disagreement list, so a caller can tell a full list from a
    // truncated one without hard-coding 32.
    m.add("CHAIN_CHECK_SAMPLE_LIMIT", ChainCheck::SAMPLE_LIMIT)?;
    Ok(())
}
