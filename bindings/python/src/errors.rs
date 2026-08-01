//! The exception hierarchy (P2).
//!
//! `DbError` has **27 variants** — the plan said 24, and undercounting it is
//! part of why this is worth doing carefully — and the crate spent several
//! releases making them specific. `DiagnosticConn` exists rather than
//! `NotFound` because an error naming the wrong subject sends a caller to fix
//! the wrong thing (D-069); the same argument produced `InvalidTimestamp`
//! against `ReplayCorrupt`, `InvalidId` against `NotFound`, and
//! `RebuildInterrupted` against `RebuildFailed`. Rendering all of that onto one
//! class with a string — which is what P1 shipped as a placeholder — discards
//! the most deliberate work in the crate at the last possible moment.
//!
//! So every variant gets its own class, and every field becomes an attribute.
//! `str(e)` is still exactly the `#[error]` rendering, so a caller who only
//! wants the sentence loses nothing.
//!
//! # Completeness is enforced by the compiler, not by a test
//!
//! [`build`] is a `match` over `DbError` **with no wildcard arm**. Adding a
//! variant upstream therefore fails to compile `macrame-py`, at the exact line
//! that needs a decision, before any wheel is built. That is strictly stronger
//! than the rule-enforcement test this project would otherwise reach for: a
//! test can only run after the thing exists, and the failure mode being
//! guarded against — a new variant quietly falling through to the base class —
//! is one a wildcard arm would make invisible.
//!
//! `tests_py/test_errors.py` still exists, and it checks the half a compiler
//! cannot: that the classes are reachable from `macrame`, that the hierarchy is
//! what it claims, and that the attributes are actually populated.
//!
//! # The hierarchy
//!
//! Intermediate classes are grouping, and several are never raised directly.
//! That is deliberate — `except macrame.TemporalError` should catch a corrupt
//! replay and an unusable archive window without naming either.
//!
//! ```text
//! MacrameError
//! ├── EngineError, MigrationError, NotFoundError, DiagnosticConnError, MacrameClosedError
//! ├── IntegrityError    ── overlapping intervals, drift, rebuild, recorded_at, weights
//! ├── ValidationError   ── edge types, ids, timestamps, model names, attribute mode
//! ├── VectorError       ── dimensions, unregistered models
//! ├── TemporalError     ── replay, snapshots, payload versions, archive
//! ├── WriterError       ── the write actor
//! └── BudgetError       ── subgraph size
//! ```

use pyo3::prelude::*;
use pyo3::types::PyType;
use pyo3::{create_exception, PyErr, PyTypeInfo};

use macrame::DbError;

// The module name is `macrame`, not `_macrame`: it is what shows up in a
// traceback and in `repr(exc)`, and the extension module is an implementation
// detail the package re-exports from.
create_exception!(
    macrame,
    MacrameError,
    pyo3::exceptions::PyException,
    "Base class for every error raised by Macrame.\n\n\
     `str(e)` is the ledger's own error rendering. Structured fields are set as \
     attributes on the subclasses."
);

create_exception!(
    macrame,
    MacrameClosedError,
    MacrameError,
    "A method was called on a Database that has already been closed.\n\n\
     No `DbError` counterpart: in Rust `Database::close` consumes the handle, so \
     the type system removes the possibility. Python cannot express that, so the \
     same guarantee is enforced at runtime."
);

// -- grouping bases ---------------------------------------------------------

create_exception!(
    macrame,
    IntegrityError,
    MacrameError,
    "The ledger's own invariants were violated, or found to be violated."
);
create_exception!(
    macrame,
    ValidationError,
    MacrameError,
    "A value handed to the ledger is not one it can accept."
);
create_exception!(
    macrame,
    VectorError,
    MacrameError,
    "An embedding or embedding model problem."
);
create_exception!(
    macrame,
    TemporalError,
    MacrameError,
    "A problem in the transaction-time machinery: replay, snapshots, archive."
);
create_exception!(
    macrame,
    WriterError,
    MacrameError,
    "The single write actor is not able to serve this handle."
);
create_exception!(
    macrame,
    BudgetError,
    MacrameError,
    "An explicit budget was exceeded. Distinct from a failure: the operation was \
     refused before it cost what it would have cost."
);

