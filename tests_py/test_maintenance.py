"""W6.4: everything 0.13.0 added, reachable from Python.

`analyze()`, `optimize()`, `checkpoint()` and the tuning knobs. A binding gap
opened in the same release that created the feature is a gap that never gets a
chance to become a convention, which is the argument for closing it here rather
than in 0.14.0.

The Rust suite already proves what these *do* — ``tests/analyze_tests.rs``,
``tests/checkpoint_tests.rs``, ``tests/wal_policy_tests.rs``,
``tests/cache_size_tests.rs``. What this file asserts is the crossing: that the
call arrives, that the report's three numbers are not interchanged on the way
out, and that the tri-state knobs keep their meanings — particularly that the
*absent* state leaves the mechanism alone rather than turning it off.
"""

from __future__ import annotations

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [
                macrame.ConceptUpsert(f"c{i:03}", f"C{i}", valid_from=T0)
                for i in range(60)
            ]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion(f"c{i:03}", f"c{i + 1:03}", "LINKS", valid_from=T0)
                for i in range(59)
            ]
        )
        yield handle


def stat1_count(db):
    """`sqlite_stat1` does not exist until something has analysed."""
    try:
        return len(db.diagnostic_query("SELECT tbl, idx FROM sqlite_stat1"))
    except macrame.MacrameError:
        return 0


# --------------------------------------------------------------------------
# analyze / optimize
# --------------------------------------------------------------------------


def test_analyze_creates_statistics_that_did_not_exist(db):
    """The before half is the load-bearing one (D-149).

    Asserting only that statistics exist afterwards would pass in a world where
    libSQL had been writing them all along, and the finding was that it had
    not.
    """
    assert stat1_count(db) == 0
    db.analyze()
    assert stat1_count(db) > 0


def test_optimize_is_repeatable_on_an_idle_database(db):
    """The property `close()` depends on, asserted from the side that uses it.

    `close()` calls `optimize()` unconditionally, which is only safe because it
    does nothing when nothing has moved.
    """
    db.analyze()
    after_analyze = stat1_count(db)
    db.optimize()
    db.optimize()
    assert stat1_count(db) == after_analyze


# --------------------------------------------------------------------------
# checkpoint
# --------------------------------------------------------------------------


def test_checkpoint_reports_what_it_moved(db):
    """The numbers are the reason the method returns something at all (D-156).

    A checkpoint that did nothing and one that reclaimed a 400 MB WAL are the
    same `None`, and telling them apart is why a caller asked.
    """
    report = db.checkpoint()
    assert not report.busy
    assert report.checkpointed_frames > 0, (
        "60 concepts and 59 edges were written and the checkpoint moved no "
        "frames — either the WAL was already checkpointed underneath us, or "
        "the FULL pass this count is read from stopped running"
    )
    # TRUNCATE ran second, so nothing is left behind.
    assert report.log_frames == 0
    assert report.is_complete()


def test_a_checkpoint_on_an_idle_database_is_complete_and_moves_nothing(db_path):
    """Zero frames is a successful checkpoint, not a failed one."""
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.checkpoint()
        again = handle.checkpoint()
    assert again.checkpointed_frames == 0
    assert again.log_frames == 0
    assert again.is_complete()


def test_the_report_does_not_transpose_its_two_frame_counts(db):
    """`log_frames` is what is left; `checkpointed_frames` is what moved.

    They are both integers and both plausible, so a swap at the boundary would
    be invisible to a type checker and to any test that only read one of them.
    After a TRUNCATE the two must differ: nothing is left, and something moved.
    """
    report = db.checkpoint()
    assert report.log_frames == 0
    assert report.checkpointed_frames > 0


def test_checkpoint_is_counted_as_its_own_command_kind(db):
    """`CommandKind::Checkpoint` reaches the Python metrics as itself.

    A new variant landing in a release that also exposes it is the case where
    a kind can silently arrive as some neighbour's string.
    """
    db.checkpoint()
    kinds = {k.kind for k in db.metrics().kinds}
    assert "checkpoint" in kinds, f"got {sorted(kinds)}"


# --------------------------------------------------------------------------
# The tuning knobs
# --------------------------------------------------------------------------


