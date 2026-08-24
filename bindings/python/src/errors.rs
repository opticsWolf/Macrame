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

use macrame::{BulkInterrupted, DbError};

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
     / `valid_to`, the `existing_from` / `existing_to` it collides with, and \
     `within_batch`. Both intervals are reported because neither alone \
     identifies the conflict.\n\n\
     `within_batch` says which guard raised this. False means the other \
     interval is a committed row and can be queried. True means it is another \
     edge in the same `write_bulk_atomic` call, refused before the transaction \
     opened — nothing named is in the database, and nothing named will be."
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
create_exception!(
    macrame,
    FutureRecordedAtError,
    IntegrityError,
    "The newest `recorded_at` in the database is from the future, so opening \
     would inherit it. Attributes: `stamp`, `limit`.\n\n\
     The clock floors itself at `MAX(recorded_at)` so stamps stay strictly \
     increasing across restarts. One row from the future therefore becomes \
     this process's floor, every stamp it issues lands at or after it, and \
     those rows are written — so the next open reads the same floor back. It \
     is the one bad value in the file that manufactures more of itself, which \
     is why this refuses the whole database rather than an operation.\n\n\
     `Database.open(path, future_stamps=\"allow\")` opens it so it can be \
     read. That is not a repair: writes made under it inherit the floor."
);
create_exception!(
    macrame,
    ArchiveSessionLeakedError,
    IntegrityError,
    "The archive-session marker table is present as committed state, which \
     disarms the three delete guards and silences the concept log-insert \
     trigger. Attribute: `marker`.\n\n\
     Grouped with the integrity errors rather than beside \
     `ArchiveViolationError`, which is temporal: that one is the guard \
     refusing a delete, which is the ledger's invariants holding. This is \
     those invariants being unenforced. The message names the remedy — the \
     fix is a single `DROP TABLE` — and an audit of deletions and missing log \
     rows since the table appeared."
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
     `as_of_valid(t)` or `as_of_recorded(t)` fixes the *topology*; node \
     attributes are a second, independent question whose default answer is live \
     text. Pass `AttributeMode.AT_TIME` for the past's text, or \
     `AttributeMode.CURRENT` to confirm live text was meant. This used to be a \
     log warning, which is invisible without a subscriber; it raises on purpose."
);
create_exception!(
    macrame,
    RecordedInstantUnreachableError,
    TemporalError,
    "`as_of_recorded` named an instant the hot log can no longer answer for. \
     Attribute: `ts`.\n\n\
     A transaction-time traversal folds `transaction_log`, and `archive()` \
     removes superseded rows from it. A traversal takes a connection, not an \
     archive path, so it cannot go and get what was moved. Call \
     `reconstruct(ts, archive_path=...)`, which can.\n\n\
     Conservative by one bit: the test is whether *anything* was ever archived, \
     not whether this instant survived it, because the archive cutoff is not \
     recorded hot-side. So an archived database refuses instants a fold might \
     have got right — the alternative is answering from a partial fold, which \
     returns nearly the right topology, and on a ledger that is worse."
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
    SnapshotCorruptError,
    TemporalError,
    "A damaged snapshot file. Attributes: `path`, `reason`.\n\n\
     Three siblings, three subjects (0.13.12, W8.2). \
     `SnapshotIncompatibleError` means another build wrote it and is ordinary \
     after an upgrade; `ReplayCorruptError` means the *ledger* is damaged, \
     which is the worst thing this library can say; this one means the cache \
     is damaged and the ledger is not. Deleting the file restores correctness \
     and costs a slower reconstruction."
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

// -- cancellation -----------------------------------------------------------

create_exception!(
    macrame,
    BulkCancelledError,
    MacrameError,
    "A chunked bulk write stopped because a `CancelToken` handed to it was \
     cancelled (0.13.8).\n\n\
     Not a fault, and the only Macrame exception a caller raises on purpose. \
     Nothing rolled back: the chunks that committed before the token was seen \
     are committed, and `written` says how many rows those were.\n\n\
     It is deliberately *not* an `IntegrityError` — nothing about the ledger is \
     wrong — and deliberately not `KeyboardInterrupt` or `CancelledError`, \
     which mean something specific to the interpreter and to asyncio \
     respectively and are not what happened."
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
///
/// # Every exception leaves here with a `written` attribute (0.13.9, D-182)
///
/// `None` by default, meaning *partial application is not a concept on this
/// path* — which is true of every path but the four chunked writes.
/// [`to_py_bulk`] overwrites it with the real count on those.
///
/// It is set **here**, in the one place every mapped exception passes through,
/// rather than in the arms that need it. Rust states this in the type system —
/// the four chunked methods return `BulkResult<usize>` and nothing else does —
/// and Python has no such type, so the attribute has to be uniform or a caller
/// writing `except MacrameError as e: log(e.written)` gets an `AttributeError`
/// raised *inside their except block*, replacing the diagnostic they were
/// trying to record. `getattr(e, "written", None)` does not fix that: it cannot
/// tell "the attribute is missing" from "this path cannot partially apply",
/// and those are different answers to the caller's real question.
fn raise<T, F>(py: Python<'_>, message: String, set: F) -> PyErr
where
    T: PyTypeInfo,
    F: FnOnce(&Bound<'_, PyAny>) -> PyResult<()>,
{
    let ty: Bound<'_, PyType> = T::type_object(py);
    match ty.call1((message,)) {
        Ok(instance) => {
            let _ = instance.setattr("written", py.None());
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

/// Convert a *chunked* bulk failure, attaching `written` (0.13.8, W7.6, D-181).
///
/// The exception class is still chosen by the cause, so `except NotFoundError`
/// keeps catching a missing concept whether it came from `upsert_concept` or
/// from the middle of a 20,000-row `write_concepts`. What this adds is the
/// count, replacing the `None` [`raise`] leaves on every exception with the
/// number of rows the chunks before the stop committed.
///
/// `None` and not `0` for the paths that never reach here (0.13.9, D-182): `0`
/// already means something on a chunked path — *the first chunk failed* — so
/// reusing it for *there are no chunks* would make `e.written == 0` ambiguous
/// between two different execution models. `e.written is not None` is the test
/// for "is this database in a partial state", and it has to stay exact.
pub(crate) fn to_py_bulk(err: BulkInterrupted) -> PyErr {
    Python::attach(|py| {
        let e = build(py, err.cause);
        let _ = e.value(py).setattr("written", err.written);
        e
    })
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

        // No `written` here: `raise` defaults it and `to_py_bulk` fills it in
        // (0.13.9, D-182). Only the chunk loop can produce this variant and it
        // always arrives inside a `BulkInterrupted`, so the count is real by
        // the time a caller sees one. The exception is `_raise_db_error`, which
        // constructs the variant directly for the mapping tests and has no
        // batch behind it -- `None` is the honest answer there.
        DbError::BulkCancelled => raise::<BulkCancelledError, _>(py, m, |_| Ok(())),

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

        DbError::SnapshotCorrupt { path, reason } => raise::<SnapshotCorruptError, _>(py, m, |e| {
            e.setattr("path", path)?;
            e.setattr("reason", reason)
        }),

        DbError::PayloadVersion { got, max } => raise::<PayloadVersionError, _>(py, m, |e| {
            e.setattr("got", got)?;
            e.setattr("max", max)
        }),

        DbError::ArchiveViolation { table } => {
            raise::<ArchiveViolationError, _>(py, m, |e| e.setattr("table", table))
        }

        DbError::ArchiveSessionLeaked { marker } => {
            raise::<ArchiveSessionLeakedError, _>(py, m, |e| e.setattr("marker", marker))
        }

        // Two attributes and not one, both always present (0.13.10, W7.7,
        // D-183). `as_of` named a method removed in 0.12.17 and said nothing
        // about which clock the caller had asked about; a Python caller who
        // passed `as_of_recorded=` got an attribute called `as_of` back.
        // Whichever axis was not stated is `None`, which reads the same way it
        // does on the traversal call that produced this.
        DbError::AttributeModeUnstated { instants } => {
            raise::<AttributeModeUnstatedError, _>(py, m, |e| {
                e.setattr("as_of_valid", instants.valid())?;
                e.setattr("as_of_recorded", instants.recorded())
            })
        }

        DbError::RecordedInstantUnreachable { ts } => {
            raise::<RecordedInstantUnreachableError, _>(py, m, |e| e.setattr("ts", ts))
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
                e.setattr("existing_to", overlap.existing_to)?;
                e.setattr("within_batch", overlap.within_batch)
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

        DbError::FutureRecordedAt { stamp, limit } => {
            raise::<FutureRecordedAtError, _>(py, m, |e| {
                e.setattr("stamp", stamp)?;
                e.setattr("limit", limit)
            })
        }
    }
}

/// The error for touching a closed handle.
///
/// The one Macrame exception that does not come from a [`DbError`], and so the
/// one that does not pass through [`raise`]. It carries `written = None` for
/// the same reason everything else does (0.13.9, D-182): a caller inspecting an
/// exception should never have to know which of the two construction sites
/// produced it.
pub(crate) fn closed_error() -> PyErr {
    let err = MacrameClosedError::new_err(
        "this Database is closed. Reopen it with Database.open(path); a closed \
         handle is not reusable, because close() shut down the write actor and \
         wrote the final snapshot.",
    );
    Python::attach(|py| {
        let _ = err.value(py).setattr("written", py.None());
    });
    err
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
        BulkCancelledError,
        // integrity
        OverlappingIntervalError,
        SingleOpenViolationError,
        NegativeEdgeWeightError,
        CurrentDriftError,
        RebuildFailedError,
        RebuildInterruptedError,
        RecordedAtRegressionError,
        FutureRecordedAtError,
        ArchiveSessionLeakedError,
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
        SnapshotCorruptError,
        PayloadVersionError,
        ArchiveViolationError,
        ArchiveWindowError,
        RecordedInstantUnreachableError,
        // writer
        WriterUnavailableError,
        WriterDroppedResponderError,
        WriterStoppedError,
        // budget
        SubgraphTooLargeError,
    );
    Ok(())
}
