"""A ceiling that bounds work rather than the answer (C-8, W13.5, D-252).

The sixth holding of W6's convention. ``limit`` was left off ``ReadPlan`` in
0.15.9 on the grounds that a public field which silently does nothing is worse
than three loose arguments, and this is the release that gives it something to
do — on both sides at once.

**What is worth testing from Python is the pair, not the number.** ``limit``
alone would be a keyword that returns a shorter list, which is what the defect
being fixed already did; what makes it honest is ``traverse_ids_explained``,
because the walk's rows and the ids that survive its projection are different
counts and ``len(ids) == limit`` therefore cannot answer whether the ceiling
bit. The fixture below is built so those two numbers actually differ.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"
NOW = "2026-06-01T00:00:00.000000Z"


def edge(source, target):
    return macrame.EdgeAssertion(source, target, "KNOWS", valid_from=T0, weight=1.0)


@pytest.fixture
def db(db_path):
    """`m0 -> z1..z3`, each `z -> a{z}1..3`: nearest nodes, alphabetically last.

    The shape separates the two places a ``LIMIT`` can go. Bounding the walk
    keeps ``m0`` and the ``z``s; bounding the sorted projection would keep the
    ``a``s, and the two answers share nothing.
    """
    ids = ["m0"] + [f"z{z}" for z in (1, 2, 3)]
    ids += [f"a{z}{a}" for z in (1, 2, 3) for a in (1, 2, 3)]
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(i, i.upper(), valid_from=T0) for i in ids]
        )
        for z in (1, 2, 3):
            handle.assert_edge(edge("m0", f"z{z}"))
            for a in (1, 2, 3):
                handle.assert_edge(edge(f"z{z}", f"a{z}{a}"))
        yield handle


def test_a_limited_walk_keeps_the_nodes_nearest_the_start(db):
    """The cut falls on the walk's queue, not on the sorted projection.

    Both answers are four ids and only one of them is the near end, so this
    fails loudly if the ceiling ever moves outside the recursion.
    """
    assert db.traverse_ids("m0", max_depth=3, limit=4, now=NOW) == [
        "m0",
        "z1",
        "z2",
        "z3",
    ]


def test_a_ceiling_above_the_graph_changes_nothing(db):
    whole = db.traverse_ids("m0", max_depth=3, now=NOW)
    assert len(whole) == 13
    assert db.traverse_ids("m0", max_depth=3, limit=1000, now=NOW) == whole


def test_the_walk_says_whether_the_ceiling_bit(db):
    """`truncated` is the answer `len(ids)` cannot give.

    Three readings of the same traversal: no ceiling, a ceiling the walk never
    reaches, and one it does. The first two must be indistinguishable in the
    flag, or the flag is reporting "a limit was set" rather than "the limit was
    reached".
    """
    unbounded = db.traverse_ids_explained("m0", max_depth=3, now=NOW)
    assert unbounded[1] is False

    slack = db.traverse_ids_explained("m0", max_depth=3, limit=1000, now=NOW)
    assert slack == unbounded

    cut = db.traverse_ids_explained("m0", max_depth=3, limit=4, now=NOW)
    assert cut[0] == ["m0", "z1", "z2", "z3"]
    assert cut[1] is True


def test_traverse_ids_is_the_explained_call_without_the_answer(db):
    """The plain method must not be a second implementation of the walk."""
    for kwargs in ({}, {"limit": 4}, {"limit": 1000}):
        assert (
            db.traverse_ids("m0", max_depth=3, now=NOW, **kwargs)
            == db.traverse_ids_explained("m0", max_depth=3, now=NOW, **kwargs)[0]
        )


def test_a_hydrated_traversal_takes_the_ceiling_too(db):
    """`traverse` bounds its walk, and hydrates only what the walk returned."""
    nodes = db.traverse(
        "m0",
        max_depth=3,
        limit=4,
        attribute_mode=macrame.AttributeMode.CURRENT,
        now=NOW,
    )
    assert sorted(n.id for n in nodes) == ["m0", "z1", "z2", "z3"]


def test_a_plan_carries_the_ceiling(db):
    """`ReadPlan` gained a fourth field, and it is readable and comparable."""
    plan = macrame.ReadPlan(valid=NOW, limit=5)
    assert plan.limit == 5
    assert macrame.ReadPlan(valid=NOW).limit is None
    assert plan != macrame.ReadPlan(valid=NOW)
    assert "limit=Some(5)" in repr(plan)


def test_a_plan_ceiling_bounds_the_whole_ledger_read(db):
    """`edges` has no walk, so its ceiling is a plain LIMIT and an exact signal.

    The rows kept are arbitrary — that read states no order — so the assertion
    is on the count and on containment, which is all the surface promises.
    """
    every = db.edges(macrame.ReadPlan(valid=NOW))
    assert len(every) == 12

    some = db.edges(macrame.ReadPlan(valid=NOW, limit=5))
    assert len(some) == 5
    assert all(e in every for e in some)


def test_the_ceiling_is_not_offered_where_it_could_not_be_reported(db):
    """`load_subgraph` and `search_filtered` refuse the keyword, by construction.

    A subgraph's bound is `byte_budget`, which raises rather than truncating,
    and a filtered search's is `probe_cap`, which is this same ceiling under
    the name that surface already had. Neither takes a second one, and this
    records that as a decision rather than an omission.
    """
    with pytest.raises(TypeError):
        db.load_subgraph("m0", max_hops=2, limit=4)
