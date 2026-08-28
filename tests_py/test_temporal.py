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
    (source, target, edge_type, valid_from, valid_to, branch) = db.reconstruct(
        now()
    ).edges[0]
    assert (source, target, edge_type) == ("a", "b", "CITES")
    assert valid_from == dt.datetime(2026, 1, 1, tzinfo=UTC)
    assert valid_to is None
    assert branch == "main"


def test_a_belief_is_labelled_with_the_lineage_holding_it(db):
    """The sixth field, and why it is not the fifth-and-a-half (0.14.5, D-221).

    ``reconstruct`` folds the whole ledger rather than one lineage's view of it,
    so on a forked database one edge key can arrive twice. Until 0.14.5 those
    two rows were indistinguishable and one of them was dropped on the way out
    of the fold, with the survivor decided by write order.

    There is no ``fork()`` from Python yet, so what this can reach is the shape
    and the default: every belief is labelled, and on a database that has never
    forked every label is the trunk. The two-lineage case is
    ``branch_storage_tests::a_reconstruction_keeps_both_lineages_beliefs``.
    """
    edges = db.reconstruct(now()).edges
    assert edges, "the fixture asserts edges"
    for e in edges:
        assert len(e) == 6, f"a belief without its lineage: {e}"
        assert e[5] == "main"


def test_reconstruct_before_anything_was_recorded_is_an_empty_state(db):
    """The empty state, and it says why it is empty (0.8.0, B5, D-121).

    Through 0.7.0 this raised ``ReplayCorruptError: … archive database file …
    does not exist``, naming a file the caller had never made — the binding
    derives ``*_archive.db`` from the database path. This test asserted that,
    and recorded it as a rough edge the binding declined to paper over, on the
    stated ground that *translating this into an empty state would mean claiming
    a real missing archive is also nothing to worry about.*

    That objection was right, and it turned out not to be a reason to keep the
    behaviour — it was a reason to answer the question it was really asking.
    ``transaction_log.seq_id`` is ``AUTOINCREMENT`` and only an archive session
    may delete from the table, so a log whose ids are exactly ``1..MAX`` has
    provably never been archived from, and *before recorded history* and *the
    cold file is gone* stop being the same state on disk. The second still
    raises; see ``a_missing_archive_is_an_error_when_rows_were_actually_archived``
    in the Rust suite.
    """
    state = db.reconstruct("2025-01-01T00:00:00.000000Z")
    assert state.concepts == {}
    assert state.edges == []
    assert state.predates_recorded_history


def test_an_empty_state_says_which_kind_of_empty_it_is(db):
    """The flag is the whole point, so both of its values are asserted.

    ``concepts == {}`` is not self-describing: a ledger that had not started and
    one whose contents were all retired look identical. A flag that is only ever
    checked true would be decoration.
    """
    assert db.reconstruct("2025-01-01T00:00:00.000000Z").predates_recorded_history
    assert not db.reconstruct(now()).predates_recorded_history


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

    # The same rule stated as a timedelta. Until 0.12.20 these two took a
    # different path and gave a different answer: `Duration` cannot hold a
    # negative, so the extraction failed and the fallback then failed to read
    # the timedelta as a float — reporting "expected a datetime.timedelta" to a
    # caller who was holding one. `timedelta(0)` was worse: `Duration` *can*
    # hold zero, so it passed here and was refused as `0`, which is one rule
    # with two answers depending on how the caller typed it (W6.3).
    for bad in (dt.timedelta(0), dt.timedelta(seconds=-1), dt.timedelta(days=-30)):
        with pytest.raises(ValueError, match="positive"):
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
        lambda: handle.rehydrate(["anything"]),
        lambda: handle.verify_snapshot_chain(stamp),
    ):
        with pytest.raises(macrame.MacrameClosedError):
            call()


# --------------------------------------------------------------------------
# rehydrate, and the doctrine claim through the boundary (0.9.0, C5)
# --------------------------------------------------------------------------

# Past every timestamp the crate stamps, `recorded_at` included — a cutoff in
# the past archives nothing, because transaction time is stamped at write.
FUTURE = "2099-01-01T00:00:00.000000Z"


def _seed_archivable(handle):
    """One concept that will go cold, one that will not.

    ``cold`` is superseded first so the log has rows to archive as well as the
    row itself, then closed and retired so it satisfies the archivability
    predicate; ``keep`` stays open and stays hot. No edges anywhere, because a
    concept named by any surviving link is not archivable at all (D-128).
    """
    handle.write_concepts(
        [
            macrame.ConceptUpsert("keep", "Keep", valid_from=T0, content="stays hot"),
            macrame.ConceptUpsert("cold", "Cold v1", valid_from=T0, content="first"),
        ]
    )
    handle.write_concepts(
        [
            macrame.ConceptUpsert(
                "cold",
                "Cold v2",
                valid_from=T0,
                valid_to=T1,
                retired=True,
                content="second",
            )
        ]
    )
    return handle


