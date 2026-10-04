"""The blob store through the binding (0.19.0, P4, D-281).

Parity with `tests/blob_tests.rs` rather than a re-derivation of it: the Rust
suite owns the archive's reference semantics, and this file owns the half only
Python can check — that the address equals `hashlib.sha256(data).hexdigest()`
(acceptance gate 1, and the interoperability D-281 amendment 3 rests on), that
the bytes come back as `bytes`, and that both refusals raise their typed
exception with the attributes a caller would read.
"""

import hashlib

import pytest

import macrame

# Far enough ahead that everything this file writes is older.
CUTOFF = "2099-01-01T00:00:00.000000Z"


@pytest.fixture
def db(db_path):
    with macrame.Database.open(db_path, snapshot_every_entries=None) as handle:
        yield handle


@pytest.mark.parametrize(
    "data", [b"", b"abc", bytes(range(256)) * 300], ids=["empty", "abc", "76800"]
)
def test_the_address_is_what_hashlib_prints(db, data):
    digest = db.blob_put(data)
    assert digest == hashlib.sha256(data).hexdigest()

    got = db.blob_get(digest)
    assert isinstance(got, bytes)
    assert got == data

    stat = db.blob_stat(digest)
    assert isinstance(stat, macrame.BlobStat)
    assert (stat.sha256, stat.size, stat.location) == (digest, len(data), "hot")
    assert stat.put_at.tzinfo is not None


def test_an_absent_blob_is_none_and_a_malformed_address_raises(db):
    assert db.blob_get("0" * 64) is None
    assert db.blob_stat("0" * 64) is None

    upper = hashlib.sha256(b"").hexdigest().upper()
    for call in (db.blob_get, db.blob_stat):
        with pytest.raises(macrame.InvalidDigestError):
            call(upper)
        with pytest.raises(macrame.ValidationError):
            call("abc")


def test_the_cap_raises_with_both_numbers_and_open_can_move_it(db_path):
    with macrame.Database.open(
        db_path, snapshot_every_entries=None, max_blob_bytes=16
    ) as db:
        db.blob_put(b"x" * 16)
        with pytest.raises(macrame.BlobTooLargeError) as caught:
            db.blob_put(b"x" * 17)
        assert (caught.value.size, caught.value.max) == (17, 16)
        assert isinstance(caught.value, macrame.BudgetError)

    assert macrame.DEFAULT_MAX_BLOB_BYTES == 8 * 1024 * 1024


def test_an_unreferenced_blob_goes_cold_and_is_still_readable(db):
    digest = db.blob_put(b"nobody names me")

    report = db.archive(CUTOFF)
    assert report.blobs_archived == 1
    assert report.blobs_restored == 0
    assert report.blob_scan_bytes >= 0

    assert db.blob_stat(digest).location == "cold"
    assert db.blob_get(digest) == b"nobody names me"


def test_an_archive_without_blobs_scans_nothing(db):
    report = db.archive(CUTOFF)
    assert (report.blobs_archived, report.blobs_restored) == (0, 0)
    assert report.blob_scan_bytes == 0
