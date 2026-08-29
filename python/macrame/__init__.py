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
    CHUNK_ROWS_ANNOTATIONS,
    CHUNK_ROWS_CONCEPTS,
    CHUNK_ROWS_EDGES,
    CHUNK_ROWS_EMBEDDINGS,
    MAX_ARCHIVE_SESSIONS,
    OPEN,
    RRF_K,
    Annotation,
    ArchiveReport,
    RehydrateReport,
    ArchiveSessionLeakedError,
    ArchiveViolationError,
    ArchiveWindowError,
    RecordedInstantUnreachableError,
    FutureRecordedAtError,
    AttributeMode,
    AttributeModeUnstatedError,
    HalfLifeWithoutInstantError,
    BudgetError,
    BulkCancelledError,
    CancelToken,
    ChainCheck,
    CheckpointReport,
    ConceptUpsert,
    CurrentDriftError,
    Database,
    Branch,
    BranchError,
    BranchExistsError,
    BranchMismatchError,
    CrossLineageError,
    DiagnosticConnError,
    DimMismatchError,
    ForkPrecedesParentError,
    EdgeAssertion,
    EdgeRef,
    EngineError,
    FilterStrategy,
    IntegrityError,
    Interval,
    InvalidEdgeTypeError,
    InvalidBranchIdError,
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
    SnapshotCorruptError,
    SnapshotIncompatibleError,
    Subgraph,
    SubgraphTooLargeError,
    TemporalError,
    UnknownBranchError,
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
    # bulk control (W7.6)
    "CancelToken",
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
    # lineage (W12.7)
    "Branch",
    # temporal (P4.3)
    "MaterializedState",
    "ArchiveReport",
    "RehydrateReport",
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
    "CheckpointReport",
    "KindMetrics",
    "BUCKET_BOUNDS_MICROS",
    # write-path budgeting
    "estimate_bulk_hold",
    "BULK_ATOMIC_WARN_HOLD",
    "CHUNK_ROWS_EDGES",
    "CHUNK_ROWS_CONCEPTS",
    "CHUNK_ROWS_ANNOTATIONS",
    "CHUNK_ROWS_EMBEDDINGS",
    "MAX_ARCHIVE_SESSIONS",
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
    "BulkCancelledError",
    "SingleOpenViolationError",
    "NegativeEdgeWeightError",
    "CurrentDriftError",
    "RebuildFailedError",
    "RebuildInterruptedError",
    "RecordedAtRegressionError",
    "ArchiveSessionLeakedError",
    # validation
    "InvalidEdgeTypeError",
    "InvalidIdError",
    "InvalidTimestampError",
    "InvalidModelNameError",
    "InvalidBranchIdError",
    "AttributeModeUnstatedError",
    "HalfLifeWithoutInstantError",
    # vector
    "DimMismatchError",
    "ModelNotRegisteredError",
    # temporal
    "ReplayCorruptError",
    "SnapshotCorruptError",
    "SnapshotIncompatibleError",
    "PayloadVersionError",
    "ArchiveViolationError",
    "ArchiveWindowError",
    "RecordedInstantUnreachableError",
    "FutureRecordedAtError",
    # branch
    "BranchError",
    "UnknownBranchError",
    "BranchExistsError",
    "ForkPrecedesParentError",
    "CrossLineageError",
    "BranchMismatchError",
    # view
    "BranchView",
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


