"""P3 acceptance: coercion at the boundary.

The ledger speaks canonical strings; Python speaks ``datetime``. Everything here
is about the translation being lossless in the directions it claims and refusing
— loudly — in the directions it cannot serve.
"""

from __future__ import annotations

import array
import datetime as dt

import pytest

import macrame
from macrame import _macrame

UTC = dt.timezone.utc
T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-06-01T12:30:45.123456Z"


# --------------------------------------------------------------------------
# Timestamps in
# --------------------------------------------------------------------------


def test_a_canonical_string_passes_through_unchanged():
    assert _macrame._coerce_timestamp(T1) == T1


def test_second_precision_is_widened_not_rejected():
    """The crate's own `normalize` accepts the legacy form; the binding does not
    add a rule the library does not have."""
    assert _macrame._coerce_timestamp("2026-01-01T00:00:00Z") == T0


def test_an_aware_datetime_is_converted():
    d = dt.datetime(2026, 6, 1, 12, 30, 45, 123456, tzinfo=UTC)
    assert _macrame._coerce_timestamp(d) == T1


def test_a_non_utc_datetime_is_shifted_not_stripped():
    """The offset is applied, not discarded.

    Discarding it would store a wall-clock reading as though it were UTC — the
    exact silent repair this layer exists to prevent, and one that produces a
    plausible timestamp that is simply the wrong instant.
    """
    berlin = dt.timezone(dt.timedelta(hours=2))
    d = dt.datetime(2026, 6, 1, 14, 30, 45, 123456, tzinfo=berlin)
    assert _macrame._coerce_timestamp(d) == T1


def test_a_naive_datetime_is_refused():
    """**Not** assumed to be UTC.

    §4.1's rule about timestamps, applied one layer out: a naive datetime does
    not say which instant it names, and picking one for the caller is a wrong
    answer in a temporal query later, shifted by an amount nothing records.
    """
    d = dt.datetime(2026, 6, 1, 12, 30, 45)
    with pytest.raises(macrame.InvalidTimestampError, match="naive"):
        _macrame._coerce_timestamp(d)


def test_a_bare_date_is_refused():
    """Which midnight, in which zone, is what a `date` does not answer."""
    with pytest.raises(macrame.InvalidTimestampError, match="names a day"):
        _macrame._coerce_timestamp(dt.date(2026, 6, 1))


@pytest.mark.parametrize(
    "bad",
    [
        "2026-06-01T12:30:45+01:00",  # offset, not Z
        "2026-06-01T12:30:45.123Z",  # milliseconds
        "2026-06-01 12:30:45Z",  # space, not T
        "2026-06-01",
        "",
        "yesterday",
    ],
)
def test_non_canonical_strings_are_refused(bad):
    with pytest.raises(macrame.InvalidTimestampError):
        _macrame._coerce_timestamp(bad)


@pytest.mark.parametrize("bad", [42, 1.5, [], {}, object()])
def test_a_value_that_is_not_a_time_is_a_type_error(bad):
    with pytest.raises(TypeError):
        _macrame._coerce_timestamp(bad)


# --------------------------------------------------------------------------
# The open sentinel
# --------------------------------------------------------------------------


def test_none_means_the_open_sentinel():
    assert _macrame._coerce_timestamp(None) == macrame.OPEN


def test_the_sentinel_comes_back_as_none():
    assert _macrame._render_timestamp(macrame.OPEN) is None


def test_the_sentinel_string_is_still_accepted_on_the_way_in():
    """A caller round-tripping stored values must not be forced through `None`."""
    assert _macrame._coerce_timestamp(macrame.OPEN) == macrame.OPEN


def test_datetime_max_is_recognised_as_the_sentinel():
    """Someone will pass it. It is the same instant, so it must mean the same
    thing rather than storing a value one microsecond from the sentinel."""
    d = dt.datetime(9999, 12, 31, 23, 59, 59, 999999, tzinfo=UTC)
    assert _macrame._coerce_timestamp(d) == macrame.OPEN


