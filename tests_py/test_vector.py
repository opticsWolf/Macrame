"""P4.4 acceptance: embeddings, search, and the filter planner.

The DiskANN index, the bm25 ranking and D-007's cost model are the crate's. What
this file asserts is the boundary: that a model name is refused where it is
*named* rather than deep in SQL, that both embedding forms reach the same stored
vector, and — the one most likely to bite — that ``score`` means opposite things
in the two search directions and each list arrives already sorted the right way.
"""

from __future__ import annotations

import struct
from datetime import timedelta

import pytest

import macrame

T0 = "2026-01-01T00:00:00.000000Z"
# A later validity and an instant after both, for the decay tests.
T_LATER = "2026-06-29T00:00:00.000000Z"
T_NOW = "2026-06-30T00:00:00.000000Z"
DIM = 4


def packed(*values):
    """The fast path: little-endian float32 bytes, as numpy would produce."""
    return struct.pack(f"<{len(values)}f", *values)


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        handle.write_concepts(
            [
                macrame.ConceptUpsert("a", "Alpha", valid_from=T0, content="alpha bravo"),
                macrame.ConceptUpsert("b", "Bravo", valid_from=T0, content="bravo charlie"),
                macrame.ConceptUpsert("c", "Charlie", valid_from=T0, content="charlie delta"),
            ]
        )
        handle.bulk_import(
            [
                macrame.EdgeAssertion("a", "b", "CITES", valid_from=T0),
                macrame.EdgeAssertion("b", "c", "CITES", valid_from=T0),
            ]
        )
        handle.register_model("mini", DIM)
        handle.upsert_embeddings(
            "mini",
            [
                ("a", [1.0, 0.0, 0.0, 0.0]),
                ("b", packed(0.0, 1.0, 0.0, 0.0)),
                ("c", [0.0, 0.0, 1.0, 0.0]),
            ],
        )
        handle.rebuild_fts()
        yield handle


# --------------------------------------------------------------------------
# Model registration
# --------------------------------------------------------------------------


def test_registering_the_same_model_twice_at_the_same_dim_is_fine(db):
    db.register_model("mini", DIM)


def test_registering_at_a_different_dim_raises(db):
    with pytest.raises(macrame.DimMismatchError):
        db.register_model("mini", DIM + 1)


def test_an_invalid_model_name_is_refused_where_it_is_named(db):
    # A model name reaches SQL as an *identifier*, so it cannot be bound as a
    # parameter. Refusing it here is what keeps a bad name from becoming a
    # syntax error from somewhere underneath.
    with pytest.raises(macrame.InvalidModelNameError):
        db.register_model("not a name!", DIM)


def test_searching_an_unregistered_model_raises(db):
    with pytest.raises(macrame.ModelNotRegisteredError):
        db.search_vector("nosuch", [1.0, 0.0, 0.0, 0.0])


def test_the_registry_can_be_read_back_and_not_only_written_to(db):
    """W6.2 closes the write-without-read asymmetry on this surface.

    Until 0.12.19 Python could register a model and not enumerate what was
    registered, so "is this already set up?" was answerable only by registering
    it again and reading the exception — which works, and is a write issued to
    ask a question.
    """
    assert db.registered_models() == ["mini"]

    db.register_model("other", DIM + 3)
    # Name order, and the names are the ones `register_model` takes back.
    assert db.registered_models() == ["mini", "other"]
    for name in db.registered_models():
        db.register_model(name, db.declared_dimension(name))


def test_the_declared_dimension_is_the_one_storage_enforces(db):
    """Read from the column type, so there is only one number (D-037).

    Asserted against the enforcement rather than against `DIM`: a cached
    registry would also return 4 here, and the point is that this *is* the
    schema's declaration, so a vector of that length is accepted and one of
    that length plus a byte is not.
    """
    dim = db.declared_dimension("mini")
    assert dim == DIM

    db.upsert_embeddings("mini", [("a", [0.5] * dim)])
    with pytest.raises(macrame.DimMismatchError):
        db.upsert_embeddings("mini", [("a", [0.5] * (dim + 1))])


def test_the_dimension_of_an_unregistered_model_raises(db):
    """A list answers membership; this answers a number, so absence is an error.

    Returning `None` would make the common use — allocate a buffer of this many
    floats — silently produce a zero-length one.
    """
    with pytest.raises(macrame.ModelNotRegisteredError):
        db.declared_dimension("nosuch")


