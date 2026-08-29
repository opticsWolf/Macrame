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
from typing import Any, Callable, Final, Iterator, Sequence, TypedDict

__version__: str

# A canonical timestamp string, or an aware datetime. Naive datetimes raise.
Timestamp = str | datetime
# Packed little-endian f32 bytes (the fast path), or any sequence of floats.
Embedding = bytes | Sequence[float]
# `(source, target, edge_type, valid_from, valid_to)`, timestamps as datetimes.
# One lineage's view — the caller named it, or meant the trunk.
Edge = tuple[str, str, str, datetime, datetime]
# The same, plus the lineage holding the belief. What a fold of the whole ledger
# answers with, where one edge key may be believed by several lineages at once.
EdgeBelief = tuple[str, str, str, datetime, datetime, str]

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
#: Most sessions one `archive_windowed` call will run. Above this it refuses
#: rather than clamping (`ArchiveWindowError`) — a window that implies more than
#: this many sessions is a caller who meant a wider window.
MAX_ARCHIVE_SESSIONS: Final[int]
#: The largest chunk each bulk path will ever ask for.
#:
#: **Ceilings, not sizes.** Since 0.12.0 the bulk loops time each chunk and size
#: the next one from its measured hold (D-143, D-146); these bound that search
#: from above. A populated database converges *below* them, so dividing a batch
#: by one of these predicts a transaction count that is a lower bound at best.
CHUNK_ROWS_EDGES: Final[int]
CHUNK_ROWS_CONCEPTS: Final[int]
CHUNK_ROWS_ANNOTATIONS: Final[int]
CHUNK_ROWS_EMBEDDINGS: Final[int]

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

    It is a function of the row count alone as of 0.13.6 (D-179). Until then the
    batch's *shape* mattered — 20,000 corrections to one relationship's history
    cost 18.1 s against 2.6 s for 20,000 unrelated edges, because the
    within-batch overlap guard compared every pair. That guard sorts and sweeps
    now and the two shapes agree, so the estimate does not ask what the edges
    point at. Machine-specific, and an order of magnitude rather than a promise.
    """

# -------------------------------------------------------------- value types ---

class AttributeMode:
    """Which text a temporal traversal returns (T3.2, D-085).

    Leaving it unset on a traversal that sets either `as_of_valid` or
    `as_of_recorded` raises `AttributeModeUnstatedError`. `None` is *unstated*,
    not `CURRENT`, and that difference is the whole mechanism: live text attached
    to a historical topology is the wrong answer, delivered silently.

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
        branch: str | None = None,
    ) -> None: ...
    def on_branch(self, branch: str) -> ConceptUpsert:
        """This upsert on `branch`, as a **new** object (0.14.9).

        The constructor's ``branch=`` is the ordinary path; this is what
        `BranchView` uses to stamp an upsert it did not build.
        """
    @property
    def branch(self) -> str | None:
        """The lineage this concept is minted on, or None for the trunk.

        A branch inherits its parent's concepts and may not restate them —
        doing so raises `CrossLineageError`.
        """
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
        branch: str | None = None,
    ) -> None: ...
    def on_branch(self, branch: str) -> EdgeAssertion:
        """This assertion on `branch`, as a **new** object (0.14.9).

        The constructor's ``branch=`` is the ordinary path; this is what
        `BranchView` uses to stamp an assertion it did not build, without
        having to know which fields the caller set.
        """
    @property
    def branch(self) -> str | None:
        """The lineage this edge is asserted on, or None for the trunk.

        A write on a branch adds a row beside the ancestor's rather than over
        it, so the parent's history is unchanged by anything a branch does.
        """
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
    def edges(self) -> list[EdgeBelief]:
        """One entry per lineage per edge, not one per edge.

        A fork and its ancestor believing different things about one edge key
        are two beliefs, and both are here, each labelled. For one lineage's
        view of an instant use ``query_as_of_edges`` or a traversal's
        ``branch=``, which resolve; do not filter this list by hand, because
        resolution is nearest-ancestor and not equality.
        """

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

