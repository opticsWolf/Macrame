//! Test hooks. Underscore-prefixed, absent from `macrame.__all__`, shipped in
//! the wheel on purpose.
//!
//! # Why these are in the released artifact rather than behind a feature
//!
//! A `testing` Cargo feature would mean the wheel that is tested is not the
//! wheel that is published, which is the one property a packaging test exists
//! to establish. These are a handful of functions that construct values and
//! raise; they touch no ledger state and hold no resources.
//!
//! # What this closes, and what it does not
//!
//! P2's completeness is enforced in two places, and neither alone is enough:
//!
//! - **The compiler** guarantees every `DbError` variant *has* a mapping —
//!   `errors::build` is a `match` with no wildcard arm, so a new variant
//!   upstream fails the build.
//! - **This module plus `tests_py/test_errors.py`** guarantees the mapping is
//!   *correct*: that each variant reaches the class it should, under the base
//!   it should, with its fields actually populated. A compiler cannot check
//!   that a `setattr` used the right name.
//!
//! [`DB_ERROR_VARIANTS`] is the seam between the two, and it is the one thing
//! here that is not machine-enforced from the Rust side. The Python test
//! compares it against the variants parsed out of `src/error.rs`, so a variant
//! added upstream and mapped but not sampled fails there.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use macrame::error::Overlap;
use macrame::DbError;

/// Every `DbError` variant, by name.
///
/// Checked against `src/error.rs` by `tests_py/test_errors.py`. Do not curate
/// this list — if a variant is genuinely untestable, the test should say so and
/// skip it by name, where the exemption is visible.
pub(crate) const DB_ERROR_VARIANTS: &[&str] = &[
    "Engine",
    "Migration",
    "InvalidEdgeType",
    "SingleOpenViolation",
    "NotFound",
    "DimMismatch",
    "InvalidModelName",
    "InvalidBranchId",
    "UnknownBranch",
    "BranchExists",
    "ForkPrecedesParent",
    "ModelNotRegistered",
    "SubgraphTooLarge",
    "NegativeEdgeWeight",
    "ReplayCorrupt",
    "SnapshotIncompatible",
    "SnapshotCorrupt",
    "PayloadVersion",
    "ArchiveViolation",
    "AttributeModeUnstated",
    "RecordedInstantUnreachable",
    "HalfLifeWithoutInstant",
    "FutureRecordedAt",
    "DiagnosticConn",
    "ArchiveWindow",
    "InvalidTimestamp",
    "InvalidId",
    "OverlappingInterval",
    "CurrentDrift",
    "RebuildFailed",
    "RebuildInterrupted",
    "WriterUnavailable",
    "WriterDroppedResponder",
    "WriterStopped",
    "RecordedAtRegression",
    "ArchiveSessionLeaked",
    "BulkCancelled",
];

#[pyfunction]
pub(crate) fn _db_error_variants() -> Vec<&'static str> {
    DB_ERROR_VARIANTS.to_vec()
}

/// Raise the Python exception a given `DbError` variant maps to.
///
/// The sample values are deliberately distinctive rather than plausible —
/// `"src-1"`, `4242`, `"2026-02-03T04:05:06.000007Z"` — so an assertion that a
/// field arrived can distinguish "the right field" from "a field".
#[pyfunction]
pub(crate) fn _raise_db_error(name: &str) -> PyResult<()> {
    let err = sample(name).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown DbError variant {name:?}; known: {DB_ERROR_VARIANTS:?}"
        ))
    })?;
    Err(crate::errors::to_py(err))
}

