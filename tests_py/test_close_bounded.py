"""The two halves of the A-6 remedy, and what each of them does not do.

0.15.22 ([D-264]) measured the wait: every call holds the read lock for its
whole duration, so ``close()`` blocks acquiring the write lock for as long as
whatever is in flight takes — 74 ms behind 500 edges, 992 behind 4,000 — with no
error and nothing to log. ``test_close_contention.py`` is that measurement and
still passes unchanged; this file is the remedy, and the remedy is two separate
things that are easy to confuse:

1. **The *closing* flag.** ``close()`` raises it before it asks for the write
   lock, so calls arriving afterwards fail fast with ``MacrameClosedError``
   rather than queueing in front of the shutdown. Without it a hot caller can
   keep feeding the thing ``close()`` is waiting for, and the bound in (2)
   becomes a bound on a wait that never ends.

2. **The bounded wait.** ``close(timeout=...)`` gives up after a stated time and
   raises ``CloseTimeoutError`` saying how many calls were still inside and for
   how long it waited.

**What neither does is cancel anything**, and the tests below are mostly there
to prove that rather than to prove the timeout fires. The in-flight call still
completes and still returns its true result; the write actor still drains; and
the writes at risk are none, because ``Database::high`` awaits a ``oneshot`` per
command, so a write that has returned to its caller was committed before it
returned. A timed-out handle stays *closing* on purpose: un-setting the flag
would race a ``close()`` about to take the lock a moment later, and "on its way
out" is the honest description of that handle either way.
"""

from __future__ import annotations

import threading
import time

import pytest

import macrame
from macrame import ConceptUpsert, EdgeAssertion

TS = "2026-01-01T00:00:00.000000Z"

# Enough work to still be running when close() is called, and enough that the
# timeout below expires well inside it rather than at its edge.
IMPORT_EDGES = 4_000

# Short relative to the import, long relative to a scheduling hiccup.
TIMEOUT_S = 0.2