def test_why_the_sentinel_is_not_a_datetime():
    """Probe P3-a, kept as a test so the reasoning cannot quietly rot.

    If a future CPython makes these work, the `None` representation becomes a
    choice rather than a necessity, and this failing is the signal to revisit
    D-096. Until then it documents *why* an open interval is `None`.
    """
    d = dt.datetime(9999, 12, 31, 23, 59, 59, 999999, tzinfo=UTC)
    with pytest.raises(OverflowError):
        d.astimezone(dt.timezone(dt.timedelta(hours=1)))
    with pytest.raises(OverflowError):
        d + dt.timedelta(microseconds=1)


# --------------------------------------------------------------------------
# Timestamps out
# --------------------------------------------------------------------------


def test_a_stored_timestamp_comes_back_aware_and_utc():
    d = _macrame._render_timestamp(T1)
    assert isinstance(d, dt.datetime)
    assert d.tzinfo is not None and d.utcoffset() == dt.timedelta(0)
    assert (d.year, d.month, d.day) == (2026, 6, 1)
    assert (d.hour, d.minute, d.second, d.microsecond) == (12, 30, 45, 123456)


@pytest.mark.parametrize("canonical", [T0, T1, "1970-01-01T00:00:00.000000Z"])
def test_timestamps_round_trip(canonical):
    assert _macrame._coerce_timestamp(_macrame._render_timestamp(canonical)) == canonical


def test_microseconds_survive_a_round_trip_at_a_whole_second():
    """The case `isoformat()` would have broken.

    Python omits `.000000` when the microseconds are zero, so an
    `isoformat()`-based renderer would emit a 20-character string for every
    timestamp landing exactly on a second — non-canonical, and only for *some*
    values, which is the worst way for a format bug to behave.
    """
    assert _macrame._coerce_timestamp(_macrame._render_timestamp(T0)) == T0
    assert len(T0) == 27


# --------------------------------------------------------------------------
# Value types
# --------------------------------------------------------------------------


def test_an_edge_assertion_takes_keywords_and_normalises():
    e = macrame.EdgeAssertion("a", "b", "LINKS", valid_from="2026-01-01T00:00:00Z")
    assert e.source == "a" and e.target == "b" and e.edge_type == "LINKS"
    assert e.valid_from == dt.datetime(2026, 1, 1, tzinfo=UTC)
    assert e.valid_to is None
    assert e.weight == 1.0
    assert e.properties == "{}"


def test_validation_happens_in_the_constructor():
    """**A deviation from the plan, and the reason is bulk writes.**

    Deferring to the write means a list of ten thousand edges fails with
    "invalid edge type" and no indication which one, from a traceback pointing
    at the write. Here the traceback points at the line that built it.
    """
    with pytest.raises(macrame.InvalidEdgeTypeError):
        macrame.EdgeAssertion("a", "b", "lowercase", valid_from=T0)
    with pytest.raises(macrame.InvalidEdgeTypeError):
        macrame.EdgeAssertion("a", "b", "HAS|PIPE", valid_from=T0)


def test_an_invalid_id_is_refused_at_construction():
    with pytest.raises(macrame.InvalidIdError):
        macrame.EdgeAssertion("a|b", "c", "LINKS", valid_from=T0)


def test_valid_from_is_required():
    with pytest.raises(TypeError):
        macrame.EdgeAssertion("a", "b", "LINKS")
    with pytest.raises(TypeError):
        macrame.ConceptUpsert("a", "A")


def test_a_concept_upsert_carries_its_fields():
    c = macrame.ConceptUpsert(
        "a", "A", valid_from=T0, content="body", embedding_model="nomic_v1"
    )
    assert (c.id, c.title, c.content) == ("a", "A", "body")
    assert c.embedding_model == "nomic_v1"
    assert c.retired is False
    assert c.valid_to is None


def test_value_types_compare_by_value():
    """So `assert result == expected` works, and so a list of them dedupes."""
    a = macrame.EdgeAssertion("a", "b", "LINKS", valid_from=T0)
    b = macrame.EdgeAssertion("a", "b", "LINKS", valid_from=T0)
    c = macrame.EdgeAssertion("a", "b", "LINKS", valid_from=T0, weight=0.5)
    assert a == b
    assert a != c


