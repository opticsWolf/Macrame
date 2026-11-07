"""Supersede in one write, from Python (0.22.0, M1b, D-312).

The close of a concept's open belief and the new belief in the same call, on
the same stamp: the case the D-291 guard made two calls, and a crash between
them a task with no open belief (Silk PR #3, ``e3f5666``).
"""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

import macrame

JAN = "2026-01-01T00:00:00.000000Z"
FEB = "2026-02-01T00:00:00.000000Z"
JUN = "2026-06-01T00:00:00.000000Z"
SEP = "2026-09-01T00:00:00.000000Z"
NOV = "2026-11-01T00:00:00.000000Z"
FAR = "2029-01-01T00:00:00.000000Z"


def ts(z: str) -> datetime:
    return datetime.fromisoformat(z).astimezone(UTC)


@pytest.fixture
def db(tmp_path):
    with macrame.Database.open(tmp_path / "history.macrame") as d:
        d.upsert_concept(macrame.ConceptUpsert("a", "a", valid_from=JAN))
        yield d


def supersede(db, **kwargs):
    kw = {"valid_from": SEP, **kwargs}
    c = macrame.ConceptUpsert("a", kwargs.get("title", "a2"), **kw)
    return db.upsert_concept(c, supersede=True)


def concept_changes(db):
    return [c for c in db.changes(0) if c.table == "concepts"]


def test_one_write_cuts_the_open_interval(db):
    supersede(db)
    [v] = db.concepts_by_id(["a"])
    assert v.title == "a2"
    assert (v.valid_from, v.valid_to) == (ts(SEP), None)

    # The close rides the same recorded stamp as the open: no instant between
    # them where no belief is open.
    stamps = {c.recorded_at for c in concept_changes(db)}
    assert len(stamps) == 2, f"the pre-act stamp and the act stamp: {stamps}"


def test_as_of_valid_in_the_old_interval_returns_the_old_text(db):
    supersede(db)
    state = db.reconstruct_as_of(as_of_valid=ts(FEB))
    assert state.concepts["a"].title == "a"
    at_sep = db.reconstruct_as_of(as_of_valid=ts(SEP))
    assert at_sep.concepts["a"].title == "a2"


def test_the_same_start_refuses(db):
    with pytest.raises(macrame.ConceptSingleOpenError):
        supersede(db, valid_from=JAN)
    with pytest.raises(macrame.ConceptSingleOpenError):
        supersede(db, valid_from="2025-06-01T00:00:00.000000Z")
    [v] = db.concepts_by_id(["a"])
    assert v.title == "a"
    assert concept_changes(db)[0].recorded_at == v.recorded_at


def test_nothing_open_is_an_ordinary_upsert(db):
    db.upsert_concept(
        macrame.ConceptUpsert("a", "a", valid_from=JAN, valid_to=ts(JUN))
    )
    supersede(db, valid_from=NOV)
    [v] = db.concepts_by_id(["a"])
    assert v.title == "a2"
    assert (v.valid_from, v.valid_to) == (ts(NOV), None)


def test_a_batch_row_supersedes(db):
    report = db.write_batch(
        [
            macrame.ConceptUpsert("a", "a2", valid_from=ts(SEP), supersede=True),
        ]
    )
    assert report.concepts == 1
    rows = [c for c in concept_changes(db) if c.entity_id == "a"]
    assert len(rows) == 3, f"original, close, open: {rows}"
    assert rows[1].recorded_at == rows[2].recorded_at == report.recorded_at


def test_an_inherited_concept_is_not_superseded_by_the_branch(db):
    alt = db.fork("alt", "main")
    view = macrame.BranchView(db, alt.id)
    with pytest.raises(macrame.CrossLineageError):
        view.upsert_concept(
            macrame.ConceptUpsert("a", "a2", valid_from=ts(SEP)),
            supersede=True,
        )
    [v] = db.concepts_by_id(["a"])
    assert v.title == "a"
    assert len(concept_changes(db)) == 1