@pytest.fixture
def archivable_db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        yield _seed_archivable(handle)


def test_the_archive_report_carries_concepts_through_the_boundary(archivable_db):
    """C1/C2's new field is Python-visible and is not always zero.

    ``concepts_archived >= 0`` would pass against a binding that never read the
    field at all, which is the shape of failure a count getter has.
    """
    report = archivable_db.archive(FUTURE)
    assert report.concepts_archived == 1
    assert "concepts=1" in repr(report)


def test_rehydrate_returns_a_report_that_reads_as_python(archivable_db):
    archivable_db.archive(FUTURE)
    report = archivable_db.rehydrate(["cold"])
    assert isinstance(report, macrame.RehydrateReport)
    assert report.concepts_rehydrated == 1
    # The clean exit: nothing claimed the freed rowid, so nothing was reassigned.
    assert report.rowids_reassigned == 0
    assert "Some(" not in repr(report)
    assert "concepts=1" in repr(report)


def test_rehydrating_an_id_that_is_not_cold_is_skipped_not_refused(archivable_db):
    """The caller's list comes from a cold-side query; staleness is normal.

    Asserted with a real id and an invented one together, so a binding that
    raised on the unknown one — or quietly counted it — fails either way.
    """
    archivable_db.archive(FUTURE)
    report = archivable_db.rehydrate(["cold", "never-existed", "keep"])
    assert report.concepts_rehydrated == 1


def test_archive_then_rehydrate_leaves_reconstruct_bit_identical(db_path, tmp_path):
    """**The release's whole point, asserted from Python** (C3's gate, C5 step 3).

    Not duplication of the Rust test. That one proves the ledger is traceable
    across the round trip; this one proves the *boundary* does not lose the
    property — the same argument §14.7 makes about R15 reaching through rather
    than being absorbed. A binding that rendered `content` from a stale cache,
    or dropped a field on the way out, would pass every test above and fail
    here.

    The control is a second database seeded identically and never archived, so
    the comparison is against what the answer should have been rather than
    against an earlier reading of the same handle.
    """
    control_path = tmp_path / "control.db"
    with macrame.Database.open(control_path, snapshot_every_entries=None) as control:
        expected = _seed_archivable(control).reconstruct(now())
        expected_concepts = dict(expected.concepts)
        expected_edges = list(expected.edges)

    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        _seed_archivable(handle)
        assert handle.archive(FUTURE).concepts_archived == 1
        assert handle.rehydrate(["cold"]).concepts_rehydrated == 1

        actual = handle.reconstruct(now())
        assert dict(actual.concepts) == expected_concepts
        assert list(actual.edges) == expected_edges
        # And the round trip did not resurrect the retirement: `cold` was
        # retired before it was archived and must still be absent from the fold.
        assert "cold" not in actual.concepts
        assert "keep" in actual.concepts


def test_a_rehydrated_concept_is_back_in_the_hot_table_not_merely_in_the_ledger(
    archivable_db,
):
    """The half `reconstruct` cannot see (D-130), asserted from Python too.

    The fold reads `transaction_log` and never touches the `concepts` table, so
    the equality test above would pass even if rehydration put the row back with
    every column garbled. The Rust gate is two tests for that reason and so is
    this one.

    **The reader is the archivability predicate, reached through `archive`**, and
    that is the correction this test needed. The obvious readers are not
    available: `keyword_search` and `load_subgraph` both filter `retired = 0`,
    and an archivable concept is retired by definition (D-128), so neither can
    see an archived *or* a rehydrated concept from either side of the round trip
    — the same discovery C3 made about `load_subgraph` in the crate. Python has
    no raw SQL, so the FTS index half is simply not assertable across this
    boundary and the Rust suite keeps it.

    What is left is better than a consolation. Archiving again succeeds only if
    the row is genuinely in the hot table *with the column values the predicate
    reads* — `retired`, `valid_to`, `recorded_at` — so it tests the columns
    rather than merely the row's presence. And a second rehydration moving
    nothing is what says the concept is hot rather than still cold: archivability
    itself would not say so, because the predicate is a pure function of the
    concept's own columns and answers the same on both sides of the boundary.
    """
    assert archivable_db.archive(FUTURE).concepts_archived == 1
    assert archivable_db.rehydrate(["cold"]).concepts_rehydrated == 1

    # It is hot now, so there is nothing cold left to bring back.
    assert archivable_db.rehydrate(["cold"]).concepts_rehydrated == 0

    # And it is hot *with its columns intact*, which is what lets the predicate
    # admit it a second time.
    assert archivable_db.archive(FUTURE).concepts_archived == 1
