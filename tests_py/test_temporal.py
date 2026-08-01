"""P4.3 acceptance: reconstruct, archive, and the chain check.

The fold, the archive's cutoff arithmetic and the chain comparison are the
crate's and are covered by the Rust suite. What is asserted here is what the
boundary adds: that timestamps inside these results follow P3's rule wherever
they appear, that a window argument is refused rather than clamped, and that
``ChainCheck`` arrives with the field that matters — ``diverged()`` — rather
than as a tuple whose two anchors invite being compared.
"""

from __future__ import annotations

import datetime as dt

import pytest

import macrame

UTC = dt.timezone.utc
T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-03-01T00:00:00.000000Z"
T2 = "2026-06-01T00:00:00.000000Z"


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


def now():
    return dt.datetime.now(UTC)


# --------------------------------------------------------------------------
# reconstruct
# --------------------------------------------------------------------------


def test_reconstruct_returns_the_world_at_an_instant(db):
    st = db.reconstruct(now())
    assert sorted(st.concepts) == ["a", "b", "c"]
    assert st.concepts["a"].title == "A"
    assert len(st.edges) == 2
    assert st.seq_anchor > 0


def test_reconstructed_edges_carry_datetimes_and_none_for_open(db):
    (source, target, edge_type, valid_from, valid_to) = db.reconstruct(now()).edges[0]
    assert (source, target, edge_type) == ("a", "b", "CITES")
    assert valid_from == dt.datetime(2026, 1, 1, tzinfo=UTC)
    assert valid_to is None


def test_reconstruct_before_anything_was_recorded_raises_about_cold_storage(db):
    """Not an empty state — and the error names a file the caller never made.

    An instant older than the oldest thing on hand sends the fold to cold
    storage, and on a ledger that has never been archived there is no cold file,
    so this is ``ReplayCorruptError: … archive database file … does not exist``.

    It is the crate's behaviour rather than the binding's, and it is asserted
    here rather than smoothed over, because a Python caller asking about a date
    before their data existed is not doing anything strange and the message
    tells them about an implementation detail instead. Recorded as a rough edge
    for the crate; the binding does not paper over it, since translating this
    into an empty state would mean claiming a *real* missing archive is also
    nothing to worry about.
    """
    with pytest.raises(macrame.ReplayCorruptError, match="does not exist"):
        db.reconstruct("2025-01-01T00:00:00.000000Z")


def test_reconstruct_accepts_a_string_or_a_datetime(db):
    stamp = now()
    assert db.reconstruct(stamp).seq_anchor == db.reconstruct(
        stamp.strftime("%Y-%m-%dT%H:%M:%S.%f") + "Z"
    ).seq_anchor


def test_a_naive_datetime_is_refused(db):
    with pytest.raises(macrame.InvalidTimestampError, match="naive"):
        db.reconstruct(dt.datetime(2026, 6, 1))


# --------------------------------------------------------------------------
# query_as_of_edges
# --------------------------------------------------------------------------


def test_as_of_edges_are_current_belief_at_the_instant(db):
    edges = db.query_as_of_edges()
    assert sorted((e[0], e[1]) for e in edges) == [("a", "b"), ("b", "c")]
    assert edges[0][4] is None


def test_as_of_edges_defaults_to_the_handles_clock(db):
    assert db.query_as_of_edges() == db.query_as_of_edges(now())


def test_as_of_edges_before_the_valid_interval_is_empty(db):
    assert db.query_as_of_edges("2025-01-01T00:00:00.000000Z") == []


# --------------------------------------------------------------------------
# archive
# --------------------------------------------------------------------------


def test_archive_returns_a_report(db):
    r = db.archive(T1)
    assert r.links_archived >= 0
    assert r.log_entries_archived >= 0
    # `horizon` is an int or None, never a Rust Option rendered into the repr.
    assert r.horizon is None or isinstance(r.horizon, int)
    assert "Some(" not in repr(r)


def test_archive_creates_the_cold_file(db):
    db.archive(T1)
    assert db.archive_path.exists()


def test_archive_windowed_returns_one_report_per_session(db):
    reports = db.archive_windowed(T2, dt.timedelta(days=30))
    assert reports
    assert all(isinstance(r, macrame.ArchiveReport) for r in reports)


def test_archive_windowed_accepts_seconds_as_well_as_timedelta(db):
    assert db.archive_windowed(T1, 86_400 * 30) is not None


def test_a_window_that_does_not_advance_is_refused_not_clamped(db):
    # Refused here, before the ledger sees it: a zero window reaches
    # ArchiveWindowError with a message about session counts, which is a true
    # statement about the wrong problem.
    for bad in (0, -1, float("nan"), float("inf")):
        with pytest.raises(ValueError):
            db.archive_windowed(T1, bad)


def test_a_window_of_the_wrong_type_is_a_type_error(db):
    with pytest.raises(TypeError, match="timedelta"):
        db.archive_windowed(T1, "30 days")


# --------------------------------------------------------------------------
# verify_snapshot_chain
# --------------------------------------------------------------------------


def test_a_healthy_chain_does_not_diverge(db):
    check = db.verify_snapshot_chain(now())
    assert check.diverged() is False
    assert check.concept_disagreements == []
    assert check.edge_disagreements == []
    assert check.truncated is False


def test_the_two_anchors_are_reported_and_are_not_the_check(db):
    """They may legitimately differ, so an equality assertion here would be a
    check that reports divergence which is not there — worse than no check."""
    check = db.verify_snapshot_chain(now())
    assert isinstance(check.composed_anchor, int)
    assert isinstance(check.folded_anchor, int)
    assert check.diverged() is False  # regardless of whether those two agree


def test_the_chain_check_counts_both_sides(db):
    check = db.verify_snapshot_chain(now())
    assert check.composed_concepts == check.folded_concepts == 3
    assert check.composed_edges == check.folded_edges == 2


def test_the_sample_limit_is_exposed(db):
    assert macrame.CHAIN_CHECK_SAMPLE_LIMIT == 32


def test_the_repr_reads_as_python(db):
    # `false` is Rust. A repr is read by a Python programmer.
    assert "diverged=False" in repr(db.verify_snapshot_chain(now()))


# --------------------------------------------------------------------------
# Lifecycle
# --------------------------------------------------------------------------


def test_temporal_calls_on_a_closed_handle_raise(db_path):
    handle = macrame.Database.open(db_path, snapshot_every_entries=None)
    stamp = now()
    handle.close()
    for call in (
        lambda: handle.reconstruct(stamp),
        lambda: handle.query_as_of_edges(),
        lambda: handle.archive(T1),
        lambda: handle.archive_windowed(T1, 86_400),
        lambda: handle.verify_snapshot_chain(stamp),
    ):
        with pytest.raises(macrame.MacrameClosedError):
            call()
