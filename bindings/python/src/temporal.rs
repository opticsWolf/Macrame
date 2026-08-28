//! The temporal surface (P4.3): reconstruct, archive, and the chain check.
//!
//! # Edges are tuples, and the timestamps inside them are not strings
//!
//! [`macrame::temporal::query_as_of_edges`] answers with
//! `(source, target, edge_type, valid_from, valid_to)`. That stays a tuple here
//! — a five-field record with no behaviour is what a tuple is for, and a
//! `#[pyclass]` per shape would be three more names in `dir(macrame)` for no
//! capability.
//!
//! # `MaterializedState.edges` carries a sixth field, and the two shapes differ
//! # for a reason rather than by accident (0.14.5, D-221)
//!
//! A belief is `(source, target, edge_type, valid_from, valid_to, branch)`, and
//! the split is exactly **resolved against unresolved**. `query_as_of_edges`
//! answers for one lineage — the caller's, or the trunk when they named none —
//! so the label would repeat what the caller already said. `reconstruct` asks a
//! whole-ledger question and a forked ledger answers it with two beliefs about
//! one edge; without the label those are two indistinguishable rows, which is
//! the defect [D-221](../../../docs/architecture/s13-decision-register.md#d-221)
//! records rather than a shape worth copying. So the difference is visible in
//! the type, and a caller who wants one lineage's view of an instant calls the
//! reader that resolves rather than filtering a fold by hand.
//!
//! **Six fields, and the rule below says more than five should be a class.** It
//! does not reach here, for the reason that rule is a proxy for: there is
//! nothing new to get wrong. `branch` is a `str` appended after a
//! `datetime | None`, so a misindex fails on type rather than returning a
//! plausible wrong value, and it carries no relationship to another field and no
//! derived question. What it does cost is that the *next* field would break
//! unpacking again — which the Rust `EdgeBelief` avoids with
//! `#[non_exhaustive]` and Python cannot. Recorded as the price of the shape
//! rather than argued away: at that point this becomes a class, and the trigger
//! is a seventh field, not a preference.
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
//!
//! # `RehydrateReport` is a class with two fields, which this rule does not reach
//!
//! By field count it should be a tuple, and it is not, for two reasons the count
//! cannot see. It is the **counterpart of `ArchiveReport`**, and a caller who
//! reads `report.concepts_archived` going out and `report[0]` coming back has to
//! learn which direction returns which shape — the pair is the unit a caller
//! thinks in, so the pair is what should look alike. And `rowids_reassigned` is
//! precisely the field a positional index gets wrong: two `int`s in a tuple,
//! where one is *work done* and the other is *something unusual happened*, is an
//! invitation to read `[0]` and mean `[1]`. The rule above is about arity as a
//! proxy for "is there anything here to get wrong"; here there is, at arity two.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use macrame::temporal::{
    ArchiveReport, ChainCheck, EdgeBelief, MaterializedState, RehydrateReport,
};

use crate::graph::PyNodeAttributes;
use crate::timestamps::from_canonical;

/// `(source, target, edge_type, valid_from, valid_to)`, timestamps rendered.
///
/// The **resolved** shape: one lineage's view, so there is no label to carry.
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

