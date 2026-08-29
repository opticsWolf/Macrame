"""Writing on a lineage, from Python (§15.4, W12.8, D-225).

``test_branches.py`` opens with a paragraph headed *the half that is
deliberately absent*: no write there names a branch, because at 0.14.7 none
could. This file is that half, and it is the fourth holding of W6's convention
— the binding ships in the release that creates the feature, not the one after.

The lineage is a keyword rather than a class. ``branch=`` on ``EdgeAssertion``
and ``ConceptUpsert`` is the same keyword the four traversal entry points have
taken since 0.14.4, because it is the same question asked of the other half of
the ledger: *which lineage is this about*. ``BranchId`` deliberately has no
Python class (D-224) — the Rust type earns its keep by making the validated
form unforgeable through the type system, and Python has no equivalent to
enforce, so validation happens at the boundary and raises
``InvalidBranchIdError``.

**``retire_edge`` takes ``branch=`` where Rust has two methods.** Rust splits
into ``retire_edge`` and ``retire_edge_on`` because a sixth positional argument
would make every existing call site read as though it had made a lineage
decision it never made. Python has keyword defaults, so the split would buy
nothing and cost a second name to keep in step.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2020-01-01T00:00:00.000000Z"
T1 = "2021-01-01T00:00:00.000000Z"
T2 = "2022-01-01T00:00:00.000000Z"
NOW = "2030-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in "abcd"]
        )
        yield handle


def rows_on(db, branch):
    return db.diagnostic_query(
        "SELECT COUNT(*) FROM links WHERE branch_id = ?", [branch]
    )[0][0]


# ───────────────────────────────────────────────────────────────────────────
# The write lands where it says it does
# ───────────────────────────────────────────────────────────────────────────


def test_an_edge_asserted_on_a_branch_is_invisible_to_the_trunk(db):
    db.assert_edge(macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0))
    alt = db.fork("alt")
    db.assert_edge(
        macrame.EdgeAssertion("b", "c", "LEADSTO", valid_from=T0, branch=alt.id)
    )

    assert db.traverse_ids("a", max_depth=5) == ["a", "b"]
    assert db.traverse_ids("a", max_depth=5, branch=alt.id) == ["a", "b", "c"]
    assert rows_on(db, "alt") == 1
    assert rows_on(db, "main") == 1


def test_the_keyword_round_trips_on_the_assertion(db):
    plain = macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0)
    named = macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, branch="alt")
    assert plain.branch is None
    assert named.branch == "alt"

    concept = macrame.ConceptUpsert("z", "Z", valid_from=T0, branch="alt")
    assert concept.branch == "alt"
    assert macrame.ConceptUpsert("z", "Z", valid_from=T0).branch is None


def test_a_branch_supersedes_an_inherited_edge_by_writing_beside_it(db):
    db.assert_edge(
        macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, weight=1.0)
    )
    alt = db.fork("alt")
    db.assert_edge(
        macrame.EdgeAssertion(
            "a", "b", "LEADSTO", valid_from=T0, weight=9.0, branch=alt.id
        )
    )

    # Two rows for one edge key: the parent's survives the branch's write.
    assert (
        db.diagnostic_query(
            "SELECT COUNT(*) FROM links_current "
            "WHERE source_id = 'a' AND target_id = 'b'"
        )[0][0]
        == 2
    )

    on_branch = db.load_subgraph("a", 1, 1_000_000, as_of_valid=NOW, branch=alt.id)
    assert on_branch.out_edges("a")[0].weight == 9.0
    trunk = db.load_subgraph("a", 1, 1_000_000, as_of_valid=NOW)
    assert trunk.out_edges("a")[0].weight == 1.0


# ───────────────────────────────────────────────────────────────────────────
# Shadow retirement
# ───────────────────────────────────────────────────────────────────────────


def test_retiring_an_inherited_edge_shadows_it_and_leaves_the_parent_alone(db):
    db.assert_edge(macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0))
    alt = db.fork("alt")

    db.retire_edge("a", "b", "LEADSTO", T0, T1, branch=alt.id)

    assert db.traverse_ids("a", max_depth=5, as_of_valid=NOW, branch=alt.id) == ["a"]
    assert db.traverse_ids("a", max_depth=5, as_of_valid=NOW) == ["a", "b"]
    # The row that closed it carries the branch's id; the parent's is untouched.
    assert rows_on(db, "alt") == 1


def test_retiring_without_a_branch_is_still_the_trunks_own_retirement(db):
    db.assert_edge(macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0))
    db.fork("alt")
    db.retire_edge("a", "b", "LEADSTO", T0, T1)
    assert db.traverse_ids("a", max_depth=5, as_of_valid=NOW) == ["a"]
    assert rows_on(db, "alt") == 0


# ───────────────────────────────────────────────────────────────────────────
# The overlap guard, in both directions
# ───────────────────────────────────────────────────────────────────────────


def test_a_branch_may_not_overlap_an_interval_it_inherited(db):
    db.assert_edge(
        macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, valid_to=T2)
    )
    alt = db.fork("alt")

    with pytest.raises(macrame.OverlappingIntervalError) as caught:
        db.assert_edge(
            macrame.EdgeAssertion(
                "a", "b", "LEADSTO", valid_from=T1, valid_to=NOW, branch=alt.id
            )
        )
    assert caught.value.source_id == "a"


def test_the_trunk_is_not_refused_for_overlapping_what_a_branch_believes(db):
    alt = db.fork("alt")
    db.assert_edge(
        macrame.EdgeAssertion(
            "a", "b", "LEADSTO", valid_from=T0, valid_to=T2, branch=alt.id
        )
    )
    # The guard read `links_current` with no lineage predicate until 0.14.8 and
    # would have found the branch's row here. The trunk cannot see it.
    db.assert_edge(
        macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T1, valid_to=NOW)
    )
    assert rows_on(db, "main") == 1
    assert rows_on(db, "alt") == 1


# ───────────────────────────────────────────────────────────────────────────
# Refusals
# ───────────────────────────────────────────────────────────────────────────


def test_every_write_refuses_an_unregistered_lineage_by_name(db):
    calls = [
        lambda: db.assert_edge(
            macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, branch="ghost")
        ),
        lambda: db.retire_edge("a", "b", "LEADSTO", T0, T1, branch="ghost"),
        lambda: db.upsert_concept(
            macrame.ConceptUpsert("z", "Z", valid_from=T0, branch="ghost")
        ),
        lambda: db.write_bulk_atomic(
            [macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, branch="ghost")]
        ),
    ]
    for call in calls:
        with pytest.raises(macrame.UnknownBranchError) as caught:
            call()
        assert caught.value.branch == "ghost"

    assert rows_on(db, "main") == 0


def test_a_name_the_ledger_cannot_accept_is_refused_at_the_constructor(db):
    # The validation is at the boundary because there is no `BranchId` class to
    # carry it, which is the one asymmetry D-224 records deliberately.
    with pytest.raises(macrame.InvalidBranchIdError):
        macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, branch="alt ")
    with pytest.raises(macrame.InvalidBranchIdError):
        macrame.ConceptUpsert("z", "Z", valid_from=T0, branch="alt ")


def test_a_branch_mints_a_concept_and_may_not_restate_an_inherited_one(db):
    alt = db.fork("alt")
    db.upsert_concept(macrame.ConceptUpsert("mine", "Mine", valid_from=T0, branch=alt.id))

    with pytest.raises(macrame.CrossLineageError) as caught:
        db.upsert_concept(
            macrame.ConceptUpsert("a", "Renamed", valid_from=T0, branch=alt.id)
        )
    assert caught.value.id == "a"
    assert caught.value.held_by == "main"
    assert caught.value.attempted == "alt"
    assert isinstance(caught.value, macrame.BranchError)


# ───────────────────────────────────────────────────────────────────────────
# Batches
# ───────────────────────────────────────────────────────────────────────────


def test_a_bulk_import_lands_each_edge_on_the_lineage_it_names(db):
    alt = db.fork("alt")
    assert (
        db.bulk_import(
            [
                macrame.EdgeAssertion("a", "b", "LEADSTO", valid_from=T0, branch=alt.id),
                macrame.EdgeAssertion("b", "c", "LEADSTO", valid_from=T0, branch=alt.id),
                macrame.EdgeAssertion("c", "d", "LEADSTO", valid_from=T0),
            ]
        )
        == 3
    )
    assert rows_on(db, "alt") == 2
    assert rows_on(db, "main") == 1


def test_a_batch_contradicts_itself_within_a_lineage_and_not_across_two(db):
    alt = db.fork("alt")
    with pytest.raises(macrame.OverlappingIntervalError):
        db.write_bulk_atomic(
            [
                macrame.EdgeAssertion(
                    "a", "b", "LEADSTO", valid_from=T0, valid_to=NOW, branch=alt.id
                ),
                macrame.EdgeAssertion(
                    "a", "b", "LEADSTO", valid_from=T1, valid_to=NOW, branch=alt.id
                ),
            ]
        )

    # The same two intervals on two lineages are two beliefs, not a contradiction.
    assert (
        db.write_bulk_atomic(
            [
                macrame.EdgeAssertion(
                    "a", "b", "LEADSTO", valid_from=T0, valid_to=NOW, branch=alt.id
                ),
                macrame.EdgeAssertion(
                    "a", "b", "LEADSTO", valid_from=T1, valid_to=NOW
                ),
            ]
        )
        == 2
    )
