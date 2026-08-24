"""W6.3: the transaction-time axis, assertable from Python at last.

Until 0.12.20 ``tests_py`` had no way to influence ``recorded_at``. Every stamp
came from the wall clock, so the only assertions available were *this is a
timestamp* and *this one is after that one* — which is defect K's shape on the
side that never received D-062's fix: the axis exists, the tests cannot see it.

The seam is deliberately narrow. ``macrame._macrame._FakeClock`` and
``Database._open_with_clock`` are underscore-prefixed, absent from ``__all__``,
and take a *fake* rather than a ``Clock`` implementation — see §14.6 for the
objection this shape answers rather than overrides.
"""

from __future__ import annotations

import datetime as dt

import pytest

import macrame
from macrame import _macrame

T0 = "2026-01-01T00:00:00.000000Z"
# In the past, and it has to be (0.13.5, W7.4, D-178). This was
# 2030-06-01 — a fake clock set well ahead of the wall clock, which is exactly
# "a test fixture that escaped", the case §3.4 names. It never mattered while
# `MAX(recorded_at)` was absorbed without question; now a file stamped from it
# is refused on reopen, and the first thing the new guard caught was this.
#
# Only the reopening test would have failed, which is the more interesting half:
# the fixture was wrong in every test in this file and observable in one.
START = "2026-06-01T12:00:00.000000Z"


@pytest.fixture
def clock():
    return _macrame._FakeClock(START)


def recorded_at(db, concept_id):
    """The live row's stamp — one per concept, updated in place."""
    rows = db.diagnostic_query(
        "SELECT recorded_at FROM concepts WHERE id = ?", [concept_id]
    )
    return [r[0] for r in rows]


def logged_at(db, concept_id):
    """Every stamp the *log* holds for this concept, oldest first.

    Concepts are versioned in `transaction_log` rather than as rows in
    `concepts` (§4.1) — the hot table carries the current version only. So the
    transaction-time history of a concept is here and nowhere else, which is
    exactly the axis W6.3 exists to make visible.
    """
    rows = db.diagnostic_query(
        "SELECT recorded_at FROM transaction_log "
        " WHERE table_name = 'concepts' AND entity_id = ? "
        " ORDER BY seq_id",
        [concept_id],
    )
    return [r[0] for r in rows]


def test_the_stamp_a_write_records_is_the_one_the_clock_was_set_to(db_path, clock):
    """The whole capability: an exact `recorded_at`, not merely a plausible one.

    The stamp is asserted to the microsecond against `START`, which no wall
    clock will produce — that is what makes this an assertion about the
    transaction-time axis rather than about clock skew. Asserted against the
    constant rather than a literal, so moving `START` cannot leave a test
    passing for the wrong reason.
    """
    with macrame.Database._open_with_clock(
        db_path, clock, snapshot_every_entries=None
    ) as db:
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))
        stamps = recorded_at(db, "a")

    assert len(stamps) == 1
    assert stamps[0].startswith(START[:19])


def test_advancing_the_clock_moves_the_axis_and_valid_time_stays_put(db_path, clock):
    """The two axes move independently, and now a test can watch it happen.

    Two versions of one concept asserted at the *same* valid time a year apart
    in transaction time. That is the correction case — belief changed, the
    world did not — and before W6.3 the Python suite could state it only in
    prose.
    """
    with macrame.Database._open_with_clock(
        db_path, clock, snapshot_every_entries=None
    ) as db:
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))
        clock.advance(dt.timedelta(days=365))
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha, corrected", valid_from=T0))

        stamps = logged_at(db, "a")
        # The hot table carries the newer of the two and not both.
        assert len(recorded_at(db, "a")) == 1

    assert len(stamps) == 2
    assert stamps[0].startswith(START[:10])
    # A year on from START, and the boundary is not asserted to the day because
    # the advance is a duration rather than a calendar step.
    assert stamps[1].startswith("2027-05-31") or stamps[1].startswith("2027-06-01")


def test_advance_takes_seconds_as_well_as_a_timedelta(db_path, clock):
    """The same coercion `archive_windowed`'s window takes, for the same reason."""
    before = clock.peek()
    clock.advance(90)
    after = clock.peek()
    assert (after - before) == dt.timedelta(seconds=90)


def test_peek_does_not_issue_the_stamp_it_shows(clock):
    """Otherwise a test that looked at the clock would change what it measured."""
    assert clock.peek() == clock.peek()


def test_the_clock_cannot_be_wound_backwards(clock):
    """A backwards fake would let a test build a state the triggers forbid.

    `trg_concepts_monotonic_ra` exists to make a `recorded_at` regression
    unreachable. A clock that could go back would let a fixture reach it and
    then assert something about the result, which is testing a state the
    product does not have.
    """
    with pytest.raises(ValueError):
        clock.advance(-60)
    with pytest.raises(ValueError):
        clock.advance(dt.timedelta(seconds=-60))


