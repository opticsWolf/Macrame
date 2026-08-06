"""Type stubs for the compiled extension.

Hand-written, and that is a decision rather than an omission (D-109).
``pyo3-stub-gen`` exists and would add a build step, a proc-macro attribute on
every ``#[pymethods]`` block, and generated output that still needs editing by
hand wherever a signature is more precise in Python than in Rust — which here is
most of the interesting ones. ``Optional[Any]`` is what a generator writes for a
timestamp; ``str | datetime | None`` is what the boundary actually accepts, and
the difference is the whole value of a stub.

What keeps this file honest is not discipline, it is
``tests_py/test_stubs.py``: it compares this file's names against the
extension's ``dir()`` in **both** directions, class by class, member by member.
A method added in Rust and never stubbed fails there, which is the failure this
file would otherwise have.

Conventions, applied throughout:

* **A timestamp going in** is ``str | datetime`` — a canonical
  ``YYYY-MM-DDTHH:MM:SS.ffffffZ`` string, or an *aware* ``datetime``. Naive
  datetimes are refused at runtime; no annotation can express that, so it is
  said here instead.
* **A timestamp coming out** is always an aware UTC ``datetime``.
* **An open interval is ``None``**, never a sentinel datetime — ``OPEN`` is the
  stored string for callers who need to name it, and ``datetime`` cannot carry
  it safely (it is exactly ``datetime.max``, which overflows ``.astimezone()``
  east of UTC).
* **Attributes on exceptions are annotations, not assignments.** They are set on
  the instance by the Rust mapping layer, so they exist on a raised error and on
  nothing else. A type checker needs the declaration; ``hasattr`` on the class
  will still be False.
"""

from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Final, Iterator, Sequence

__version__: str

# A canonical timestamp string, or an aware datetime. Naive datetimes raise.
Timestamp = str | datetime
# Packed little-endian f32 bytes (the fast path), or any sequence of floats.
Embedding = bytes | Sequence[float]
# `(source, target, edge_type, valid_from, valid_to)`, timestamps as datetimes.
Edge = tuple[str, str, str, datetime, datetime]

# ---------------------------------------------------------------- constants ---

#: The stored upper sentinel, `9999-12-31T23:59:59.999999Z`. Returned as `None`
#: by every getter; this is here for callers who compare against raw storage.
OPEN: Final[str]
#: RRF's rank constant. Hybrid fusion scores `1/(k + rank)`.
RRF_K: Final[int]
#: The hold above which `write_bulk_atomic` warns (D-081). A `timedelta`, so an
#: `estimate_bulk_hold` result can be compared against it directly.
BULK_ATOMIC_WARN_HOLD: Final[timedelta]
#: Histogram bucket upper bounds, in microseconds, for `KindMetrics.buckets`.
BUCKET_BOUNDS_MICROS: Final[list[int]]
#: How many disagreements `ChainCheck` collects before it truncates.
CHAIN_CHECK_SAMPLE_LIMIT: Final[int]

# ---------------------------------------------------------------- functions ---

def engine_linked() -> bool:
    """True when a real libSQL engine is linked into this wheel.

    Not a tautology: it opens an in-memory database and executes a statement, so
    a wheel that built and installed with no engine in it answers False rather
    than importing happily and failing at first use.
    """

def chunk_budget_ms() -> int:
    """The chunk budget the write actor holds itself to, in milliseconds."""

def estimate_bulk_hold(edges: Sequence[EdgeAssertion]) -> timedelta:
    """How long `write_bulk_atomic` will hold the write lock for this batch.

    Call it *before* the write. The batch is one transaction under one stamp and
    cannot be chunked, so this is also how long every other writer waits.
    """

# -------------------------------------------------------------- value types ---