// -- direct children of MacrameError ---------------------------------------

create_exception!(
    macrame,
    EngineError,
    MacrameError,
    "libSQL reported an error."
);
create_exception!(
    macrame,
    MigrationError,
    MacrameError,
    "A schema migration failed. Attributes: `to`, `reason`."
);
create_exception!(
    macrame,
    NotFoundError,
    MacrameError,
    "A node does not exist. Attribute: `id`.\n\n\
     Means *absent*, not *refused* — see `InvalidIdError` for an id the ledger \
     would not accept, which is a different thing to go and fix."
);
create_exception!(
    macrame,
    DiagnosticConnError,
    MacrameError,
    "A read-only diagnostic connection could not be opened. Attributes: `path`, `reason`."
);

// -- IntegrityError ---------------------------------------------------------

create_exception!(
    macrame,
    OverlappingIntervalError,
    IntegrityError,
    "Two valid-time intervals for one relationship claim the same instant.\n\n\
     Attributes: `source_id`, `target_id`, `edge_type`, the asserted `valid_from` \
     / `valid_to`, and the stored `existing_from` / `existing_to`. Both intervals \
     are reported because neither alone identifies the conflict."
);
create_exception!(
    macrame,
    SingleOpenViolationError,
    IntegrityError,
    "The relationship already has an open interval; retire it first. \
     Attributes: `source_id`, `target_id`, `edge_type`."
);
create_exception!(
    macrame,
    NegativeEdgeWeightError,
    IntegrityError,
    "An edge weight shortest-path analytics cannot use. \
     Attributes: `source_id`, `target_id`, `weight`.\n\n\
     An integrity error, not a validation one, and the distinction matters: it is \
     raised at *load* time about data already stored, so a caller who reads it as \
     'your input was bad' looks in the wrong place."
);
create_exception!(
    macrame,
    CurrentDriftError,
    IntegrityError,
    "The materialized current-belief table diverges from the ledger. Attribute: `n`."
);
create_exception!(
    macrame,
    RebuildFailedError,
    IntegrityError,
    "A rebuild ran and did not repair the drift. Attribute: `n`.\n\n\
     A reason to distrust the table. Contrast `RebuildInterruptedError`."
);
create_exception!(
    macrame,
    RebuildInterruptedError,
    IntegrityError,
    "A chunked rebuild was abandoned before it could be swapped in. Attribute: `reason`.\n\n\
     The repair **did not run**. `links_current` is untouched and whatever was \
     true of it before is still true. The action is to retry."
);
create_exception!(
    macrame,
    RecordedAtRegressionError,
    IntegrityError,
    "A concept update carried a `recorded_at` that does not advance. \
     Attributes: `got`, `had`."
);

// -- ValidationError --------------------------------------------------------

create_exception!(
    macrame,
    InvalidEdgeTypeError,
    ValidationError,
    "An edge type outside `[A-Z0-9]+`. Attribute: `edge_type`."
);
create_exception!(
    macrame,
    InvalidIdError,
    ValidationError,
    "An identifier the crate's encodings cannot represent. Attributes: `id`, `reason`.\n\n\
     Refused, not missing — contrast `NotFoundError`, which would invite you to \
     create it with the same id and be refused again."
);
create_exception!(
    macrame,
    InvalidTimestampError,
    ValidationError,
    "A timestamp not in canonical form. Attributes: `value`, `reason`."
);
create_exception!(
    macrame,
    InvalidModelNameError,
    ValidationError,
    "An embedding model name that cannot be a SQL identifier. Attribute: `model`."
);
create_exception!(
    macrame,
    AttributeModeUnstatedError,
    ValidationError,
    "A traversal asked about the past without saying which text it wanted. \
     Attribute: `as_of`.\n\n\
     `as_of(t)` fixes the *topology* at `t`; node attributes are a second, \
     independent question whose default answer is live text. Pass \
     `AttributeMode.AT_TIME` for the past's text, or `AttributeMode.CURRENT` to \
     confirm live text was meant. This used to be a log warning, which is \
     invisible without a subscriber; it raises on purpose."
);

