"""P2 acceptance: every ``DbError`` variant reaches a distinct Python class.

Completeness is enforced in two places and neither alone is enough:

* **The compiler.** ``errors::build`` is a ``match`` over ``DbError`` with no
  wildcard arm, so a variant added upstream fails to build ``macrame-py`` at the
  line that needs a decision. That is stronger than any test — a test can only
  run once the thing exists, and the failure being guarded against is a new
  variant falling silently through to the base class, which is exactly what a
  wildcard arm would hide.
* **This file.** A compiler cannot check that a ``setattr`` used the right name,
  that the class sits under the right base, or that the class is reachable from
  ``macrame``. That is what is checked here.

``EXPECTED`` below deliberately restates the Rust mapping. Two independent
statements that must agree is the point; a table derived from the code it is
checking would agree with itself.
"""

from __future__ import annotations

import datetime as dt
import re
from pathlib import Path

import pytest

import macrame
from macrame import _macrame

REPO = Path(__file__).resolve().parent.parent


# variant -> (class name, base class name, {attribute: expected value})
EXPECTED: dict[str, tuple[str, str, dict]] = {
    "Engine": ("EngineError", "MacrameError", {}),
    "Migration": ("MigrationError", "MacrameError", {"to": 42, "reason": "sample-reason"}),
    "NotFound": ("NotFoundError", "MacrameError", {"id": "missing-1"}),
    "DiagnosticConn": (
        "DiagnosticConnError",
        "MacrameError",
        {"path": "sample.db", "reason": "sample-reason"},
    ),
    # Cancellation is not an integrity failure, so it hangs off the base class
    # directly (0.13.8, D-181). `written` is on every exception and is not a
    # per-variant field, so it is not listed here for this variant or any other
    # -- `test_every_raised_error_carries_written` covers it once, below.
    "BulkCancelled": ("BulkCancelledError", "MacrameError", {}),
    # -- integrity --
    "OverlappingInterval": (
        "OverlappingIntervalError",
        "IntegrityError",
        {
            "source_id": "src-1",
            "target_id": "tgt-1",
            "edge_type": "LINKS",
            "valid_from": "2026-03-01T00:00:00.000000Z",
            "valid_to": "2026-09-01T00:00:00.000000Z",
            "existing_from": "2026-01-01T00:00:00.000000Z",
            "existing_to": "2026-06-01T00:00:00.000000Z",
            "within_batch": False,
        },
    ),
    "SingleOpenViolation": (
        "SingleOpenViolationError",
        "IntegrityError",
        {"source_id": "src-1", "target_id": "tgt-1", "edge_type": "LINKS"},
    ),
    "NegativeEdgeWeight": (
        "NegativeEdgeWeightError",
        "IntegrityError",
        {"source_id": "src-1", "target_id": "tgt-1", "weight": -1.5},
    ),
    "CurrentDrift": ("CurrentDriftError", "IntegrityError", {"n": 4242}),
    "RebuildFailed": ("RebuildFailedError", "IntegrityError", {"n": 4242}),
    "RebuildInterrupted": (
        "RebuildInterruptedError",
        "IntegrityError",
        {"reason": "sample-reason"},
    ),
    "RecordedAtRegression": (
        "RecordedAtRegressionError",
        "IntegrityError",
        {"got": "2026-01-01T00:00:00.000000Z", "had": "2026-06-01T00:00:00.000000Z"},
    ),
    "ArchiveSessionLeaked": (
        "ArchiveSessionLeakedError",
        "IntegrityError",
        {"marker": "macrame_archive_session"},
    ),
    # -- validation --
    "InvalidEdgeType": ("InvalidEdgeTypeError", "ValidationError", {"edge_type": "bad-type"}),
    "InvalidId": ("InvalidIdError", "ValidationError", {"id": "bad|id", "reason": "sample-reason"}),
    "InvalidTimestamp": (
        "InvalidTimestampError",
        "ValidationError",
        {"value": "not-a-time", "reason": "sample-reason"},
    ),
    "InvalidModelName": ("InvalidModelNameError", "ValidationError", {"model": "Bad-Model"}),
    # A trailing space, which is the pair of names the type exists for: it is
    # invisible in every terminal and is a second lineage in an append-only
    # table. Under `ValidationError` and not `BranchError` on purpose — the
    # hierarchy groups by what went wrong, not by which feature was called.
    "InvalidBranchId": ("InvalidBranchIdError", "ValidationError", {"branch": "release "}),
    # -- branch (W12.7) --
    "UnknownBranch": ("UnknownBranchError", "BranchError", {"branch": "ghost"}),
    "BranchExists": ("BranchExistsError", "BranchError", {"branch": "main"}),
    "ForkPrecedesParent": (
        "ForkPrecedesParentError",
        "BranchError",
        {
            "branch": "behind",
            "parent": "ahead",
            "forked_at": "2026-01-01T00:00:00.000000Z",
            "parent_forked_at": "2999-01-01T00:00:00.000000Z",
        },
    ),
    # -- branch (W12.8, W12.9) --
    "CrossLineage": (
        "CrossLineageError",
        "BranchError",
        {"id": "socrates", "held_by": "main", "attempted": "alt"},
    ),
    "BranchMismatch": (
        "BranchMismatchError",
        "BranchError",
        {"view": "alt", "named": "other"},
    ),
    # Both axes on the sample, so the pair is checked rather than one of them
    # (0.13.10, D-183). `as_of` was the old single field and named a keyword
    # that stopped existing in 0.13.2.
    "AttributeModeUnstated": (
        "AttributeModeUnstatedError",
        "ValidationError",
        {
            "as_of_valid": "2026-02-03T04:05:06.000007Z",
            "as_of_recorded": "2026-04-05T06:07:08.000009Z",
        },
    ),
    # No attributes, and that is the assertion: the remedy is a keyword on the
    # same call, so there is nothing to carry (0.13.20, D-193).
    "HalfLifeWithoutInstant": ("HalfLifeWithoutInstantError", "ValidationError", {}),
    "RecordedInstantUnreachable": (
        "RecordedInstantUnreachableError",
        "TemporalError",
        {"ts": "2026-02-03T04:05:06.000007Z"},
    ),
    "FutureRecordedAt": (
        "FutureRecordedAtError",
        "IntegrityError",
        {"stamp": "2065-02-03T04:05:06.000007Z", "limit": "2026-02-04T04:05:06.000007Z"},
    ),
    # -- vector --
    "DimMismatch": (
        "DimMismatchError",
        "VectorError",
        {"got": 7, "expected": 768, "model": "nomic_v1"},
    ),
    "ModelNotRegistered": (
        "ModelNotRegisteredError",
        "VectorError",
        {"model": "nomic_v1", "table": "embeddings_nomic_v1"},
    ),
    # -- temporal --
    "ReplayCorrupt": (
        "ReplayCorruptError",
        "TemporalError",
        {"seq": 4242, "reason": "sample-reason"},
    ),
    "SnapshotIncompatible": (
        "SnapshotIncompatibleError",
        "TemporalError",
        {"path": "sample.snap", "reason": "sample-reason"},
    ),
    "SnapshotCorrupt": (
        "SnapshotCorruptError",
        "TemporalError",
        {"path": "sample.snap", "reason": "sample-reason"},
    ),
    "PayloadVersion": ("PayloadVersionError", "TemporalError", {"got": 9, "max": 2}),
    "ArchiveViolation": ("ArchiveViolationError", "TemporalError", {"table": "links"}),
    "ArchiveWindow": (
        "ArchiveWindowError",
        "TemporalError",
        {"window": dt.timedelta(seconds=90), "reason": "sample-reason"},
    ),
    # -- writer --
    "WriterUnavailable": ("WriterUnavailableError", "WriterError", {}),
    "WriterDroppedResponder": ("WriterDroppedResponderError", "WriterError", {}),
    "WriterStopped": ("WriterStoppedError", "WriterError", {"reason": "sample-reason"}),
    # -- budget --
    "SubgraphTooLarge": ("SubgraphTooLargeError", "BudgetError", {"n": 4242, "budget": 1000}),
}


