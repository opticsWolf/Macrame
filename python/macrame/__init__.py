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

Timestamps
----------
Pass a canonical string or an **aware** ``datetime``; naive datetimes are
refused rather than assumed to be UTC. Timestamps come back as aware UTC
``datetime`` objects.

An **open interval is ``None``**, in both directions::

    macrame.EdgeAssertion("a", "b", "LINKS", valid_from=t0)          # open
    macrame.EdgeAssertion("a", "b", "LINKS", valid_from=t0, valid_to=None)

``datetime`` cannot safely carry the stored sentinel — it is exactly
``datetime.max``, and ``.astimezone()`` overflows for any zone east of UTC —
so ``None`` is what an unbounded end looks like here. `OPEN` is the stored
string, for callers who need to name it. Sorting a mixed column needs
``key=lambda r: (r.valid_to is None, r.valid_to)``.

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

The `Database` surface is complete: writes, traversals and `load_subgraph`, the
temporal surface, vector and hybrid search, integrity repair, and actor metrics.
Type stubs ship alongside it and `py.typed` is set, so a checker sees the whole
signature — including which arguments take an aware ``datetime`` and which
return one. The reasoning behind each decision is in
``docs/Macrame Python Bindings Plan v0.7.0.md`` and §14 of the architecture set.

A `Subgraph` is an opaque handle, not a dict: it answers `degree()`,
`out_edges()`, `dijkstra()` and the rest without copying itself into Python,
because it is the one thing the ledger materialises under a byte budget.
Call `.to_dict()` when you want the copy.
"""

from __future__ import annotations

import os as _os

from ._macrame import (
    BUCKET_BOUNDS_MICROS,
    BULK_ATOMIC_WARN_HOLD,
    CHAIN_CHECK_SAMPLE_LIMIT,
    OPEN,
    RRF_K,
    Annotation,
    ArchiveReport,
    ArchiveViolationError,
    ArchiveWindowError,
    AttributeMode,
    AttributeModeUnstatedError,
    BudgetError,
    ChainCheck,
    ConceptUpsert,
    CurrentDriftError,
    Database,
    DiagnosticConnError,
    DimMismatchError,
    EdgeAssertion,
    EdgeRef,
    EngineError,
    FilterStrategy,
    IntegrityError,
    Interval,
    InvalidEdgeTypeError,
    InvalidIdError,
    InvalidModelNameError,
    InvalidTimestampError,
    KindMetrics,
    MacrameClosedError,
    MacrameError,
    MaterializedState,
    MetricsSnapshot,
    MigrationError,
    ModelNotRegisteredError,
    NegativeEdgeWeightError,
    NodeAttributes,
    NodeData,
    NotFoundError,
    OverlappingIntervalError,
    PayloadVersionError,
    RebuildFailedError,
    RebuildInterruptedError,
    RebuildReport,
    RecordedAtRegressionError,
    ReplayCorruptError,
    SingleOpenViolationError,
    SnapshotIncompatibleError,
    Subgraph,
    SubgraphTooLargeError,
    TemporalError,
    VectorHit,
    CostEstimate,
    ValidationError,
    VectorError,
    WriterDroppedResponderError,
    WriterError,
    WriterStoppedError,
    WriterUnavailableError,
    __version__,
    chunk_budget_ms,
    engine_linked,
    estimate_bulk_hold,
)

__all__ = [
    # handle
    "Database",
    # value types
    "ConceptUpsert",
    "EdgeAssertion",
    "Annotation",
    "Interval",
    "AttributeMode",
    "OPEN",
    # read path (P4.2)
    "Subgraph",
    "NodeAttributes",
    "NodeData",
    "EdgeRef",
    # temporal (P4.3)
    "MaterializedState",
    "ArchiveReport",
    "ChainCheck",
    "CHAIN_CHECK_SAMPLE_LIMIT",
    # vector (P4.4)
    "VectorHit",
    "FilterStrategy",
    "CostEstimate",
    "RRF_K",
    # integrity and metrics (P4.5, P4.6)
    "RebuildReport",
    "MetricsSnapshot",
    "KindMetrics",
    "BUCKET_BOUNDS_MICROS",
    # write-path budgeting
    "estimate_bulk_hold",
    "BULK_ATOMIC_WARN_HOLD",
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
