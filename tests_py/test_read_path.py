"""P4.2 acceptance: the read path.

The traversal's SQL and the subgraph loader's byte accounting are the crate's,
covered by the Rust suite. What is new here is that the *pairing* rules survive
the boundary — the ones that exist because a wrong answer would otherwise arrive
looking like a right one:

- ``as_of`` without a stated ``attribute_mode`` raises rather than defaulting
  (D-085).
- ``AttributeMode.OMIT`` on ``traverse`` is refused rather than answering with an
  empty list nobody can interpret.
- an unstated ``min_weight`` on ``load_subgraph`` lets a negative weight reach
  the guard, and a stated one filters it (D-073).

Plus the thing D-097 is about: a ``Subgraph`` answers questions without being
copied into Python.
"""

from __future__ import annotations

import datetime as dt

import pytest

import macrame

UTC = dt.timezone.utc
T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-06-01T00:00:00.000000Z"
T2 = "2026-09-01T00:00:00.000000Z"

# Large enough that nothing in this file is refused for size; the refusal has a
# test of its own that sets the budget deliberately low.
ROOMY = 1 << 20


@pytest.fixture
def db(db_path):
    """A → B → C by ``CITES``, plus A → D by ``KNOWS``.

    Two edge types because the filter tests need one to exclude, and a chain
    three deep because ``max_depth`` is only meaningful past two.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [
                macrame.ConceptUpsert(n, n.upper(), valid_from=T0, content=f"body of {n}")
                for n in ("a", "b", "c", "d")
            ]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion("a", "b", "CITES", valid_from=T0, weight=1.0),
                macrame.EdgeAssertion("b", "c", "CITES", valid_from=T0, weight=0.5),
                macrame.EdgeAssertion("a", "d", "KNOWS", valid_from=T0, weight=0.25),
            ]
        )
        yield handle


# --------------------------------------------------------------------------
# traverse_ids
# --------------------------------------------------------------------------


def test_traversal_reaches_the_whole_component(db):
    assert db.traverse_ids("a", max_depth=3) == ["a", "b", "c", "d"]


def test_depth_bounds_the_walk(db):
    assert db.traverse_ids("a", max_depth=1) == ["a", "b", "d"]


def test_edge_types_filter_the_walk(db):
    assert db.traverse_ids("a", max_depth=3, edge_types=["CITES"]) == ["a", "b", "c"]


def test_min_weight_filters_the_walk(db):
    # KNOWS at 0.25 falls below; CITES at 0.5 and 1.0 do not.
    assert db.traverse_ids("a", max_depth=3, min_weight=0.5) == ["a", "b", "c"]


def test_a_start_node_that_does_not_exist_reaches_nothing(db):
    # Not even itself. The CTE's seed is a row of `concepts`, so an id nobody
    # asserted has nothing to seed from -- whereas a real concept with no edges
    # comes back alone. The two cases are distinguishable, which is what makes
    # the empty list readable.
    assert db.traverse_ids("nope") == []
    assert db.traverse_ids("d") == ["d"]


def test_now_defaults_to_the_handles_clock(db):
    # Passing the present explicitly and not passing it must agree.
    now = dt.datetime.now(UTC)
    assert db.traverse_ids("a", max_depth=3, now=now) == db.traverse_ids("a", max_depth=3)


def test_a_traversal_before_the_edges_existed_reaches_nothing(db):
    before = "2025-01-01T00:00:00.000000Z"
    assert db.traverse_ids("a", max_depth=3, now=before) == ["a"]


# --------------------------------------------------------------------------
# traverse, and the as_of / attribute_mode pairing
# --------------------------------------------------------------------------


def test_traverse_hydrates_attributes(db):
    got = db.traverse("a", max_depth=3, attribute_mode=macrame.AttributeMode.CURRENT)
    assert [n.id for n in got] == ["a", "b", "c", "d"]
    assert got[0].title == "A"
    assert got[0].content == "body of a"
    assert got[0].embedding_model is None


def test_attribute_mode_may_be_omitted_when_as_of_is(db):
    # The requirement is on the *pairing*. A live traversal with no as_of has
    # only one sensible reading, and D-085 did not make it an error.
    assert [n.id for n in db.traverse("a", max_depth=1)] == ["a", "b", "d"]


def test_as_of_without_an_attribute_mode_raises(db):
    with pytest.raises(macrame.AttributeModeUnstatedError) as e:
        db.traverse("a", as_of=T1)
    assert e.value.as_of == T1


def test_as_of_with_a_stated_mode_is_accepted(db):
    got = db.traverse("a", max_depth=3, as_of=T1, attribute_mode=macrame.AttributeMode.CURRENT)
    assert [n.id for n in got] == ["a", "b", "c", "d"]


def test_the_two_axes_are_visible_from_python(db):
    """Valid time and transaction time, disagreeing -- which is the point.

    Everything here was *asserted* to be true from T0 (valid time) but was
    *recorded* when this test ran, in 2026-08. So at T1:

    - the topology is there, because the edges claim to have been true then;
    - ``CURRENT`` hydrates it, because live text is live regardless of instant;
    - ``AT_TIME`` finds nothing, because on 2026-06-01 nobody had written it
      down yet.

    That is Doctrine II, and the empty list is the correct answer rather than a
    gap. It is also the concrete reason D-085 refuses to default the mode: these
    two calls differ by one keyword and disagree completely.
    """
    assert db.traverse_ids("a", max_depth=3, as_of=T1) == ["a", "b", "c", "d"]
    live = db.traverse("a", max_depth=3, as_of=T1, attribute_mode=macrame.AttributeMode.CURRENT)
    believed = db.traverse("a", max_depth=3, as_of=T1, attribute_mode=macrame.AttributeMode.AT_TIME)
    assert [n.id for n in live] == ["a", "b", "c", "d"]
    assert believed == []


def test_as_of_accepts_an_aware_datetime(db):
    got = db.traverse(
        "a",
        max_depth=1,
        as_of=dt.datetime(2026, 6, 1, tzinfo=UTC),
        attribute_mode=macrame.AttributeMode.CURRENT,
    )
    assert [n.id for n in got] == ["a", "b", "d"]


def test_a_naive_datetime_is_refused_on_as_of_too(db):
    # P3's rule is not re-implemented per call site; this asserts it reaches
    # this one.
    with pytest.raises(macrame.InvalidTimestampError, match="naive"):
        db.traverse("a", as_of=dt.datetime(2026, 6, 1), attribute_mode=macrame.AttributeMode.CURRENT)


def test_omit_on_traverse_is_refused_and_names_the_alternative(db):
    with pytest.raises(ValueError, match="traverse_ids"):
        db.traverse("a", attribute_mode=macrame.AttributeMode.OMIT)


def test_traverse_ids_is_what_omit_is_for(db):
    # The refusal above is only defensible because this returns the answer.
    assert db.traverse_ids("a", max_depth=1) == ["a", "b", "d"]


# --------------------------------------------------------------------------
# load_subgraph
# --------------------------------------------------------------------------


def test_a_subgraph_carries_nodes_and_both_adjacencies(db):
    g = db.load_subgraph("a", 3, ROOMY)
    assert len(g) == 4
    assert "c" in g
    assert "nope" not in g
    assert g.edge_count() == 3
    assert g.total_weight() == pytest.approx(1.75)
    assert g.is_closed()


def test_out_and_in_edges_point_at_the_other_end(db):
    g = db.load_subgraph("a", 3, ROOMY)
    out = g.out_edges("a")
    assert sorted((e.node, e.edge_type) for e in out) == [("b", "CITES"), ("d", "KNOWS")]
    assert [e.node for e in g.in_edges("b")] == ["a"]
    assert g.out_edges("d") == []


def test_degree_counts_both_directions(db):
    g = db.load_subgraph("a", 3, ROOMY)
    assert g.degree("b") == 2  # a->b in, b->c out
    assert g.degree("a") == 2  # two out, none in
    assert g.weighted_degree("b") == pytest.approx(1.5)


def test_an_absent_node_has_no_edges_rather_than_an_error(db):
    g = db.load_subgraph("a", 3, ROOMY)
    assert g.out_edges("nope") == []
    assert g.degree("nope") == 0
    assert g.node("nope") is None


def test_iteration_yields_node_ids_in_order(db):
    g = db.load_subgraph("a", 3, ROOMY)
    assert list(g) == ["a", "b", "c", "d"]


def test_node_returns_the_hydrated_concept(db):
    g = db.load_subgraph("a", 3, ROOMY)
    n = g.node("a")
    assert n.title == "A"
    assert n.valid_from == dt.datetime(2026, 1, 1, tzinfo=UTC)
    assert n.valid_to is None  # the open sentinel, as None

    # `content` is **not loaded by default** as of 0.8.0 (B3, D-116), and `None`
    # means "not loaded" rather than "empty" — a sentinel that is a valid value
    # of the type could not be told apart from a genuinely empty document.
    #
    # This asserted `n.content == "body of a"` until B3. There is currently **no
    # way to request content through the binding**: `load_subgraph` grows a
    # `content=` keyword in B7, which is where the stub, `s14` and the README
    # example are updated too. Until then this is a real gap in the Python
    # surface, and it is named here rather than left for someone to discover.
    assert n.content is None


def test_edge_intervals_render_as_datetimes_with_none_for_open(db):
    g = db.load_subgraph("a", 3, ROOMY)
    e = g.out_edges("a")[0]
    assert e.valid_from == dt.datetime(2026, 1, 1, tzinfo=UTC)
    assert e.valid_to is None


def test_to_dict_is_a_copy_and_is_opt_in(db):
    g = db.load_subgraph("a", 3, ROOMY)
    d = g.to_dict()
    assert set(d) == {"nodes", "out_adj", "in_adj"}
    assert set(d["nodes"]) == {"a", "b", "c", "d"}
    assert d["nodes"]["b"].title == "B"
    assert [e.node for e in d["out_adj"]["a"]] == ["b", "d"]
    # in_adj carries no key for a node nothing points at.
    assert "a" not in d["in_adj"]


def test_estimated_bytes_is_positive_and_under_a_roomy_budget(db):
    g = db.load_subgraph("a", 3, ROOMY)
    assert 0 < g.estimated_bytes() < ROOMY


def test_a_budget_that_does_not_fit_is_refused(db):
    with pytest.raises(macrame.SubgraphTooLargeError) as e:
        db.load_subgraph("a", 3, 1)
    assert e.value.budget == 1


def test_edge_types_filter_the_walk_and_the_returned_adjacency(db):
    # D-073's decision: filtering only the walk would hand a caller who asked
    # for CITES a graph reached via CITES and populated with KNOWS as well.
    g = db.load_subgraph("a", 3, ROOMY, edge_types=["CITES"])
    assert "d" not in g
    assert [e.edge_type for e in g.out_edges("a")] == ["CITES"]


# --------------------------------------------------------------------------
# The min_weight default, which is not the traversal's
# --------------------------------------------------------------------------


def test_a_negative_weight_cannot_be_written_through_this_binding(db):
    """Which is why the guard has no positive test here, and that is the finding.

    ``load_subgraph``'s unstated ``min_weight`` is ``-inf`` so that a negative
    weight reaches ``NegativeEdgeWeightError`` instead of being silently dropped
    — Dijkstra and A* are unsound over them (D-039). As of 0.6.0 that guard is
    **unreachable from a ledger this binding wrote**: T2.1 put
    ``CHECK (weight >= 0.0 …)`` on ``links``, so the row is refused at the write.

    It stays reachable for a file migrated from before v7, whose historical rows
    predate the constraint, and ``links_current`` carries no weight check of its
    own. So the mapping is kept as the crate has it rather than simplified on the
    strength of a constraint that only holds for new writes.
    """
    with pytest.raises(macrame.MacrameError):
        db.assert_edge(
            macrame.EdgeAssertion("a", "c", "NEGATIVE", valid_from=T2, weight=-1.0)
        )


def test_a_stated_min_weight_excludes_rather_than_refuses(db):
    g = db.load_subgraph("a", 3, ROOMY, min_weight=0.5)
    assert "d" not in g  # KNOWS at 0.25
    assert g.edge_count() == 2


# --------------------------------------------------------------------------
# Lifecycle
# --------------------------------------------------------------------------


def test_reads_on_a_closed_handle_raise(db_path):
    handle = macrame.Database.open(db_path, snapshot_every_entries=None)
    handle.close()
    for call in (
        lambda: handle.traverse_ids("a"),
        lambda: handle.traverse("a"),
        lambda: handle.load_subgraph("a", 1, ROOMY),
    ):
        with pytest.raises(macrame.MacrameClosedError):
            call()


def test_a_subgraph_outlives_the_handle_that_loaded_it(db_path):
    """It is a value, not a cursor.

    Worth asserting because the opaque-handle shape invites the opposite
    assumption — that it holds a connection and dies with the database.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts([macrame.ConceptUpsert("a", "A", valid_from=T0)])
        g = handle.load_subgraph("a", 1, ROOMY)
    assert len(g) == 1
    assert g.node("a").title == "A"