fn sample(name: &str) -> Option<DbError> {
    Some(match name {
        "Engine" => DbError::Engine(libsql::Error::ConnectionFailed("sample".into())),
        "Migration" => DbError::Migration {
            to: 42,
            reason: "sample-reason".into(),
        },
        "InvalidEdgeType" => DbError::InvalidEdgeType("bad-type".into()),
        "SingleOpenViolation" => DbError::SingleOpenViolation {
            source_id: "src-1".into(),
            target_id: "tgt-1".into(),
            edge_type: "LINKS".into(),
        },
        "NotFound" => DbError::NotFound("missing-1".into()),
        "DimMismatch" => DbError::DimMismatch {
            got: 7,
            expected: 768,
            model: "nomic_v1".into(),
        },
        "InvalidModelName" => DbError::InvalidModelName("Bad-Model".into()),
        // A trailing space, which is the pair of names this type exists for:
        // invisible in every terminal, and a second lineage in the ledger.
        "InvalidBranchId" => DbError::InvalidBranchId("release ".into()),
        "UnknownBranch" => DbError::UnknownBranch("ghost".into()),
        "BranchExists" => DbError::BranchExists("main".into()),
        "ForkPrecedesParent" => DbError::ForkPrecedesParent {
            branch: "behind".into(),
            parent: "ahead".into(),
            forked_at: "2026-01-01T00:00:00.000000Z".into(),
            parent_forked_at: "2999-01-01T00:00:00.000000Z".into(),
        },
        "ModelNotRegistered" => DbError::ModelNotRegistered {
            model: "nomic_v1".into(),
            table: "embeddings_nomic_v1".into(),
        },
        "SubgraphTooLarge" => DbError::SubgraphTooLarge {
            n: 4242,
            budget: 1000,
        },
        "NegativeEdgeWeight" => DbError::NegativeEdgeWeight {
            source_id: "src-1".into(),
            target_id: "tgt-1".into(),
            weight: -1.5,
        },
        "ReplayCorrupt" => DbError::ReplayCorrupt {
            seq: 4242,
            reason: "sample-reason".into(),
        },
        "SnapshotIncompatible" => DbError::SnapshotIncompatible {
            path: "sample.snap".into(),
            reason: "sample-reason".into(),
        },
        "SnapshotCorrupt" => DbError::SnapshotCorrupt {
            path: "sample.snap".into(),
            reason: "sample-reason".into(),
        },
        "PayloadVersion" => DbError::PayloadVersion { got: 9, max: 2 },
        "ArchiveViolation" => DbError::ArchiveViolation {
            table: "links".into(),
        },
        // Both axes, so the sample proves both attributes cross and exercises
        // the two-instant rendering (0.13.10, D-183).
        "AttributeModeUnstated" => DbError::AttributeModeUnstated {
            instants: macrame::StatedInstants::Both {
                valid: "2026-02-03T04:05:06.000007Z".into(),
                recorded: "2026-04-05T06:07:08.000009Z".into(),
            },
        },
        // No fields: the caller passed one knob too few and the message says
        // which (0.13.20, D-193).
        "HalfLifeWithoutInstant" => DbError::HalfLifeWithoutInstant,
        "RecordedInstantUnreachable" => DbError::RecordedInstantUnreachable {
            ts: "2026-02-03T04:05:06.000007Z".into(),
        },
        "FutureRecordedAt" => DbError::FutureRecordedAt {
            stamp: "2065-02-03T04:05:06.000007Z".into(),
            limit: "2026-02-04T04:05:06.000007Z".into(),
        },
        "DiagnosticConn" => DbError::DiagnosticConn {
            path: "sample.db".into(),
            reason: "sample-reason".into(),
        },
        "ArchiveWindow" => DbError::ArchiveWindow {
            window: std::time::Duration::from_secs(90),
            reason: "sample-reason".into(),
        },
        "InvalidTimestamp" => DbError::InvalidTimestamp {
            value: "not-a-time".into(),
            reason: "sample-reason".into(),
        },
        "InvalidId" => DbError::InvalidId {
            id: "bad|id".into(),
            reason: "sample-reason".into(),
        },
        "OverlappingInterval" => DbError::OverlappingInterval {
            overlap: Box::new(Overlap {
                source_id: "src-1".into(),
                target_id: "tgt-1".into(),
                edge_type: "LINKS".into(),
                valid_from: "2026-03-01T00:00:00.000000Z".into(),
                valid_to: "2026-09-01T00:00:00.000000Z".into(),
                existing_from: "2026-01-01T00:00:00.000000Z".into(),
                existing_to: "2026-06-01T00:00:00.000000Z".into(),
                within_batch: false,
            }),
        },
        "CurrentDrift" => DbError::CurrentDrift { n: 4242 },
        "RebuildFailed" => DbError::RebuildFailed { n: 4242 },
        "RebuildInterrupted" => DbError::RebuildInterrupted {
            reason: "sample-reason".into(),
        },
        "WriterUnavailable" => DbError::WriterUnavailable,
        "WriterDroppedResponder" => DbError::WriterDroppedResponder,
        "WriterStopped" => DbError::WriterStopped("sample-reason".into()),
        "RecordedAtRegression" => DbError::RecordedAtRegression {
            got: "2026-01-01T00:00:00.000000Z".into(),
            had: "2026-06-01T00:00:00.000000Z".into(),
        },
        // The real marker name, not a distinctive placeholder: this message
        // tells the reader to `DROP TABLE <marker>`, so a sample carrying a
        // fake name would make the remedy assertion pass against a string no
        // user could ever run.
        "BulkCancelled" => DbError::BulkCancelled,
        "ArchiveSessionLeaked" => DbError::ArchiveSessionLeaked {
            marker: "macrame_archive_session".into(),
        },
        _ => return None,
    })
}

