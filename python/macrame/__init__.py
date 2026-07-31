"""Macrame — a bitemporal graph ledger on libSQL.

The distribution is ``macrame-db`` and the import path is ``macrame``, matching
the crate, which publishes as ``macrame-db`` and is imported as ``macrame``.

Quick start::

    import macrame

    with macrame.Database.open("kb.db") as db:
        print(db.schema_version)

**Use the context manager.** ``close()`` is the only path that writes the final
snapshot and the only one that can report the write actor's exit status; a
handle that is merely garbage-collected loses both, at a time Python chooses.

Errors
------
Every error is a subclass of `MacrameError`, and every one carries its
structured fields as attributes rather than only in the message::

    try:
        ...
    except macrame.OverlappingIntervalError as e:
        print(e.source_id, e.valid_from, e.existing_from)

The intermediate classes — `IntegrityError`, `ValidationError`, `VectorError`,
`TemporalError`, `WriterError`, `BudgetError` — exist to be caught as groups and
are not raised directly.

P1 shipped the handle lifecycle and P2 the error hierarchy. The write, read,
temporal and vector surfaces arrive in P4.x — see
``docs/Macrame Python Bindings Plan v0.7.0.md``.
"""

from __future__ import annotations

import os as _os

from ._macrame import (
    ArchiveViolationError,
    ArchiveWindowError,
    AttributeModeUnstatedError,
    BudgetError,
    CurrentDriftError,
    Database,
    DiagnosticConnError,
    DimMismatchError,
    EngineError,
    IntegrityError,
    InvalidEdgeTypeError,
    InvalidIdError,
    InvalidModelNameError,
    InvalidTimestampError,
    MacrameClosedError,
    MacrameError,
    MigrationError,
    ModelNotRegisteredError,
    NegativeEdgeWeightError,
    NotFoundError,
    OverlappingIntervalError,
    PayloadVersionError,
    RebuildFailedError,
    RebuildInterruptedError,
    RecordedAtRegressionError,
    ReplayCorruptError,
    SingleOpenViolationError,
    SnapshotIncompatibleError,
    SubgraphTooLargeError,
    TemporalError,
    ValidationError,
    VectorError,
    WriterDroppedResponderError,
    WriterError,
    WriterStoppedError,
    WriterUnavailableError,
    __version__,
    chunk_budget_ms,
    engine_linked,
)

__all__ = [
    # handle
    "Database",
    # base and groups
    "MacrameError",
    "MacrameClosedError",
    "IntegrityError",
    "ValidationError",
    "VectorError",
    "TemporalError",
    "WriterError",
    "BudgetError",
    # direct
    "EngineError",
    "MigrationError",
    "NotFoundError",
    "DiagnosticConnError",
    # integrity
    "OverlappingIntervalError",
    "SingleOpenViolationError",
    "NegativeEdgeWeightError",
    "CurrentDriftError",
    "RebuildFailedError",
    "RebuildInterruptedError",
    "RecordedAtRegressionError",
    # validation
    "InvalidEdgeTypeError",
    "InvalidIdError",
    "InvalidTimestampError",
    "InvalidModelNameError",
    "AttributeModeUnstatedError",
    # vector
    "DimMismatchError",
    "ModelNotRegisteredError",
    # temporal
    "ReplayCorruptError",
    "SnapshotIncompatibleError",
    "PayloadVersionError",
    "ArchiveViolationError",
    "ArchiveWindowError",
    # writer
    "WriterUnavailableError",
    "WriterDroppedResponderError",
    "WriterStoppedError",
    # budget
    "SubgraphTooLargeError",
    # module
    "__version__",
    "chunk_budget_ms",
    "engine_linked",
]


def _install_fork_guard() -> None:
    """Make ``fork()`` fail loudly instead of hanging.

    The extension keeps one process-wide tokio runtime. A ``fork()`` child
    inherits that runtime as a struct whose worker threads did *not* come with
    it, so the first database call in the child waits forever on a thread pool
    that does not exist.

    This does not make forking work. It converts a silent hang — the worst
    available outcome — into an exception that names the cause. The supported
    answer is the ``spawn`` start method, already the default on Windows and
    macOS::

        multiprocessing.set_start_method("spawn")

    ``os.register_at_fork`` is POSIX-only, hence the guard; on Windows there is
    no ``fork`` to protect against.
    """
    register = getattr(_os, "register_at_fork", None)
    if register is None:
        return
    from ._macrame import _mark_forked

    register(after_in_child=_mark_forked)


_install_fork_guard()
