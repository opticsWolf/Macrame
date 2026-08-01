"""P6 item 5: the phases wired together, in one workflow.

Every other file in this suite tests one phase. That is the right shape for
finding a broken method and the wrong shape for finding a broken *seam*: each
phase's fixture builds exactly what that phase needs, so nothing ever asks
whether a `Subgraph` loaded after an archive is still closed, or whether the
metrics counters see the whole of a session rather than one call.

**These are smoke tests. They prove the wiring, not the semantics.** The ledger's
own guarantees have 325 Rust tests; a Python re-assertion of bitemporality would
be a second, weaker copy free to drift. What is asserted here is only that one
phase's output is accepted by the next.

Ordering is deliberate and the tests are not independent of each other's
*reasoning*, though each builds its own ledger: they walk the lifecycle a real
application walks — populate, read, search, repair, archive, reconstruct, close.
"""

from __future__ import annotations

import datetime as dt
import struct

import pytest

import macrame

UTC = dt.timezone.utc
T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-03-01T00:00:00.000000Z"
ROOMY = 1 << 20
DIM = 4

# A small citation network: two clusters joined by one bridge, so the
# neighbourhood reads are non-trivial and the communities are a real question.
CONCEPTS = [
    ("bitemporal", "Bitemporal Modelling", "valid time and transaction time"),
    ("valid-time", "Valid Time", "when a fact was true in the world"),
    ("txn-time", "Transaction Time", "when a fact was recorded"),
    ("sqlite", "SQLite", "an embedded database engine"),
    ("libsql", "libSQL", "a fork of SQLite with vector search"),
]
EDGES = [
    ("bitemporal", "valid-time", "CITES", 1.0),
    ("bitemporal", "txn-time", "CITES", 1.0),
    ("valid-time", "txn-time", "RELATES", 0.5),
    ("bitemporal", "libsql", "RUNSON", 0.25),  # the bridge
    ("libsql", "sqlite", "FORKS", 1.0),
]


def vector_for(concept_id: str) -> bytes:
    """A deterministic unit-ish vector per concept, packed as the fast path takes."""
    seed = sum(ord(c) for c in concept_id)
    axis = seed % DIM
    return struct.pack(f"<{DIM}f", *[1.0 if i == axis else 0.0 for i in range(DIM)])


@pytest.fixture
def kb(db_path):
    """A populated ledger, built the way an application would build one."""
    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        db.write_concepts(
            [
                macrame.ConceptUpsert(cid, title, valid_from=T0, content=body)
                for cid, title, body in CONCEPTS
            ]
        )
        db.bulk_import(
            [
                macrame.EdgeAssertion(s, t, kind, valid_from=T0, weight=w)
                for s, t, kind, w in EDGES
            ]
        )
        db.register_model("mini", DIM)
        db.upsert_embeddings("mini", [(cid, vector_for(cid)) for cid, _, _ in CONCEPTS])
        db.rebuild_fts()
        yield db


def test_write_then_read_then_analyse(kb):
    """P4.1 → P4.2 → P4.7, the path most applications actually take."""
    assert kb.traverse_ids("bitemporal", max_depth=3) == [
        "bitemporal",
        "libsql",
        "sqlite",
        "txn-time",
        "valid-time",
    ]

    graph = kb.load_subgraph("bitemporal", 3, ROOMY)
    assert len(graph) == 5
    assert graph.is_closed()

    # The algorithms accept what the loader produced, which is the seam.
    distances = graph.dijkstra("bitemporal")
    assert distances["sqlite"] == pytest.approx(1.25)  # 0.25 bridge + 1.0 fork
    cost, path = graph.astar("bitemporal", "sqlite")
    assert cost == pytest.approx(distances["sqlite"])
    assert path == ["bitemporal", "libsql", "sqlite"]
    assert set(graph.louvain()) == set(graph)


def test_search_agrees_with_the_graph_it_searches(kb):
    """P4.4 → P4.2: a filtered search is a vector query *and* a traversal.

    The point is not the ranking — that is the crate's. It is that the traversal
    half of `search_filtered` reaches the same nodes `traverse_ids` does, so the
    two halves are looking at one graph.
    """
    reachable = set(kb.traverse_ids("bitemporal", max_depth=1))
    hits, plan = kb.search_filtered(
        "mini", vector_for("bitemporal"), "bitemporal", max_depth=1, top_k=10
    )
    assert {h.concept_id for h in hits} <= reachable
    assert plan.candidates == len(reachable)
    assert plan.candidates_capped is False


