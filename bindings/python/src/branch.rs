//! The lineage surface (§15.4, W12.7).
//!
//! Lands in the same release as the Rust half, which is W6's finding applied
//! rather than restated: a binding gap opened in the release that created the
//! feature never becomes a convention. This is the fourth holding — `branch=`
//! on the traversal entry points at 0.14.4, the belief's lineage label on
//! `MaterializedState.edges` at 0.14.5, nothing owed at 0.14.6 because it added
//! no surface in either language, and `fork`/`branches` here. `diff` at
//! 0.14.11 is the sixth — 0.14.10 was the one *lapse*, and it was closed in the
//! release that found it rather than left as a second one.
//!
//! # Why `BranchId` is not a Python class
//!
//! The Rust type exists to make an unvalidated name unrepresentable at the call
//! site, which is a compile-time argument and there is no compile step here. A
//! `macrame.BranchId` would be a wrapper Python callers construct only to hand
//! straight back, and the validation would run at construction rather than at
//! the call — the same instant, one more name. So the Python surface takes
//! `str` and validates on the way in, raising `InvalidBranchIdError` from the
//! call that used the name. `Branch` *is* a class, because it carries four
//! fields out and a tuple would make three of them positional trivia.

use pyo3::prelude::*;

use crate::errors::to_py;
use crate::timestamps::from_canonical;

/// One lineage: its name, its parent, and where it was cut.
///
/// `frozen` for the reason every row type here is: it is a snapshot of a row in
/// an append-only table, and a mutable copy would invite the belief that
/// changing it changes anything.
#[pyclass(name = "Branch", module = "macrame", frozen)]
pub(crate) struct PyBranch {
    pub(crate) inner: macrame::Branch,
}

#[pymethods]
impl PyBranch {
    /// The lineage's own name.
    #[getter]
    fn id(&self) -> &str {
        self.inner.id.as_str()
    }

    /// The lineage this one was cut from, or `None` for the trunk.
    #[getter]
    fn parent(&self) -> Option<&str> {
        self.inner.parent.as_ref().map(|p| p.as_str())
    }

    /// The instant this lineage stopped inheriting its parent's later writes,
    /// or `None` for the trunk.
    ///
    /// This is the visibility cutoff a branched read is bounded by: the branch
    /// sees its parent's history up to and including this instant, and nothing
    /// the parent records after it.
    #[getter]
    fn forked_at<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.inner.forked_at {
            Some(ts) => from_canonical(py, ts).map(Some),
            None => Ok(None),
        }
    }

    /// When the row was written.
    ///
    /// Equal to `forked_at` for every branch this release can create — forking
    /// from a past instant is additive later, and the two columns exist so that
    /// it can be.
    ///
    /// **Not comparable with a `recorded_at`.** The trunk's `created_at` is
    /// stamped during migration, before the database's clock is resolved, so on
    /// an injected-clock database it is not on the same timeline as the ledger.
    /// Use `forked_at`, which `fork()` issues from the same clock as every
    /// write.
    #[getter]
    fn created_at<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.created_at)
    }

    /// Whether this names the trunk.
    #[getter]
    fn is_main(&self) -> bool {
        self.inner.id.is_main()
    }

    fn __repr__(&self) -> String {
        match &self.inner.parent {
            Some(parent) => format!(
                "<macrame.Branch {} from {} at {}>",
                self.inner.id,
                parent,
                self.inner.forked_at.as_deref().unwrap_or("?")
            ),
            None => format!("<macrame.Branch {} (trunk)>", self.inner.id),
        }
    }
}

/// One row of `a`'s belief that `b` does not share (§15.4, W12.11).
///
/// `frozen` and `eq` for the same reason [`PyBranch`] is frozen: it is a
/// snapshot of what two lineages believed at one instant, and the equality is
/// what makes a diff assertable in a test without spelling seven getters.
///
/// **`branch_id` is which lineage *wrote* the row, and it is not the answer to
/// "who diverged".** It is here because it is the only way to tell an
/// assertion `a` made from an inherited row `a` still holds while `b` has moved
/// on — and the second case is the one §15.4's cheap characterization was
/// missing. A caller that wants "what did this branch do" filters on it; a
/// caller that wants "where do these two disagree" already has that in the fact
/// that the row came back at all.
#[pyclass(
    name = "Divergence",
    module = "macrame",
    frozen,
    eq,
    skip_from_py_object
)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyDivergence {
    pub(crate) inner: macrame::Divergence,
}

#[pymethods]
impl PyDivergence {
    #[getter]
    fn source_id(&self) -> &str {
        &self.inner.source_id
    }

    #[getter]
    fn target_id(&self) -> &str {
        &self.inner.target_id
    }

    #[getter]
    fn edge_type(&self) -> &str {
        &self.inner.edge_type
    }

    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_from)
    }

    /// `None` for an open interval, per P3's sentinel rule.
    ///
    /// A closed one here is frequently the *whole* divergence: a branch that
    /// shadow-retires an inherited edge differs from its parent in this field
    /// and in nothing else.
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_to)
    }

    #[getter]
    fn weight(&self) -> f64 {
        self.inner.weight
    }

    /// The lineage whose row this is — not necessarily either side of the diff.
    #[getter]
    fn branch_id(&self) -> &str {
        &self.inner.branch_id
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.Divergence {}->{} type={:?} on {} w={}>",
            self.inner.source_id,
            self.inner.target_id,
            self.inner.edge_type,
            self.inner.branch_id,
            self.inner.weight
        )
    }
}

/// Validate a name at the boundary, so the error names the call that used it.
pub(crate) fn branch_id(name: &str) -> PyResult<macrame::BranchId> {
    macrame::BranchId::new(name).map_err(to_py)
}