def test_the_shadow_tables_libsql_creates_are_not_reported_as_models(db):
    """DiskANN's own tables live in `main` and match nothing a caller registered.

    The filter is in the crate; this asserts it survives the crossing, because
    a caller iterating `registered_models()` and calling `declared_dimension()`
    on each would otherwise hit a table with no `embedding` column.
    """
    for name in db.registered_models():
        assert not name.endswith("_shadow")
        db.declared_dimension(name)


# --------------------------------------------------------------------------
# Embeddings
# --------------------------------------------------------------------------


def test_a_wrong_length_vector_raises_and_names_both_dimensions(db):
    with pytest.raises(macrame.DimMismatchError) as e:
        db.upsert_embeddings("mini", [("a", [1.0, 0.0])])
    assert e.value.expected == DIM


def test_the_packed_and_sequence_forms_agree(db):
    """b was stored packed; storing the same vector as a list must not move it."""
    before = db.search_vector("mini", [0.0, 1.0, 0.0, 0.0], top_k=1)[0]
    db.upsert_embeddings("mini", [("b", [0.0, 1.0, 0.0, 0.0])])
    after = db.search_vector("mini", [0.0, 1.0, 0.0, 0.0], top_k=1)[0]
    assert before.concept_id == after.concept_id == "b"
    assert before.score == pytest.approx(after.score)


def test_upsert_embeddings_returns_a_count(db):
    assert db.upsert_embeddings("mini", [("a", [1.0, 0.0, 0.0, 0.0])]) == 1


def test_rows_of_the_wrong_shape_are_a_type_error(db):
    with pytest.raises(TypeError, match="concept_id"):
        db.upsert_embeddings("mini", ["a"])


def test_a_string_embedding_is_refused_rather_than_iterated(db):
    # A str is a sequence of length-1 strings and would otherwise fail much
    # deeper in, with a message about float conversion.
    with pytest.raises(TypeError, match="str"):
        db.upsert_embeddings("mini", [("a", "1234")])


# --------------------------------------------------------------------------
# search_vector — smaller is closer
# --------------------------------------------------------------------------


def test_vector_search_returns_the_nearest_first(db):
    hits = db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], top_k=3)
    assert hits[0].concept_id == "a"
    assert hits[0].score == pytest.approx(0.0, abs=1e-6)


def test_vector_search_scores_ascend(db):
    scores = [h.score for h in db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], top_k=3)]
    assert scores == sorted(scores)


def test_top_k_bounds_the_result(db):
    assert len(db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], top_k=2)) == 2


def test_a_vector_hit_from_a_plain_search_has_no_arm_ranks(db):
    hit = db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], top_k=1)[0]
    assert hit.vector_rank is None
    assert hit.keyword_rank is None


# --------------------------------------------------------------------------
# keyword_search — bm25, negative, ascending
# --------------------------------------------------------------------------


def test_keyword_search_finds_the_text(db):
    ids = [cid for cid, _ in db.keyword_search("charlie")]
    assert set(ids) == {"b", "c"}


def test_keyword_ranks_are_negative_and_ascending(db):
    ranks = [rank for _, rank in db.keyword_search("charlie")]
    assert all(r <= 0 for r in ranks)
    assert ranks == sorted(ranks)


def test_punctuation_is_escaped_rather_than_becoming_a_syntax_error(db):
    # Untrusted text with FTS5 operators in it is a query, not an injection and
    # not a crash.
    assert db.keyword_search('alpha "OR" (bravo') == db.keyword_search('alpha "OR" (bravo')


def test_a_retired_concept_is_not_a_search_result(db):
    db.upsert_concept(
        macrame.ConceptUpsert("c", "Charlie", valid_from=T0, content="charlie delta", retired=True)
    )
    assert "c" not in [cid for cid, _ in db.keyword_search("delta")]


# Ends `c`'s validity, and two instants either side of it.
T_END = "2026-01-05T00:00:00.000000Z"
T_WITHIN = "2026-01-03T00:00:00.000000Z"
T_AFTER = "2026-01-06T00:00:00.000000Z"


