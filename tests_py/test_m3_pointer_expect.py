"""M3 — the conditional pointer move (0.22.0, plan §4.2, D-313).

Each claim is a string the open target must equal, or ``None`` — the draft's
claim-if-free, meaning no target ought to be open — or ``macrame.UNSET``, the
default the keyword's absence spells, which checks nothing. The claim is
adjudicated inside the move's own transaction against the open state that
transaction reads; the loser of a race gets ``PointerConflictError`` with the
winner in ``current`` and writes nothing. On a batch the row's claim
adjudicates inside the batch's transaction — a claim and the move it implies
fail together.
"""

import time

import pytest

import macrame

JAN = "2026-01-01T00:00:00.000000Z"
BAR = chr(0x2500) * 62
SEP = chr(183)


def ts(minute=0, second=0.0, base=None):
    """A unique, strictly-increasing valid_from, one per call."""
    t = time.time() if base is None else base
    stamp = "2026-01-01T00:%02d:%09.6fZ" % (minute, second + (t % 60))
    return stamp


def canon(stamp):
    """Canonical bookkeeping: the helper's ms precision is enough."""
    return stamp


def turns(db, n, kind="HEAD"):
    stamps = []
    for i in range(n):
        vfrom = JAN if i == 0 else stamps[i - 1].replace("00:00:0", "00:00:1")
        db.upsert_concept(macrame.ConceptUpsert("t%d" % i, "turn %d" % i, valid_from=JAN))
        db.set_pointer("session", kind, "t%d" % i, valid_from=ts(minute=i))
        stamps.append(ts(minute=i))
    return stamps


@pytest.fixture
def db(tmp_path):
    with macrame.Database.open(tmp_path / "p.db") as d:
        d.register_edge_kind("HEAD", True)
        # The trunk mints the session and the first turn. The pointer is
        # deliberately NOT established here: claim-if-free claims need a
        # (source, kind) nothing has claimed yet.
        d.upsert_concept(macrame.ConceptUpsert("session", "s", valid_from=JAN))
        d.upsert_concept(macrame.ConceptUpsert("t0", "turn 0", valid_from=JAN))
        yield d


def establish(db, minute=0):
    """Open the session's HEAD at t0 with a claim-less move."""
    return db.set_pointer("session", "HEAD", "t0", valid_from=ts(minute=minute))


def head_at(db):
    return db.pointer("session", "HEAD")


def test_two_writers_racing_a_claim_admit_one_and_refuse_the_other(db):
    # Two claim-if-free moves for the same virgin pointer: the first takes
    # it, the second sees the winner in `current` and writes nothing.
    t1 = macrame.ConceptUpsert("t1", "turn 1", valid_from=JAN)
    t2 = macrame.ConceptUpsert("t2", "turn 2", valid_from=JAN)
    db.write_batch([t1, t2])
    winner = db.set_pointer(
        "session", "HEAD", "t1", valid_from=ts(minute=1), expect=None
    )
    assert winner.changed
    with pytest.raises(macrame.PointerConflictError) as exc:
        db.set_pointer("session", "HEAD", "t2", valid_from=ts(minute=2), expect=None)
    assert exc.value.expected is None
    assert exc.value.current == "t1"
    # The refused claim left the pointer where the winner left it.
    assert head_at(db) == "t1"


def test_an_expect_that_names_the_open_target_moves(db):
    establish(db)
    change = db.set_pointer("session", "HEAD", "t0", valid_from=ts(minute=1), expect="t0")
    assert not change.changed
    d = macrame.ConceptUpsert("d0", "drift", valid_from=JAN)
    db.write_batch([d])
    db.set_pointer("session", "HEAD", "d0", valid_from=ts(minute=2), expect="t0")
    assert head_at(db) == "d0"


def test_an_expect_that_names_a_stale_target_is_refused(db):
    establish(db)
    d = macrame.ConceptUpsert("d0", "drift", valid_from=JAN)
    db.write_batch([d])
    # A claim that named t1 has been stale since establish wrote: the open
    # target was t0 all along.
    with pytest.raises(macrame.PointerConflictError) as exc:
        db.set_pointer("session", "HEAD", "d0", valid_from=ts(minute=2), expect="t1")
    assert exc.value.expected == "t1"
    assert exc.value.current == "t0"
    assert head_at(db) == "t0"


def test_an_expect_on_a_branch_reads_the_branchs_own_view(db):
    establish(db)
    alt = db.fork("alt", "main")
    view = macrame.BranchView(db, alt.id)
    a1 = macrame.ConceptUpsert("a1", "alt 1", valid_from=JAN)
    view.write_batch([a1])
    # The branch's view inherited the trunk's open target, so a claim against
    # it holds and the move lands on the branch alone.
    view.set_pointer("session", "HEAD", "a1", valid_from=ts(minute=2), expect="t0")
    assert view.pointer("session", "HEAD") == "a1"
    # And the claim the branch made is against its own view only: the trunk
    # still reads the target it had before, unaware of any of it.
    assert head_at(db) == "t0"


def test_a_conflicting_pointer_row_rolls_its_whole_batch(db):
    establish(db)
    d = macrame.ConceptUpsert("d0", "drift", valid_from=JAN)
    before = db.head_seq()
    with pytest.raises(macrame.PointerConflictError):
        db.write_batch(
            [d],
            pointers=[("session", "HEAD", "d0", ts(minute=2), 1.0, None, None)],
        )
    # The claim row expected no open target and found one; the concept the
    # batch carried did not land either, and the ledger is where it was.
    assert [v.id for v in db.concepts_by_id(["d0"])] == []
    assert db.head_seq() == before
