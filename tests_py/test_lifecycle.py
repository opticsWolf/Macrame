"""P1 acceptance: the runtime boundary and the handle lifecycle.

These test the *boundary*, not the ledger. The ledger has 24 Rust test binaries
and 300 tests; re-asserting bitemporal semantics through Python would be a
second, weaker copy free to drift. What is genuinely new here is that an async
Rust API is being driven from a synchronous, GIL-holding, non-deterministically
collected language — and every test below is about something that can only go
wrong at that seam.
"""

from __future__ import annotations

import gc
import pathlib
import threading
import time
import warnings

import pytest

import macrame
from macrame import _macrame


# --------------------------------------------------------------------------
# Open and close
# --------------------------------------------------------------------------


def test_open_then_close_writes_the_final_snapshot(db_path):
    """The round trip, and the reason ``close()`` is not optional.

    ``close()`` is the only path that writes the final anchor. Asserting the
    handle merely closes without error would pass just as well if the snapshot
    step had been dropped, so the observable consequence is what is checked.
    """
    db = macrame.Database.open(db_path)
    snapshots = db.snapshots_dir
    db.close()

    assert snapshots.is_dir(), "close() did not create the snapshot directory"
    anchors = list(snapshots.iterdir())
    assert anchors, "close() wrote no final snapshot"


def test_the_context_manager_closes(db_path):
    """``with`` is the supported form, so it must actually close."""
    with macrame.Database.open(db_path) as db:
        assert db.is_closed is False
    assert db.is_closed is True


def test_the_context_manager_closes_even_when_the_body_raises(db_path):
    """A failure in the body must not cost the final snapshot."""
    with pytest.raises(ZeroDivisionError):
        with macrame.Database.open(db_path) as db:
            _ = 1 / 0
    assert db.is_closed is True


def test_the_context_manager_does_not_swallow_the_body_error(db_path):
    """``__exit__`` returns False. A database wrapper that ate exceptions would
    be a serious trap, and it is one line away."""
    with pytest.raises(ValueError, match="deliberate"):
        with macrame.Database.open(db_path):
            raise ValueError("deliberate")


def test_close_is_idempotent(db_path):
    """So an explicit ``close()`` inside a ``with`` block is not an error."""
    db = macrame.Database.open(db_path)
    db.close()
    db.close()
    with macrame.Database.open(db_path) as db2:
        db2.close()


def test_a_reopened_database_sees_the_schema(db_path):
    """Two sequential handles on one file, which is the ordinary case.

    Also the closest this suite comes to R15's territory: sequential opens are
    measured clean (500 in one process, 0/10), and it is *concurrent* opens
    that fault. If this ever becomes flaky, read ``.cargo/config.toml``.
    """
    with macrame.Database.open(db_path) as db:
        first = db.schema_version
    with macrame.Database.open(db_path) as db:
        assert db.schema_version == first


# --------------------------------------------------------------------------
# Use after close
# --------------------------------------------------------------------------


def test_use_after_close_raises_rather_than_panicking(db_path):
    """The guarantee Rust gets from the type system, enforced at runtime.

    ``Database::close`` consumes ``self``, so in Rust this cannot be written.
    Python can write it, and what it must not do is panic — a Rust panic across
    the FFI boundary is at best a ``pyo3_runtime.PanicException`` with a
    backtrace about internals.
    """
    db = macrame.Database.open(db_path)
    db.close()
    with pytest.raises(macrame.MacrameClosedError):
        _ = db.schema_version


def test_the_closed_error_is_catchable_as_the_base_error(db_path):
    """``except MacrameError`` has to catch everything Macrame raises."""
    assert issubclass(macrame.MacrameClosedError, macrame.MacrameError)
    db = macrame.Database.open(db_path)
    db.close()
    with pytest.raises(macrame.MacrameError):
        _ = db.snapshots_dir


def test_a_closed_handle_still_says_what_it_was(db_path):
    """``path`` and ``__repr__`` answer after close.

    A closed handle that cannot name its own file makes every traceback
    mentioning it useless.
    """
    db = macrame.Database.open(db_path)
    db.close()
    assert db.path == db_path
    assert "closed" in repr(db)