def test_an_instant_reaches_every_search_surface(db):
    """0.13.19 (W9.4, D-192): F-32 was that none of them could take one.

    `c` is the nearest vector to the query, holds the only occurrence of
    "delta", and is the far end of the traversal — so it is in every surface's
    answer, and closing its valid interval has to be visible in all four.

    The `T_WITHIN` arm is the one that discriminates. A bound written as "the
    interval is still open" passes the absent case and the `T_AFTER` case and
    fails only here.
    """
    q = [0.0, 0.0, 1.0, 0.0]
    db.upsert_concept(
        macrame.ConceptUpsert(
            "c", "Charlie", valid_from=T0, valid_to=T_END, content="charlie delta"
        )
    )

    assert "c" in [cid for cid, _ in db.keyword_search("delta")]
    assert "c" not in [cid for cid, _ in db.keyword_search("delta", as_of_valid=T_AFTER)]
    assert "c" in [cid for cid, _ in db.keyword_search("delta", as_of_valid=T_WITHIN)]

    assert db.search_vector("mini", q, top_k=1)[0].concept_id == "c"
    assert "c" not in [h.concept_id for h in db.search_vector("mini", q, as_of_valid=T_AFTER)]
    assert "c" in [h.concept_id for h in db.search_vector("mini", q, as_of_valid=T_WITHIN)]

    assert "c" in [h.concept_id for h in db.hybrid_search("mini", "delta", q)]
    assert "c" not in [
        h.concept_id for h in db.hybrid_search("mini", "delta", q, as_of_valid=T_AFTER)
    ]

    # `search_filtered` has no instant of its own: `as_of_valid` reaches the
    # walk and the ranking together, and `now` alone leaves both in the present.
    hits, _ = db.search_filtered("mini", q, "a", max_depth=3, now=T_AFTER)
    assert "c" in [h.concept_id for h in hits]
    hits, _ = db.search_filtered("mini", q, "a", max_depth=3, now=T_AFTER, as_of_valid=T_AFTER)
    assert "c" not in [h.concept_id for h in hits]


def test_a_half_life_prices_age_into_the_ranking(db):
    """0.13.20 (W9.5, D-193): one parameter, off by default, at ranking only.

    `c` is the nearer vector and the older concept; `b` is farther and
    newer. Undecayed the near one wins, and with a half-life short against the
    gap in their ages the fresh one does — the reversal, not merely a change,
    because a decay applied with the wrong sign also changes the order.
    """
    # Between `b` and `c`, and closer to `c`: the fixture's vectors are
    # orthogonal, so a query along one of them leaves the other two tied and
    # the second place decided by an id tie-break rather than by distance.
    q = [0.0, 0.6, 0.8, 0.0]
    # `b` becomes true a day before the instant; `a` and `c` stay at T0, half a
    # year older. Nothing else about any of them changes.
    db.upsert_concept(
        macrame.ConceptUpsert("b", "Bravo", valid_from=T_LATER, content="bravo charlie")
    )

    def ids(**kw):
        return [h.concept_id for h in db.search_vector("mini", q, top_k=2, **kw)]

    assert ids(as_of_valid=T_NOW) == ["c", "b"]
    assert ids(as_of_valid=T_NOW, half_life=timedelta(days=30)) == ["b", "c"]
    # Seconds are the same span said differently.
    assert ids(as_of_valid=T_NOW, half_life=30 * 86400.0) == ["b", "c"]
    # Off by default: the parameter is what changes the answer.
    assert ids(as_of_valid=T_NOW, half_life=None) == ["c", "b"]


def test_a_half_life_without_an_instant_raises(db):
    with pytest.raises(macrame.HalfLifeWithoutInstantError, match="as_of_valid"):
        db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], half_life=timedelta(days=1))
    with pytest.raises(macrame.HalfLifeWithoutInstantError):
        db.keyword_search("charlie", half_life=1.0)
    with pytest.raises(macrame.HalfLifeWithoutInstantError):
        db.hybrid_search("mini", "charlie", [1.0, 0.0, 0.0, 0.0], half_life=1.0)


def test_a_half_life_that_cannot_be_a_span_is_refused(db):
    with pytest.raises(TypeError, match="timedelta"):
        db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], as_of_valid=T_NOW, half_life="a week")
    with pytest.raises(ValueError):
        db.search_vector("mini", [1.0, 0.0, 0.0, 0.0], as_of_valid=T_NOW, half_life=-1.0)


