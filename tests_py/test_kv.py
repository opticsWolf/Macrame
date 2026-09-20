"""`kv_store` through the binding (0.18.0, P3, D-280).

Parity with `tests/kv_tests.rs` rather than a re-derivation of it: the Rust
suite owns the schema-shape assertions, and this file owns the half only Python
can check — that the four methods are reachable, that the `Option` comes back
as `None` rather than as an empty string, and that a bad key raises the typed
exception rather than returning nothing.
"""

import pytest

import macrame


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        yield handle


def test_a_key_round_trips_and_overwrites(db):
    assert db.kv_get("okf:epoch") is None

    db.kv_put("okf:epoch", "7")
    assert db.kv_get("okf:epoch") == "7"

    # Plain overwrite, no history. A versioned KV would re-create the ledger
    # through the back door for state whose previous value nobody wants.
    db.kv_put("okf:epoch", "8")
    assert db.kv_get("okf:epoch") == "8"
    assert len(db.kv_scan("okf:", 10)) == 1


def test_an_absent_key_and_an_empty_value_are_different_answers(db):
    """The distinction Python is most likely to lose, so it is pinned here.

    `None` is *no such key*; `""` is a key whose value is empty. The column is
    `NOT NULL`, so there is no third state below them — and `if db.kv_get(k):`
    cannot tell them apart, which is why the docstring says so too.
    """
    db.kv_put("okf:cursor", "")
    assert db.kv_get("okf:cursor") == ""
    assert db.kv_get("okf:missing") is None


def test_a_delete_reports_whether_it_removed_anything(db):
    assert db.kv_delete("okf:epoch") is False

    db.kv_put("okf:epoch", "7")
    assert db.kv_delete("okf:epoch") is True
    assert db.kv_get("okf:epoch") is None

    # A physical delete, and not a Doctrine V violation: the table is not in
    # the ledger, so no past state could be asked to explain the absence.
    assert db.kv_delete("okf:epoch") is False


def test_a_scan_is_bounded_ordered_and_stops_at_the_prefix(db):
    for key in [
        "okf:filehash:a.md",
        "okf:filehash:b.md",
        "okf:filehash:c.md",
        # Above the prefix's range under BINARY collation: the scan's upper
        # bound is `okf;` — `:` plus one — and `_` (0x5F) sorts above it.
        "okf_sibling",
        "other:key",
    ]:
        db.kv_put(key, "v")

    assert [k for k, _ in db.kv_scan("okf:filehash:", 10)] == [
        "okf:filehash:a.md",
        "okf:filehash:b.md",
        "okf:filehash:c.md",
    ]

    two = db.kv_scan("okf:filehash:", 2)
    assert len(two) == 2
    assert two[0] == ("okf:filehash:a.md", "v")

    # An empty prefix means everything, still bounded.
    assert len(db.kv_scan("", 100)) == 5
    assert len(db.kv_scan("", 3)) == 3


@pytest.mark.parametrize("bad", ["", "has space", "has|pipe", "café", "k" * 257])
def test_a_bad_key_is_refused_at_the_boundary(db, bad):
    with pytest.raises(macrame.InvalidKvKeyError) as excinfo:
        db.kv_put(bad, "v")
    assert excinfo.value.key == bad


def test_every_method_validates_and_an_empty_prefix_is_legal(db):
    for call in (
        lambda: db.kv_put("has space", "v"),
        lambda: db.kv_get("has space"),
        lambda: db.kv_delete("has space"),
        lambda: db.kv_scan("has space", 10),
    ):
        with pytest.raises(macrame.InvalidKvKeyError):
            call()

    # An empty *key* is not addressable; an empty *prefix* is.
    with pytest.raises(macrame.InvalidKvKeyError):
        db.kv_put("", "v")
    assert db.kv_scan("", 10) == []


def test_the_store_is_branch_global(db):
    """The semantic D-280 states, rather than an omission to be tidied later.

    An application that wants per-lineage operational state puts the branch
    name in the key.
    """
    db.fork("feature", "main")

    db.kv_put("okf:epoch", "7")
    assert db.kv_get("okf:epoch") == "7"

    # Nothing copies tables on a fork — a branch is a row plus a label carried
    # on writes — so there is exactly one row and every lineage reads it.
    assert len(db.kv_scan("okf:", 10)) == 1
    assert {b.id for b in db.branches()} == {"main", "feature"}
