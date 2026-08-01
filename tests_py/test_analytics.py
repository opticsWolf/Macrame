"""P4.7 acceptance: the algorithms over a Subgraph, and P4.5/P4.6 alongside.

The algorithms themselves are the crate's. What is new at this boundary is
``astar``: it is the only method in these bindings that calls **into** Python
from Rust, which inverts P1's arrangement, and the callback signature it has to
satisfy — ``Fn(&str, &str) -> f64`` — cannot report failure. So the interesting
assertions here are about what happens when the heuristic misbehaves.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"
ROOMY = 1 << 20


@pytest.fixture
def graph(db_path):
    """Two triangles joined by one edge — enough for communities to be a
    question and for a shorter path to exist than the direct one."""
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        nodes = ["a", "b", "c", "x", "y", "z"]
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in nodes]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion(s, t, "LINK", valid_from=T0, weight=w)
                for s, t, w in [
                    ("a", "b", 1.0),
                    ("b", "c", 1.0),
                    ("a", "c", 5.0),  # the long way round
                    ("c", "x", 1.0),  # the bridge
                    ("x", "y", 1.0),
                    ("y", "z", 1.0),
                    ("x", "z", 1.0),
                ]
            ]
        )
        yield handle.load_subgraph("a", 6, ROOMY)


# --------------------------------------------------------------------------
# dijkstra
# --------------------------------------------------------------------------


def test_dijkstra_prefers_the_cheap_path(graph):
    d = graph.dijkstra("a")
    assert d["a"] == 0.0
    assert d["c"] == 2.0  # a->b->c, not the direct edge at 5.0


def test_unreachable_nodes_are_absent_rather_than_infinite(graph):
    # Nothing points back into `a`, so a walk from `z` reaches nothing else.
    d = graph.dijkstra("z")
    assert d == {"z": 0.0}
    assert "a" not in d


def test_dijkstra_from_a_node_that_is_not_here_is_empty(graph):
    assert graph.dijkstra("nope") == {}


# --------------------------------------------------------------------------
# astar
# --------------------------------------------------------------------------


def test_astar_with_no_heuristic_agrees_with_dijkstra(graph):
    cost, path = graph.astar("a", "z")
    assert cost == graph.dijkstra("a")["z"]
    assert path[0] == "a" and path[-1] == "z"


def test_astar_returns_the_path_inclusive_of_both_ends(graph):
    _, path = graph.astar("a", "c")
    assert path == ["a", "b", "c"]


def test_an_unreachable_goal_is_none(graph):
    assert graph.astar("z", "a") is None


def test_a_zero_heuristic_and_none_agree(graph):
    assert graph.astar("a", "z", lambda node, goal: 0.0) == graph.astar("a", "z")


def test_the_heuristic_receives_the_node_and_the_goal(graph):
    seen = []

    def h(node, goal):
        seen.append((node, goal))
        return 0.0

    graph.astar("a", "z", h)
    assert seen
    assert all(goal == "z" for _, goal in seen)
    assert seen[0][0] == "a"


def test_an_exception_from_the_heuristic_propagates(graph):
    """The callback signature cannot return a Result, so the error is captured
    and re-raised after the search — not swallowed, and not a panic."""

    def h(node, goal):
        raise KeyError(node)

    with pytest.raises(KeyError):
        graph.astar("a", "z", h)


def test_a_nan_heuristic_is_refused_rather_than_poisoning_the_queue(graph):
    with pytest.raises(ValueError, match="finite"):
        graph.astar("a", "z", lambda node, goal: float("nan"))


def test_an_infinite_heuristic_is_refused(graph):
    with pytest.raises(ValueError, match="finite"):
        graph.astar("a", "z", lambda node, goal: float("inf"))


def test_a_heuristic_returning_a_non_number_is_refused(graph):
    with pytest.raises(TypeError):
        graph.astar("a", "z", lambda node, goal: "close")


def test_an_admissible_heuristic_still_finds_the_shortest_path(graph):
    # Underestimating is allowed; the answer must not change.
    cost, _ = graph.astar("a", "z", lambda node, goal: 0.5)
    assert cost == graph.dijkstra("a")["z"]


# --------------------------------------------------------------------------
# scc, k_core, louvain, modularity
# --------------------------------------------------------------------------


def test_scc_partitions_every_node(graph):
    components = graph.scc()
    assert sorted(n for c in components for n in c) == ["a", "b", "c", "x", "y", "z"]


def test_k_core_is_a_set(graph):
    core = graph.k_core(2)
    assert isinstance(core, set)
    assert core <= set(graph)


def test_a_high_k_leaves_nothing(graph):
    assert graph.k_core(99) == set()


def test_k_core_of_zero_keeps_everything(graph):
    assert graph.k_core(0) == set(graph)


def test_louvain_assigns_every_node_a_community(graph):
    communities = graph.louvain()
    assert set(communities) == set(graph)
    assert all(isinstance(c, int) for c in communities.values())


def test_modularity_judges_a_partition_rather_than_trusting_louvain(graph):
    """The point of exposing `modularity` separately: a detector returning one
    node per community satisfies "modularity did not decrease" by *being* the
    singleton partition, and measuring Q is what tells the two apart."""
    singletons = {node: i for i, node in enumerate(graph)}
    assert graph.modularity(graph.louvain()) >= graph.modularity(singletons)


def test_modularity_of_an_arbitrary_partition_is_a_number(graph):
    assert isinstance(graph.modularity({node: 0 for node in graph}), float)


# --------------------------------------------------------------------------
# The graph is a value: the algorithms need no handle
# --------------------------------------------------------------------------


def test_algorithms_run_after_the_database_is_closed(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in ("a", "b")]
        )
        handle.assert_edge(macrame.EdgeAssertion("a", "b", "LINK", valid_from=T0))
        g = handle.load_subgraph("a", 2, ROOMY)
    assert g.dijkstra("a") == {"a": 0.0, "b": 1.0}
    assert g.astar("a", "b")[1] == ["a", "b"]
    assert g.louvain()
