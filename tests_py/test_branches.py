"""The lineage surface, from Python (§15.4, W12.7).

Until this release a second lineage could not be built from this side at all —
``test_read_path.py``'s branch section says so of itself — so the branch tests
here could only pin the *keyword*. ``db.fork()`` closes that, and these are the
first Python assertions about a real fork: that it costs one row, that the
branch reads its parent's history and stops at the fork point, and that the
three refusals arrive as their own classes rather than as a bare
``MacrameError``.

**The half that lives next door.** No write here names a branch. Through
0.14.7 that was because none could; since 0.14.8 it is because
``test_branch_writes.py`` is where writing on a lineage is pinned, and this file
is about the lifecycle — what a fork costs, what it reads, and how it refuses.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2020-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in "abcd"]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0),
                macrame.EdgeAssertion("b", "c", "LEADSTO", valid_from=T0),
            ]
        )
        yield handle


def ledger_counts(db):
    """Every table a fork must leave alone."""
    return {
        table: db.diagnostic_query(f"SELECT COUNT(*) FROM {table}")[0][0]
        for table in ("links", "links_current", "concepts", "transaction_log")
    }


# ───────────────────────────────────────────────────────────────────────────
# The trunk, and what a fork costs
# ───────────────────────────────────────────────────────────────────────────


def test_a_ledger_that_never_forked_still_reports_its_lineage(db):
    """The trunk is a row, not an absence a caller has to know means ``main``."""
    (trunk,) = db.branches()
    assert trunk.id == "main"
    assert trunk.is_main
    assert trunk.parent is None
    assert trunk.forked_at is None


def test_a_fork_is_one_row_and_touches_no_ledger_table(db):
    """§17 acceptance 1, from this side.

    The count that matters is not ``branches``. It is the other four, which must
    be identical before and after: a design that copied the parent's projection
    would move ``links_current``, and one that logged the fork as a ledger act
    would move ``transaction_log``.
    """
    before = ledger_counts(db)
    for i in range(100):
        db.fork(f"alt/{i}")
    assert ledger_counts(db) == before
    assert len(db.branches()) == 101


def test_fork_defaults_to_the_trunk(db):
    """``from`` is optional, because the overwhelmingly common parent is ``main``."""
    alt = db.fork("alt")
    assert alt.parent == "main"
    assert not alt.is_main


def test_the_listing_is_trunk_first_then_creation_order(db):
    for name in ("first", "second"):
        db.fork(name)
    db.fork("third", "second")
    assert [b.id for b in db.branches()] == ["main", "first", "second", "third"]


# ───────────────────────────────────────────────────────────────────────────
# The two halves, composed
# ───────────────────────────────────────────────────────────────────────────


def test_a_fork_reads_its_parents_history_and_stops_at_the_fork_point(db):
    """Fork, churn the trunk, read the branch — D-223 through the binding.

    Before this release every test of this shape was in Rust and built its
    ``branches`` row by hand, so the fork instant was a constant the test chose.
    Here it is whatever ``fork`` stamped, which is the value the reader compares
    against.
    """
    alt = db.fork("alt")

    # The trunk moves on after the fork.
    db.assert_edge(macrame.EdgeAssertion("c", "d", "LEADSTO", valid_from=T0))

    assert db.traverse_ids("a", max_depth=5) == ["a", "b", "c", "d"]
    assert db.traverse_ids("a", max_depth=5, branch=alt.id) == ["a", "b", "c"]


def test_the_fork_point_is_a_datetime_and_the_trunks_is_none(db):
    """P3's timestamp rule reaches these fields like every other instant."""
    import datetime as dt

    alt = db.fork("alt")
    assert isinstance(alt.forked_at, dt.datetime)
    assert alt.forked_at.tzinfo is not None
    assert alt.forked_at == alt.created_at

    (trunk,) = [b for b in db.branches() if b.is_main]
    assert trunk.forked_at is None
    assert isinstance(trunk.created_at, dt.datetime)


def test_repr_says_which_lineage_and_where_it_was_cut(db):
    alt = db.fork("alt")
    assert "alt" in repr(alt) and "main" in repr(alt)
    (trunk,) = [b for b in db.branches() if b.is_main]
    assert "trunk" in repr(trunk)


# ───────────────────────────────────────────────────────────────────────────
# The refusals, each as its own class
# ───────────────────────────────────────────────────────────────────────────


def test_an_unknown_parent_is_named(db):
    with pytest.raises(macrame.UnknownBranchError, match="ghost") as exc:
        db.fork("alt", "ghost")
    assert exc.value.branch == "ghost"
    assert len(db.branches()) == 1, "the refused fork left a row behind"


def test_a_taken_name_is_refused_rather_than_returning_the_other_branch(db):
    """An ignored insert would hand back a lineage with a different parent."""
    db.fork("alt")
    db.fork("other")
    with pytest.raises(macrame.BranchExistsError, match="alt"):
        db.fork("alt", "other")
    (alt,) = [b for b in db.branches() if b.id == "alt"]
    assert alt.parent == "main"


def test_the_trunk_cannot_be_forked_into_existence_twice(db):
    with pytest.raises(macrame.BranchExistsError, match="main"):
        db.fork("main")


@pytest.mark.parametrize("bad", ["", " release", "release ", "rel\tease", "x" * 129])
def test_a_name_the_ledger_cannot_accept_is_refused_at_the_call(db, bad):
    """And with its own class, so a batch of user input can catch validation.

    The whitespace pair is the reason the rule exists: ``branches`` is
    append-only, so a name with a trailing space is not a typo anyone can fix
    afterwards — it is a second lineage that prints as the first.
    """
    with pytest.raises(macrame.InvalidBranchIdError):
        db.fork(bad)
    assert len(db.branches()) == 1


@pytest.mark.parametrize(
    "ok", ["b9", "3f2504e0-4f89-11d3-9a0c-0305e82c3301", "turn/17/alt/3", "Release-1"]
)
def test_the_rule_is_wider_than_the_model_name_rule(db, ok):
    """A UUID and a path-like turn id are what the use case actually generates.

    ``ModelName`` refuses all four, and for a reason that does not apply here: a
    model name is spliced into a table identifier, and a branch id is always a
    bound value.
    """
    assert db.fork(ok).id == ok


def test_the_branch_errors_are_catchable_as_a_group(db):
    """``BranchError`` groups the lifecycle failures; validation stays with
    validation.

    ``InvalidBranchIdError`` is deliberately under ``ValidationError`` and not
    here: the hierarchy groups by what went wrong, not by which feature the call
    belonged to.
    """
    with pytest.raises(macrame.BranchError):
        db.fork("alt", "ghost")
    with pytest.raises(macrame.BranchError):
        db.fork("main")
    assert issubclass(macrame.InvalidBranchIdError, macrame.ValidationError)
    assert not issubclass(macrame.InvalidBranchIdError, macrame.BranchError)
    assert issubclass(macrame.BranchError, macrame.MacrameError)
