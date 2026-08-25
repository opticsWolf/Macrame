"""P4.5 and P4.6 acceptance: integrity repair and actor metrics.

The rebuild's correctness is the crate's. What is asserted here is that the
counters are *real* in the wheel — which is the whole of D-093, since the crate
compiles them out by default and a Python caller cannot turn them back on — and
that the two rebuild paths agree about the answer while differing in how they
hold the lock.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(n, n.upper(), valid_from=T0) for n in ("a", "b", "c")]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion("a", "b", "CITES", valid_from=T0),
                macrame.EdgeAssertion("b", "c", "CITES", valid_from=T0),
            ]
        )
        yield handle


# --------------------------------------------------------------------------
# Integrity
# --------------------------------------------------------------------------


def test_a_healthy_ledger_has_no_drift(db):
    assert db.audit_current() == 0


def test_rebuild_reprojects_and_leaves_no_drift(db):
    report = db.rebuild_current()
    assert report.rows_rebuilt == 2
    assert report.drift_after == 0


def test_the_chunked_rebuild_reaches_the_same_answer(db):
    assert db.rebuild_current_chunked() == db.rebuild_current()


def test_a_rebuild_is_idempotent(db):
    first = db.rebuild_current()
    assert db.rebuild_current() == first
    assert db.audit_current() == 0


def test_rebuild_fts_is_callable_and_leaves_search_working(db):
    db.rebuild_fts()
    assert db.keyword_search("A") is not None


def test_integrity_calls_on_a_closed_handle_raise(db_path):
    handle = macrame.Database.open(db_path, snapshot_every_entries=None)
    handle.close()
    for call in (
        handle.audit_current,
        handle.rebuild_current,
        handle.rebuild_current_chunked,
        handle.metrics,
    ):
        with pytest.raises(macrame.MacrameClosedError):
            call()


# --------------------------------------------------------------------------
# Metrics — the counters are on in the wheel (D-093)
# --------------------------------------------------------------------------


def test_the_counters_are_real_rather_than_compiled_out(db):
    """The wheel builds with `--features metrics`, so this must not be zero.

    A default Rust build answers zero here from a zero-sized type. If this ever
    starts passing with `turns == 0`, the wheel lost the feature and
    `chunk_budget_ms()` went back to being a number nobody can check against.
    """
    m = db.metrics()
    assert m.turns > 0


def test_every_kind_reported_has_been_seen(db):
    m = db.metrics()
    assert m.kinds
    assert all(k.turns > 0 for k in m.kinds)
    assert {"bulk_import_chunk", "write_concepts_chunk"} <= {k.kind for k in m.kinds}


def test_the_longest_hold_names_its_kind(db):
    kind, duration = db.metrics().longest
    assert isinstance(kind, str)
    assert duration.total_seconds() >= 0


def test_nothing_here_breaks_the_budget(db):
    # Not a performance assertion: this fixture writes five rows. It asserts
    # that `violations()` is the shape a caller can act on, and that the good
    # answer is an empty list rather than None.
    assert db.metrics().violations() == []


def test_buckets_are_one_longer_than_the_bounds(db):
    kind = db.metrics().kinds[0]
    assert len(kind.buckets) == len(macrame.BUCKET_BOUNDS_MICROS) + 1
    assert sum(kind.buckets) == kind.turns


def test_the_bucket_bounds_are_exposed_and_ascending(db):
    bounds = list(macrame.BUCKET_BOUNDS_MICROS)
    assert bounds == sorted(bounds)
    assert bounds


def test_queue_depth_is_reported(db):
    m = db.metrics()
    assert m.depth_samples > 0
    assert m.high_depth_mean >= 0.0
    assert m.high_depth_max >= 0
    assert m.low_depth_mean >= 0.0


def test_a_rebuild_shows_up_as_its_own_kind(db):
    """`shadow_rebuild` is deliberately not folded into `rebuild_current`.

    The two have opposite latency profiles, and the point of the chunked path is
    that its turns are short — averaging them together would hide exactly the
    improvement it was built for.
    """
    db.rebuild_current()
    db.rebuild_current_chunked()
    kinds = {k.kind for k in db.metrics().kinds}
    assert "rebuild_current" in kinds
    assert "shadow_rebuild" in kinds


def test_analyze_and_optimize_are_separate_kinds(db):
    """Split in 0.13.24 (D-197), and the Python strings move with the enum.

    The kind names cross the boundary as strings built by position from
    `CommandKind::ALL`, so a Rust-side reorder relabels axes in a language the
    Rust compiler is not looking at. Asserting both names here is what makes
    that a red test on this side too.

    Both are checked against both counters, because the regression the split can
    have -- the `incremental` flag read backwards -- swaps the names and leaves
    every total correct. `.get(..., 0)` rather than `[...]`: this surface drops
    kinds with no turns, so a kind that has not run is absent rather than zero.
    """
    db.analyze()
    counts = {k.kind: k.turns for k in db.metrics().kinds}
    assert (counts.get("analyze", 0), counts.get("optimize", 0)) == (1, 0), counts

    db.optimize()
    counts = {k.kind: k.turns for k in db.metrics().kinds}
    assert (counts.get("analyze", 0), counts.get("optimize", 0)) == (1, 1), counts


def test_metrics_accumulate_rather_than_reset(db):
    before = db.metrics().turns
    db.upsert_concept(macrame.ConceptUpsert("d", "D", valid_from=T0))
    assert db.metrics().turns > before