def test_all_three_search_arms_answer_about_the_same_corpus(kb):
    """P4.4 internally: vector, keyword and hybrid over one set of concepts."""
    by_vector = {h.concept_id for h in kb.search_vector("mini", vector_for("sqlite"), top_k=5)}
    by_keyword = {cid for cid, _ in kb.keyword_search("database", top_k=5)}
    by_hybrid = {
        h.concept_id
        for h in kb.hybrid_search("mini", "database", vector_for("sqlite"), top_k=5)
    }
    known = {cid for cid, _, _ in CONCEPTS}

    assert by_vector <= known and by_vector
    assert by_keyword <= known and by_keyword
    # Fusion cannot invent a hit neither arm found.
    assert by_hybrid <= (by_vector | by_keyword)


def test_repair_does_not_disturb_what_reads_see(kb):
    """P4.5 → P4.2: `links_current` is derivative, so a rebuild is a no-op to a reader."""
    before = kb.traverse_ids("bitemporal", max_depth=3)
    assert kb.audit_current() == 0

    assert kb.rebuild_current().drift_after == 0
    assert kb.traverse_ids("bitemporal", max_depth=3) == before

    assert kb.rebuild_current_chunked().drift_after == 0
    assert kb.traverse_ids("bitemporal", max_depth=3) == before


def test_archive_then_reconstruct_then_verify(kb):
    """P4.3 end to end, in the order an operator would run it."""
    now = dt.datetime.now(UTC)
    state_before = kb.reconstruct(now)

    kb.archive(T1)
    assert kb.archive_path.exists()

    # Cold storage is where the history went, not where it stopped existing.
    state_after = kb.reconstruct(now)
    assert set(state_after.concepts) == set(state_before.concepts)
    assert len(state_after.edges) == len(state_before.edges)

    check = kb.verify_snapshot_chain(now)
    assert check.diverged() is False
    assert check.folded_concepts == len(CONCEPTS)


def test_a_subgraph_loaded_before_an_archive_is_still_intact_after_it(kb):
    """The seam a per-phase file cannot reach: a value outliving a mutation.

    `Subgraph` is an opaque handle (D-101), which invites the assumption that it
    is a view over the database. It is not — it is a copy taken at load time —
    and an archive running afterwards must not change what it says.
    """
    graph = kb.load_subgraph("bitemporal", 3, ROOMY)
    edges_before = graph.edge_count()

    kb.archive(T1)

    assert graph.edge_count() == edges_before
    assert graph.is_closed()
    assert graph.dijkstra("bitemporal")["sqlite"] == pytest.approx(1.25)


def test_the_counters_saw_the_whole_session(kb):
    """P4.6 across every other phase: metrics accumulate over a session.

    Each phase's own test checks its own kind in isolation. This is the only
    place that asks whether one handle's counters cover writes, a model
    registration, embeddings, an FTS rebuild and a repair together.
    """
    kb.rebuild_current()
    kb.rebuild_current_chunked()

    snapshot = kb.metrics()
    kinds = {k.kind for k in snapshot.kinds}
    assert {
        "write_concepts_chunk",
        "bulk_import_chunk",
        "register_model",
        "upsert_embedding_chunk",
        "rebuild_fts",
        "rebuild_current",
        "shadow_rebuild",
    } <= kinds, sorted(kinds)

    assert snapshot.turns == sum(k.turns for k in snapshot.kinds)
    assert snapshot.violations() == []


def test_a_reopened_ledger_answers_the_same_questions(db_path):
    """Close, reopen, and every phase still reads what the first session wrote.

    The one test here that does not use the `kb` fixture, because the thing under
    test is the boundary the fixture holds open.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        db.write_concepts(
            [
                macrame.ConceptUpsert(cid, title, valid_from=T0, content=body)
                for cid, title, body in CONCEPTS
            ]
        )
        db.bulk_import(
            [
                macrame.EdgeAssertion(s, t, kind, valid_from=T0, weight=w)
                for s, t, kind, w in EDGES
            ]
        )
        db.register_model("mini", DIM)
        db.upsert_embeddings("mini", [(cid, vector_for(cid)) for cid, _, _ in CONCEPTS])
        first = db.traverse_ids("bitemporal", max_depth=3)
        version = db.schema_version

    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        assert db.schema_version == version
        assert db.traverse_ids("bitemporal", max_depth=3) == first
        assert db.load_subgraph("bitemporal", 3, ROOMY).edge_count() == len(EDGES)
        assert db.search_vector("mini", vector_for("sqlite"), top_k=1)
        assert db.audit_current() == 0