/// `(source, target, edge_type, valid_from, valid_to, branch)`, as above plus
/// the lineage holding the belief (0.14.5, D-221).
///
/// The **unresolved** shape. Not written in terms of `edge_to_py` and a
/// concatenation, because the two are only incidentally a prefix of each other:
/// they answer different questions and one of them is free to stop being a
/// tuple without dragging the other with it.
pub(crate) fn belief_to_py<'py>(py: Python<'py>, e: &EdgeBelief) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(
        py,
        [
            e.source_id.clone().into_pyobject(py)?.into_any(),
            e.target_id.clone().into_pyobject(py)?.into_any(),
            e.edge_type.clone().into_pyobject(py)?.into_any(),
            from_canonical(py, &e.valid_from)?,
            from_canonical(py, &e.valid_to)?,
            e.branch_id.clone().into_pyobject(py)?.into_any(),
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

    /// `[(source, target, edge_type, valid_from, valid_to, branch)]`.
    ///
    /// One entry per lineage per edge, not one per edge: a fork and its ancestor
    /// believing different things about one edge key are two beliefs, and this
    /// is the reader that says so (0.14.5, D-221).
    #[getter]
    fn edges<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyTuple>>> {
        self.inner
            .edges
            .iter()
            .map(|e| belief_to_py(py, e))
            .collect()
    }

    /// Whether nothing had been recorded yet at [`timestamp`] (0.8.0, D-121).
    ///
    /// An empty state has two meanings and only the caller knows which one
    /// matters: *everything had been retired by then* is a fact about the data,
    /// *the ledger had not started* is a fact about the question. Both arrive as
    /// `concepts == {}` and `edges == []`.
    ///
    /// This is the half of B5 that is visible from Python. The other half is
    /// that asking it no longer raises `ReplayCorruptError` — which it did, on
    /// any database with at least one write, naming a `*_archive.db` the caller
    /// had never created.
    #[getter]
    fn predates_recorded_history(&self) -> bool {
        self.inner.predates_recorded_history
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.MaterializedState at={} concepts={} edges={}{}>",
            self.inner.timestamp,
            self.inner.concepts.len(),
            self.inner.edges.len(),
            if self.inner.predates_recorded_history {
                " predates_recorded_history"
            } else {
                ""
            }
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
    /// Concepts moved to the cold file (0.9.0, C2). Always `0` before schema
    /// v9, where no concept could leave the hot table at all.
    #[getter]
    fn concepts_archived(&self) -> usize {
        self.inner.concepts_archived
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
            "<macrame.ArchiveReport links={} concepts={} log={} horizon={}>",
            self.inner.links_archived,
            self.inner.concepts_archived,
            self.inner.log_entries_archived,
            match self.inner.horizon {
                Some(h) => h.to_string(),
                None => "None".to_string(),
            }
        )
    }
}

/// What one rehydration moved back out of cold storage (0.9.0, C3, D-131).
///
/// **`rowids_reassigned` is exposed rather than kept internal**, and it is the
/// only field here that is not a plain count of work done. A rehydrated concept
/// normally reclaims the `rowid_pk` it had before it went cold; when something
/// else has taken that value in the meantime it gets a fresh one and the search
/// index is re-pointed to match. That is the one respect in which the row coming
/// back differs from the row that left, so a caller holding rowids across the
/// boundary — the only kind of caller for whom it matters — can see that it
/// happened rather than discovering it through a stale join.
#[pyclass(name = "RehydrateReport", module = "macrame", frozen)]
pub(crate) struct PyRehydrateReport {
    pub(crate) inner: RehydrateReport,
}

#[pymethods]
impl PyRehydrateReport {
    /// Concepts moved back into the hot tables.
    ///
    /// Ids not present in the cold file are skipped rather than raising, so this
    /// can be smaller than the list passed in — the caller's list usually comes
    /// from an earlier cold-side query and being partially stale is the normal
    /// case, not an error.
    #[getter]
    fn concepts_rehydrated(&self) -> usize {
        self.inner.concepts_rehydrated
    }
    /// Of those, how many could not keep their original `rowid_pk`.
    #[getter]
    fn rowids_reassigned(&self) -> usize {
        self.inner.rowids_reassigned
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.RehydrateReport concepts={} rowids_reassigned={}>",
            self.inner.concepts_rehydrated, self.inner.rowids_reassigned
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
    m.add_class::<PyRehydrateReport>()?;
    m.add_class::<PyChainCheck>()?;
    // The cap on each disagreement list, so a caller can tell a full list from a
    // truncated one without hard-coding 32.
    m.add("CHAIN_CHECK_SAMPLE_LIMIT", ChainCheck::SAMPLE_LIMIT)?;
    Ok(())
}