# --------------------------------------------------------------------------
# The GIL
# --------------------------------------------------------------------------


def test_a_database_call_releases_the_gil():
    """**The central claim of P1.**

    If the GIL were held for the duration of a call, one thread inside a
    traversal would stop the whole interpreter — for an embedded database, the
    difference between a library and a global lock.

    ``_block_for_testing`` goes through the same ``block_on`` as every real
    method, so this is not a test of a special case: if it releases, they do.
    A real database operation is not used because at P1 none is slow enough to
    tell the two behaviours apart.

    Note the ticker's ``time.sleep`` does *not* make this vacuous. Sleeping
    releases the GIL, but waking requires re-acquiring it, so a main thread
    holding the GIL for the full duration lets the ticker make essentially no
    progress.
    """
    ticks = 0
    stop = threading.Event()

    def ticker():
        nonlocal ticks
        while not stop.is_set():
            ticks += 1
            time.sleep(0.001)

    t = threading.Thread(target=ticker, daemon=True)
    t.start()
    try:
        time.sleep(0.05)  # let the ticker reach its loop
        before = ticks
        _macrame._block_for_testing(0.5)
        progressed = ticks - before
    finally:
        stop.set()
        t.join(timeout=5)

    # 0.5s at ~1ms per tick is ~500 in the ideal case. The threshold is set an
    # order of magnitude below that: the test distinguishes "ran" from "did not
    # run at all", and must not become a timing-sensitive flake on a loaded CI
    # box.
    assert progressed > 25, (
        f"the ticker advanced {progressed} times during a 0.5s call. "
        "The GIL was not released — check that block_on still goes through "
        "Python::detach."
    )


