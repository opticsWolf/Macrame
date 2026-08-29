"""One lineage's handle, from Python (§15.4, W12.9, D-226).

``BranchView`` is the fifth holding of W6's convention — the binding ships in
the release that creates the feature, not the one after — and the first one
that is **not** a pyo3 class.

That is the finding worth pinning here. The Rust view wraps an
``Arc<Database>``, and the ``Arc`` is the design: ``Database::close`` takes
``self`` by value, so a view there *cannot* end the handle it reads through and
the restriction is structural rather than documented. Python has no move
semantics to build that out of — ``close()`` is a method on the ``Database``
object the caller already holds — so the Python view delivers the ergonomics
and not the guarantee, and says so. Writing it in Python rather than
duplicating the delegation in pyo3 is what keeps that honest: every method
passes ``branch=`` through to the binding and does nothing else.

It is also why there is no ``db.view(...)``: in Rust that method exists to
clone the ``Arc``, and here there is no ``Arc`` to clone. Second deliberate
asymmetry in the branch surface, after ``BranchId`` having no Python class.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2020-01-01T00:00:00.000000Z"
T1 = "2021-01-01T00:00:00.000000Z"
NOW = "2030-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in "abcd"]
        )
        yield handle


def edge(source, target, **kw):
    return macrame.EdgeAssertion(source, target, "LEADSTO", valid_from=T0, **kw)


def rows_on(db, branch):
    return db.diagnostic_query(
        "SELECT COUNT(*) FROM links WHERE branch_id = ?", [branch]
    )[0][0]


# ───────────────────────────────────────────────────────────────────────────
# The view is the branch, at every door
# ───────────────────────────────────────────────────────────────────────────


def test_every_write_through_a_view_lands_on_its_lineage(db):
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)

    view.assert_edge(edge("a", "b"))
    view.write_bulk_atomic([edge("b", "c")])
    view.bulk_import([edge("c", "d")])
    view.upsert_concept(macrame.ConceptUpsert("mine", "Mine", valid_from=T0))
    view.write_concepts([macrame.ConceptUpsert("mine2", "M2", valid_from=T0)])

    assert rows_on(db, "alt") == 3
    assert rows_on(db, "main") == 0
    assert db.diagnostic_query(
        "SELECT id FROM concepts WHERE branch_id = 'alt' ORDER BY id"
    ) == [("mine",), ("mine2",)]


def test_a_view_reads_what_passing_branch_by_hand_reads(db):
    db.assert_edge(edge("a", "b"))
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)
    view.assert_edge(edge("b", "c"))

    assert view.traverse_ids("a", max_depth=5) == db.traverse_ids(
        "a", max_depth=5, branch="alt"
    )
    assert view.traverse_ids("a", max_depth=5) == ["a", "b", "c"]
    # The trunk is unmoved by either.
    assert db.traverse_ids("a", max_depth=5) == ["a", "b"]

    g = view.load_subgraph("a", 5, 1_000_000, as_of_valid=NOW)
    assert g.edge_count() == 2


def test_the_view_reads_the_as_of_edges_of_its_own_lineage(db):
    """The sixth read on the view, and the last one Python was missing.

    The Rust view has had ``query_as_of_edges`` since 0.14.9; this one could not
    have it, because the binding underneath took no lineage until 0.14.10
    (D-227). One delegating line, like every other method here — the assertion
    is that it means the same thing as naming the branch by hand, and that the
    trunk is not moved by either.
    """
    db.assert_edge(edge("a", "b"))
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)
    view.assert_edge(edge("b", "c"))

    def pairs(rows):
        return sorted((e[0], e[1]) for e in rows)

    assert pairs(view.query_as_of_edges()) == pairs(
        db.query_as_of_edges(branch="alt")
    )
    assert pairs(view.query_as_of_edges()) == [("a", "b"), ("b", "c")]
    assert pairs(db.query_as_of_edges()) == [("a", "b")]


def test_a_view_of_the_trunk_is_the_trunk(db):
    db.fork("alt")
    trunk = macrame.BranchView(db, "main")

    trunk.assert_edge(edge("a", "b"))
    assert rows_on(db, "main") == 1
    assert rows_on(db, "alt") == 0
    assert trunk.traverse_ids("a", max_depth=5) == ["a", "b"]


def test_retiring_through_a_view_shadows_rather_than_touching_the_parent(db):
    db.assert_edge(edge("a", "b"))
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)

    view.retire_edge("a", "b", "LEADSTO", T0, T1)

    assert view.traverse_ids("a", max_depth=5, as_of_valid=NOW) == ["a"]
    assert db.traverse_ids("a", max_depth=5, as_of_valid=NOW) == ["a", "b"]
    assert rows_on(db, "alt") == 1
    assert rows_on(db, "main") == 1


# ───────────────────────────────────────────────────────────────────────────
# The refusal
# ───────────────────────────────────────────────────────────────────────────


def test_a_view_refuses_a_write_that_names_another_lineage(db):
    alt = db.fork("alt")
    db.fork("other")
    view = macrame.BranchView(db, alt.id)
    foreign = edge("a", "b", branch="other")

    calls = [
        lambda: view.assert_edge(foreign),
        lambda: view.write_bulk_atomic([edge("a", "b"), foreign]),
        lambda: view.bulk_import([foreign]),
        lambda: view.upsert_concept(
            macrame.ConceptUpsert("z", "Z", valid_from=T0, branch="other")
        ),
    ]
    for call in calls:
        with pytest.raises(macrame.BranchMismatchError) as caught:
            call()
        assert caught.value.view == "alt"
        assert caught.value.named == "other"
        assert isinstance(caught.value, macrame.BranchError)

    assert rows_on(db, "alt") == 0
    assert rows_on(db, "other") == 0


def test_a_view_accepts_a_write_that_names_its_own_lineage(db):
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)
    # Redundant but not wrong: the caller said what the view already says.
    view.assert_edge(edge("a", "b", branch="alt"))
    assert rows_on(db, "alt") == 1


def test_a_view_of_an_unregistered_lineage_is_refused_at_first_use_by_name(db):
    db.fork("alt")
    # Construction cannot fail and does no I/O — the check is the operation's.
    ghost = macrame.BranchView(db, "ghost")
    assert ghost.id == "ghost"

    for call in (
        lambda: ghost.assert_edge(edge("a", "b")),
        lambda: ghost.traverse_ids("a", max_depth=5),
    ):
        with pytest.raises(macrame.UnknownBranchError) as caught:
            call()
        assert caught.value.branch == "ghost"


# ───────────────────────────────────────────────────────────────────────────
# The stamping, and what it is built on
# ───────────────────────────────────────────────────────────────────────────


def test_on_branch_returns_a_new_object_and_keeps_every_field(db):
    e = macrame.EdgeAssertion(
        "a", "b", "LEADSTO", valid_from=T0, weight=7.5, properties='{"k":1}'
    )
    stamped = e.on_branch("alt")
    assert e.branch is None, "the original is not mutated"
    assert stamped.branch == "alt"
    assert (stamped.source, stamped.target, stamped.edge_type) == ("a", "b", "LEADSTO")
    assert stamped.weight == 7.5
    assert stamped.properties == '{"k":1}'

    c = macrame.ConceptUpsert("z", "Z", valid_from=T0, content="body")
    stamped_c = c.on_branch("alt")
    assert c.branch is None
    assert stamped_c.branch == "alt"
    assert stamped_c.content == "body"


def test_a_name_the_ledger_cannot_accept_is_refused_at_the_stamp(db):
    with pytest.raises(macrame.InvalidBranchIdError):
        macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0).on_branch("alt ")


# ───────────────────────────────────────────────────────────────────────────
# The lifecycle it deliberately does not have
# ───────────────────────────────────────────────────────────────────────────


def test_the_view_is_a_wrapper_and_the_handle_is_the_same_object(db):
    alt = db.fork("alt")
    a = macrame.BranchView(db, alt.id)
    b = macrame.BranchView(db, alt.id)
    c = macrame.BranchView(db, "main")

    assert a == b
    assert a != c
    assert a.database is db, "no copy: the view holds the handle the caller passed"
    assert repr(a).startswith("BranchView(branch='alt'")


def test_the_view_diffs_from_its_own_lineage(db):
    """The sixth read on the view, and the only one whose direction matters.

    ``view.diff(other)`` is ``db.diff(view.id, other)`` — this lineage on the
    left. An edge only the *other* side holds comes back from the call with the
    arguments the other way round, which is a fact about diffs and not about
    the view, and the view does nothing to hide it.
    """
    alt = db.fork("alt")
    view = macrame.BranchView(db, alt.id)
    view.assert_edge(edge("a", "b"))

    assert view.diff("main") == db.diff("alt", "main")
    (row,) = view.diff("main")
    assert (row.source_id, row.target_id, row.branch_id) == ("a", "b", "alt")
    assert view.diff(alt.id) == []
