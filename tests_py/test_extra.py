"""``concepts.extra`` through the binding (0.18.0, P1, D-278).

Parity with the Rust suite rather than a re-derivation of it. The crate owns the
schema shape, the payload version and the query plan; what only Python can check
is here:

- the constructor validates, so a bad value is reported at the line that wrote
  it and not at a bulk write ten thousand rows later (D-100).
- ``extra`` comes back as a ``str`` and never as ``None`` — the column is
  ``NOT NULL DEFAULT '{}'``, so a concept written before 0.18.0 and one written
  without attributes are the same answer.
- **unstated is not empty**, which is the property most easily lost at a
  boundary where an omitted keyword arrives as ``None``: a re-upsert that says
  nothing about attributes preserves them, and clearing is ``extra="{}"`` said
  out loud (D-283).
"""

from __future__ import annotations

import json

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"
T1 = "2026-06-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        yield handle


def attrs(db, concept_id):
    """The hydrated attributes of one concept, as a traversal returns them."""
    got = db.traverse(concept_id, max_depth=0, attribute_mode=macrame.AttributeMode.CURRENT)
    assert [n.id for n in got] == [concept_id]
    return got[0]


# ------------------------------------------------------------- round trip ---


def test_a_concept_carries_its_attributes_through_the_write_and_the_read(db):
    db.upsert_concept(
        macrame.ConceptUpsert(
            "c1", "C1", valid_from=T0, extra=json.dumps({"layer": "note", "n": 3})
        )
    )
    assert json.loads(attrs(db, "c1").extra) == {"layer": "note", "n": 3}


def test_a_concept_written_without_attributes_reads_as_an_empty_object(db):
    """Not ``None``, and the difference matters to every caller.

    ``json.loads`` on the value always works, so no reader needs a branch for
    the concept that has no attributes — including every concept written by a
    build before 0.18.0, which the v20 -> v21 rung gave the same default.
    """
    db.upsert_concept(macrame.ConceptUpsert("c1", "C1", valid_from=T0))
    got = attrs(db, "c1")
    assert got.extra == "{}"
    assert json.loads(got.extra) == {}


def test_the_upsert_reports_what_it_states_and_None_when_it_states_nothing():
    """``ConceptUpsert.extra`` is the *statement*, not the stored value.

    ``None`` here is ``unstated``; it is not the empty object, and nothing
    normalises it into one before the write.
    """
    assert macrame.ConceptUpsert("c1", "C1", valid_from=T0).extra is None
    stated = macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"note"}')
    assert stated.extra == '{"layer":"note"}'


# ------------------------------------------------- unstated is not empty ----


def test_a_reupsert_that_says_nothing_preserves_the_attributes(db):
    """The whole point of ``COALESCE`` over ``excluded.extra`` (D-283).

    Code that knows nothing about attributes — a title fixer, an importer
    written against 0.17 — re-upserts rows all the time. If an omitted keyword
    wiped the column, that code would erase a caller's attributes *and the log
    would record the erasure as a deliberate belief change*, which is the worst
    shape this could fail in: irreversible by design and indistinguishable from
    an intention.
    """
    db.upsert_concept(
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"note"}')
    )
    db.upsert_concept(macrame.ConceptUpsert("c1", "C1 renamed", valid_from=T0))

    got = attrs(db, "c1")
    assert got.title == "C1 renamed"
    assert json.loads(got.extra) == {"layer": "note"}


def test_clearing_is_said_out_loud(db):
    db.upsert_concept(
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"note"}')
    )
    db.upsert_concept(macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra="{}"))
    assert attrs(db, "c1").extra == "{}"


def test_a_bulk_write_carries_attributes_the_same_way(db):
    """The chunked path and the singular one share one statement (D-056).

    Stated as a test because they have drifted before: two spellings of the
    same write is how a fix lands in one path and not the other.
    """
    db.write_concepts(
        [
            macrame.ConceptUpsert(f"c{i}", f"C{i}", valid_from=T0, extra=f'{{"i":{i}}}')
            for i in range(5)
        ]
    )
    for i in range(5):
        assert json.loads(attrs(db, f"c{i}").extra) == {"i": i}

    db.write_concepts(
        [macrame.ConceptUpsert(f"c{i}", f"C{i} again", valid_from=T0) for i in range(5)]
    )
    for i in range(5):
        assert json.loads(attrs(db, f"c{i}").extra) == {"i": i}


# -------------------------------------------------------- the belief change --


def test_changing_attributes_is_a_belief_change_the_ledger_replays(db):
    """``extra`` is a logged column, so ``reconstruct`` answers about it.

    An attribute that were invisible to the log would be a field sitting plainly
    in ``concepts`` that no past state could explain — which is defect V, and
    the reason the payload carries every column the row does.
    """
    db.upsert_concept(
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"draft"}')
    )
    db.upsert_concept(
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"final"}')
    )

    assert json.loads(attrs(db, "c1").extra) == {"layer": "final"}


# --------------------------------------------------------------- refusals ---