def _variants_declared_in_rust() -> list[str]:
    """Parse ``pub enum DbError`` out of ``src/error.rs``.

    Reading the source rather than trusting a list is the same move the Rust
    suite makes for the decision register and the bench file: the thing being
    described is the authority, and a hand-kept copy of it drifts.
    """
    src = (REPO / "src" / "error.rs").read_text(encoding="utf-8")
    body = src.split("pub enum DbError {", 1)[1]
    depth, end = 1, 0
    for i, ch in enumerate(body):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                end = i
                break
    names = []
    attr_depth = 0
    for line in body[:end].splitlines():
        stripped = line.strip()
        # An attribute can span lines, and a `#[error("…")]` message wraps
        # wherever the text wraps. Skipping only the line that *starts* with
        # `#[` left the continuations in, so any wrapped line beginning with a
        # capital was read as a variant — `FutureStampPolicy::Allow`, sitting
        # in the message that tells a caller how to open a refused database,
        # was reported as a `DbError` variant that no sample table covers
        # (0.13.5, W7.4). Track the attribute\'s parens instead.
        if attr_depth == 0 and stripped.startswith("#["):
            attr_depth = 1
        if attr_depth:
            attr_depth += stripped.count("(") - stripped.count(")")
            if stripped.endswith("]") and attr_depth <= 1:
                attr_depth = 0
            continue
        if not stripped or stripped.startswith(("///", "//")):
            continue
        m = re.match(r"^([A-Z]\w*)", stripped)
        if m:
            names.append(m.group(1))
    return names