// -- VectorError ------------------------------------------------------------

create_exception!(
    macrame,
    DimMismatchError,
    VectorError,
    "An embedding of the wrong length. Attributes: `got`, `expected`, `model`."
);
create_exception!(
    macrame,
    ModelNotRegisteredError,
    VectorError,
    "The embedding model has no table. Attributes: `model`, `table`."
);

// -- TemporalError ----------------------------------------------------------

create_exception!(
    macrame,
    ReplayCorruptError,
    TemporalError,
    "The transaction log could not be folded. Attributes: `seq`, `reason`."
);
create_exception!(
    macrame,
    SnapshotIncompatibleError,
    TemporalError,
    "A snapshot this build cannot read. Attributes: `path`, `reason`.\n\n\
     Distinct from `ReplayCorruptError` on purpose: corruption is a fault, an \
     incompatible snapshot is the ordinary consequence of an upgrade, and the \
     correct response is to discard the file and fold from the log."
);
create_exception!(
    macrame,
    PayloadVersionError,
    TemporalError,
    "A log payload version this build cannot read. Attributes: `got`, `max`."
);
create_exception!(
    macrame,
    ArchiveViolationError,
    TemporalError,
    "A physical delete was attempted outside an archive session. Attribute: `table`."
);
create_exception!(
    macrame,
    ArchiveWindowError,
    TemporalError,
    "An unusable archive window. Attributes: `window` (a `timedelta`), `reason`.\n\n\
     An error rather than a silent clamp: rounding a narrow window up would \
     archive over boundaries the caller did not choose, invisibly."
);

// -- WriterError ------------------------------------------------------------

create_exception!(
    macrame,
    WriterUnavailableError,
    WriterError,
    "The write actor is not running. Reopen the Database."
);
create_exception!(
    macrame,
    WriterDroppedResponderError,
    WriterError,
    "The write actor dropped the response channel mid-request."
);
create_exception!(
    macrame,
    WriterStoppedError,
    WriterError,
    "The write actor did not shut down cleanly. Attribute: `reason`.\n\n\
     Only `close()` can report this, which is one of the two reasons `close()` \
     is not optional."
);

// -- BudgetError ------------------------------------------------------------

create_exception!(
    macrame,
    SubgraphTooLargeError,
    BudgetError,
    "A subgraph exceeded its byte budget. Attributes: `n`, `budget`."
);

