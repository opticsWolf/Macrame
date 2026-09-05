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
NOW_TS = "2030-01-01T00:00:00.000000Z"


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

    What this reaches is the shape and the default: every belief is labelled,
    and on a database that has never forked every label is the trunk. The
    two-lineage case is below, in the ``reconstruct_on`` block — which is also
    where the question this labelling was *for* finally gets answered from
    Python (0.15.17, D-259).
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
# reconstruct_on: one lineage's view of a fold (0.15.17, D-259, review C-10)
# --------------------------------------------------------------------------


@pytest.fixture
def forked(db):
    """The trunk, a fork, and both sides moving after the fork point.

    ``reconstruct`` labels each belief with its lineage; until 0.15.17 that was
    all Python could do with it, because the label alone does not say which
    lineage a *reader* should take an edge from. Everything below is about the
    difference.
    """
    # The `db` fixture seeds a, b, c and the edges a->b and b->c; `d` is the
    # endpoint the trunk's post-fork edge needs.
    db.write_concepts([macrame.ConceptUpsert("d", "D", valid_from=T0)])
    alt = db.fork("alt")
    # Both sides diverge: the trunk gains an edge the branch must not see, the
    # branch gains one of its own, and the branch shadows an inherited key by
    # re-asserting it with a closed interval.
    db.assert_edge(macrame.EdgeAssertion("c", "d", "CITES", valid_from=T0))
    db.assert_edge(macrame.EdgeAssertion("a", "c", "CITES", valid_from=T0, branch=alt.id))
    db.assert_edge(
        macrame.EdgeAssertion("b", "c", "CITES", valid_from=T0, valid_to=T1, branch=alt.id)
    )
    return db, alt


def test_a_lineages_view_of_a_fold_drops_what_it_forked_before(forked):
    """The cutoff, from Python.

    ``reconstruct`` returns the trunk's post-fork edge because the ledger holds
    it. ``reconstruct_on`` does not, because ``alt`` forked before it was
    written — and no filter over the six-tuples could have told the two cases
    apart, which is the whole of review C-10.
    """
    db, alt = forked
    at = now()

    whole = {(e[0], e[1], e[5]) for e in db.reconstruct(at).edges}
    view = {(e[0], e[1], e[5]) for e in db.reconstruct_on(at, alt.id).edges}

    assert ("c", "d", "main") in whole, "the ledger holds the trunk's post-fork edge"
    assert ("c", "d", "main") not in view, f"`alt` forked before it: {view}"
    assert ("a", "b", "main") in view, "pre-fork, and inherited"
    assert ("a", "c", "alt") in view, "the branch's own"
    # `b->c` is held by both lineages. One row comes back, from the nearer one.
    held = [e for e in db.reconstruct_on(at, alt.id).edges if (e[0], e[1]) == ("b", "c")]
    assert len(held) == 1, f"one belief per key: {held}"
    assert held[0][5] == "alt", "the nearest lineage holding the key"


def test_the_trunks_view_is_its_own_rows_and_not_the_branches(forked):
    db, alt = forked
    view = {(e[0], e[1], e[5]) for e in db.reconstruct_on(now(), "main").edges}
    assert ("c", "d", "main") in view
    assert ("a", "c", "alt") not in view, f"a descendant is not an ancestor: {view}"


def test_on_an_unforked_ledger_the_two_reconstructions_agree(db):
    """One lineage, so there is nothing to resolve and nothing to cut."""
    at = now()
    assert db.reconstruct(at).edges == db.reconstruct_on(at, "main").edges


def test_the_ancestry_is_readable_and_says_where_each_cutoff_is(forked):
    """The rule ``reconstruct_on`` applies, published rather than only obeyed."""
    db, alt = forked

    anc = db.ancestry(alt.id)
    assert [(a[0], a[1]) for a in anc] == [("alt", 0), ("main", 1)]
    assert anc[0][2] is None, "the reader has no cutoff"
    assert isinstance(anc[1][2], dt.datetime), "an ancestor is cut at the fork point"
    assert anc[1][2].tzinfo is not None, "P3: timestamps out are aware"

    # A root is itself and nothing above it, forked ledger or not.
    assert [(a[0], a[1], a[2]) for a in db.ancestry("main")] == [("main", 0, None)]


def test_an_unregistered_lineage_is_refused_by_name_not_answered_for_the_trunk(db):
    """D-069's failure is a right-looking answer to a question nobody asked."""
    for call in (
        lambda: db.reconstruct_on(now(), "ghost"),
        lambda: db.ancestry("ghost"),
    ):
        with pytest.raises(macrame.UnknownBranchError) as e:
            call()
        assert "ghost" in str(e.value)


def test_reconstruct_on_accepts_a_string_or_a_datetime(forked):
    """P3 applies to the instant here as it does to ``reconstruct``'s."""
    db, alt = forked
    assert (
        db.reconstruct_on(NOW_TS, alt.id).edges
        == db.reconstruct_on(dt.datetime(2030, 1, 1, tzinfo=UTC), alt.id).edges
    )


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


# --------------------------------------------------------------------------
# archive and lineage (0.14.12, D-229)
# --------------------------------------------------------------------------
#
# No binding changed here. These exist because the repair is *observable* from
# Python — `archive`, `retire_edge(branch=)` and `query_as_of_edges(branch=)` are
# all bound — and D-227's finding is that a repair Python cannot observe is a
# repair nobody there can test. The predicates matched edge keys across lineages,
# so one lineage's write archived another's current belief; `audit_current`
# reported no drift throughout, because the projection was honestly re-derived
# from a ledger that had been wrongly pruned.


def pairs(edges):
    return sorted((e[0], e[1]) for e in edges)


def test_a_branch_writing_at_the_trunks_key_leaves_the_trunk_alone(db):
    """The trunk lost an edge it still believed, because a branch disagreed."""
    db.fork("alt")
    db.assert_edge(
        macrame.EdgeAssertion("a", "b", "CITES", valid_from=T0, weight=2.0, branch="alt")
    )

    db.archive(FUTURE)

    assert pairs(db.query_as_of_edges(T1)) == [("a", "b"), ("b", "c")]
    assert pairs(db.query_as_of_edges(T1, branch="alt")) == [("a", "b"), ("b", "c")]


def test_archiving_does_not_resurrect_what_a_branch_retired(db):
    """A maintenance operation that asserts nothing must un-assert nothing.

    A branch retires an inherited edge by writing its **own** closed row at the
    ancestor's key. Archiving that row as "a closed interval, therefore history"
    deletes the branch's disbelief and lets the ancestor's open row win the
    resolution again.
    """
    db.fork("alt")
    db.retire_edge("b", "c", "CITES", T0, T1, branch="alt")

    assert pairs(db.query_as_of_edges(T2, branch="alt")) == [("a", "b")]

    db.archive(FUTURE)

    assert pairs(db.query_as_of_edges(T2, branch="alt")) == [
        ("a", "b")
    ], "the archive un-retired an edge the branch had stopped believing"
    assert pairs(db.query_as_of_edges(T2)) == [("a", "b"), ("b", "c")]