# --------------------------------------------------------------------------
# Completeness
# --------------------------------------------------------------------------


def test_the_sample_table_covers_every_variant_in_the_rust_enum():
    """The seam between the two enforcement mechanisms.

    The compiler guarantees every variant is *mapped*; this guarantees every
    variant is *exercised*. Without it, a variant could be added, mapped with a
    plausible-looking arm, and never once constructed.
    """
    declared = set(_variants_declared_in_rust())
    sampled = set(_macrame._db_error_variants())
    assert declared == sampled, (
        f"missing from the Rust sample table: {sorted(declared - sampled)}; "
        f"sampled but not in src/error.rs: {sorted(sampled - declared)}"
    )


def test_this_test_file_covers_every_variant():
    declared = set(_variants_declared_in_rust())
    assert declared == set(EXPECTED), (
        f"not asserted here: {sorted(declared - set(EXPECTED))}; "
        f"asserted but gone from src/error.rs: {sorted(set(EXPECTED) - declared)}"
    )


def test_every_variant_maps_to_a_distinct_class():
    """No two variants share a class.

    Sharing would be the quiet version of flattening: a caller catching
    ``NotFoundError`` and getting it for a refused id is back to reading the
    message to find out what happened.
    """
    classes = [cls for cls, _, _ in EXPECTED.values()]
    assert len(classes) == len(set(classes)), "two variants map to one class"


# --------------------------------------------------------------------------
# The mapping itself
# --------------------------------------------------------------------------


@pytest.mark.parametrize("variant", sorted(EXPECTED))
def test_the_variant_raises_its_class_under_its_base(variant):
    cls_name, base_name, _ = EXPECTED[variant]
    cls = getattr(macrame, cls_name)
    base = getattr(macrame, base_name)

    assert issubclass(cls, base), f"{cls_name} is not a {base_name}"
    assert issubclass(cls, macrame.MacrameError)

    with pytest.raises(cls) as caught:
        _macrame._raise_db_error(variant)
    # Not merely a subclass of the expected class — exactly it, or the test
    # would pass for anything more specific that happened to be raised.
    assert type(caught.value) is cls


@pytest.mark.parametrize("variant", sorted(EXPECTED))
def test_the_structured_fields_arrive_as_attributes(variant):
    """**The point of P2.**

    The crate spent several releases making these errors specific — a
    ``DiagnosticConnError`` rather than a ``NotFoundError`` because an error
    naming the wrong subject sends a caller to fix the wrong thing. Fields in
    the sentence and nowhere else means every caller who wants to act on one
    parses the sentence.
    """
    _, _, fields = EXPECTED[variant]
    with pytest.raises(macrame.MacrameError) as caught:
        _macrame._raise_db_error(variant)
    exc = caught.value
    for name, expected in fields.items():
        assert hasattr(exc, name), f"{type(exc).__name__} has no attribute {name!r}"
        assert getattr(exc, name) == expected, (
            f"{type(exc).__name__}.{name} is {getattr(exc, name)!r}, expected {expected!r}"
        )


@pytest.mark.parametrize("variant", sorted(EXPECTED))
def test_str_is_still_the_ledgers_own_rendering(variant):
    """Structured fields are additive. A caller who only wants the sentence
    keeps it, unchanged from what Rust prints."""
    with pytest.raises(macrame.MacrameError) as caught:
        _macrame._raise_db_error(variant)
    text = str(caught.value)
    assert text, "empty message"
    assert not text.startswith("<"), f"looks like a repr, not a message: {text!r}"


def test_an_archive_window_crosses_as_a_timedelta():
    """Not a float of seconds.

    The caller passed a duration; they get a duration back, comparable against
    whatever they computed it from. Called out separately because it is the one
    field in the whole mapping that is not a string or a number, so it is the
    one that would silently degrade.
    """
    with pytest.raises(macrame.ArchiveWindowError) as caught:
        _macrame._raise_db_error("ArchiveWindow")
    assert isinstance(caught.value.window, dt.timedelta)
    assert caught.value.window.total_seconds() == 90.0


