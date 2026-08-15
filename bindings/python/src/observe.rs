//! Integrity reports and actor metrics (P4.5, P4.6).
//!
//! # The metrics are here because the wheel turns them on (D-093)
//!
//! `--features metrics` is off by default in the crate, and the argument for
//! that default is [§5.10](../docs/architecture/s5-modules.md)'s: a histogram
//! that is nearly free of cost is not free of *risk*, and the crate's contract
//! is a latency bound.
//!
//! The wheel builds with it on, and that is a different decision for a
//! different audience rather than a contradiction. A Rust caller who wants the
//! counters adds a feature flag to a `Cargo.toml` they already own; a Python
//! caller cannot rebuild the extension, so shipping it off would mean shipping
//! `metrics()` as a method that exists and always answers zero — or not
//! shipping it, and leaving `CHUNK_BUDGET` as a number in the docs with no way
//! to check it *in situ*, which is exactly what T1.4 was about.
//!
//! # `budget_violations()` is the question, and the buckets are the evidence
//!
//! An operator does not want a histogram; they want to know whether the actor
//! is holding the write connection longer than it promised, and for what. So
//! `violations()` comes first and the per-kind detail hangs off it.

use pyo3::prelude::*;

use macrame::integrity::RebuildReport;
use macrame::metrics::{KindSnapshot, MetricsSnapshot, BUCKET_BOUNDS_MICROS};

/// What a rebuild of `links_current` did, and what it left behind.
#[pyclass(
    name = "RebuildReport",
    module = "macrame",
    frozen,
    eq,
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PyRebuildReport {
    pub(crate) inner: RebuildReport,
}

#[pymethods]
impl PyRebuildReport {
    #[getter]
    fn rows_rebuilt(&self) -> usize {
        self.inner.rows_rebuilt
    }

    /// Drift measured **after** the rebuild, so `0` is the success condition.
    ///
    /// Non-zero here is not "some rows were missed": the rebuild reprojects
    /// from `links`, so residual drift means the projection and the audit
    /// disagree about what current belief is, which is a defect in one of them
    /// rather than in the data.
    #[getter]
    fn drift_after(&self) -> usize {
        self.inner.drift_after
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.RebuildReport rows={} drift_after={}>",
            self.inner.rows_rebuilt, self.inner.drift_after
        )
    }
}

/// One command kind's hold-time statistics.
#[pyclass(name = "KindMetrics", module = "macrame", frozen)]
pub(crate) struct PyKindMetrics {
    inner: KindSnapshot,
}

#[pymethods]
impl PyKindMetrics {
    /// The command kind, as the crate's own name for it — `"assert_edge"`,
    /// `"shadow_rebuild"`, and so on.
    #[getter]
    fn kind(&self) -> &'static str {
        self.inner.kind.as_str()
    }
    /// How many times the actor took the write connection for this kind.
    #[getter]
    fn turns(&self) -> u64 {
        self.inner.turns
    }
    /// Of those, how many exceeded this kind's budget.
    #[getter]
    fn over_budget(&self) -> u64 {
        self.inner.over_budget
    }
    #[getter]
    fn mean(&self) -> std::time::Duration {
        self.inner.mean
    }
    #[getter]
    fn longest(&self) -> std::time::Duration {
        self.inner.longest
    }
    /// Counts per `BUCKET_BOUNDS_MICROS`, plus a final overflow bucket — so this
    /// is one longer than the bounds list.
    ///
    /// The edges are fixed rather than computed, and that is deliberate: a
    /// histogram whose buckets move between builds cannot be compared across
    /// them, and comparison across builds is the only reason to keep the
    /// numbers.
    #[getter]
    fn buckets(&self) -> Vec<u64> {
        self.inner.buckets().to_vec()
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.KindMetrics {} turns={} over_budget={}>",
            self.inner.kind.as_str(),
            self.inner.turns,
            self.inner.over_budget
        )
    }
}

/// What the write actor held the write connection for (§5.10, D-079).
#[pyclass(name = "MetricsSnapshot", module = "macrame", frozen)]
pub(crate) struct PyMetricsSnapshot {
    pub(crate) inner: MetricsSnapshot,
}

#[pymethods]
impl PyMetricsSnapshot {
    /// **The kinds whose holds exceeded their budget.** Empty is the good answer.
    fn violations(&self) -> Vec<PyKindMetrics> {
        self.inner
            .budget_violations()
            .into_iter()
            .map(|k| PyKindMetrics { inner: k.clone() })
            .collect()
    }

    /// Every kind that has been seen at least once.
    ///
    /// Kinds with no turns are dropped rather than reported as zero rows: the
    /// list is evidence of what this process did, and fourteen mostly-empty
    /// entries bury the two that matter.
    #[getter]
    fn kinds(&self) -> Vec<PyKindMetrics> {
        self.inner
            .kinds
            .iter()
            .filter(|k| k.turns > 0)
            .map(|k| PyKindMetrics { inner: k.clone() })
            .collect()
    }

    /// Total turns across all kinds.
    #[getter]
    fn turns(&self) -> u64 {
        self.inner.turns
    }

    /// `(kind, duration)` of the single longest hold, or `None` if nothing ran.
    #[getter]
    fn longest(&self) -> Option<(&'static str, std::time::Duration)> {
        self.inner.longest.map(|(k, d)| (k.as_str(), d))
    }

    /// How many queue-depth samples the means below are drawn from.
    #[getter]
    fn depth_samples(&self) -> u64 {
        self.inner.depth_samples
    }
    /// Mean high-priority queue depth when the actor looked.
    ///
    /// The high queue backing up is the interesting one: it is the queue that
    /// is supposed to be short, because a command on it is what a caller is
    /// waiting for.
    #[getter]
    fn high_depth_mean(&self) -> f64 {
        self.inner.high_depth_mean
    }
    #[getter]
    fn high_depth_max(&self) -> u64 {
        self.inner.high_depth_max
    }
    #[getter]
    fn low_depth_mean(&self) -> f64 {
        self.inner.low_depth_mean
    }
    #[getter]
    fn low_depth_max(&self) -> u64 {
        self.inner.low_depth_max
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.MetricsSnapshot turns={} violations={}>",
            self.inner.turns,
            self.inner.budget_violations().len()
        )
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRebuildReport>()?;
    m.add_class::<PyKindMetrics>()?;
    m.add_class::<PyMetricsSnapshot>()?;
    // The histogram's edges, in microseconds. Exposed so a caller plotting
    // `KindMetrics.buckets` can label the axis without hard-coding it — and so
    // the off-by-one (one more bucket than bound, for the overflow) is checkable
    // rather than folklore.
    m.add("BUCKET_BOUNDS_MICROS", BUCKET_BOUNDS_MICROS)?;
    Ok(())
}