class BranchView:
    """One lineage's handle on the ledger (§15.4, 0.14.9).

    A `Database` plus a branch name, so a caller who forked reads and writes
    through the fork instead of passing ``branch=`` at every call::

        alt = db.fork("turn/17/alt/1", "main")
        view = macrame.BranchView(db, alt.id)

        view.assert_edge(macrame.EdgeAssertion("a", "b", "CITES", valid_from=t0))
        view.traverse_ids("a", max_depth=5)

    Every method here is the `Database` method of the same name with ``branch=``
    filled in, so **the view buys ergonomics and no capability**. Operations
    that are properties of the *file* rather than of a lineage — ``archive``,
    ``checkpoint``, ``verify``, ``close`` — stay on the handle and are reached
    through `database`.

    Written in Python rather than in the extension, deliberately
    -----------------------------------------------------------
    The Rust `BranchView` wraps an ``Arc<Database>``, and that `Arc` is the
    point: ``Database::close`` takes ``self`` by value, so a view there
    *cannot* end the handle it reads through — the restriction is structural.
    Python has no move semantics to build that guarantee out of. ``close()`` is
    a method on the `Database` object the caller already holds, and no wrapper
    can take it away, so a Python view offers the convenience and **not** the
    guarantee. Implementing it here rather than duplicating the delegation in
    pyo3 makes that honest and keeps the two surfaces from drifting: each method
    below passes ``branch=`` through to the binding and does nothing else.

    This is the second deliberate asymmetry in the branch surface, after
    `BranchId` having no Python class. It is also why there is no
    ``db.view(...)``: in Rust that method exists to clone the `Arc`, and here
    there is no `Arc` to clone.

    A write that names another lineage
    ----------------------------------
    An assertion carrying no ``branch`` is stamped with this view's. One
    carrying a *different* branch raises `BranchMismatchError` rather than being
    relabelled — it is evidence the caller believed something about where the
    write was going. The failure that motivates it is holding two views and
    passing one's assertion to the other.
    """

    __slots__ = ("_db", "_id")

    def __init__(self, database: Database, branch: str) -> None:
        self._db = database
        self._id = str(branch)

    # -- identity ----------------------------------------------------------

    @property
    def id(self) -> str:
        """The lineage this view reads and writes."""
        return self._id

    @property
    def database(self) -> Database:
        """The handle underneath, for what is not lineage-scoped."""
        return self._db

    def __repr__(self) -> str:
        return f"BranchView(branch={self._id!r}, path={self._db.path!r})"

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, BranchView):
            return NotImplemented
        return self._id == other._id and self._db is other._db

    def __hash__(self) -> int:
        return hash((self._id, id(self._db)))

    # -- writes ------------------------------------------------------------

    def _claim(self, item):
        """Stamp this lineage on an unnamed write; refuse a foreign one."""
        named = item.branch
        if named is None:
            return item.on_branch(self._id)
        if named != self._id:
            err = BranchMismatchError(
                f"view of branch {self._id} was handed a write naming {named}"
            )
            err.view = self._id
            err.named = named
            raise err
        return item

    def assert_edge(self, edge: EdgeAssertion) -> None:
        """`Database.assert_edge` on this lineage."""
        self._db.assert_edge(self._claim(edge))

    def retire_edge(
        self,
        source: str,
        target: str,
        edge_type: str,
        valid_from,
        valid_to,
    ) -> None:
        """`Database.retire_edge` on this lineage.

        An inherited edge is retired by writing this lineage's **own** closed
        row at the ancestor's key; the parent's row is never touched.
        """
        self._db.retire_edge(
            source, target, edge_type, valid_from, valid_to, branch=self._id
        )

    def upsert_concept(self, concept: ConceptUpsert) -> None:
        """`Database.upsert_concept`, minting on this lineage.

        A branch **inherits** its parent's concepts and may not restate one;
        that raises `CrossLineageError`.
        """
        self._db.upsert_concept(self._claim(concept))

    def write_bulk_atomic(self, edges) -> int:
        """`Database.write_bulk_atomic` with every edge on this lineage."""
        return self._db.write_bulk_atomic([self._claim(e) for e in edges])

    def bulk_import(self, edges) -> int:
        """`Database.bulk_import` with every edge on this lineage."""
        return self._db.bulk_import([self._claim(e) for e in edges])

    def write_concepts(self, concepts) -> int:
        """`Database.write_concepts` with every concept on this lineage."""
        return self._db.write_concepts([self._claim(c) for c in concepts])

    # -- reads -------------------------------------------------------------

    def traverse_ids(self, start_node: str, **kwargs):
        """`Database.traverse_ids` on this lineage."""
        return self._db.traverse_ids(start_node, branch=self._id, **kwargs)

    def traverse(self, start_node: str, **kwargs):
        """`Database.traverse` on this lineage."""
        return self._db.traverse(start_node, branch=self._id, **kwargs)

    def load_subgraph(self, start_node: str, max_hops: int, byte_budget: int, **kwargs):
        """`Database.load_subgraph` on this lineage."""
        return self._db.load_subgraph(
            start_node, max_hops, byte_budget, branch=self._id, **kwargs
        )

    def search_filtered(self, *args, **kwargs):
        """`Database.search_filtered` on this lineage."""
        return self._db.search_filtered(*args, branch=self._id, **kwargs)


def _install_fork_guard() -> None:
    """Make ``os.fork()`` fail loudly instead of hanging.

    **Not** ``Database.fork``, which cuts a lineage and has nothing to do with
    processes. The collision is unfortunate and both names are the right ones
    for their domains; this guard is about the POSIX one.

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
