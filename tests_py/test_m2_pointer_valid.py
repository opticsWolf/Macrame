"""Pointer reads at a valid instant, from Python (0.22.0, plan §4.3, D-309).

"what was the status then" is a valid-time question. The draft read the open
row and answered the workspace of now for any instant asked; the red tests
against the bare port saw exactly that — the head's current target for an
instant whose turn had long since moved, `TypeError` for the keyword itself.
"""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

import macrame

JAN = "2026-01-01T00:00:00.000000Z"


def ts(minute=0, month=1, day=1):
    return f"2026-{month:02}-{day:02}T00:{minute:02}:00.000000Z"


def turns(db, n):
    """A session's turns: one batch each, HEAD moving a minute a turn.

    Returns the recorded stamps, one per batch, so a test can bound a read at
    what was believed after turn k.
    """
    db.register_edge_kind("HEAD", True)
    db.upsert_concept(macrame.ConceptUpsert("session", "s", valid_from=JAN))
    stamps = []
    for i in range(n):
        report = db.write_batch(
            [
                macrame.ConceptUpsert(f"t{i}", f"turn {i}", valid_from=JAN),
            ],
            pointers=[("session", "HEAD", f"t{i}", ts(minute=i))],
        )
        stamps.append(report.recorded_at)
    return stamps


def test_a_pointer_read_at_a_valid_instant_answers_where_the_status_held(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as db:
        turns(db, 10)
        # t3 held from 00:03 until t4 moved at 00:04; 00:03:30 is inside it and
        # every later turn's interval is not.
        assert (
            db.pointer("session", "HEAD", as_of_valid=ts(minute=3)) == "t3"
        )


def test_a_pointer_read_before_the_first_move_names_nothing(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as db:
        turns(db, 10)
        assert db.pointer("session", "HEAD", as_of_valid="2025-12-31T00:00:00.000000Z") is None


def test_pointers_at_a_valid_instant_lists_the_workspace_of_then(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as db:
        db.register_edge_kind("CURRENT", True)
        db.write_batch(
            [
                macrame.ConceptUpsert(f, f, valid_from=JAN)
                for f in ["f:a", "f:b", "v:a1", "v:a2", "v:b1"]
            ],
            pointers=[
                ("f:a", "CURRENT", "v:a1", ts(month=2)),
                ("f:b", "CURRENT", "v:b1", ts(month=2)),
            ],
        )
        db.set_pointer("f:a", "CURRENT", "v:a2", valid_from=ts(month=3))
        # Now: f:a stands at v:a2; at mid-February it stood at v:a1, where
        # v:a2 had never been believed.
        assert db.pointers("CURRENT") == [("f:a", "v:a2"), ("f:b", "v:b1")]
        assert db.pointers("CURRENT", as_of_valid=ts(month=2, day=15)) == [
            ("f:a", "v:a1"),
            ("f:b", "v:b1"),
        ]
        # f:b never moved, so its open interval covers March too.
        assert db.pointers("CURRENT", as_of_valid=ts(month=3, day=15)) == [
            ("f:a", "v:a2"),
            ("f:b", "v:b1"),
        ]


def test_the_two_clocks_compose_on_a_pointer_read(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as db:
        stamps = turns(db, 10)
        valid = ts(minute=5)
        # At the cut that holds through turn 3 only, t3's belief is the latest
        # head known and reaches the instant — the fold of that era still
        # holds its interval open.
        assert (
            db.pointer(
                "session", "HEAD", as_of_valid=valid, as_of_recorded=stamps[3]
            )
            == "t3"
        )
        # By the cut after turn 6, t5 had held and given way, and the same
        # instant answers t5.
        assert (
            db.pointer(
                "session", "HEAD", as_of_valid=valid, as_of_recorded=stamps[6]
            )
            == "t5"
        )


def test_a_pointer_valid_read_on_a_branch_answers_the_branchs_view(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as db:
        db.register_edge_kind("CURRENT", True)
        db.write_batch(
            [
                macrame.ConceptUpsert(f, f, valid_from=JAN)
                for f in ["f:a", "f:b", "f:c", "v:a1", "v:a2", "v:b1", "v:c1"]
            ],
            pointers=[
                ("f:a", "CURRENT", "v:a1", ts(month=2)),
                ("f:b", "CURRENT", "v:b1", ts(month=2)),
            ],
        )
        alt = db.fork("alt", "main")
        view = macrame.BranchView(db, alt.id)
        view.set_pointer("f:a", "CURRENT", "v:a2", valid_from=ts(month=3))
        view.write_batch(
            pointers=[("f:c", "CURRENT", "v:c1", ts(month=3, day=2))]
        )
        # The branch's now has the move and the new source; its mid-February
        # does not.
        assert view.pointers("CURRENT") == [
            ("f:a", "v:a2"),
            ("f:b", "v:b1"),
            ("f:c", "v:c1"),
        ]
        assert view.pointers("CURRENT", as_of_valid=ts(month=2, day=15)) == [
            ("f:a", "v:a1"),
            ("f:b", "v:b1"),
        ]
        # And the trunk was never touched.
        assert db.pointers("CURRENT") == [("f:a", "v:a1"), ("f:b", "v:b1")]
        db.close()
