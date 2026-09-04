"""One value says what a read asks for (§16, F-34, W13.4, D-251).

The fifth holding of W6's convention — the binding ships in the release that
creates the feature, not the one after — and the first where the Rust and
Python spellings deliberately differ. ``macrame::ReadPlan`` is a fluent builder
because Rust has no keyword arguments; ``ReadPlan(branch=..., valid=...)`` is
the same value with the scaffolding removed. What has to match is not the
spelling but the meaning: an unset field is the ordinary read on both sides,
and a plan is inert on both sides, so every refusal belongs to the read.

**The traversal entry points do not take ``plan=``, and that is a decision.**
They have taken ``as_of_valid=``, ``as_of_recorded=`` and ``branch=`` since
0.13.2 and 0.14.4. Adding a fourth keyword naming the same three would put two
spellings of one question in a single signature, with a rule about which wins —
which is the drift D-030 and D-035 are about, arriving as a convenience. Rust's
``TraversalBuilder::plan`` exists because Rust has no keywords to compose;
Python composes them at the call site with ``**``.
"""

from __future__ import annotations

import datetime as dt
from datetime import datetime, timezone

import pytest

import macrame
from macrame import _macrame

T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-02-01T00:00:00.000000Z"
T2 = "2026-03-01T00:00:00.000000Z"
NOW = "2026-06-01T00:00:00.000000Z"
# A driven clock, set in the past: a fixture ahead of the wall clock is
# refused on reopen (D-178), which is the trap test_clock.py records.
CLOCK_START = "2026-06-01T12:00:00.000000Z"


@pytest.fixture
def db(db_path):
    """Four concepts, `a -> b` open and `b -> c` closed at `T2`, on the trunk."""
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in "abcd"]
        )
        handle.assert_edge(edge("a", "b"))
        handle.assert_edge(edge("b", "c", valid_to=T2))
        yield handle


def edge(source, target, *, valid_to=None, weight=1.0, branch=None):
    return macrame.EdgeAssertion(
        source,
        target,
        "KNOWS",
        valid_from=T0,
        valid_to=valid_to,
        weight=weight,
        branch=branch,
    )


def keys(rows):
    return sorted((r[0], r[1]) for r in rows)


def test_an_empty_plan_is_the_ordinary_read(db):
    """Every default is the read a caller gets without asking for one.

    The closed interval carries its weight here: `b -> c` ends at `T2`, `NOW`
    is past it, and an inclusive upper bound would put it back.
    """
    assert keys(db.edges(macrame.ReadPlan())) == keys(
        db.query_as_of_edges()
    )
    assert keys(db.edges(macrame.ReadPlan(valid=NOW))) == [("a", "b")]
    assert keys(db.edges(macrame.ReadPlan(valid=T1))) == [("a", "b"), ("b", "c")]


def test_a_plan_and_the_tuple_reader_answer_alike(db):
    """`query_as_of_edges` is this read with `recorded` unset and the lineage
    dropped from each row — one statement, so the two cannot disagree."""
    db.fork("alt", "main")
    db.assert_edge(edge("a", "b", weight=4.0, branch="alt"))
    db.assert_edge(edge("c", "d"))

    for branch in (None, "alt"):
        assert keys(db.edges(macrame.ReadPlan(valid=T1, branch=branch))) == keys(
            db.query_as_of_edges(T1, branch=branch)
        )

    # And the fixture separates the lineages, or the equality above is between
    # two copies of one trivial answer: the trunk wrote `c -> d` after the fork.
    assert ("c", "d") in keys(db.edges(macrame.ReadPlan(valid=T1)))
    assert ("c", "d") not in keys(db.edges(macrame.ReadPlan(valid=T1, branch="alt")))


def test_a_row_says_which_lineage_holds_it(db):
    """The sixth field, which the five-tuple reader cannot carry.

    On a forked ledger `query_as_of_edges` can say *that* `a -> b` is visible
    and not *whose* it is, and nearest-ancestor resolution is exactly the thing
    a caller cannot reconstruct by filtering.
    """
    db.fork("alt", "main")
    db.assert_edge(edge("a", "b", weight=4.0, branch="alt"))

    rows = sorted((r[0], r[1], r[5]) for r in db.edges(macrame.ReadPlan(valid=T1, branch="alt")))
    assert rows == [("a", "b", "alt"), ("b", "c", "main")]


def test_a_recorded_instant_names_a_belief_no_other_edge_read_could_ask_for(db_path):
    """The third qualifier, which `query_as_of_edges` has no argument for.

    Before this release the question — *which edges did we believe existed, as
    of March, as they stood in January* — meant walking from a start node it
    does not have, or folding the whole log with `reconstruct` and filtering.

    **On a driven clock, not the wall clock.** The fold is `recorded_at <= ts`,
    so an instant taken from `datetime.now()` between two writes is only
    *usually* strictly between them — Windows' clock granularity is coarse
    enough that the second write can land on the same microsecond, and the test
    would then fail for a reason that has nothing to do with the read.
    """
    clock = _macrame._FakeClock(CLOCK_START)
    with macrame.Database._open_with_clock(
        db_path, clock, snapshot_every_entries=None
    ) as db:
        db.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in "abcd"]
        )
        db.assert_edge(edge("a", "b"))
        db.assert_edge(edge("b", "c", valid_to=T2))

        believed = clock.peek()
        clock.advance(dt.timedelta(days=1))
        db.assert_edge(edge("c", "d"))

        assert keys(db.edges(macrame.ReadPlan(valid=T1))) == [
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
        ]
        assert keys(db.edges(macrame.ReadPlan(valid=T1, recorded=believed))) == [
            ("a", "b"),
            ("b", "c"),
        ]

        # The valid axis is untouched by the belief: `b -> c` closed at `T2`.
        assert ("b", "c") not in keys(
            db.edges(macrame.ReadPlan(valid=NOW, recorded=believed))
        )


def test_a_plan_is_a_value(db):
    """Readable, comparable and printable — the point of having one at all."""
    tuesday = datetime(2026, 2, 1, tzinfo=timezone.utc)
    plan = macrame.ReadPlan(branch="main", valid=tuesday)

    assert plan.branch == "main"
    assert plan.valid == tuesday
    assert plan.recorded is None
    assert plan == macrame.ReadPlan(branch="main", valid=T1)
    assert plan != macrame.ReadPlan(branch="main")
    assert "ReadPlan(" in repr(plan)


def test_an_unset_instant_is_unset_rather_than_the_open_sentinel():
    """`valid_to=None` on an assertion means *still open*; `valid=None` on a
    plan means *no instant was named*. The binding canonicalises both and the
    two must not share a helper's default, or every unqualified read would ask
    for the end of time."""
    assert macrame.ReadPlan(valid=None).valid is None
    assert macrame.ReadPlan().valid is None


def test_a_plan_refuses_what_it_can_and_defers_what_it_cannot(db):
    """A malformed instant is the constructor's; an unregistered lineage is the
    read's, because only the database knows which lineages exist."""
    with pytest.raises(macrame.InvalidTimestampError):
        macrame.ReadPlan(valid="last Tuesday")
    with pytest.raises(macrame.InvalidBranchIdError):
        macrame.ReadPlan(branch="")

    ghost = macrame.ReadPlan(branch="ghost")  # builds fine
    with pytest.raises(macrame.UnknownBranchError):
        db.edges(ghost)