def test_concurrent_calls_on_one_handle_do_not_raise_borrow_errors(db_path):
    """Two threads using one handle at once.

    This is why the pyclass is ``frozen`` over an ``RwLock`` rather than taking
    ``&mut self``: with ``&mut self``, a second thread entering any method while
    the first is inside a GIL-released call gets ``PyBorrowMutError`` — an error
    about pyo3's internals, for what is an ordinary concurrent read.
    """
    errors: list[BaseException] = []

    with macrame.Database.open(db_path) as db:

        def hammer():
            try:
                for _ in range(50):
                    _ = db.schema_version
                    _ = db.snapshots_dir
            except BaseException as exc:  # noqa: BLE001 - recording, not handling
                errors.append(exc)

        threads = [threading.Thread(target=hammer) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

    assert not errors, f"concurrent access raised: {errors!r}"


def test_concurrent_diagnostic_queries_on_one_handle_stay_correct(db_path):
    """The diagnostic path is the only surface that opens the file per call.

    Every other method runs on connections established at ``open``. This one
    calls ``diagnostic_conn()``, which is a fresh ``build()`` each time, and
    ``block_on`` releases the GIL — so before 0.10.0 W4.1 four threads here
    were four concurrent opens, which is R15's shape and is reproducible from
    Python (``tests_py/probes/r15_concurrent_open.py``).

    The binding now serialises this path behind a mutex. What that must not
    break is the answer: a serialised call still runs on its own connection,
    so every thread must get the same, correct row back rather than a shared
    cursor's leftovers.

    A pass here is not a proof that R15 cannot fire — four threads is far
    below the width that reproduces it. It asserts the mitigation did not cost
    correctness; the width arm is the probe, and is deliberately not a test.
    """
    errors: list[BaseException] = []
    seen: list[int] = []

    with macrame.Database.open(db_path) as db:
        db.write_concepts(
            [
                macrame.ConceptUpsert(
                    id="a",
                    title="A",
                    content="body",
                    valid_from="2026-01-01T00:00:00.000000Z",
                )
            ]
        )

        def hammer():
            try:
                for _ in range(25):
                    rows = db.diagnostic_query("SELECT COUNT(*) FROM concepts")
                    seen.append(rows[0][0])
                    plan = db.explain("SELECT * FROM concepts")
                    assert plan, "EXPLAIN QUERY PLAN returned no detail rows"
            except BaseException as exc:  # noqa: BLE001 - recording, not handling
                errors.append(exc)

        threads = [threading.Thread(target=hammer) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

    assert not errors, f"concurrent diagnostic calls raised: {errors!r}"
    assert len(seen) == 100, f"expected 100 answers, got {len(seen)}"
    assert set(seen) == {1}, f"a concurrent call saw the wrong count: {set(seen)}"


# --------------------------------------------------------------------------
# Collection without close
# --------------------------------------------------------------------------


def test_dropping_a_handle_without_closing_warns(db_path):
    """The Rust side warns through ``tracing``, which reaches no Python process.

    That is the whole reason this exists. ``tracing::warn!`` is invisible
    without a subscriber, and essentially no Python application configures one,
    so the guidance to call ``close()`` would arrive nowhere. ``ResourceWarning``
    is the established Python signal for this — it is what an unclosed file
    raises — and pytest surfaces it.
    """
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        db = macrame.Database.open(db_path)
        del db
        gc.collect()

    assert any(issubclass(w.category, ResourceWarning) for w in caught), (
        f"no ResourceWarning; got {[w.category.__name__ for w in caught]}"
    )
    message = " ".join(str(w.message) for w in caught)
    assert "close()" in message, f"the warning does not name the fix: {message}"


def test_closing_properly_warns_about_nothing(db_path):
    """The counterpart, and the one that makes the test above mean something.

    A warning that fires either way is noise, and noise trains people to filter
    the category — at which point the real signal is gone too.
    """
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        db = macrame.Database.open(db_path)
        db.close()
        del db
        gc.collect()

    assert not [w for w in caught if issubclass(w.category, ResourceWarning)], (
        "a properly closed handle still warned"
    )


# --------------------------------------------------------------------------
# Arguments and types
# --------------------------------------------------------------------------


def test_paths_come_back_as_pathlib_paths(db_path):
    """Not strings. pyo3 renders a PathBuf as ``str`` by default, which every
    caller then converts back."""
    with macrame.Database.open(db_path) as db:
        assert isinstance(db.path, pathlib.Path)
        assert isinstance(db.archive_path, pathlib.Path)
        assert isinstance(db.snapshots_dir, pathlib.Path)


def test_open_accepts_a_string_path(tmp_path):
    """``os.PathLike`` and ``str`` both, because callers pass both."""
    with macrame.Database.open(str(tmp_path / "s.db")) as db:
        assert db.schema_version > 0


def test_the_snapshot_cadence_can_be_disabled(db_path):
    """``snapshot_every_entries=None`` maps onto Rust's ``Option<SnapshotCadence>``
    being ``None`` — the setting a short-lived process wants."""
    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        assert db.schema_version > 0


@pytest.mark.parametrize("bad", [0, -1, -10_000])
def test_a_nonpositive_cadence_is_refused_rather_than_clamped(db_path, bad):
    """Refused, not repaired. A zero threshold would anchor on every poll, which
    nobody means by it, and a silent clamp becomes a mystery about snapshot
    volume much later."""
    with pytest.raises(ValueError, match="positive"):
        macrame.Database.open(db_path, snapshot_every_entries=bad)


@pytest.mark.parametrize("bad", [0.0, -1.0, float("nan"), float("inf")])
def test_a_bad_poll_interval_is_refused(db_path, bad):
    with pytest.raises(ValueError, match="positive, finite"):
        macrame.Database.open(db_path, snapshot_poll_seconds=bad)


def test_the_handle_cannot_be_constructed_directly():
    """``Database()`` must not produce a handle with no ledger behind it.

    pyo3 refuses this by default for a class with no ``#[new]``; pinned because
    adding one later would be an easy way to hand out an unusable object.
    """
    with pytest.raises(TypeError):
        macrame.Database()


def test_cadence_arguments_are_keyword_only(db_path):
    """Positional would freeze the argument order into the API forever."""
    with pytest.raises(TypeError):
        macrame.Database.open(db_path, 10_000)
