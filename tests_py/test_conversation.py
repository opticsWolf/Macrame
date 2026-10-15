"""Macrame 0.20.0 from Python: a chat harness's whole loop (D-291..D-295).

One session, written one turn per `write_batch` with a HEAD pointer; a fork
after a past turn; continuing both branches; switching back; reading,
searching and following the change feed per branch.
"""

from __future__ import annotations

import pytest

import macrame

VF = "2026-01-01T00:00:00.000000Z"


def vf(k: int) -> str:
    return f"2026-01-01T00:{k % 60:02d}:00.000000Z"


@pytest.fixture
def db(tmp_path):
    with macrame.Database.open(tmp_path / "history.macrame") as d:
        d.register_edge_kind("HEAD", functional=True)
        d.upsert_concept(macrame.ConceptUpsert("session", "s", valid_from=VF))
        yield d


def append(db, turn_id, text, k, branch=None):
    head = db.pointer("session", "HEAD", branch=branch) or "session"
    return db.write_batch(
        [macrame.ConceptUpsert(turn_id, text, valid_from=VF, content=text)],
        [macrame.EdgeAssertion(head, turn_id, "NEXT", valid_from=VF)],
        pointers=[("session", "HEAD", turn_id, vf(k))],
        branch=branch,
    )


def path(db, branch=None):
    """The branch's turns, oldest first, by walking NEXT from the session."""
    seen = db.traverse_ids("session", max_depth=64, edge_types=["NEXT"], branch=branch)
    return sorted(i for i in seen if i != "session")


def test_a_batch_is_one_stamp_and_moves_the_head(db):
    r = append(db, "t0", "hello", 0)
    assert (r.concepts, r.edges, r.retirements) == (1, 1, 0)
    assert r.pointers[0].new == "t0" and r.pointers[0].old is None and r.pointers[0].changed
    assert db.pointer("session", "HEAD") == "t0"
    [v] = db.concepts_by_id(["t0"])
    assert v.recorded_at == r.recorded_at and v.branch_id == "main"


def test_fork_after_a_turn_then_continue_and_switch(db):
    for k in range(6):
        append(db, f"t{k}", f"turn {k}", k)
    alt = db.fork("alt", "main", after="t3")
    assert alt.forked_at < alt.created_at
    assert db.pointer("session", "HEAD", branch="alt") == "t3"

    append(db, "a4", "alt four", 20, branch="alt")
    append(db, "t6", "main six", 21)
    assert path(db, "alt") == ["a4", "t0", "t1", "t2", "t3"]
    assert path(db) == ["t0", "t1", "t2", "t3", "t4", "t5", "t6"]
    assert db.pointer("session", "HEAD", branch="alt") == "a4"
    assert db.pointer("session", "HEAD") == "t6"


def test_branch_view_carries_the_lineage(db):
    append(db, "t0", "x", 0)
    db.fork("alt", "main")
    view = macrame.BranchView(db, "alt")
    head = view.pointer("session", "HEAD")
    view.write_batch(
        [macrame.ConceptUpsert("a1", "alt turn", valid_from=VF)],
        [macrame.EdgeAssertion(head, "a1", "NEXT", valid_from=VF)],
        pointers=[("session", "HEAD", "a1", vf(5))],
    )
    assert view.pointer("session", "HEAD") == "a1"
    assert db.pointer("session", "HEAD") == "t0"
    assert [c.id for c in view.concepts_by_id(["a1", "t0"])] == ["a1", "t0"]
    assert db.concepts_by_id(["a1"]) == []


def test_a_second_open_head_is_refused_and_set_pointer_moves_it(db):
    append(db, "t0", "x", 0)
    append(db, "t1", "y", 1)
    with pytest.raises(macrame.FunctionalEdgeConflictError) as e:
        db.assert_edge(macrame.EdgeAssertion("session", "t0", "HEAD", valid_from=vf(9)))
    assert e.value.open_target == "t1"
    ch = db.set_pointer("session", "HEAD", "t0", valid_from=vf(9))
    assert (ch.old, ch.new, ch.changed) == ("t1", "t0", True)
    assert db.set_pointer("session", "HEAD", "t0", valid_from=vf(10)).changed is False
    assert db.edge_kinds() == [("HEAD", True)]


def test_search_is_scoped_to_the_branch(db):
    append(db, "t0", "zebra trunk", 0)
    db.fork("alt", "main")
    append(db, "a1", "zebra alt", 5, branch="alt")
    append(db, "t1", "zebra main", 6)
    ids = lambda hits: sorted(i for i, _ in hits)  # noqa: E731
    assert ids(db.keyword_search("zebra", branch="alt")) == ["a1", "t0"]
    assert ids(db.keyword_search("zebra", branch="main")) == ["t0", "t1"]
    assert ids(db.keyword_search("zebra")) == ["a1", "t0", "t1"]


def test_the_change_feed_follows_writes(db):
    append(db, "t0", "x", 0)
    cursor = db.head_seq()
    db.fork("alt", "main")
    append(db, "a1", "alt", 5, branch="alt")
    feed = db.changes(cursor)
    assert feed and all(c.seq_id > cursor for c in feed)
    assert {c.branch_id for c in db.changes(cursor, branch="alt")} == {"alt"}
    assert {c.table for c in db.changes(cursor, tables=["concepts"])} == {"concepts"}


def test_fork_refusals(db):
    append(db, "t0", "x", 0)
    with pytest.raises(macrame.ForkInFutureError):
        db.fork("later", "main", at="2999-01-01T00:00:00.000000Z")
    with pytest.raises(macrame.ConceptNotVisibleError):
        db.fork("nope", "main", after="does-not-exist")
    with pytest.raises(ValueError):
        db.fork("both", "main", at=VF, after="t0")


def test_batch_refusals(db):
    with pytest.raises(macrame.BatchTooLargeError):
        db.write_batch(
            [macrame.ConceptUpsert(f"c{i}", "c", valid_from=VF) for i in range(macrame.MAX_BATCH_ROWS + 1)]
        )
    db.fork("alt", "main")
    db.fork("beta", "main")
    with pytest.raises(macrame.BranchMismatchError):
        db.write_batch(
            [macrame.ConceptUpsert("x", "x", valid_from=VF, branch="beta")], branch="alt"
        )


def test_one_call_append_links_from_the_head(db):
    for k, tid in enumerate(["t0", "t1", "t2"]):
        db.write_batch(
            [macrame.ConceptUpsert(tid, tid, valid_from=VF)],
            pointers=[("session", "HEAD", tid, vf(k), 1.0, "NEXT")],
        )
    assert path(db) == ["t0", "t1", "t2"]
    assert db.pointer("session", "HEAD") == "t2"