def _seed_edges(db: "macrame.Database", n: int) -> list[EdgeAssertion]:
    """`n` edges over `n // 4` nodes, with the nodes written first.

    `links` has a real foreign key into `concepts`. Setup, and it happens before
    any thread starts and before any clock in this file.
    """
    span = max(n // 4, 1)
    db.write_concepts(
        [ConceptUpsert(f"n{i}", f"node {i}", valid_from=TS) for i in range(span)]
    )
    return [
        EdgeAssertion(f"n{i % span}", f"n{(i + 1) % span}", f"REL{i % 7}", valid_from=TS)
        for i in range(n)
    ]


class _Importer:
    """A bulk import running in another thread, with a handle on its progress."""

    def __init__(self, db: "macrame.Database", n: int) -> None:
        self.db = db
        self.n = n
        self.edges = _seed_edges(db, n)
        self.started = threading.Event()
        self.done = threading.Event()
        self.result: list[object] = []
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        try:
            self.result.append(
                self.db.bulk_import(self.edges, progress=lambda _p: self.started.set())
            )
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            self.result.append(exc)
        finally:
            self.done.set()

    def __enter__(self) -> "_Importer":
        self.thread.start()
        assert self.started.wait(timeout=30), "the import never reached a chunk boundary"
        assert not self.done.is_set(), (
            f"the import of {self.n} edges finished before the test could act on "
            f"it; raise IMPORT_EDGES for this machine"
        )
        return self

    def __exit__(self, *_exc: object) -> None:
        self.thread.join(timeout=120)

    def assert_completed(self) -> None:
        assert self.done.is_set(), "the importer did not finish"
        assert not isinstance(self.result[0], BaseException), (
            f"the import did not complete: {self.result[0]!r}"
        )
        assert self.result[0] == self.n, (
            f"the import returned {self.result[0]!r} rather than all {self.n} rows"
        )


# -- 1. the closing flag ----------------------------------------------------


def test_a_call_arriving_during_a_close_fails_fast_instead_of_queueing(db_path):
    """The flag: a latecomer is told now, not after the import ahead of it.

    Before 0.15.23 this call blocked on the read lock behind `close()`'s write
    lock and eventually raised `MacrameClosedError` anyway -- the same answer,
    a second later, having made the shutdown queue one call longer.
    """
    db = macrame.Database.open(str(db_path))
    late_error: list[BaseException] = []
    late_returned = threading.Event()

    with _Importer(db, IMPORT_EDGES) as imp:
        def latecomer() -> None:
            try:
                db.schema_version
            except BaseException as exc:  # noqa: BLE001
                late_error.append(exc)
            finally:
                late_returned.set()

        with pytest.raises(macrame.CloseTimeoutError):
            db.close(timeout=TIMEOUT_S)

        # The close has raised the flag and given up waiting; the import is
        # still running. A call now must not wait for it.
        assert not imp.done.is_set(), "the import finished too early to test this"
        started = time.monotonic()
        t = threading.Thread(target=latecomer, daemon=True)
        t.start()
        assert late_returned.wait(timeout=10), "the late call never returned"
        t.join(timeout=10)
        answered_in = time.monotonic() - started

        assert late_error and isinstance(late_error[0], macrame.MacrameClosedError), (
            f"the late call raised {late_error!r} rather than MacrameClosedError"
        )
        assert answered_in < 0.1, (
            f"the late call took {answered_in * 1000:.0f} ms to be refused; it "
            f"queued behind the close instead of failing fast"
        )

        db.close()

    imp.assert_completed()


def test_the_flag_is_readable_and_says_which_state_the_handle_is_in(db_path):
    """`is_closing and not is_closed` is the state a timeout leaves behind."""
    db = macrame.Database.open(str(db_path))
    assert not db.is_closing
    assert not db.is_closed

    with _Importer(db, IMPORT_EDGES) as imp:
        with pytest.raises(macrame.CloseTimeoutError):
            db.close(timeout=TIMEOUT_S)
        assert db.is_closing, "close() did not mark the handle"
        assert not db.is_closed, "the handle reported closed while still draining"

        db.close()

    imp.assert_completed()
    assert db.is_closing
    assert db.is_closed


# -- 2. the bounded wait ----------------------------------------------------


def test_close_with_a_timeout_gives_up_and_says_what_it_was_waiting_for(db_path):
    """The timeout fires roughly on time and carries a usable diagnosis."""
    db = macrame.Database.open(str(db_path))

    with _Importer(db, IMPORT_EDGES) as imp:
        started = time.monotonic()
        with pytest.raises(macrame.CloseTimeoutError) as caught:
            db.close(timeout=TIMEOUT_S)
        waited = time.monotonic() - started

        err = caught.value
        assert err.in_flight >= 1, (
            f"in_flight was {err.in_flight} while an import was demonstrably "
            f"running; the counter is not counting"
        )
        assert err.waited >= TIMEOUT_S
        assert err.written is None, "written must be None on every error (D-182)"
        assert "close()" in str(err) and "cancel" in str(err).lower()

        # Bounded by the timeout, not by the import. The ceiling is loose
        # because the poll interval and the scheduler both round upwards; what
        # would fail here is the old behaviour, which waited out the import.
        assert waited < TIMEOUT_S + 2.0, (
            f"close(timeout={TIMEOUT_S}) took {waited:.2f}s, which is the "
            f"import's duration rather than the caller's bound"
        )

        db.close()

    imp.assert_completed()


def test_a_timed_out_close_cancels_nothing(db_path):
    """The whole point: the import still lands, in full, and close still works.

    If a bounded wait could lose work it would be a worse failure than the hang
    it replaces -- silence you can wait out, versus rows you never hear about.
    It cannot: nothing here is cancelled, and the second `close()` is the same
    close, resumed.
    """
    db = macrame.Database.open(str(db_path))

    with _Importer(db, IMPORT_EDGES) as imp:
        with pytest.raises(macrame.CloseTimeoutError):
            db.close(timeout=TIMEOUT_S)
        # Retry-close: the one call a closing handle still accepts, and it
        # waits out the rest of the import exactly as an unbounded close would.
        db.close()

    imp.assert_completed()
    assert db.is_closed

    # And the rows are really in the file, read back through a fresh handle: a
    # count returned to the importing thread only proves what that thread was
    # told. The number is not IMPORT_EDGES -- `_seed_edges` reuses endpoints on
    # purpose, so the assertions collapse into far fewer standing beliefs -- and
    # what matters here is that the import landed rather than how it folded.
    with macrame.Database.open(str(db_path)) as reopened:
        state = reopened.reconstruct("2030-01-01T00:00:00.000000Z")
        assert state.edges, "the timed-out close lost the whole import"
        assert len(state.concepts) == IMPORT_EDGES // 4


def test_a_timeout_on_an_idle_handle_is_not_a_timeout(db_path):
    """Nothing in flight, so the bound is never approached."""
    db = macrame.Database.open(str(db_path))
    started = time.monotonic()
    db.close(timeout=5.0)
    waited = time.monotonic() - started
    assert db.is_closed
    assert waited < 5.0, "an idle close consumed its whole budget"


def test_closing_twice_with_a_timeout_is_still_a_no_op(db_path):
    """Idempotence survives the new argument: `__exit__` after `close()` is fine."""
    db = macrame.Database.open(str(db_path))
    db.close(timeout=5.0)
    db.close(timeout=5.0)
    db.close()
    assert db.is_closed


@pytest.mark.parametrize("bad", [-1.0, float("nan"), float("inf")])
def test_a_nonsense_timeout_is_refused_before_anything_is_shut_down(db_path, bad):
    """A rejected argument must not leave the handle half closed.

    The check runs before the flag is raised, so a caller who fat-fingers the
    units gets a `ValueError` and a handle that still works -- rather than a
    database that is now refusing every call because of a typo.
    """
    db = macrame.Database.open(str(db_path))
    try:
        with pytest.raises(ValueError):
            db.close(timeout=bad)
        assert not db.is_closing, "a refused timeout still marked the handle"
        assert db.schema_version > 0, "the handle stopped working after a ValueError"
    finally:
        db.close()


def test_the_default_is_the_old_behaviour(db_path):
    """`close()` with no argument waits, however long it takes.

    This is the compatibility claim, and it is the reason `timeout` defaults to
    `None` rather than to some helpful-looking number: a release that quietly
    started abandoning shutdowns after five seconds would be a behaviour change
    dressed as a fix.
    """
    db = macrame.Database.open(str(db_path))

    with _Importer(db, IMPORT_EDGES) as imp:
        started = time.monotonic()
        db.close()
        waited = time.monotonic() - started

    imp.assert_completed()
    assert db.is_closed
    assert waited > 0.05, (
        f"close() returned in {waited * 1000:.0f} ms with an import in flight; "
        f"it is supposed to wait for it"
    )