class AttributeMode:
    """Which text a temporal traversal returns (T3.2, D-085).

    Leaving it unset on a traversal that also sets `as_of` raises
    `AttributeModeUnstatedError`. `None` is *unstated*, not `CURRENT`, and that
    difference is the whole mechanism: live text attached to a historical
    topology is the wrong answer, delivered silently.

    `OMIT` is deliberately absent from the traversal surface — see D-102.
    """

    CURRENT: Final[AttributeMode]
    AT_TIME: Final[AttributeMode]
    OMIT: Final[AttributeMode]
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class FilterStrategy:
    """Which plan the filtered search took (D-007)."""

    POST_FILTER: Final[FilterStrategy]
    PRE_FILTER_CTE: Final[FilterStrategy]
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class Interval:
    """A half-open valid-time interval, `[valid_from, valid_to)`."""

    def __init__(self, valid_from: Timestamp, valid_to: Timestamp | None = None) -> None: ...
    @property
    def valid_from(self) -> datetime: ...
    @property
    def valid_to(self) -> datetime | None: ...
    def is_open(self) -> bool: ...
    def contains(self, ts: Timestamp) -> bool: ...
    def overlaps(self, other: Interval) -> bool: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class ConceptUpsert:
    """A concept to write. Validated **in this constructor**, not at the write.

    A caller builds a list and hands it to `write_concepts`; validating at the
    write would report one bad value out of ten thousand with a traceback
    pointing at the write, where nothing can be fixed (D-100).
    """

    def __init__(
        self,
        id: str,
        title: str,
        *,
        valid_from: Timestamp,
        content: str = ...,
        embedding_model: str | None = None,
        valid_to: Timestamp | None = None,
        retired: bool = False,
    ) -> None: ...
    @property
    def id(self) -> str: ...
    @property
    def title(self) -> str: ...
    @property
    def content(self) -> str: ...
    @property
    def embedding_model(self) -> str | None: ...
    @property
    def retired(self) -> bool: ...
    @property
    def valid_from(self) -> datetime: ...
    @property
    def valid_to(self) -> datetime | None: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class EdgeAssertion:
    """An edge to assert. `edge_type` must match `[A-Z0-9]+` — no underscores.

    Validated in the constructor (D-100), so an `EdgeAssertion` that exists is
    one the ledger will accept.
    """

    def __init__(
        self,
        source: str,
        target: str,
        edge_type: str,
        *,
        valid_from: Timestamp,
        valid_to: Timestamp | None = None,
        weight: float = 1.0,
        properties: str = ...,
    ) -> None: ...
    @property
    def source(self) -> str: ...
    @property
    def target(self) -> str: ...
    @property
    def edge_type(self) -> str: ...
    @property
    def weight(self) -> float: ...
    @property
    def properties(self) -> str: ...
    @property
    def valid_from(self) -> datetime: ...
    @property
    def valid_to(self) -> datetime | None: ...
    def interval(self) -> Interval: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class Annotation:
    """A derived label to attach to a concept."""

    def __init__(self, concept_id: str, label: str, value: str) -> None: ...
    @property
    def concept_id(self) -> str: ...
    @property
    def label(self) -> str: ...
    @property
    def value(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

# ---------------------------------------------------------------- read path ---

class NodeAttributes:
    """A concept's identity and text, as a traversal returns it."""

    @property
    def id(self) -> str: ...
    @property
    def title(self) -> str: ...
    @property
    def content(self) -> str: ...
    @property
    def embedding_model(self) -> str | None: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class NodeData:
    """A node as a `Subgraph` holds it: text plus the interval it was valid in."""

    @property
    def title(self) -> str: ...
    @property
    def content(self) -> str | None:
        """The document text, or `None` when the load did not fetch it.

        Not loaded by default since 0.8.0 (D-116). `None` means *not loaded*,
        never *empty* — the same refusal of a valid-value sentinel that makes an
        open interval `None` rather than a far-future datetime. Pass
        `content=True` to `load_subgraph` to fetch it.
        """
        ...
    @property
    def embedding_model(self) -> str | None: ...
    @property
    def valid_from(self) -> datetime: ...
    @property
    def valid_to(self) -> datetime | None: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class EdgeRef:
    """One end of an edge, from a `Subgraph`'s point of view."""

    @property
    def node(self) -> str: ...
    @property
    def edge_type(self) -> str: ...
    @property
    def weight(self) -> float: ...
    @property
    def valid_from(self) -> datetime: ...
    @property
    def valid_to(self) -> datetime | None: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class Subgraph:
    """A materialised neighbourhood — an **opaque handle, not a dict** (D-101).

    It is the one thing the ledger materialises under a byte budget, so it
    answers questions about itself rather than copying itself into Python. It is
    also a *value*: loaded at a point in time, and unaffected by anything the
    database does afterwards.

    `to_dict()` is the explicit purchase of the copy.
    """

    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[str]: ...
    def __contains__(self, node: str) -> bool: ...
    def __repr__(self) -> str: ...
    def node(self, node: str) -> NodeData | None: ...
    def out_edges(self, node: str) -> list[EdgeRef]: ...
    def in_edges(self, node: str) -> list[EdgeRef]: ...
    def degree(self, node: str) -> int: ...
    def weighted_degree(self, node: str) -> float: ...
    def total_weight(self) -> float: ...
    def edge_count(self) -> int: ...
    def estimated_bytes(self) -> int: ...
    def is_closed(self) -> bool:
        """True when every edge endpoint is also a node in this subgraph."""

    def to_dict(self) -> dict[str, Any]:
        """A plain-`dict` copy: `{"nodes": {id: NodeData}, "out_adj": …, "in_adj": …}`.

        The node values are `NodeData` objects, not nested dicts, so absent
        content is `NodeData.content is None` rather than a missing key.
        """
    def dijkstra(self, start: str) -> dict[str, float]: ...
    def astar(
        self,
        start: str,
        goal: str,
        heuristic: Callable[[str, str], float] | None = None,
    ) -> tuple[float, list[str]] | None:
        """Shortest path, optionally guided by a Python `heuristic`.

        The one method that does **not** release the GIL (D-104): the callback is
        Python, and re-attaching per expansion costs two transitions a node. A
        heuristic that raises, or returns a `NaN`, an infinity or a non-number,
        is captured and re-raised here — the Rust callback signature cannot
        report failure, so the alternative is a silently wrong path.
        """

    def scc(self) -> list[list[str]]: ...
    def k_core(self, k: int) -> set[str]: ...
    def louvain(self) -> dict[str, int]: ...
    def modularity(self, communities: dict[str, int]) -> float: ...

# ----------------------------------------------------------------- temporal ---

class MaterializedState:
    """The world as believed at an instant."""

    @property
    def timestamp(self) -> datetime: ...
    @property
    def seq_anchor(self) -> int: ...
    @property
    def concepts(self) -> dict[str, NodeAttributes]: ...
    @property
    def edges(self) -> list[Edge]: ...
    @property
    def predates_recorded_history(self) -> bool:
        """Whether nothing had been recorded yet at ``timestamp``.

        An empty state means either that everything had been retired by then or
        that the ledger had not started; both arrive as ``concepts == {}`` and
        ``edges == []``, so the difference is carried here rather than inferred.
        """

    def __repr__(self) -> str: ...

class ArchiveReport:
    """What one archive run moved to cold storage."""

    @property
    def links_archived(self) -> int: ...
    @property
    def concepts_archived(self) -> int: ...
    @property
    def log_entries_archived(self) -> int: ...
    @property
    def horizon(self) -> int | None: ...
    def __repr__(self) -> str: ...

class ChainCheck:
    """Whether snapshot composition agrees with a fold from genesis (D-092).

    Reports; does not repair. A snapshot is derivative under Doctrine VI, so the
    fix is to delete the snapshot directory — and rewriting the file would
    destroy the only evidence that composition has a defect.
    """

    def diverged(self) -> bool:
        """The question worth asking. The two anchors are deliberately not
        comparable directly — they count different things."""

    @property
    def timestamp(self) -> datetime: ...
    @property
    def composed_anchor(self) -> int: ...
    @property
    def folded_anchor(self) -> int: ...
    @property
    def composed_concepts(self) -> int: ...
    @property
    def folded_concepts(self) -> int: ...
    @property
    def composed_edges(self) -> int: ...
    @property
    def folded_edges(self) -> int: ...
    @property
    def concept_disagreements(self) -> list[str]: ...
    @property
    def edge_disagreements(self) -> list[str]: ...
    @property
    def truncated(self) -> bool:
        """True when a list hit `CHAIN_CHECK_SAMPLE_LIMIT` — so its length is a
        sample size, not a count."""

# ------------------------------------------------------------------- vector ---

class VectorHit:
    """One search result. A class rather than a tuple (D-105).

    `vector_rank` and `keyword_rank` are `None` when that arm did not return the
    hit, which is exactly what a hybrid caller needs to know and what a
    `(id, score)` tuple cannot say.
    """

    @property
    def concept_id(self) -> str: ...
    @property
    def score(self) -> float: ...
    @property
    def vector_rank(self) -> int | None: ...
    @property
    def keyword_rank(self) -> int | None: ...
    def __repr__(self) -> str: ...

class CostEstimate:
    """Which plan the filtered search chose, and the bytes it compared."""

    @property
    def strategy(self) -> FilterStrategy: ...
    @property
    def candidates(self) -> int: ...
    @property
    def candidates_capped(self) -> bool:
        """True when the traversal hit `probe_cap`, so `candidates` is the cap
        rather than the neighbourhood's size."""

    @property
    def post_filter_bytes(self) -> int: ...
    @property
    def pre_filter_bytes(self) -> int: ...
    @property
    def k_prime(self) -> int: ...
    def __repr__(self) -> str: ...

# ------------------------------------------------- integrity and the counters ---

class RebuildReport:
    """What a `links_current` rebuild did, and whether drift survived it."""

    @property
    def rows_rebuilt(self) -> int: ...
    @property
    def drift_after(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...
    def __repr__(self) -> str: ...

class KindMetrics:
    """One command kind's counters (D-079, D-093)."""

    @property
    def kind(self) -> str: ...
    @property
    def turns(self) -> int: ...
    @property
    def over_budget(self) -> int: ...
    @property
    def mean(self) -> timedelta: ...
    @property
    def longest(self) -> timedelta: ...
    @property
    def buckets(self) -> list[int]:
        """Counts per `BUCKET_BOUNDS_MICROS` bucket, plus one overflow bucket."""

    def __repr__(self) -> str: ...

class MetricsSnapshot:
    """The write actor's counters at one instant.

    These are **real in the wheel**: it is built with `--features metrics`
    unconditionally, because a Python caller cannot turn on a Cargo feature
    (D-093).
    """

    def violations(self) -> list[KindMetrics]:
        """Kinds that exceeded `CHUNK_BUDGET`. Empty is the expected answer."""

    @property
    def kinds(self) -> list[KindMetrics]: ...
    @property
    def turns(self) -> int: ...
    @property
    def longest(self) -> tuple[str, timedelta] | None: ...
    @property
    def depth_samples(self) -> int: ...
    @property
    def high_depth_mean(self) -> float: ...
    @property
    def high_depth_max(self) -> int: ...
    @property
    def low_depth_mean(self) -> float: ...
    @property
    def low_depth_max(self) -> int: ...
    def __repr__(self) -> str: ...

# ----------------------------------------------------------------- the handle ---

class Database:
    """An open ledger. **Use it as a context manager.**

    `close()` is the only path that writes the final snapshot and the only one
    that can report the write actor's exit status; a handle that is merely
    garbage-collected loses both, at a time Python chooses.

    One process may hold one open handle per path.
    """

    @staticmethod
    def open(
        path: str | Path,
        *,
        snapshot_every_entries: int | None = ...,
        snapshot_poll_seconds: float = 5.0,
    ) -> Database: ...
    def close(self) -> None:
        """Shut down the write actor and write the final snapshot.

        Idempotent, so `__exit__` after an explicit `close()` is fine. Two things
        are lost by never calling it, and only one is obvious: the final snapshot
        (the next `reconstruct` folds from an older anchor — slower, not wrong,
        since a snapshot is derivative under Doctrine VI), and the write actor's
        exit status, which no other method can return.
        """

    def __enter__(self) -> Database: ...
    def __exit__(self, *args: Any) -> None: ...
    def __repr__(self) -> str: ...
    @property
    def is_closed(self) -> bool: ...
    @property
    def path(self) -> Path: ...
    @property
    def archive_path(self) -> Path: ...
    @property
    def snapshots_dir(self) -> Path: ...
    @property
    def schema_version(self) -> int: ...

    # -- writes ----------------------------------------------------------------
    def assert_edge(self, edge: EdgeAssertion) -> None: ...
    def retire_edge(
        self,
        source: str,
        target: str,
        edge_type: str,
        valid_from: Timestamp,
        valid_to: Timestamp,
    ) -> None: ...
    def upsert_concept(self, concept: ConceptUpsert) -> None: ...
    def write_bulk_atomic(self, edges: Sequence[EdgeAssertion]) -> int:
        """Assert many edges in **one transaction under one stamp** (D-014).

        The batch is one act, so it cannot be chunked — splitting it is the thing
        this method exists not to do. The actor's hold is therefore a function of
        `len(edges)`, and every other writer in the process waits that long:
        ~34 ms at 500 rows, ~155 ms at 2,000, ~1.0 s at 10,000, ~2.6 s at 20,000
        (T1.3, D-081). Call `estimate_bulk_hold(edges)` first to find out which.

        A caller who needs the latency bound and not the atomicity wants
        `bulk_import` — the same write, chunked, and explicitly not atomic
        overall.
        """

    def bulk_import(self, edges: Sequence[EdgeAssertion]) -> int: ...
    def write_concepts(self, concepts: Sequence[ConceptUpsert]) -> int: ...
    def write_analytics_annotations(self, annotations: Sequence[Annotation]) -> int: ...

    # -- reads -----------------------------------------------------------------
    def traverse_ids(
        self,
        start_node: str,
        *,
        max_depth: int = 2,
        edge_types: Sequence[str] | None = None,
        min_weight: float = 0.0,
        as_of: Timestamp | None = None,
        now: Timestamp | None = None,
    ) -> list[str]: ...
    def traverse(
        self,
        start_node: str,
        *,
        max_depth: int = 2,
        edge_types: Sequence[str] | None = None,
        min_weight: float = 0.0,
        attribute_mode: AttributeMode | None = None,
        as_of: Timestamp | None = None,
        now: Timestamp | None = None,
    ) -> list[NodeAttributes]:
        """Traverse and hydrate node text.

        `attribute_mode` left unset alongside `as_of` raises
        `AttributeModeUnstatedError` rather than returning live text for a
        historical topology.
        """

    def load_subgraph(
        self,
        start_node: str,
        max_hops: int,
        byte_budget: int,
        *,
        edge_types: Sequence[str] | None = None,
        min_weight: float | None = None,
        now: Timestamp | None = None,
        content: bool = False,
    ) -> Subgraph:
        """Materialise a neighbourhood under a byte budget.

        An unstated `min_weight` is `-inf`, not `0.0` (D-103): this loader is for
        analysis, and a default of zero would silently drop the negative weights
        the algorithms are allowed to see.

        `content=False` leaves `NodeData.content` as `None` (D-116). No algorithm
        on the returned graph reads the text, and at realistic document sizes it
        is most of `byte_budget`, so asking for it is opt-in. The default matches
        the crate's rather than softening it.
        """

    # -- temporal --------------------------------------------------------------
    def reconstruct(self, ts: Timestamp) -> MaterializedState: ...
    def query_as_of_edges(self, ts: Timestamp | None = None) -> list[Edge]: ...
    def archive(self, cutoff: Timestamp) -> ArchiveReport: ...
    def archive_windowed(
        self, cutoff: Timestamp, window: timedelta | float
    ) -> list[ArchiveReport]:
        """Archive in windows, each its own transaction.

        `window` is a `timedelta` or seconds. A window that would not terminate —
        non-positive, non-finite — is **refused, not clamped**.
        """

    def verify_snapshot_chain(self, ts: Timestamp) -> ChainCheck: ...

    # -- vector ----------------------------------------------------------------
    def register_model(self, model: str, dim: int) -> None: ...
    def upsert_embeddings(
        self, model: str, rows: Sequence[tuple[str, Embedding]]
    ) -> int: ...
    def search_vector(
        self, model: str, query: Embedding, *, top_k: int = 10
    ) -> list[VectorHit]: ...
    def keyword_search(
        self, query: str, *, top_k: int = 10, raw: bool = False
    ) -> list[tuple[str, float]]: ...
    def hybrid_search(
        self,
        model: str,
        query_text: str,
        query_vector: Embedding,
        *,
        top_k: int = 10,
        depth: int | None = None,
        rrf_k: int | None = None,
        raw: bool = False,
    ) -> list[VectorHit]: ...
    def search_filtered(
        self,
        model: str,
        query: Embedding,
        start_node: str,
        *,
        max_depth: int = 2,
        edge_types: Sequence[str] | None = None,
        min_weight: float = 0.0,
        top_k: int = 10,
        byte_budget: int | None = None,
        probe_cap: int | None = None,
        strategy: FilterStrategy | None = None,
        now: Timestamp | None = None,
    ) -> tuple[list[VectorHit], CostEstimate]:
        """Vector search restricted to a traversed neighbourhood.

        Returns the hits *and* the plan that produced them: which strategy was
        chosen and the byte estimates it was chosen on.
        """

    def rebuild_fts(self) -> None: ...

    # -- integrity and introspection -------------------------------------------
    def audit_current(self) -> int: ...
    def rebuild_current(self) -> RebuildReport: ...
    def rebuild_current_chunked(self) -> RebuildReport: ...
    def metrics(self) -> MetricsSnapshot: ...
    def diagnostic_query(
        self, sql: str, params: Sequence[Any] | None = None
    ) -> list[tuple[Any, ...]]:
        """Run a read-only query on a connection belonging to this call.

        Opens the file `SQLITE_OPEN_READ_ONLY` — an OS-level boundary, not a
        reversible `PRAGMA` — runs `sql`, and drops the connection. Values come
        back **as stored**: a timestamp column is the canonical string, not a
        `datetime`, because this path's job is to show what is actually there.

        For ordinary reads use the typed surface — `traverse`, `load_subgraph`,
        `reconstruct`, the search methods — which coerces and validates.
        """

    def explain(self, sql: str) -> list[str]: ...

# --------------------------------------------------------------- exceptions ---

class MacrameError(Exception):
    """Base of every error this library raises."""

class MacrameClosedError(MacrameError):
    """Raised by any method on a closed handle. A closed handle is not
    reusable — reopen with `Database.open(path)`."""

# The six intermediate classes exist to be caught as groups, and are never
# raised directly.
class IntegrityError(MacrameError): ...
class ValidationError(MacrameError): ...
class VectorError(MacrameError): ...
class TemporalError(MacrameError): ...
class WriterError(MacrameError): ...
class BudgetError(MacrameError): ...

class EngineError(MacrameError):
    """An error from libSQL itself, carried across without interpretation."""

class MigrationError(MacrameError):
    to: int
    reason: str

class NotFoundError(MacrameError):
    id: str

class DiagnosticConnError(MacrameError):
    path: str
    reason: str

class InvalidEdgeTypeError(ValidationError):
    edge_type: str

class InvalidIdError(ValidationError):
    id: str
    reason: str

class InvalidModelNameError(ValidationError):
    model: str

class InvalidTimestampError(ValidationError):
    value: str
    reason: str

class AttributeModeUnstatedError(ValidationError):
    as_of: str

class SingleOpenViolationError(IntegrityError):
    source_id: str
    target_id: str
    edge_type: str

class OverlappingIntervalError(IntegrityError):
    """The seven fields are flattened onto the exception rather than nested
    behind an `.overlap` object: `valid_*` is what the caller asserted,
    `existing_*` is what it collided with."""

    source_id: str
    target_id: str
    edge_type: str
    valid_from: str
    valid_to: str
    existing_from: str
    existing_to: str

class NegativeEdgeWeightError(IntegrityError):
    source_id: str
    target_id: str
    weight: float

class CurrentDriftError(IntegrityError):
    n: int

class RebuildFailedError(IntegrityError):
    n: int

class RebuildInterruptedError(IntegrityError):
    reason: str

class RecordedAtRegressionError(IntegrityError):
    got: str
    had: str

class DimMismatchError(VectorError):
    got: int
    expected: int
    model: str

class ModelNotRegisteredError(VectorError):
    model: str
    table: str

class ReplayCorruptError(TemporalError):
    seq: int
    reason: str

class SnapshotIncompatibleError(TemporalError):
    path: str
    reason: str

class PayloadVersionError(TemporalError):
    got: int
    max: int

class ArchiveViolationError(TemporalError):
    table: str

class ArchiveWindowError(TemporalError):
    """`window` crosses as a `timedelta`: the caller passed a duration and gets a
    duration back, comparable against whatever they computed it from."""

    window: timedelta
    reason: str

class WriterUnavailableError(WriterError): ...
class WriterDroppedResponderError(WriterError): ...

class WriterStoppedError(WriterError):
    reason: str

class SubgraphTooLargeError(BudgetError):
    n: int
    budget: int

# ------------------------------------------------------------------ private ---
#
# Underscore-prefixed, absent from `__all__`, and not part of the supported
# surface — but they are real, and two of them are *used*: `__init__.py` installs
# `_mark_forked` as an `os.register_at_fork` handler, and the suite reaches for
# the rest. Declaring them is what lets a checker run over the package itself
# rather than only over code that imports it.
#
# They ship in the released wheel rather than behind a Cargo feature, because a
# wheel that is tested is not the wheel that is published if the test hooks are
# compiled out.

def _mark_forked() -> None:
    """Poison this process's handles after `fork()`. A forked child inherits the
    file descriptors and not the tokio runtime, so a write from one is a write to
    a database no actor is serialising."""

def _block_for_testing(seconds: float) -> None: ...
def _db_error_variants() -> list[str]: ...
def _raise_db_error(name: str) -> None: ...
def _coerce_embedding(value: Embedding) -> list[float]: ...
def _coerce_timestamp(value: Timestamp | None) -> str: ...
def _render_timestamp(value: str) -> datetime | None: ...