/// Build an exception of type `T` carrying `message`, then let `set` attach the
/// structured fields.
///
/// Attribute failures are swallowed: we are already on an error path, and
/// replacing a `NotFoundError` with an `AttributeError` about the machinery
/// that was trying to describe it would be a strictly worse thing to hand a
/// caller.
fn raise<T, F>(py: Python<'_>, message: String, set: F) -> PyErr
where
    T: PyTypeInfo,
    F: FnOnce(&Bound<'_, PyAny>) -> PyResult<()>,
{
    let ty: Bound<'_, PyType> = T::type_object(py);
    match ty.call1((message,)) {
        Ok(instance) => {
            let _ = set(&instance);
            PyErr::from_value(instance)
        }
        // Constructing the exception failed, which should not happen; the
        // original error still has to reach the caller somehow.
        Err(e) => e,
    }
}

/// Convert a ledger error into a Python exception.
///
/// Re-acquires the GIL, because this is called from inside `Python::detach`
/// closures — the whole point of `database.rs`'s `with_db` is that the ledger
/// runs with the GIL released, so the error path is where it comes back. That
/// costs a GIL acquire per raised error and nothing on the success path.
pub(crate) fn to_py(err: DbError) -> PyErr {
    Python::attach(|py| build(py, err))
}

/// The mapping. **No wildcard arm** — see the module docs.
fn build(py: Python<'_>, err: DbError) -> PyErr {
    // Taken before the fields are moved out, so every exception's `str()` is
    // the ledger's own `#[error]` rendering verbatim.
    let m = err.to_string();

    match err {
        DbError::Engine(_) => raise::<EngineError, _>(py, m, |_| Ok(())),

        DbError::Migration { to, reason } => raise::<MigrationError, _>(py, m, |e| {
            e.setattr("to", to)?;
            e.setattr("reason", reason)
        }),

        DbError::InvalidEdgeType(t) => {
            raise::<InvalidEdgeTypeError, _>(py, m, |e| e.setattr("edge_type", t))
        }

        DbError::SingleOpenViolation {
            source_id,
            target_id,
            edge_type,
        } => raise::<SingleOpenViolationError, _>(py, m, |e| {
            e.setattr("source_id", source_id)?;
            e.setattr("target_id", target_id)?;
            e.setattr("edge_type", edge_type)
        }),

        DbError::NotFound(id) => raise::<NotFoundError, _>(py, m, |e| e.setattr("id", id)),

        DbError::DimMismatch {
            got,
            expected,
            model,
        } => raise::<DimMismatchError, _>(py, m, |e| {
            e.setattr("got", got)?;
            e.setattr("expected", expected)?;
            e.setattr("model", model)
        }),

        DbError::InvalidModelName(model) => {
            raise::<InvalidModelNameError, _>(py, m, |e| e.setattr("model", model))
        }

        DbError::ModelNotRegistered { model, table } => {
            raise::<ModelNotRegisteredError, _>(py, m, |e| {
                e.setattr("model", model)?;
                e.setattr("table", table)
            })
        }

        DbError::SubgraphTooLarge { n, budget } => raise::<SubgraphTooLargeError, _>(py, m, |e| {
            e.setattr("n", n)?;
            e.setattr("budget", budget)
        }),

        DbError::NegativeEdgeWeight {
            source_id,
            target_id,
            weight,
        } => raise::<NegativeEdgeWeightError, _>(py, m, |e| {
            e.setattr("source_id", source_id)?;
            e.setattr("target_id", target_id)?;
            e.setattr("weight", weight)
        }),

        DbError::ReplayCorrupt { seq, reason } => raise::<ReplayCorruptError, _>(py, m, |e| {
            e.setattr("seq", seq)?;
            e.setattr("reason", reason)
        }),

        DbError::SnapshotIncompatible { path, reason } => {
            raise::<SnapshotIncompatibleError, _>(py, m, |e| {
                e.setattr("path", path)?;
                e.setattr("reason", reason)
            })
        }

        DbError::PayloadVersion { got, max } => raise::<PayloadVersionError, _>(py, m, |e| {
            e.setattr("got", got)?;
            e.setattr("max", max)
        }),

        DbError::ArchiveViolation { table } => {
            raise::<ArchiveViolationError, _>(py, m, |e| e.setattr("table", table))
        }

        DbError::AttributeModeUnstated { as_of } => {
            raise::<AttributeModeUnstatedError, _>(py, m, |e| e.setattr("as_of", as_of))
        }

        DbError::DiagnosticConn { path, reason } => raise::<DiagnosticConnError, _>(py, m, |e| {
            e.setattr("path", path)?;
            e.setattr("reason", reason)
        }),

        // `window` crosses as a `timedelta`. A float of seconds would be the
        // easy thing and the wrong one: the caller passed a duration and gets a
        // duration back, comparable against whatever they computed it from.
        DbError::ArchiveWindow { window, reason } => raise::<ArchiveWindowError, _>(py, m, |e| {
            e.setattr("window", window)?;
            e.setattr("reason", reason)
        }),

        DbError::InvalidTimestamp { value, reason } => {
            raise::<InvalidTimestampError, _>(py, m, |e| {
                e.setattr("value", value)?;
                e.setattr("reason", reason)
            })
        }

        DbError::InvalidId { id, reason } => raise::<InvalidIdError, _>(py, m, |e| {
            e.setattr("id", id)?;
            e.setattr("reason", reason)
        }),

        // Flattened rather than nested behind an `.overlap` object, which is
        // what the plan proposed. The seven fields carry their own distinction
        // in their names — `valid_*` is what the caller asserted, `existing_*`
        // is what it collided with — so a wrapper type would add a hop without
        // adding information, and `e.source_id` is what a Python caller reaches
        // for first.
        DbError::OverlappingInterval { overlap } => {
            raise::<OverlappingIntervalError, _>(py, m, |e| {
                e.setattr("source_id", overlap.source_id)?;
                e.setattr("target_id", overlap.target_id)?;
                e.setattr("edge_type", overlap.edge_type)?;
                e.setattr("valid_from", overlap.valid_from)?;
                e.setattr("valid_to", overlap.valid_to)?;
                e.setattr("existing_from", overlap.existing_from)?;
                e.setattr("existing_to", overlap.existing_to)
            })
        }

        DbError::CurrentDrift { n } => raise::<CurrentDriftError, _>(py, m, |e| e.setattr("n", n)),

        DbError::RebuildFailed { n } => {
            raise::<RebuildFailedError, _>(py, m, |e| e.setattr("n", n))
        }

        DbError::RebuildInterrupted { reason } => {
            raise::<RebuildInterruptedError, _>(py, m, |e| e.setattr("reason", reason))
        }

        DbError::WriterUnavailable => raise::<WriterUnavailableError, _>(py, m, |_| Ok(())),

        DbError::WriterDroppedResponder => {
            raise::<WriterDroppedResponderError, _>(py, m, |_| Ok(()))
        }

        DbError::WriterStopped(reason) => {
            raise::<WriterStoppedError, _>(py, m, |e| e.setattr("reason", reason))
        }

        DbError::RecordedAtRegression { got, had } => {
            raise::<RecordedAtRegressionError, _>(py, m, |e| {
                e.setattr("got", got)?;
                e.setattr("had", had)
            })
        }
    }
}

/// The error for touching a closed handle.
pub(crate) fn closed_error() -> PyErr {
    MacrameClosedError::new_err(
        "this Database is closed. Reopen it with Database.open(path); a closed \
         handle is not reusable, because close() shut down the write actor and \
         wrote the final snapshot.",
    )
}

/// Register every class on the module.
///
/// Order matters only in that a base must exist before anything reads it; pyo3
/// resolves the inheritance at type-creation time, so this is really just a
/// list. It is grouped to read like the hierarchy.
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    macro_rules! add {
        ($($t:ty),* $(,)?) => {$(
            m.add(stringify!($t), py.get_type::<$t>())?;
        )*};
    }
    add!(
        MacrameError,
        MacrameClosedError,
        // bases
        IntegrityError,
        ValidationError,
        VectorError,
        TemporalError,
        WriterError,
        BudgetError,
        // direct
        EngineError,
        MigrationError,
        NotFoundError,
        DiagnosticConnError,
        // integrity
        OverlappingIntervalError,
        SingleOpenViolationError,
        NegativeEdgeWeightError,
        CurrentDriftError,
        RebuildFailedError,
        RebuildInterruptedError,
        RecordedAtRegressionError,
        // validation
        InvalidEdgeTypeError,
        InvalidIdError,
        InvalidTimestampError,
        InvalidModelNameError,
        AttributeModeUnstatedError,
        // vector
        DimMismatchError,
        ModelNotRegisteredError,
        // temporal
        ReplayCorruptError,
        SnapshotIncompatibleError,
        PayloadVersionError,
        ArchiveViolationError,
        ArchiveWindowError,
        // writer
        WriterUnavailableError,
        WriterDroppedResponderError,
        WriterStoppedError,
        // budget
        SubgraphTooLargeError,
    );
    Ok(())
}