class RehydrateReport:
    """What one rehydration moved back out of cold storage (0.9.0, D-131).

    A class rather than a tuple despite having two fields, because it is the
    counterpart of `ArchiveReport` — the pair is the unit a caller thinks in —
    and because two bare ``int``s where one is *work done* and the other is
    *something unusual happened* is an invitation to read ``[0]`` and mean
    ``[1]``.
    """

    @property
    def concepts_rehydrated(self) -> int:
        """How many concepts actually moved.

        Ids absent from the cold file are skipped rather than raising, so this
        may be smaller than the list passed in.
        """

    @property
    def rowids_reassigned(self) -> int:
        """Of those, how many could not reclaim their original ``rowid_pk``.

        Normally zero. Non-zero means something took the row's old identifier
        while it was cold, so it came back with a fresh one and the search index
        was re-pointed to match — the only respect in which a rehydrated row
        differs from the row that was archived.
        """

    def __repr__(self) -> str: ...

class Branch:
    """One lineage: its name, its parent, and where it was cut.

    Returned by `Database.fork` and `Database.branches`. A snapshot of a row in
    an append-only table, so it is frozen — changing it would change nothing.
    """

    @property
    def id(self) -> str:
        """The lineage's own name."""

    @property
    def parent(self) -> str | None:
        """The lineage this one was cut from, or `None` for the trunk."""

    @property
    def forked_at(self) -> datetime | None:
        """When this lineage stopped inheriting its parent's later writes.

        `None` for the trunk. This is the visibility cutoff a branched read is
        bounded by: the branch sees its parent's history up to and including
        this instant, and nothing the parent records after it.
        """

    @property
    def created_at(self) -> datetime:
        """When the row was written.

        Equal to `forked_at` for every branch this release can create. **Not
        comparable with a `recorded_at`** — the trunk's is stamped during
        migration, before the database's clock is resolved.
        """

    @property
    def is_main(self) -> bool:
        """Whether this names the trunk."""

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

class CheckpointReport:
    """What a `checkpoint()` did (D-156).

    Named fields rather than a tuple because two of the three are easy to swap:
    `log_frames` is what is *left* in the WAL, `checkpointed_frames` is what was
    *moved* out of it.
    """

    @property
    def busy(self) -> bool:
        """SQLite gave up waiting for a reader or writer.

        Not an error, and not ignorable: frames may still have moved, so the
        main file is not yet self-contained.
        """

    @property
    def log_frames(self) -> int:
        """Frames left in the WAL. `0` when the checkpoint completed."""

    @property
    def checkpointed_frames(self) -> int:
        """Frames moved back into the database file."""

    def is_complete(self) -> bool:
        """Not busy, and nothing left in the WAL."""

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
    @property
    def low_starved_turns(self) -> int:
        """Turns taken on high-priority work while low-priority work was queued.

        The actor's select is biased and has no floor. This rising is not by
        itself a problem -- a busy database should prefer interactive writes --
        so read it beside `low_starved_run_max` (0.12.10, D-153).
        """

    @property
    def low_starved_run_max(self) -> int:
        """The most turns any single low-priority command has waited.

        The number that distinguishes "prioritised" from "starved". There is
        deliberately no forced-yield policy behind it: whether one is needed is
        the question this number exists to answer (0.12.10, D-153).
        """

    def __repr__(self) -> str: ...

class BulkProgress(TypedDict):
    """One chunk's worth of progress, passed to a `progress=` callback (0.13.8).

    Reported *after* the chunk commits, so `written` counts rows that are in
    the database and will stay there even if the next chunk fails.
    """

    written: int
    total: int
    rows: int
    held_ms: float