// -- the injectable clock (W6.3) ---------------------------------------------

/// A [`macrame::prelude::FakeClock`], reachable from Python for tests only.
///
/// # Why this is here and not on the supported surface
///
/// §14.6 lists `open_with_clock` among the things the binding deliberately does
/// not expose, and that entry stands: a clock injected into a production ledger
/// writes a `recorded_at` axis that no longer records anything. What the entry
/// did *not* weigh is the cost on the other side — `tests_py` could not assert
/// on `recorded_at` at all, which is defect K's exact shape on the half that
/// never received D-062's fix.
///
/// The resolution is the same one this module already applies to
/// `_raise_db_error`: underscore-prefixed, absent from `macrame.__all__` and
/// from the stub's public surface, shipped in the wheel because a `testing`
/// feature would mean the tested wheel is not the published one. What is
/// exposed is a **fake**, not the `Clock` trait — a caller cannot supply their
/// own implementation, so the objection's subject does not exist here.
///
/// # It is not fully deterministic, and that is the crate's contract
///
/// On a database that already holds rows, `open_tuned` raises the clock to
/// their newest `recorded_at` before the actor starts ([`Clock::raise_floor`]).
/// Reopening a populated file with a clock set to the epoch would otherwise
/// abort the first concept write on `trg_concepts_monotonic_ra`. Tests wanting
/// exact stamps start from an empty file.
#[pyclass(name = "_FakeClock", module = "macrame")]
pub(crate) struct PyFakeClock {
    pub(crate) inner: std::sync::Arc<macrame::prelude::FakeClock>,
}

#[pymethods]
impl PyFakeClock {
    /// `initial` is a canonical string or an aware `datetime`, as everywhere.
    #[new]
    fn new(initial: &Bound<'_, PyAny>) -> PyResult<Self> {
        let canonical = crate::timestamps::to_canonical(Some(initial))?;
        let at = macrame::util::parse_iso8601_utc(&canonical).map_err(crate::errors::to_py)?;
        Ok(Self {
            inner: std::sync::Arc::new(macrame::prelude::FakeClock::new(at)),
        })
    }

    /// Move the clock forward by a `timedelta` or a number of seconds.
    ///
    /// Forward only. A fake that can go backwards would let a test set up a
    /// state the monotonic triggers exist to make unreachable, and then assert
    /// something about it.
    fn advance(&self, by: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.advance(crate::database::to_duration(by)?);
        Ok(())
    }

    /// The stamp this clock would issue next, without issuing it.
    fn peek<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        crate::timestamps::from_canonical(py, &self.inner.peek())
    }

    fn __repr__(&self) -> String {
        format!("_FakeClock(next={})", self.inner.peek())
    }
}