# --------------------------------------------------------------------------
# The distinctions the crate paid for
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("a", "b", "why"),
    [
        ("NotFoundError", "InvalidIdError", "absent vs refused (D-069)"),
        ("NotFoundError", "DiagnosticConnError", "a file is not a node (D-069/D-091)"),
        ("RebuildFailedError", "RebuildInterruptedError", "did not repair vs did not run"),
        ("ReplayCorruptError", "SnapshotIncompatibleError", "a fault vs an upgrade"),
        # Three subjects, not two (0.13.12, W8.2, D-185). A damaged cache is
        # neither a damaged ledger nor a foreign file, and the response to it
        # -- delete the snapshot, fold from the log -- is its own.
        ("SnapshotCorruptError", "SnapshotIncompatibleError", "damaged vs foreign"),
        ("SnapshotCorruptError", "ReplayCorruptError", "the cache vs the ledger"),
        ("InvalidTimestampError", "ReplayCorruptError", "caller input vs damaged ledger"),
        ("SingleOpenViolationError", "OverlappingIntervalError", "the sentinel vs the general case"),
    ],
)
def test_deliberately_separated_errors_are_not_the_same_class(a, b, why):
    """Each pair is a distinction some release of this crate was spent making.

    Collapsing any of them is the exact regression P2 exists to prevent, and it
    is the kind that no functional test would notice — both sides still raise.
    """
    ca, cb = getattr(macrame, a), getattr(macrame, b)
    assert ca is not cb, why
    assert not issubclass(ca, cb) and not issubclass(cb, ca), (
        f"{a} and {b} are related by inheritance, so `except` catches both: {why}"
    )


def test_the_grouping_bases_are_catchable_as_groups():
    """``except TemporalError`` should catch a corrupt replay and an unusable
    archive window without naming either."""
    for variant in ("ReplayCorrupt", "ArchiveWindow", "PayloadVersion"):
        with pytest.raises(macrame.TemporalError):
            _macrame._raise_db_error(variant)
    for variant in ("CurrentDrift", "RebuildFailed", "OverlappingInterval"):
        with pytest.raises(macrame.IntegrityError):
            _macrame._raise_db_error(variant)


@pytest.mark.parametrize("variant", sorted(EXPECTED))
def test_every_raised_error_carries_written(variant):
    """`written` is on every exception, and it is `None` unless it means
    something (0.13.9, D-182).

    Python has no way to say what Rust says in the type system — that only the
    four chunked methods return `BulkResult<usize>` — so the alternative to a
    universal attribute is a caller writing `except MacrameError as e:
    log(e.written)` and getting an `AttributeError` raised *inside their except
    block*, which replaces the failure they were trying to record with one about
    the logging. Set centrally in `raise()`, so this holds for every variant
    without any arm having to remember it.

    `None` here and not `0`: `_raise_db_error` constructs the variant with no
    batch behind it, and on a chunked path `0` already means *the first chunk
    failed*. The int case is `test_a_failure_partway_says_how_much_of_the_batch_survived`
    in test_write_path.py.
    """
    with pytest.raises(macrame.MacrameError) as caught:
        _macrame._raise_db_error(variant)
    exc = caught.value
    assert hasattr(exc, "written"), (
        f"{type(exc).__name__} has no `written`; every Macrame exception carries "
        f"one so that inspecting an error never depends on which path raised it"
    )
    assert exc.written is None, (
        f"{type(exc).__name__}.written is {exc.written!r}; a directly constructed "
        f"variant has no partial write behind it, and `None` is what says so"
    )


def test_every_exported_error_is_a_macrame_error():
    """Nothing in ``__all__`` ending in Error escapes the base class."""
    for name in macrame.__all__:
        if not name.endswith("Error"):
            continue
        cls = getattr(macrame, name)
        assert issubclass(cls, macrame.MacrameError), f"{name} is not a MacrameError"


def test_the_exceptions_report_the_public_module():
    """``macrame.NotFoundError`` in a traceback, not ``_macrame.NotFoundError``.

    The extension module is an implementation detail; a traceback naming it
    sends a reader looking for a module they should never import.
    """
    assert macrame.NotFoundError.__module__ == "macrame"
    assert macrame.MacrameError.__module__ == "macrame"


def test_an_unknown_variant_name_is_a_value_error():
    """The test hook's own guard, so a typo in a parametrisation fails loudly
    rather than silently exercising nothing."""
    with pytest.raises(ValueError, match="unknown DbError variant"):
        _macrame._raise_db_error("NoSuchVariant")