class CancelToken:
    """A flag another thread raises to stop a running chunked write (0.13.8).

    The four chunked methods hold the GIL released for their whole run, so the
    thread that called one cannot cancel it — some *other* thread has to, which
    is what this is for. `cancel()` is an atomic store and is safe from a signal
    handler, a UI callback, or a watchdog.

        token = macrame.CancelToken()
        threading.Timer(30.0, token.cancel).start()
        try:
            db.bulk_import(edges, cancel=token)
        except macrame.BulkCancelledError as e:
            print(f"stopped with {e.written} rows committed")

    The stop lands at a chunk boundary. Nothing rolls back.
    """

    def __init__(self) -> None: ...
    def cancel(self) -> None: ...
    @property
    def cancelled(self) -> bool: ...
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
        wal_autocheckpoint: int | str | None = None,
        writer_cache_size: int | None = None,
        reader_cache_size: int | None = None,
        future_stamps: float | str | None = None,
    ) -> Database:
        """Open a ledger, running migrations and starting the write actor.

        Every tuning keyword defaults to *leave it alone*, and none of them
        spells that as "off". `wal_autocheckpoint` takes `None` (SQLite's own
        1,000-page default), the string `"disabled"`, or a positive page count
        — `0` is refused rather than read as SQLite's disable overload, since a
        computed threshold that came out zero is a bug and the overload would
        turn it into a WAL that grows for the life of the process.

        The two cache sizes are SQLite `cache_size` units: negative is KiB,
        positive is pages. They are separate because the writer is one
        connection and the readers are several.

        `future_stamps` decides what happens when the newest stored
        `recorded_at` is ahead of the wall clock. `None` refuses beyond a day;
        a number is your own tolerance in seconds (`0` refuses anything ahead
        at all); `"allow"` opens the file regardless. The refusal exists
        because the clock floors itself at `MAX(recorded_at)`, so a stamp from
        the future is inherited by every write that follows and written back
        into rows the next open reads — the one bad value in the file that
        manufactures more of itself. Raises `FutureRecordedAtError`.
        """

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
        *,
        branch: str | None = None,
    ) -> None:
        """Close an open interval by asserting its replacement (Doctrine III).

        With `branch=`, this is **shadow retirement**: the branch writes its
        own row at the ancestor's key and the ancestor's row is untouched, so
        the edge leaves this lineage's view and stays in its parent's. Rust
        splits this into `retire_edge` and `retire_edge_on` because it has no
        keyword defaults; here one method takes both.
        """
    def upsert_concept(self, concept: ConceptUpsert) -> None: ...
    def write_bulk_atomic(self, edges: Sequence[EdgeAssertion]) -> int:
        """Assert many edges in **one transaction under one stamp** (D-014).

        The batch is one act, so it cannot be chunked — splitting it is the thing
        this method exists not to do. The actor's hold is therefore a function of
        `len(edges)`, and every other writer in the process waits that long:
        ~33 ms at 500 rows, ~165 ms at 2,000, ~0.9 s at 10,000, ~2.2 s at 20,000
        (T1.3, D-081; re-measured after D-179, which removed the batch shape's
        effect on all four). Call `estimate_bulk_hold(edges)` for a given batch.

        A caller who needs the latency bound and not the atomicity wants
        `bulk_import` — the same write, chunked, and explicitly not atomic
        overall.
        """

    def bulk_import(
        self,
        edges: Sequence[EdgeAssertion],
        *,
        progress: Callable[[BulkProgress], object] | None = None,
        cancel: CancelToken | None = None,
    ) -> int:
        """Import edges on the background channel, chunked and **atomic per
        chunk, not overall** (D-011).

        A failure partway leaves the chunks before it committed, so every
        exception this raises carries `written`: the number of rows that are in
        the database and staying there (0.13.8, D-181). The exception class is
        still chosen by what went wrong — `except SingleOpenViolationError`
        catches the same thing it always did.

        `progress` is called after every chunk with one dict. It runs on the
        ledger's thread with the GIL re-acquired, so it is on the critical path:
        update a counter, do not write a file. An exception raised there stops
        the import and is what propagates.

        `cancel` stops the import at the next chunk boundary. Nothing rolls
        back. This call runs with the GIL released, so whatever calls
        `token.cancel()` has to be another thread.
        """

    def write_concepts(
        self,
        concepts: Sequence[ConceptUpsert],
        *,
        progress: Callable[[BulkProgress], object] | None = None,
        cancel: CancelToken | None = None,
    ) -> int:
        """Upsert many concepts on the background channel, chunked (D-011).

        Every row is a ledger write. Chunked, so see `bulk_import` for what
        `written`, `progress` and `cancel` mean — they are the chunk loop's
        properties and not this method's.
        """

    def write_analytics_annotations(
        self,
        annotations: Sequence[Annotation],
        *,
        progress: Callable[[BulkProgress], object] | None = None,
        cancel: CancelToken | None = None,
    ) -> int:
        """Write derived analytics results, chunked and off-ledger (D-041).

        Chunked, so see `bulk_import` for `written`, `progress` and `cancel`.
        """

    # -- reads -----------------------------------------------------------------
    def traverse_ids(
        self,
        start_node: str,
        *,
        max_depth: int = 2,
        edge_types: Sequence[str] | None = None,
        min_weight: float = 0.0,
        as_of_valid: Timestamp | None = None,
        as_of_recorded: Timestamp | None = None,
        branch: str | None = None,
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
        as_of_valid: Timestamp | None = None,
        as_of_recorded: Timestamp | None = None,
        branch: str | None = None,
        now: Timestamp | None = None,
    ) -> list[NodeAttributes]:
        """Traverse and hydrate node text.

        `attribute_mode` left unset alongside either instant raises
        `AttributeModeUnstatedError` rather than returning live text for a
        historical topology.

        `as_of` became `as_of_valid` and `as_of_recorded` in 0.13.2 (W7.1). The
        old keyword reached the valid-time columns for topology and the
        transaction-time column for attributes, so one name asked two questions.
        `as_of_valid` is *what was true*; `as_of_recorded` is *what we believed*;
        setting both asks the bitemporal question.
        """

    def load_subgraph(
        self,
        start_node: str,
        max_hops: int,
        byte_budget: int,
        *,
        edge_types: Sequence[str] | None = None,
        min_weight: float | None = None,
        as_of_valid: Timestamp | None = None,
        as_of_recorded: Timestamp | None = None,
        branch: str | None = None,
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

    def rehydrate(self, ids: list[str]) -> RehydrateReport:
        """Bring named concepts back out of cold storage (0.9.0, D-131).

        A physical move back, not a re-assertion: it mints no transaction-time
        facts, so `reconstruct` at any instant answers the same before archival,
        while archived, and after rehydration. Ids not in the cold file are
        skipped rather than refused.

        A write, so it queues through the actor and waits out any transaction in
        flight — a channel wait `busy_timeout` does not bound.
        """

    def verify_snapshot_chain(self, ts: Timestamp) -> ChainCheck: ...

    # -- maintenance -----------------------------------------------------------
    def analyze(self) -> None:
        """Refresh the planner's statistics unconditionally (`ANALYZE`).

        `PRAGMA analysis_limit` damps the hold by 3-4x; it does not make it
        independent of the table, which this docstring claimed until 0.13.24.
        Measured: 5.26 ms at 10,000 edges, 19.1 ms at 40,000, against a 3 ms
        chunk budget -- so every call appears in `metrics().violations()` under
        the kind `"analyze"`, permanently and by design (D-166, D-197).

        Call after a bulk import; prefer `optimize()` for routine upkeep.
        """

    def optimize(self) -> None:
        """Re-analyse only what has gone stale (`PRAGMA optimize`).

        A no-op on an idle database, which is what makes it safe on a schedule.
        `close()` already runs it. Reported as its own kind, `"optimize"`,
        since 0.13.24.

        **Not a cheaper `analyze()`.** The staleness test is SQLite's own and it
        is a *ratio*: measured, growth of 2x and 5x left the statistics
        untouched and only 25x rewrote them. Below that it declines in ~0.1 ms;
        above it, it costs what `analyze()` costs. So a bulk load does not make
        this refresh the statistics it invalidated -- use `analyze()` when you
        need that (D-197).
        """

    def checkpoint(self) -> CheckpointReport:
        """Move WAL frames back into the main database file.

        Check `busy` before treating the file as self-contained: a busy
        checkpoint gave up waiting for a reader and may have moved only some
        frames.
        """


    # -- vector ----------------------------------------------------------------
    def fork(self, name: str, frm: str = "main") -> Branch:
        """Cut a new lineage from an existing one, and return it.

        O(1) in rows written: one row in `branches`, and nothing else. A branch
        inherits its parent's history by resolution at read rather than by
        owning a copy, so forking a thousand times leaves every ledger table
        byte-identical.

        The fork point is *now*. The branch sees its parent's history up to this
        instant and nothing the parent records after it, which is what `branch=`
        on the traversal entry points reads.

        The lineage is readable and **not yet writable**: no write takes a
        branch, so `assert_edge` after a `fork` lands on the trunk.

        Raises `UnknownBranchError` for an unregistered parent,
        `BranchExistsError` for a taken name (including `"main"`),
        `InvalidBranchIdError` for a name the ledger cannot accept, and
        `ForkPrecedesParentError` when the clock would place the fork point
        before the parent's own.
        """

    def branches(self) -> list[Branch]:
        """Every lineage, trunk first then creation order.

        A database that has never forked returns exactly one `Branch`.
        """

    def register_model(self, model: str, dim: int) -> None: ...
    def registered_models(self) -> list[str]:
        """Every model registered in this database, in name order.

        Read from the schema, not from a registry — so it cannot drift from
        what exists. Returns names, not tables: each is a string
        `register_model` and `search_vector` accept back.
        """

    def declared_dimension(self, model: str) -> int:
        """The dimension `model`'s table declares.

        Raises `ModelNotRegisteredError` if the model has no table — which is
        the difference from `registered_models()`: that answers membership,
        this answers the number you size a vector against.
        """

    def upsert_embeddings(
        self,
        model: str,
        rows: Sequence[tuple[str, Embedding]],
        *,
        progress: Callable[[BulkProgress], object] | None = None,
        cancel: CancelToken | None = None,
    ) -> int:
        """Store or replace vectors for `model`, chunked (D-048).

        The longest-running write the crate has — DiskANN maintenance makes an
        embedding its most expensive row — and therefore the one most likely to
        want `cancel`. See `bulk_import` for what `written`, `progress` and
        `cancel` mean.
        """
    def search_vector(
        self,
        model: str,
        query: Embedding,
        *,
        top_k: int = 10,
        as_of_valid: Timestamp | None = None,
        half_life: timedelta | float | None = None,
    ) -> list[VectorHit]:
        """Nearest `top_k` concepts by cosine distance. Smaller score is closer.

        `as_of_valid` reads the corpus as it was: a concept is a result only
        while its own valid interval contains that instant. Absent, the search
        is over what is true now (0.13.19, D-192).

        Valid time only. The index keeps one row per concept and no history of
        what its vector used to be, so there is nothing to read at a past
        `recorded_at` — that question goes to `reconstruct`.

        `half_life` weights a hit by the age of what it matched, `0.5 ** (age /
        half_life)`, measured from `as_of_valid` — which it therefore requires,
        raising `HalfLifeWithoutInstantError` rather than defaulting to now. The
        returned score is still a distance, so the list still ascends: decay
        moves a stale hit *away*, never nearer. A decaying search reads
        `max(5 * top_k, 50)` candidates before reordering, because re-ranking a
        top-k is not the top-k of the re-ranking (0.13.20, D-193).
        """

    def keyword_search(
        self,
        query: str,
        *,
        top_k: int = 10,
        raw: bool = False,
        as_of_valid: Timestamp | None = None,
        half_life: timedelta | float | None = None,
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
        as_of_valid: Timestamp | None = None,
        half_life: timedelta | float | None = None,
    ) -> list[VectorHit]:
        """Vector and keyword arms, fused by reciprocal rank. Larger is better.

        `as_of_valid` applies to **both** arms. It could not apply to one: RRF
        fuses two rank lists, and bounding only the vector arm would fuse what
        was true then with what is true now into a single list that is neither.

        `half_life` applies to both arms too, and *before* the fusion: RRF adds
        ranks, so a factor on the fused score would leave both orderings — the
        only thing it reads — exactly as they were (0.13.20, D-193).
        """
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
        as_of_valid: Timestamp | None = None,
        branch: str | None = None,
    ) -> tuple[list[VectorHit], CostEstimate]:
        """Vector search restricted to a traversed neighbourhood.

        Returns the hits *and* the plan that produced them: which strategy was
        chosen and the byte estimates it was chosen on.

        `as_of_valid` bounds the traversal **and** the ranking, because it is
        one instant rather than two: a past neighbourhood scored against the
        present corpus is the bug this parameter exists to prevent, not a
        configuration (0.13.19, D-192). Absent, both read the present.
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

    written: int | None
    """How many rows were committed before this error stopped the write.

    `None` on every path where partial application is not a concept — reads,
    single writes, `write_bulk_atomic`, connection errors — and an `int` on the
    four chunked paths (`bulk_import`, `write_concepts`, `upsert_embeddings`,
    `write_analytics_annotations`), which are atomic per chunk and not overall.

    So `e.written is not None` is the test for *did this leave the database
    partially written*, and when it is an int those rows are committed and
    staying committed. `0` means the first chunk failed, which is why the "not
    applicable" case is `None` and not `0` — they are different facts.

    Present on every Macrame exception (0.13.9), so
    `except MacrameError as e: log(e.written)` never raises an `AttributeError`
    from inside the handler that was trying to record the original failure.
    """

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
class BranchError(MacrameError): ...

class UnknownBranchError(BranchError):
    """A branch that is not registered.

    Raised by `fork()` for an unknown parent, and by any read naming a lineage
    the ledger has never heard of — refused rather than answered for the trunk,
    which is the answer a caller is least able to notice.
    """

    branch: str

class BranchExistsError(BranchError):
    """A fork asked for a name that is taken, `"main"` included."""

    branch: str

class ForkPrecedesParentError(BranchError):
    """A fork point earlier than its parent's own.

    Such a branch would inherit nothing whatever from the parent it names, so
    its parent link and its visible history would say different things.
    """

    branch: str
    parent: str
    forked_at: str
    parent_forked_at: str

class CrossLineageError(BranchError):
    """A branch tried to restate a concept another lineage holds.

    `concepts` is keyed by identity, so two lineages disagreeing about one
    concept is two rows with one id. A branch **inherits** its parent's
    concepts; what it may do is mint one of its own.
    """

    id: str
    held_by: str
    attempted: str

class BranchMismatchError(BranchError):
    """A `BranchView` was handed a write naming a different lineage.

    The view stamps its own lineage on an assertion that names none. One that
    names a *different* lineage is contradicted rather than relabelled.
    """

    view: str
    named: str

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

class BulkCancelledError(MacrameError):
    """A chunked bulk write stopped because a `CancelToken` was cancelled.

    Not a fault, and the only Macrame exception a caller raises on purpose.
    Nothing rolled back: `written` is how many rows the chunks before the stop
    committed, and they are still committed (0.13.8). Only a chunked path can
    raise this, so `written` is an int here in practice — it is declared on
    `MacrameError` because every exception carries it (0.13.9).
    """

class InvalidEdgeTypeError(ValidationError):
    edge_type: str

class InvalidIdError(ValidationError):
    id: str
    reason: str

class InvalidModelNameError(ValidationError):
    model: str

class InvalidBranchIdError(ValidationError):
    """A branch name the ledger cannot accept.

    Wider than the model-name rule, and for a reason: a branch id is always a
    bound value, never a spliced identifier, so hyphens, dots, slashes, spaces
    and capitals are all fine. Refused are empty, over 128 characters, control
    characters, and leading or trailing whitespace — that last pair because
    `branches` is append-only, so a name with a trailing space is not a typo
    anyone can fix, it is a second lineage that prints as the first.
    """

    branch: str

class InvalidTimestampError(ValidationError):
    value: str
    reason: str

class AttributeModeUnstatedError(ValidationError):
    """A traversal set an instant and left `attribute_mode` unstated.

    Both attributes are always present; the one the traversal did not set is
    `None` (0.13.10). They are named for the keywords that produce them, because
    the remedy is a keyword on the same call — and `as_of`, which this carried
    until 0.13.10, has not been one since 0.13.2.
    """

    as_of_valid: str | None
    as_of_recorded: str | None

class HalfLifeWithoutInstantError(ValidationError):
    """A search set `half_life` and left `as_of_valid` unstated.

    No attributes: the caller passed one knob too few and the sentence says
    which. Age is relative to an instant, and the crate reads no wall clock on a
    read path — defaulting to now would make every decayed search quietly a
    search about the present (0.13.20).
    """

class RecordedInstantUnreachableError(TemporalError):
    ts: str

class SingleOpenViolationError(IntegrityError):
    source_id: str
    target_id: str
    edge_type: str

class OverlappingIntervalError(IntegrityError):
    """The fields are flattened onto the exception rather than nested behind an
    `.overlap` object: `valid_*` is what the caller asserted, `existing_*` is
    what it collided with, and `within_batch` says where that one lives.

    `within_batch` is False for the ordinary case — the other interval is a
    committed row, and `query_as_of_edges` will show it. It is True when
    `write_bulk_atomic` was handed a batch that contradicts itself, which is
    caught before the transaction opens: the interval named is another edge in
    the same call, the batch is refused whole, and querying for it finds
    nothing (0.13.7)."""

    source_id: str
    target_id: str
    edge_type: str
    valid_from: str
    valid_to: str
    existing_from: str
    existing_to: str
    within_batch: bool

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

class FutureRecordedAtError(IntegrityError):
    stamp: str
    limit: str

class ArchiveSessionLeakedError(IntegrityError):
    marker: str

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

class SnapshotCorruptError(TemporalError):
    """A damaged snapshot file, as distinct from a foreign one and from a
    damaged ledger (0.13.12, W8.2).

    `SnapshotIncompatibleError` means another build wrote it, which is ordinary
    after an upgrade. `ReplayCorruptError` means the transaction log itself is
    damaged, which is the worst thing this library can report. This one means
    the *cache* is damaged: the ledger is untouched, deleting the file restores
    correctness, and the cost is a slower reconstruction.

    Nothing in normal operation raises it to a caller — a damaged snapshot is
    skipped and the fold runs from the log — so seeing one means `load_snapshot`
    was called directly, or that every snapshot on disk is unusable.
    """

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