def test_an_annotation_is_not_a_concept_upsert():
    """D-041, pinned as a type distinction.

    An upsert is a statement about the world; an annotation is a function of an
    algorithm over a graph. Conflating them overwrote concept content with
    labels and recorded every analytics rerun as a new version of the world.
    """
    ann = macrame.Annotation("a", "louvain.community", "3")
    assert not isinstance(ann, macrame.ConceptUpsert)
    assert (ann.concept_id, ann.label, ann.value) == ("a", "louvain.community", "3")


def test_intervals_answer_the_three_questions():
    open_iv = macrame.Interval(T0)
    closed = macrame.Interval(T0, T1)
    assert open_iv.is_open() and not closed.is_open()
    assert closed.contains("2026-03-01T00:00:00.000000Z")
    assert not closed.contains(T1)  # half-open: the end is excluded
    assert closed.contains(T0)
    assert open_iv.overlaps(closed)


def test_an_edge_reports_its_own_interval():
    e = macrame.EdgeAssertion("a", "b", "LINKS", valid_from=T0, valid_to=T1)
    assert e.interval() == macrame.Interval(T0, T1)


def test_the_attribute_mode_enum_has_the_three_modes():
    assert macrame.AttributeMode.CURRENT != macrame.AttributeMode.AT_TIME
    assert macrame.AttributeMode.OMIT is not None


# --------------------------------------------------------------------------
# Embeddings
# --------------------------------------------------------------------------


def test_a_list_of_floats_is_accepted():
    assert _macrame._coerce_embedding([1.0, 2.0, 3.5]) == [1.0, 2.0, 3.5]


def test_an_array_module_float_array_is_accepted():
    assert _macrame._coerce_embedding(array.array("f", [1.0, 2.0])) == [1.0, 2.0]


def test_packed_float32_bytes_are_accepted():
    """The documented fast path, since abi3 removed the buffer protocol."""
    packed = array.array("f", [1.0, 2.0, 3.5]).tobytes()
    assert _macrame._coerce_embedding(packed) == [1.0, 2.0, 3.5]


def test_a_numpy_float32_array_round_trips_both_ways():
    np = pytest.importorskip("numpy")
    vec = np.array([1.0, 2.0, 3.5], dtype=np.float32)
    assert _macrame._coerce_embedding(vec) == [1.0, 2.0, 3.5]
    assert _macrame._coerce_embedding(vec.astype("<f4").tobytes()) == [1.0, 2.0, 3.5]


def test_a_numpy_float64_array_is_converted_not_reinterpreted():
    """The dangerous case: float64 bytes read as float32 would be garbage of
    twice the length. Element-wise conversion is the only correct reading."""
    np = pytest.importorskip("numpy")
    vec = np.array([1.0, 2.0, 3.5], dtype=np.float64)
    assert _macrame._coerce_embedding(vec) == [1.0, 2.0, 3.5]


def test_a_tuple_of_small_ints_is_not_mistaken_for_packed_bytes():
    """**The bug an earlier draft of this shipped.**

    Accepting anything that extracts as ``Vec<u8>`` on the packed path also
    swallows a tuple of small ints and reinterprets it as float32 — a silent
    wrong answer producing a valid embedding of a quarter the length, which the
    dimension check would then blame on the model.
    """
    assert _macrame._coerce_embedding((1, 2, 3, 4)) == [1.0, 2.0, 3.0, 4.0]
    assert _macrame._coerce_embedding([0, 0, 0, 0]) == [0.0, 0.0, 0.0, 0.0]


def test_a_truncated_packed_buffer_is_refused():
    """Refused, not truncated: a length that is not a multiple of four means the
    caller's dtype is not what they think it is."""
    with pytest.raises(ValueError, match="multiple of 4"):
        _macrame._coerce_embedding(b"\x00\x01\x02")


def test_a_string_is_not_an_embedding():
    """A `str` is a sequence of length-1 strings and would otherwise fail deep
    inside the extraction with an unhelpful message."""
    with pytest.raises(TypeError, match="got str"):
        _macrame._coerce_embedding("1.0 2.0")
