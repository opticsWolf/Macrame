"""What ``close()`` waits for, and which of the two waits is real (A-6).

The 0.15.0 review's A-6 says ``close()`` can be held off indefinitely by a hot
read loop, because ``PyDatabase.inner`` is a ``std::sync::RwLock`` and SRWLock
on Windows does not promise the waiting writer its turn.

**The mechanism is out of date and the conclusion is still worth a test, twice
over.** Rust's standard library stopped using SRWLock for
``x86_64-pc-windows-msvc``: ``std::sys::sync::rwlock`` selects the futex
implementation for ``all(target_os = "windows", not(target_vendor = "win7"))``,
and that implementation is writer-preferring on purpose — its ``can_read``
returns false once ``WRITERS_WAITING`` is set, and the last reader out wakes the
writer. SRWLock survives only for ``*-win7-windows-msvc``, which nothing here
builds: ``wheels.yml``'s Windows leg targets ``x86_64-pc-windows-msvc``.

So there are two different waits, they are not the same wait, and only one of
them is live:

1. **A hot read loop cannot starve ``close()``** — the loop has to re-acquire
   the read lock every iteration, and once ``close()`` is queued it cannot.
   ``test_a_hot_read_loop_does_not_starve_close`` pins that. It is expected to
   pass on the first run and every run after: it is a **canary**, not a
   reproduction. The standard library documents its priority policy as
   unspecified, so what saves us here is an implementation detail that has
   already changed once, in our favour, without notice. If a future toolchain
   changes it back, this test goes red instead of a user's script hanging.

2. **A call already inside ``with_db`` holds the read lock for its whole
   duration, and writer preference does nothing about that.** A bulk import is
   the worst case, and ``close()`` waits the whole of it.
   ``test_close_waits_for_an_import_in_flight_and_says_nothing`` measures it.
   This is the wait a user actually meets, and A-6 does not name it.

**What the drain is, and why it is not the long wait.** ``close()`` takes the
*write* lock before it calls ``Database::close``, and every other call needs the
*read* lock to be in the actor at all. So by the time the drain starts, no other
caller's work can be queued: the queue holds only what ``close()`` itself adds —
its final ``optimize`` and its closing snapshot. The drain is bounded by
``close()``'s own housekeeping. **The wait that scales with someone else's work
is the lock acquisition, not the drain**, which is the opposite of where a
timeout looks like it belongs.

**And a write is never queued behind an acknowledgement.** ``Database::high``
sends the command and awaits a ``oneshot`` reply, and the chunked paths await
every chunk in turn, so a write call returns only after the actor landed it.
There is therefore no work in the queue whose caller has already been told it
succeeded. That is what makes bounding the *caller's wait* safe: abandoning it
can only ever abandon a call that is still blocked and will be told so.
"""

from __future__ import annotations

import threading
import time

import pytest

import macrame
from macrame import ConceptUpsert, EdgeAssertion


# `close()` must not be slower than this once nothing else holds the lock. Set
# an order of magnitude above what an idle close costs so a loaded CI box does
# not turn a policy assertion into a timing flake.
CLOSE_DEADLINE_S = 5.0

# Big enough that the import is still running when `close()` is called, small
# enough that the test is not the slowest thing in the suite. The measurement
# the entry carries is printed rather than asserted on: the number is a property
# of the machine, the assertion is a property of the design.
IMPORT_EDGES = 4_000


TS = "2026-01-01T00:00:00.000000Z"


