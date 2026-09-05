"""A concept outliving its lineage, through the boundary (0.15.11, W15.1, D-253).

``archive_branch()`` forgets a lineage and takes its ``branches`` row along;
``rehydrate()`` brings a concept back still carrying the lineage it was minted
on. For exactly one input those two disagree, and until this release the
disagreement surfaced as ``EngineError`` reading ``FOREIGN KEY constraint
failed`` — which named neither the concept nor the branch.

The Rust file ``tests/rehydrate_lineage_tests.rs`` pins the refusal itself. What
is asserted here is what only the boundary can get wrong: that the class is
reachable as ``macrame.BranchArchivedError``, that it is catchable as a
``BranchError`` alongside the rest of the family, that both names arrive as
attributes rather than only inside the message, and that the remedy the message
names works from Python too.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-02-01T00:00:00.000000Z"
# Past every stamp the crate writes: `recorded_at` is transaction time, so a
# cutoff in the past archives nothing at all.
FUTURE = "2099-01-01T00:00:00.000000Z"


@pytest.fixture
def forgotten(db_path):
    """``t`` cold on the trunk, ``d`` cold on a lineage that has been forgotten.

    The two reach the cold file by different arms on purpose — ``t`` through
    ``archive()``, whose lineage is intact, and ``d`` through
    ``archive_branch()``, whose lineage is not — so ``t`` is a control and not a
    second copy of ``d``.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as db:
        db.write_concepts(
            [
                macrame.ConceptUpsert(
                    "t", "Title", valid_from=T0, valid_to=T1, retired=True, content="t"
                )
            ]
        )
        db.fork("alt")
        db.write_concepts(
            [macrame.ConceptUpsert("d", "Title", valid_from=T0, content="d", branch="alt")]
        )

        assert db.archive(FUTURE).concepts_archived == 1, (
            "the trunk concept must go cold through the concept arm, or the "
            "control below proves nothing"
        )
        db.archive_branch("alt")
        yield db


def test_the_refusal_names_the_concept_and_the_lineage(forgotten):
    with pytest.raises(macrame.BranchArchivedError) as excinfo:
        forgotten.rehydrate(["d"])

    assert excinfo.value.branch == "alt"
    assert excinfo.value.concept == "d"


def test_it_is_catchable_as_a_branch_error(forgotten):
    """The hierarchy is what a caller writes ``except`` against.

    Under ``BranchError`` and not ``EngineError``: a caller catching engine
    faults is catching "the database is unwell", and a lineage forgotten on
    purpose is not that.
    """
    with pytest.raises(macrame.BranchError):
        forgotten.rehydrate(["d"])

    with pytest.raises(macrame.MacrameError):
        forgotten.rehydrate(["d"])


def test_a_cold_concept_on_a_living_lineage_still_comes_back(forgotten):
    """The over-refusal guard: being cold is not the condition, being orphaned is."""
    assert forgotten.rehydrate(["t"]).concepts_rehydrated == 1


def test_a_refused_call_writes_nothing(forgotten):
    """``t`` is asked for first and is rehydratable, and must still be cold after.

    The Rust side owns this claim too. It is repeated here because the binding
    is free to loop over ids itself, and a version that called through once per
    id would satisfy every other test in this file.
    """
    with pytest.raises(macrame.BranchArchivedError):
        forgotten.rehydrate(["t", "d"])

    assert (
        forgotten.diagnostic_query("SELECT COUNT(*) FROM concepts WHERE id = 't'")[0][0]
        == 0
    ), "the id ahead of the refusal was kept, so the call is not all-or-nothing"


def test_re_registering_the_lineage_makes_it_succeed(forgotten):
    """The message's advice, taken from Python.

    Without this the refusal could be a dead end and every other assertion here
    would still pass.
    """
    with pytest.raises(macrame.BranchArchivedError):
        forgotten.rehydrate(["d"])

    forgotten.fork("alt")
    assert forgotten.rehydrate(["d"]).concepts_rehydrated == 1
    assert (
        forgotten.diagnostic_query("SELECT branch_id FROM concepts WHERE id = 'd'")[0][0]
        == "alt"
    ), "the concept must come back on the lineage it was minted on"