@pytest.mark.parametrize(
    "value",
    [
        '["not", "an", "object"]',
        '"a bare string"',
        "7",
        "null",
        "not json at all",
        "",
    ],
)
def test_a_value_that_is_not_a_json_object_is_refused_in_the_constructor(value):
    """An *object* specifically, and the reason is the index.

    ``json_extract(extra, '$.layer')`` on an array or a bare scalar is a path
    that cannot match, so an index over one answers every query with silence.
    A caller who passes an array has made a mistake, and silence is the one
    report that would never tell them.
    """
    with pytest.raises(macrame.InvalidExtraError) as excinfo:
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra=value)
    assert excinfo.value.id == "c1"
    assert excinfo.value.reason


def test_a_value_over_the_cap_is_refused(db):
    big = json.dumps({"blob": "x" * (64 * 1024)})
    with pytest.raises(macrame.InvalidExtraError):
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra=big)

    # And the cap is a real boundary rather than a round number in prose: just
    # under it is accepted and written.
    room = 64 * 1024 - len(json.dumps({"blob": ""}))
    ok = json.dumps({"blob": "x" * room})
    assert len(ok.encode()) == 64 * 1024
    db.upsert_concept(macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra=ok))
    assert json.loads(attrs(db, "c1").extra)["blob"] == "x" * room


# ------------------------------------------------------ register_extra_index --


def test_registering_an_index_is_idempotent_and_takes_no_registry(db):
    """Assert it unconditionally at startup; that *is* the record.

    Create-if-absent with no registry table, which is what makes a restored
    backup safe: a file that came back without the index gets it on the next
    open, and nothing has to have remembered that it should have been there.
    """
    db.upsert_concept(
        macrame.ConceptUpsert("c1", "C1", valid_from=T0, extra='{"layer":"note"}')
    )
    db.register_extra_index("$.layer")
    db.register_extra_index("$.layer")
    db.register_extra_index("$.a.b")

    assert json.loads(attrs(db, "c1").extra) == {"layer": "note"}


@pytest.mark.parametrize(
    "path",
    [
        "$.a[0]",
        "layer",
        "$",
        "$.",
        "$.a-b",
        "$.a.b.",
        "$.a['b']",
        "$.a b",
        "",
    ],
)
def test_a_path_outside_the_grammar_is_refused(db, path):
    """Narrower than SQLite's grammar, and deliberately.

    The path is interpolated into DDL rather than bound — an index expression
    cannot take a parameter — so the grammar is the thing standing between a
    caller's string and the schema.
    """
    with pytest.raises(macrame.InvalidExtraPathError) as excinfo:
        db.register_extra_index(path)
    assert excinfo.value.path == path


# ---------------------------------------------------------------------------
# The subgraph path is opt-in (0.18.0, D-286)
# ---------------------------------------------------------------------------


def test_subgraph_does_not_carry_attributes_unless_asked(db):
    """`extra=False` is the default, and it is not a stand-in for empty."""
    db.upsert_concept(macrame.ConceptUpsert("a", "A", valid_from=T0, extra='{"layer":"note"}'))
    db.upsert_concept(macrame.ConceptUpsert("b", "B", valid_from=T0))
    db.assert_edge(macrame.EdgeAssertion("a", "b", "KNOWS", valid_from=T0, weight=1.0))

    plain = db.load_subgraph("a", 3, 1 << 20)
    assert plain.node("a").extra is None
    assert plain.node("b").extra is None


def test_subgraph_carries_attributes_when_asked(db):
    """And `"{}"` comes back as a value, not as `None`."""
    db.upsert_concept(macrame.ConceptUpsert("a", "A", valid_from=T0, extra='{"layer":"note"}'))
    db.upsert_concept(macrame.ConceptUpsert("b", "B", valid_from=T0))
    db.assert_edge(macrame.EdgeAssertion("a", "b", "KNOWS", valid_from=T0, weight=1.0))

    loaded = db.load_subgraph("a", 3, 1 << 20, extra=True)
    assert json.loads(loaded.node("a").extra) == {"layer": "note"}
    # `b` never stated attributes, so the column holds its default -- which is
    # a loaded empty object and *not* the absence of a load.
    assert loaded.node("b").extra == "{}"
    assert loaded.node("b").extra is not None


def test_asking_for_attributes_is_charged_to_the_byte_budget(db):
    """The budget refuses, rather than truncating, so this is observable."""
    big = '{"pad":"' + "a" * 4000 + '"}'
    for i in range(10):
        db.upsert_concept(macrame.ConceptUpsert(f"n{i}", f"N{i}", valid_from=T0, extra=big))
    for i in range(9):
        db.assert_edge(macrame.EdgeAssertion(f"n{i}", f"n{i + 1}", "KNOWS", valid_from=T0, weight=1.0))

    plain = db.load_subgraph("n0", 20, 1 << 24)
    snug = plain.estimated_bytes() * 2

    # Comfortable without attributes...
    db.load_subgraph("n0", 20, snug)
    # ...and refused with them, because they are charged rather than exempt.
    with pytest.raises(macrame.SubgraphTooLargeError):
        db.load_subgraph("n0", 20, snug, extra=True)