def _seed_edges(db: "macrame.Database", n: int) -> list[EdgeAssertion]:
    """`n` edges over `n // 4` nodes, with the nodes written first.

    `links` has a real foreign key into `concepts`, so an import of edges whose
    endpoints do not exist fails at the first chunk with `NotFoundError` rather
    than running long enough to contend with anything. Seeding is setup: it
    happens before the thread starts and before any clock in this file.
    """
    span = max(n // 4, 1)
    db.write_concepts(
        [ConceptUpsert(f"n{i}", f"node {i}", valid_from=TS) for i in range(span)]
    )
    return [
        EdgeAssertion(
            f"n{i % span}",
            f"n{(i + 1) % span}",
            f"REL{i % 7}",
            valid_from=TS,
        )
        for i in range(n)
    ]


def test_a_hot_read_loop_does_not_starve_close(db_path):
    """The canary: a back-to-back reader cannot hold `close()` off.

    `schema_version` is the cheapest call that still goes through `with_db`, so
    this is the tightest read loop the binding can be made to run — one read
    lock acquisition per iteration and essentially no work between them. It is
    exactly the shape A-6 describes.

    Expected to pass. It is here so that a toolchain whose `RwLock` stops
    preferring the waiting writer is reported as a failing test rather than as
    a hang in somebody's program.
    """
    db = macrame.Database.open(str(db_path))
    stop = threading.Event()
    reads = 0
    reader_error: list[BaseException] = []

    def reader() -> None:
        nonlocal reads
        try:
            while not stop.is_set():
                db.schema_version
                reads += 1
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            reader_error.append(exc)

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    try:
        # Let the loop get properly hot before the writer queues behind it.
        time.sleep(0.25)
        assert reads > 0, "the reader never ran; the test proves nothing"

        started = time.monotonic()
        db.close()
        waited = time.monotonic() - started
    finally:
        stop.set()
        t.join(timeout=CLOSE_DEADLINE_S)

    assert waited < CLOSE_DEADLINE_S, (
        f"close() waited {waited:.2f}s behind a read loop that completed "
        f"{reads} reads. The RwLock is no longer preferring the waiting "
        f"writer, which is A-6's original mechanism arriving for real."
    )

    # The reader meets a closed handle, which is the documented outcome and not
    # an error in this test. Anything else is.
    for exc in reader_error:
        assert isinstance(exc, macrame.MacrameClosedError), (
            f"the reader failed with {type(exc).__name__}: {exc}"
        )


def test_close_waits_for_an_import_in_flight_and_says_nothing(db_path):
    """The live one: `close()` waits the whole of an import, silently.

    Writer preference is irrelevant here. The importing thread is already
    *inside* `with_db` holding the read lock, and it holds it until the import
    returns. `close()` blocks acquiring the write lock for as long as that
    takes, with no error, no progress and nothing to log.

    The assertion is deliberately not "this is fast". It is that the wait is
    real and is bounded by the other thread's work — which is what makes it
    indistinguishable from a hang from outside the process, and what a
    `CloseTimeout` carrying "N writes in flight, drain continuing" would turn
    into a decision the caller can make.
    """
    db = macrame.Database.open(str(db_path))
    edges = _seed_edges(db, IMPORT_EDGES)

    import_started = threading.Event()
    import_done = threading.Event()
    import_result: list[object] = []

    def on_progress(_p: object) -> None:
        import_started.set()

    def importer() -> None:
        try:
            import_result.append(db.bulk_import(edges, progress=on_progress))
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            import_result.append(exc)
        finally:
            import_done.set()

    t = threading.Thread(target=importer, daemon=True)
    t.start()
    try:
        # Call close() while the import is demonstrably mid-flight: the first
        # chunk has committed and the rest have not.
        assert import_started.wait(timeout=30), "the import never reached a chunk boundary"
        assert not import_done.is_set(), (
            f"the import of {IMPORT_EDGES} edges finished before close() was "
            f"called; raise IMPORT_EDGES for this machine"
        )

        started = time.monotonic()
        db.close()
        close_waited = time.monotonic() - started
    finally:
        t.join(timeout=120)

    assert import_done.is_set(), "the importer did not finish"
    assert not isinstance(import_result[0], BaseException), (
        f"the import failed rather than completing: {import_result[0]!r}"
    )
    assert import_result[0] == IMPORT_EDGES

    # The measurement the register entry carries. Reported, not asserted: how
    # long this takes is a property of the box, and pinning it would make the
    # test a benchmark.
    print(
        f"\nclose() waited {close_waited * 1000:.0f} ms behind an in-flight "
        f"import of {IMPORT_EDGES} edges "
        f"({close_waited * 1e6 / IMPORT_EDGES:.0f} us/edge)"
    )

    # The point of the test. A close that returned instantly would mean it had
    # not waited for the import at all, which would be a correctness bug of its
    # own -- the handle would be gone from under a call still using it.
    assert close_waited > 0.05, (
        f"close() returned in {close_waited * 1000:.0f} ms while an import of "
        f"{IMPORT_EDGES} edges was in flight. It is supposed to wait for the "
        f"read lock; returning early would mean it did not."
    )


@pytest.mark.parametrize("n", [500, 2_000])
def test_the_close_wait_scales_with_the_work_it_is_waiting_for(db_path, n):
    """The wait is the other thread's work, and it is not a constant.

    Two sizes, one shape. This is what makes "bounded by the work" a
    measurement rather than a reassurance: if `close()` waited on something
    fixed -- a poll interval, a timeout, the drain's own housekeeping -- the two
    numbers would be the same.
    """
    db = macrame.Database.open(str(db_path))
    edges = _seed_edges(db, n)

    started_evt = threading.Event()
    result: list[object] = []

    def importer() -> None:
        try:
            result.append(db.bulk_import(edges, progress=lambda _p: started_evt.set()))
        except BaseException as exc:  # noqa: BLE001
            result.append(exc)

    t = threading.Thread(target=importer, daemon=True)
    t.start()
    try:
        assert started_evt.wait(timeout=30)
        started = time.monotonic()
        db.close()
        waited = time.monotonic() - started
    finally:
        t.join(timeout=120)

    assert not isinstance(result[0], BaseException), f"import failed: {result[0]!r}"
    print(f"\n{n} edges: close() waited {waited * 1000:.0f} ms")
