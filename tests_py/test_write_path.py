"""P4.1 acceptance: the write path.

The ledger's own semantics are covered by 300 Rust tests and are not re-asserted
here. What is new at this boundary is that values built in Python reach the Write
Actor intact, that counts come back, and that the ledger's typed errors arrive as
the P2 exception classes **with their fields populated** — which until now was
only tested against synthetic errors from a hook.

Reads use ``diagnostic_query``, pulled forward from P4.6. Without a read path
there is no way to tell a write that landed from a method that returned a
plausible count and did nothing.
"""

from __future__ import annotations

import datetime as dt

import pytest

import macrame

UTC = dt.timezone.utc
T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-06-01T00:00:00.000000Z"
T2 = "2026-09-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    """A ledger with three concepts and no snapshot cadence.

    No cadence because these tests assert on row counts, and a background
    anchor writer is one more thing touching the file for no benefit here.
    """
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [macrame.ConceptUpsert(f"n{i}", f"N{i}", valid_from=T0) for i in range(4)]
        )
        yield handle


def count(handle, table):
    return handle.diagnostic_query(f"SELECT COUNT(*) FROM {table}")[0][0]


# --------------------------------------------------------------------------
# Concepts
# --------------------------------------------------------------------------


def test_a_concept_written_one_at_a_time_lands(db):
    db.upsert_concept(macrame.ConceptUpsert("solo", "Solo", valid_from=T0, content="body"))
    rows = db.diagnostic_query(
        "SELECT title, content FROM concepts WHERE id = ?1", ["solo"]
    )
    assert rows == [("Solo", "body")]


def test_bulk_concepts_return_a_count_and_land(db):
    n = db.write_concepts(
        [macrame.ConceptUpsert(f"b{i}", f"B{i}", valid_from=T0) for i in range(25)]
    )
    assert n == 25
    assert count(db, "concepts") == 4 + 25


def test_a_datetime_survives_the_whole_way_to_storage(db):
    """The P3 coercion, end to end rather than through a test hook."""
    when = dt.datetime(2026, 3, 15, 9, 30, 0, 500000, tzinfo=UTC)
    db.upsert_concept(macrame.ConceptUpsert("d", "D", valid_from=when))
    stored = db.diagnostic_query("SELECT valid_from FROM concepts WHERE id='d'")[0][0]
    assert stored == "2026-03-15T09:30:00.500000Z"


def test_an_open_valid_to_is_stored_as_the_sentinel(db):
    """`None` in Python is the sentinel in the column. The binding's whole
    timestamp contract, checked against what is actually written."""
    db.upsert_concept(macrame.ConceptUpsert("o", "O", valid_from=T0, valid_to=None))
    stored = db.diagnostic_query("SELECT valid_to FROM concepts WHERE id='o'")[0][0]
    assert stored == macrame.OPEN


# --------------------------------------------------------------------------
# Edges
# --------------------------------------------------------------------------


def test_an_asserted_edge_lands_in_current_belief(db):
    db.assert_edge(macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0))
    rows = db.diagnostic_query(
        "SELECT source_id, target_id, edge_type, valid_to FROM links_current"
    )
    assert rows == [("n0", "n1", "LINKS", macrame.OPEN)]


def test_retiring_closes_the_open_interval(db):
    db.assert_edge(macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0))
    db.retire_edge("n0", "n1", "LINKS", T0, T1)
    rows = db.diagnostic_query("SELECT valid_to FROM links_current")
    assert rows == [(T1,)]


def test_retiring_to_an_open_end_is_refused(db):
    """`None` means open, and retiring to an open end is not a retirement.

    Refused here rather than passed down: the ledger would answer with a
    single-open violation about a row the caller did not think they wrote.
    """
    db.assert_edge(macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0))
    with pytest.raises(ValueError, match="not a retirement"):
        db.retire_edge("n0", "n1", "LINKS", T0, None)


def test_bulk_import_returns_a_count_and_lands(db):
    # Endpoints first: `foreign_keys = ON` is set on every connection, so an
    # edge to a concept that does not exist is a FK failure, not a dangling row.
    db.write_concepts(
        [macrame.ConceptUpsert(f"t{i}", f"T{i}", valid_from=T0) for i in range(30)]
    )
    edges = [
        macrame.EdgeAssertion("n0", f"t{i}", "LINKS", valid_from=T0)
        for i in range(30)
    ]
    assert db.bulk_import(edges) == 30
    assert count(db, "links_current") == 30


def test_write_bulk_atomic_returns_a_count_and_lands(db):
    db.write_concepts(
        [macrame.ConceptUpsert(f"a{i}", f"A{i}", valid_from=T0) for i in range(20)]
    )
    edges = [
        macrame.EdgeAssertion("n1", f"a{i}", "LINKS", valid_from=T0)
        for i in range(20)
    ]
    assert db.write_bulk_atomic(edges) == 20
    assert count(db, "links_current") == 20


def test_an_edge_to_a_concept_that_does_not_exist_is_refused(db):
    """The referential guard, from the Python side.

    Worth pinning because it is the first thing anyone hits when they write
    edges before concepts, and the error that comes back is an engine-level FK
    failure rather than one of the ledger's typed errors — so a reader needs to
    know it is the schema talking, not a bug in the binding.
    """
    with pytest.raises(macrame.EngineError, match="FOREIGN KEY"):
        db.assert_edge(macrame.EdgeAssertion("n0", "nope", "LINKS", valid_from=T0))


# --------------------------------------------------------------------------
# The distinction D-041 exists to enforce
# --------------------------------------------------------------------------


def test_annotations_do_not_reach_the_transaction_log(db):
    """**The load-bearing test of this phase.**

    An annotation is a function of an algorithm over a graph, not a statement
    about the world. `analytics_annotations` carries no log trigger, so
    rerunning Louvain replaces the previous pass instead of recording that the
    world changed. Before D-041 these were one call, and analytics output
    overwrote concept content and versioned it.

    If this ever fails, the two writes have been merged again and every
    analytics rerun is now history.
    """
    before = count(db, "transaction_log")
    n = db.write_analytics_annotations(
        [macrame.Annotation(f"n{i}", "louvain.community", str(i)) for i in range(4)]
    )
    assert n == 4
    assert count(db, "analytics_annotations") == 4
    assert count(db, "transaction_log") == before, (
        "an annotation reached the transaction log: the analytics write and the "
        "ledger write have been conflated again (D-041)"
    )


def test_concepts_do_reach_the_transaction_log(db):
    """The contrast that makes the test above mean something.

    Without it, a `write_analytics_annotations` that silently wrote nothing at
    all would pass.
    """
    before = count(db, "transaction_log")
    db.write_concepts([macrame.ConceptUpsert("logged", "L", valid_from=T0)])
    assert count(db, "transaction_log") > before


# --------------------------------------------------------------------------
# Errors, end to end
# --------------------------------------------------------------------------


def test_an_overlapping_interval_raises_with_both_intervals(db):
    """P2's mapping against a real ledger error rather than a synthetic one.

    Both intervals are reported because neither alone identifies the conflict:
    the caller knows what they asserted and not what it collided with.
    """
    db.assert_edge(
        macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0, valid_to=T1)
    )
    with pytest.raises(macrame.OverlappingIntervalError) as caught:
        db.assert_edge(
            macrame.EdgeAssertion(
                "n0", "n1", "LINKS", valid_from="2026-03-01T00:00:00.000000Z", valid_to=T2
            )
        )
    e = caught.value
    assert (e.source_id, e.target_id, e.edge_type) == ("n0", "n1", "LINKS")
    assert e.valid_from == "2026-03-01T00:00:00.000000Z"  # what was asserted
    assert e.existing_from == T0  # what it hit


def test_a_second_open_interval_raises_single_open(db):
    db.assert_edge(macrame.EdgeAssertion("n1", "n2", "LINKS", valid_from=T0))
    with pytest.raises(macrame.SingleOpenViolationError) as caught:
        db.assert_edge(macrame.EdgeAssertion("n1", "n2", "LINKS", valid_from=T1))
    assert caught.value.source_id == "n1"


def test_a_non_overlapping_later_interval_is_accepted(db):
    """The counterpart: the guards must not refuse legitimate history.

    A closed interval followed by a disjoint later one is ordinary bitemporal
    use, and a test suite that only proves things are rejected cannot tell a
    working guard from a broken write path.
    """
    db.assert_edge(
        macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0, valid_to=T1)
    )
    db.assert_edge(macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T2))
    assert count(db, "links") == 2


@pytest.mark.parametrize(
    "call",
    [
        lambda d: d.upsert_concept(macrame.ConceptUpsert("x", "X", valid_from=T0)),
        lambda d: d.assert_edge(macrame.EdgeAssertion("n0", "n1", "LINKS", valid_from=T0)),
        lambda d: d.write_concepts([]),
        lambda d: d.bulk_import([]),
        lambda d: d.write_bulk_atomic([]),
        lambda d: d.write_analytics_annotations([]),
        lambda d: d.diagnostic_query("SELECT 1"),
        lambda d: d.explain("SELECT 1"),
    ],
)
def test_every_write_refuses_a_closed_handle(db_path, call):
    handle = macrame.Database.open(db_path, snapshot_every_entries=None)
    handle.close()
    with pytest.raises(macrame.MacrameClosedError):
        call(handle)


# --------------------------------------------------------------------------
# The hold estimate (T1.3)
# --------------------------------------------------------------------------


def test_the_hold_estimate_is_available_before_the_write():
    """T1.3's whole delivery, and the reason it must exist in Python.

    `write_bulk_atomic` is the one write with no latency bound. A caller who
    cannot ask what a batch will cost is back at 0.5.x, where the only statement
    of it was the word "uncapped" in a doc table.
    """
    edges = [macrame.EdgeAssertion(f"s{i}", "t", "LINKS", valid_from=T0) for i in range(500)]
    held = macrame.estimate_bulk_hold(edges)
    assert isinstance(held, dt.timedelta)
    # Measured on libSQL 0.9.30: 500 rows is ~33 ms. The band is wide because
    # the estimate is a shape, not a promise.
    assert dt.timedelta(milliseconds=10) < held < dt.timedelta(milliseconds=200)


def test_the_estimate_no_longer_depends_on_the_batchs_shape():
    """The 7× case the model used to exist for, closed in 0.13.6 (W7.5, D-179).

    20,000 edges spread over distinct relationships and 20,000 corrections to
    one relationship's history were the same row count and not the same cost:
    the second reached the within-batch overlap guard's expensive path on every
    pair, and the guard was quadratic. It sorts and sweeps now, the measured
    holds agree to within 15%, and a model still predicting a 7× spread would
    warn about a batch that is fine.
    """
    n = 2000
    spread = [macrame.EdgeAssertion(f"s{i}", "t", "LINKS", valid_from=T0) for i in range(n)]
    same = [
        macrame.EdgeAssertion(
            "s", "t", "LINKS", valid_from=f"2026-01-01T00:00:{i // 60:02}.{i % 60:06}Z"
        )
        for i in range(n)
    ]
    assert macrame.estimate_bulk_hold(same) == macrame.estimate_bulk_hold(spread)


def test_an_empty_batch_estimates_to_nothing():
    assert macrame.estimate_bulk_hold([]) == dt.timedelta(0)


def test_the_warn_threshold_is_exposed_as_a_timedelta():
    """So a caller compares against it rather than hard-coding 250 ms."""
    assert macrame.BULK_ATOMIC_WARN_HOLD == dt.timedelta(milliseconds=250)


def test_the_chunk_ceilings_are_reachable_and_match_the_ledger(db):
    """The four `chunk_rows` constants, exposed as ceilings (W6.1, D-143/D-146).

    Values are pinned because the point of exposing them is that a caller can
    reason about a batch without reading Rust; a constant that drifts silently
    is worse than one that is absent, since the caller has no way to notice.

    They are **ceilings**, which is what the second half asserts in the only way
    Python can: a chunk larger than the ceiling is never asked for, so a batch
    smaller than the smallest ceiling is one chunk at most, whatever the
    controller decides.
    """
    assert macrame.CHUNK_ROWS_EDGES == 90
    assert macrame.CHUNK_ROWS_CONCEPTS == 70
    assert macrame.CHUNK_ROWS_ANNOTATIONS == 600
    assert macrame.CHUNK_ROWS_EMBEDDINGS == 30

    # Under the ceiling, so it cannot be more than one chunk — and the point of
    # the constant is that a caller can determine that in advance.
    n = macrame.CHUNK_ROWS_CONCEPTS - 1
    written = db.write_concepts(
        [macrame.ConceptUpsert(f"c{i:03}", f"C{i}", valid_from=T0) for i in range(n)]
    )
    assert written == n


def test_the_archive_session_ceiling_is_reachable():
    """`archive_windowed` refuses above it, so a caller can check first (W6.1).

    Exposed for the pre-flight: span divided by window against this number is a
    computation the caller can do, and the alternative is discovering the
    refusal by catching `ArchiveWindowError` after the call has been made.
    """
    assert macrame.MAX_ARCHIVE_SESSIONS == 4096


# --------------------------------------------------------------------------
# The diagnostic read
# --------------------------------------------------------------------------


def test_the_diagnostic_connection_cannot_write(db):
    """`SQLITE_OPEN_READ_ONLY` is a boundary, not a guardrail (T5.1, D-091).

    A `PRAGMA query_only` connection would also refuse this. What distinguishes
    the two is that the pragma can be turned off in one statement, and there is
    no statement that turns this off.
    """
    with pytest.raises(macrame.MacrameError, match="readonly"):
        db.diagnostic_query("INSERT INTO concepts (id,title) VALUES ('x','X')")


def test_parameters_are_bound_not_interpolated(db):
    rows = db.diagnostic_query(
        "SELECT id FROM concepts WHERE id = ?1 OR id = ?2", ["n0", "n2"]
    )
    assert sorted(r[0] for r in rows) == ["n0", "n2"]


def test_a_value_that_cannot_be_bound_is_refused(db):
    """Refused rather than stringified.

    A `datetime` silently coerced to `str(dt)` would compare a Python repr
    against a canonical timestamp, match nothing, and read as "the data is not
    there".
    """
    with pytest.raises(TypeError, match="cannot bind"):
        db.diagnostic_query("SELECT 1 WHERE ?1 = 1", [dt.datetime.now(UTC)])


def test_null_and_blob_cross_back_as_none_and_bytes(db):
    assert db.diagnostic_query("SELECT NULL")[0][0] is None
    assert db.diagnostic_query("SELECT x'DEADBEEF'")[0][0] == b"\xde\xad\xbe\xef"


def test_explain_returns_the_plan(db):
    plan = db.explain("SELECT * FROM links_current WHERE source_id = 'n0'")
    assert plan and any("links_current" in line for line in plan)