# --------------------------------------------------------------------------
# hybrid_search — larger is better, the other way round
# --------------------------------------------------------------------------


def test_hybrid_scores_descend(db):
    scores = [h.score for h in db.hybrid_search("mini", "bravo", [1.0, 0.0, 0.0, 0.0], top_k=3)]
    assert scores == sorted(scores, reverse=True)


def test_hybrid_hits_carry_the_rank_from_each_arm(db):
    hits = db.hybrid_search("mini", "bravo", [1.0, 0.0, 0.0, 0.0], top_k=3)
    # At least one hit was found by both arms, and the ranks are 1-based.
    assert any(h.vector_rank is not None and h.keyword_rank is not None for h in hits)
    assert all(r is None or r >= 1 for h in hits for r in (h.vector_rank, h.keyword_rank))


def test_an_unregistered_model_raises_rather_than_degrading_to_keyword_only(db):
    with pytest.raises(macrame.ModelNotRegisteredError):
        db.hybrid_search("nosuch", "bravo", [1.0, 0.0, 0.0, 0.0])


def test_rrf_k_is_exposed_so_tuning_it_starts_from_something(db):
    assert isinstance(macrame.RRF_K, int)
    assert db.hybrid_search("mini", "bravo", [1.0, 0.0, 0.0, 0.0], rrf_k=macrame.RRF_K) is not None


# --------------------------------------------------------------------------
# search_filtered and the plan
# --------------------------------------------------------------------------


def test_a_filtered_search_returns_hits_and_the_plan(db):
    hits, plan = db.search_filtered("mini", [1.0, 0.0, 0.0, 0.0], "a", max_depth=3, top_k=2)
    assert [h.concept_id for h in hits][:1] == ["a"]
    assert isinstance(plan, macrame.CostEstimate)
    assert plan.strategy in (
        macrame.FilterStrategy.POST_FILTER,
        macrame.FilterStrategy.PRE_FILTER_CTE,
    )


def test_the_plan_carries_the_arithmetic_that_produced_it(db):
    _, plan = db.search_filtered("mini", [1.0, 0.0, 0.0, 0.0], "a", max_depth=3)
    assert plan.candidates == 3
    assert plan.candidates_capped is False
    assert plan.post_filter_bytes > 0
    assert plan.pre_filter_bytes > 0


def test_a_capped_probe_says_so(db):
    _, plan = db.search_filtered("mini", [1.0, 0.0, 0.0, 0.0], "a", max_depth=3, probe_cap=1)
    assert plan.candidates_capped is True
    assert plan.candidates == 1
    assert repr(plan).endswith("candidates=1+>")


def test_a_forced_strategy_is_honoured(db):
    for wanted in (macrame.FilterStrategy.POST_FILTER, macrame.FilterStrategy.PRE_FILTER_CTE):
        _, plan = db.search_filtered(
            "mini", [1.0, 0.0, 0.0, 0.0], "a", max_depth=3, strategy=wanted
        )
        assert plan.strategy == wanted


def test_the_filter_restricts_the_result_to_the_neighbourhood(db):
    # Depth 0 reaches only the start node, so nothing else can be returned
    # however close it is in vector space.
    hits, _ = db.search_filtered("mini", [0.0, 0.0, 1.0, 0.0], "a", max_depth=0, top_k=3)
    assert [h.concept_id for h in hits] == ["a"]


# --------------------------------------------------------------------------
# Lifecycle
# --------------------------------------------------------------------------


def test_vector_calls_on_a_closed_handle_raise(db_path):
    handle = macrame.Database.open(db_path, snapshot_every_entries=None)
    handle.close()
    for call in (
        lambda: handle.register_model("mini", DIM),
        lambda: handle.upsert_embeddings("mini", [("a", [1.0, 0.0, 0.0, 0.0])]),
        lambda: handle.search_vector("mini", [1.0, 0.0, 0.0, 0.0]),
        lambda: handle.keyword_search("alpha"),
        lambda: handle.hybrid_search("mini", "alpha", [1.0, 0.0, 0.0, 0.0]),
        lambda: handle.rebuild_fts(),
    ):
        with pytest.raises(macrame.MacrameClosedError):
            call()
