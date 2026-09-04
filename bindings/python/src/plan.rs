//! What a read asks for, on the Python side (0.15.9, W13.4, D-251).
//!
//! # Why this is a constructor where Rust's is a builder
//!
//! `macrame::ReadPlan` is fluent — `ReadPlan::new().on(b).valid_at(t)` — for
//! the reason every value type in that crate is: Rust has no keyword
//! arguments, so a struct with four optional fields is either a builder or a
//! function taking four `Option`s in an order nobody can remember. Python has
//! keyword arguments, and `ReadPlan(branch="exp", valid=tuesday)` is the same
//! sentence with the scaffolding removed.
//!
//! So the two are not spelled alike, on purpose, and the parity that matters is
//! that they *mean* alike: an unset field is the ordinary read on both sides —
//! the trunk, now, current belief — and a plan is inert on both sides, so every
//! refusal belongs to the read that takes one and not to building one.
//!
//! Timestamps are the exception to inertness and have to be. The binding
//! canonicalises at its boundary rather than at the read, because a `datetime`
//! is the form a Python caller actually holds and the ledger speaks strings;
//! deferring the conversion would mean carrying a `PyObject` into the runtime
//! and raising `TypeError` from inside an `await`. A malformed instant is
//! therefore refused by the constructor here and by the read in Rust, which is
//! the same instant refused in the same place a caller can see it.

use pyo3::prelude::*;
use pyo3::types::PyAny;

use macrame::ReadPlan;

use crate::timestamps::{from_canonical, to_canonical_opt};

/// The lineage, the two instants, and the ceiling a read is taken under.
#[pyclass(name = "ReadPlan", module = "macrame", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyReadPlan {
    pub(crate) inner: ReadPlan,
}

#[pymethods]
impl PyReadPlan {
    /// Every argument optional, and every default the ordinary read.
    ///
    /// `branch=None` is the trunk, `valid=None` is now, `recorded=None` is
    /// current belief — which is a projection read rather than a fold bounded
    /// at the present. The two give the same answer and only one of them is
    /// cheap, so `recorded=` is a thing to ask for and not a thing to pass
    /// through defensively.
    ///
    /// `limit=None` is the whole answer (0.15.10, W13.5). It is the one
    /// argument that does not narrow *which* rows are true — it bounds what the
    /// read costs — so a plan carrying one describes a sample. On
    /// `Database.edges` that sample is arbitrary and `len(result) == limit` is
    /// the exact signal that it was cut; the module docs say why the traversal
    /// keywords need a second return value to say the same thing.
    #[new]
    #[pyo3(signature = (*, branch = None, valid = None, recorded = None, limit = None))]
    fn new(
        branch: Option<String>,
        valid: Option<&Bound<'_, PyAny>>,
        recorded: Option<&Bound<'_, PyAny>>,
        limit: Option<usize>,
    ) -> PyResult<Self> {
        let mut plan = ReadPlan::new();
        if let Some(name) = branch {
            plan = plan.on(crate::branch::branch_id(&name)?);
        }
        if let Some(ts) = to_canonical_opt(valid)? {
            plan = plan.valid_at(ts);
        }
        if let Some(ts) = to_canonical_opt(recorded)? {
            plan = plan.recorded_at(ts);
        }
        if let Some(n) = limit {
            plan = plan.limit(n);
        }
        Ok(Self { inner: plan })
    }

    /// The lineage this plan reads, or `None` for the trunk.
    #[getter]
    fn branch(&self) -> Option<&str> {
        self.inner.branch.as_ref().map(|b| b.as_str())
    }

    /// The valid-time instant — *what was true then* — or `None` for now.
    #[getter]
    fn valid<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .valid
            .as_deref()
            .map(|ts| from_canonical(py, ts))
            .transpose()
    }

    /// The transaction-time instant — *what did we believe then* — or `None`
    /// for current belief.
    #[getter]
    fn recorded<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .recorded
            .as_deref()
            .map(|ts| from_canonical(py, ts))
            .transpose()
    }

    /// How many rows this read will pay for, or `None` for all of them.
    #[getter]
    fn limit(&self) -> Option<usize> {
        self.inner.limit
    }

    fn __repr__(&self) -> String {
        format!(
            "ReadPlan(branch={:?}, valid={:?}, recorded={:?}, limit={:?})",
            self.inner.branch.as_ref().map(|b| b.as_str()),
            self.inner.valid,
            self.inner.recorded,
            self.inner.limit
        )
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyReadPlan>()?;
    Ok(())
}