def test_reopening_a_populated_file_raises_the_clock_to_what_is_stored(db_path):
    """Determinism yields to the monotonic contract, and that is not a bug.

    A fake set to 2020 against a ledger stamped at `START` would abort the
    first write on `trg_concepts_monotonic_ra`. `open_tuned` therefore raises
    the clock to the newest stored `recorded_at` first (`Clock::raise_floor`), so
    the stamps here are *after* the stored ones rather than the ones asked for.
    Tests wanting exact stamps start from an empty file, and this is the test
    that says why.
    """
    with macrame.Database._open_with_clock(
        db_path, _macrame._FakeClock(START), snapshot_every_entries=None
    ) as db:
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))

    behind = _macrame._FakeClock("2020-01-01T00:00:00.000000Z")
    with macrame.Database._open_with_clock(
        db_path, behind, snapshot_every_entries=None
    ) as db:
        db.upsert_concept(macrame.ConceptUpsert("b", "Bravo", valid_from=T0))
        stamps = recorded_at(db, "b")

    assert stamps[0].startswith(START[:10]), (
        "the injected 2020 clock was used as-is on a populated ledger; either "
        "raise_floor stopped being called at open, or the write that should "
        "have aborted did not"
    )


def test_the_seam_is_not_on_the_public_surface(clock):
    """§14.6's entry stands: this is a hook, not a supported way to open.

    If either of these ever becomes public, the objection recorded there —
    that a clock injected into a production ledger writes a `recorded_at` axis
    which no longer records anything — needs answering rather than inheriting.
    """
    assert not hasattr(macrame, "_FakeClock")
    assert "_FakeClock" not in macrame.__all__
    assert not any(name.startswith("open_with") for name in macrame.__all__)
    assert not hasattr(macrame.Database, "open_with_clock")


# ---------------------------------------------------------------------------
# W7.4 / D-178: a stored `recorded_at` from the future is refused at open.
# ---------------------------------------------------------------------------

FROM_THE_FUTURE = "2065-01-24T00:00:00.000000Z"


def _seed_from_the_future(db_path):
    """Leave a file whose newest `recorded_at` no clock here could have made."""
    with macrame.Database._open_with_clock(
        db_path, _macrame._FakeClock(FROM_THE_FUTURE), snapshot_every_entries=None
    ) as db:
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))


def test_reopening_a_file_stamped_from_the_future_is_refused(db_path):
    """The clock floor is inherited, so the stamp would spread (W7.4, D-178).

    Every other bad value in the ledger stays where it is. This one becomes the
    floor, every stamp issued afterwards lands at or after it, and those stamps
    are written — so the open after that reads the same floor back out of rows
    this library produced. Refused at the last point where a stamp the crate
    wrote can still be told from one it did not.

    The `_FakeClock` seam is what makes this reachable from Python at all, and
    a fixture escaping through it is the case §3.4 names by example.
    """
    _seed_from_the_future(db_path)

    with pytest.raises(macrame.FutureRecordedAtError) as excinfo:
        macrame.Database.open(db_path)

    assert excinfo.value.stamp.startswith("2065-")
    # It must say how to get in: the library that refuses the file is the only
    # thing that reads this schema.
    assert "allow" in str(excinfo.value)


def test_allow_opens_a_file_the_default_refuses(db_path):
    """The escape hatch is exercised, because a caller reaches for it once.

    Documented as a reading path and not a repair — writes made under it
    inherit the floor, which is the condition being refused.
    """
    _seed_from_the_future(db_path)

    with macrame.Database.open(
        db_path, future_stamps="allow", snapshot_every_entries=None
    ) as db:
        assert db.diagnostic_query("SELECT COUNT(*) FROM concepts")[0][0] == 1


def test_an_ordinary_file_reopens_untouched(db_path):
    """Half the claim, and the half a guard gets wrong quietly.

    A bound that also refused stamps a real clock produces would be caught only
    by tests that reopen, and most do not.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        db.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))
    with macrame.Database.open(db_path, snapshot_every_entries=None):
        pass


@pytest.mark.parametrize("bad", ["never", -1.0, object()])
def test_future_stamps_refuses_what_it_cannot_read(db_path, bad):
    """A negative tolerance is a sign error, not a request for zero.

    And an unrecognised string is not silently the default: absent means *the
    bound applies*, which is the one meaning that must not be reachable by
    accident.
    """
    with pytest.raises((ValueError, TypeError)):
        macrame.Database.open(db_path, future_stamps=bad)