def test_the_disabled_wal_policy_plus_an_explicit_checkpoint_is_a_real_path(db_path):
    """§8's acceptance item 10, from Python.

    Turning autocheckpoint off is only correct if the caller checkpoints, so
    the two are asserted together: frames accumulate through the load and are
    reclaimed at the end.
    """
    with macrame.Database.open(
        db_path, snapshot_every_entries=None, wal_autocheckpoint="disabled"
    ) as handle:
        handle.write_concepts(
            [
                macrame.ConceptUpsert(f"c{i:03}", f"C{i}", valid_from=T0)
                for i in range(200)
            ]
        )
        report = handle.checkpoint()

    assert report.checkpointed_frames > 0
    assert report.is_complete()


def test_a_page_threshold_is_accepted_and_is_not_observable_from_here(db_path):
    """The value reaches the write connection, which no test can reach (D-157).

    `wal_autocheckpoint` is per-connection, and it is applied to `write_conn`
    alone because that is the only connection that commits. `diagnostic_conn`
    opens its own and reports SQLite's default whatever this is set to — so the
    assertion below is deliberately the *negative* one, and the Rust suite
    (`tests/wal_policy_tests.rs`) holds the positive half by measuring WAL
    growth instead.

    Written down rather than left out: an assertion of `== 64` here passes for
    the wrong reason on any release that stops applying the pragma at all.
    """
    with macrame.Database.open(
        db_path, snapshot_every_entries=None, wal_autocheckpoint=64
    ) as handle:
        handle.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))
        assert handle.diagnostic_query("PRAGMA wal_autocheckpoint")[0][0] == 1000


def test_zero_pages_is_refused_rather_than_read_as_sqlites_disable(db_path):
    """D-157's refusal, restated at this boundary.

    SQLite reads `wal_autocheckpoint = 0` as *disable*. Inheriting that would
    turn a caller whose arithmetic produced zero into a process with an
    unbounded WAL and nothing to indicate it.
    """
    with pytest.raises(ValueError, match="disabled"):
        macrame.Database.open(db_path, wal_autocheckpoint=0)


def test_an_unknown_wal_policy_string_names_what_is_accepted(db_path):
    with pytest.raises(ValueError, match="disabled"):
        macrame.Database.open(db_path, wal_autocheckpoint="off")


def test_the_two_cache_sizes_are_separate_knobs(db_path):
    """Split because the writer is one connection and the readers are several.

    Asserted through the reader, which is the half `diagnostic_conn` can reach
    (D-158, D-159): one number for both would mean either starving the writer
    or multiplying the readers' footprint by the connection count.
    """
    with macrame.Database.open(
        db_path,
        snapshot_every_entries=None,
        writer_cache_size=-64_000,
        reader_cache_size=-4_000,
    ) as handle:
        rows = handle.diagnostic_query("PRAGMA cache_size")
    assert rows[0][0] == -4_000


def test_the_defaults_leave_every_mechanism_alone(db_path):
    """D-155's lesson at this boundary: absent must not mean off.

    Both earlier attempts at this API put a `None` that *disables* a mechanism
    into a default, which would have silently stopped snapshot anchoring and
    WAL bounding for every caller who did not know the knob existed. The knobs
    are keywords here, so the check is that omitting them changes nothing.

    The cadence is the half that is observable: omitting
    `snapshot_every_entries` must leave anchoring **on**, not off. The two
    pragma knobs are checked in the Rust suite, for the reason
    `test_a_page_threshold_is_accepted_and_is_not_observable_from_here`
    records.
    """
    with macrame.Database.open(db_path) as handle:
        handle.upsert_concept(macrame.ConceptUpsert("a", "Alpha", valid_from=T0))
        # The writer's own cache size is not reachable either; what a reader can
        # see is that omitting `reader_cache_size` left SQLite's default in
        # place rather than applying a zero.
        assert handle.diagnostic_query("PRAGMA cache_size")[0][0] != 0

    reopened = macrame.Database.open(db_path)
    try:
        assert reopened.snapshots_dir.exists(), (
            "opening with no cadence keyword produced a handle that never "
            "anchored — the default disabled a mechanism instead of leaving "
            "it alone, which is D-155's failure exactly"
        )
    finally:
        reopened.close()
