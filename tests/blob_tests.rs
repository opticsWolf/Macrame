//! The blob store through the public surface (0.19.0, P4, [D-281], [D-287],
//! [D-288]) — acceptance gates 1 to 11.
//!
//! The rung test in `migration_tests` asserts the *shape*: the table, its
//! address `CHECK`, both guards and the candidate index (gate 13), and the
//! `verify` half of gate 10. This file asserts the behaviour, and above all the
//! one property the design exists for: **a blob is reclaimed when no hot log
//! entry names it, and not when the current state stops naming it** (gate 3).
//! A reference count would pass every other gate here and fail that one.
//!
//! Every test runs on a `FakeClock`. `put_at` is transaction time and is
//! stamped by the crate, so the age guard (gate 7) can only be placed on either
//! side of a cutoff by moving the clock, and a real clock would make the fork
//! points of gates 4 and 5 a function of timing.
//!
//! [D-281]: ../docs/architecture/s13-decision-register.md#d-281
//! [D-287]: ../docs/architecture/s13-decision-register.md#d-287
//! [D-288]: ../docs/architecture/s13-decision-register.md#d-288

#[path = "common/harness.rs"]
mod harness;

use std::sync::Arc;
use std::time::Duration;

use harness::TestHarness;
use macrame::error::{abort_kind, AbortKind};
use macrame::graph::EdgeAssertion;
use macrame::prelude::*;
use macrame::schema::ddl::ARCHIVE_SESSION_MARKER;
use macrame::DbError;

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
/// Every write in these tests lands in the first few hours of the fake clock,
/// so a cutoff a day in makes all of it old enough to archive.
const CUTOFF: &str = "1970-01-02T00:00:00.000000Z";
const STEP: Duration = Duration::from_secs(3_600);

/// The NIST FIPS 180-2 vectors, the empty input included.
const NIST: [(&[u8], &str); 3] = [
    (
        b"",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ),
    (
        b"abc",
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    ),
    (
        b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    ),
];

fn branch(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

async fn open(h: &TestHarness) -> Database {
    h.db_with_fake_clock().await
}

async fn open_tuned(h: &TestHarness, tuning: Tuning) -> Database {
    Database::open_tuned(&h.db_path, tuning.clock(Arc::clone(&h.clock) as _))
        .await
        .unwrap()
}

async fn location(db: &Database, digest: &str) -> Option<BlobLocation> {
    db.blob_stat(digest).await.unwrap().map(|s| s.location)
}

async fn hot_rows(db: &Database, digest: &str) -> i64 {
    db.read_conn()
        .query(
            "SELECT COUNT(*) FROM blobs WHERE sha256 = ?1",
            libsql::params![digest],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// One count from the cold file, opened in its own right — see
/// `branch_archive_tests::cold_scalar` for why not through the ATTACH.
async fn cold_rows(db: &Database, digest: &str) -> i64 {
    let conn = libsql::Builder::new_local(db.archive_path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.query(
        "SELECT COUNT(*) FROM blobs WHERE sha256 = ?1",
        libsql::params![digest],
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .unwrap()
    .get(0)
    .unwrap()
}

async fn concept(db: &Database, id: &str, content: &str, extra: &str) {
    db.upsert_concept(
        ConceptUpsert::new(id, "Title")
            .content(content)
            .extra(extra)
            .valid_from(EPOCH),
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// gates 1 and 2: the put, the get, the cap
// ---------------------------------------------------------------------------

/// **Gate 1.** The address is the published digest, the bytes come back equal,
/// and a re-put is a refresh of `put_at` rather than a second row.
#[tokio::test]
async fn a_blob_round_trips_under_its_nist_digest() {
    let h = TestHarness::new();
    let db = open(&h).await;

    for (bytes, digest) in NIST {
        assert_eq!(db.blob_put(bytes).await.unwrap(), digest);
        assert_eq!(db.blob_get(digest).await.unwrap().as_deref(), Some(bytes));
        let stat = db.blob_stat(digest).await.unwrap().unwrap();
        assert_eq!(stat.sha256, digest);
        assert_eq!(stat.size, bytes.len() as u64);
        assert_eq!(stat.location, BlobLocation::Hot);
    }

    let (bytes, digest) = NIST[1];
    let first = db.blob_stat(digest).await.unwrap().unwrap().put_at;
    h.advance(STEP);
    assert_eq!(db.blob_put(bytes).await.unwrap(), digest);
    let second = db.blob_stat(digest).await.unwrap().unwrap().put_at;
    assert!(
        second > first,
        "a re-put must advance put_at ({first} -> {second}): the age guard reads it"
    );
    assert_eq!(hot_rows(&db, digest).await, 1, "a re-put added a row");
}

/// An address that is not 64 lowercase hex characters is a caller error, not
/// an absent blob — the two would otherwise both be `None`.
#[tokio::test]
async fn a_malformed_digest_is_refused_rather_than_absent() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let good = NIST[0].1;
    for bad in [good.to_uppercase(), good[..63].to_string(), format!("{good}0")] {
        assert!(matches!(
            db.blob_get(&bad).await,
            Err(DbError::InvalidDigest(_))
        ));
        assert!(matches!(
            db.blob_stat(&bad).await,
            Err(DbError::InvalidDigest(_))
        ));
    }
    let absent = "0".repeat(64);
    assert_eq!(db.blob_get(&absent).await.unwrap(), None);
    assert_eq!(db.blob_stat(&absent).await.unwrap(), None);
}

/// **Gate 2.** One byte over the cap is refused naming both numbers, a raised
/// cap admits it, and the empty blob is legal.
#[tokio::test]
async fn the_cap_refuses_one_byte_over_and_a_raised_tuning_admits_it() {
    let h = TestHarness::new();
    let db = open(&h).await;

    let over = vec![7u8; DEFAULT_MAX_BLOB_BYTES + 1];
    match db.blob_put(&over).await {
        Err(DbError::BlobTooLarge { size, max, .. }) => {
            assert_eq!(size, DEFAULT_MAX_BLOB_BYTES + 1);
            assert_eq!(max, DEFAULT_MAX_BLOB_BYTES);
        }
        other => panic!("expected BlobTooLarge, got {other:?}"),
    }
    assert_eq!(db.blob_put(&[]).await.unwrap(), NIST[0].1);
    db.close().await.unwrap();

    let h = TestHarness::new();
    let db = open_tuned(
        &h,
        Tuning::default().max_blob_bytes(DEFAULT_MAX_BLOB_BYTES + 1),
    )
    .await;
    let digest = db.blob_put(&over).await.unwrap();
    assert_eq!(
        db.blob_get(&digest).await.unwrap().map(|b| b.len()),
        Some(DEFAULT_MAX_BLOB_BYTES + 1)
    );

    // And a lowered one bites below the default.
    let h = TestHarness::new();
    let db = open_tuned(&h, Tuning::default().max_blob_bytes(16)).await;
    assert!(db.blob_put(&[0u8; 16]).await.is_ok());
    assert!(matches!(
        db.blob_put(&[0u8; 17]).await,
        Err(DbError::BlobTooLarge {
            size: 17,
            max: 16,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// gates 3 to 8: what the archive keeps, sends, and brings back
// ---------------------------------------------------------------------------

/// **Gate 3 — the defect D-281 exists to prevent.** Version 2 of a concept no
/// longer names `X`, but version 1 did and is still history: `reconstruct` at
/// version 1's instant names `X`, so `X` must still be readable. It moves cold
/// with the entry that names it, and is never deleted.
#[tokio::test]
async fn a_superseded_reference_sends_its_blob_cold_and_history_can_still_read_it() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"version one's attachment").await.unwrap();
    let y = db.blob_put(b"version two's attachment").await.unwrap();

    concept(&db, "doc", "", &format!(r#"{{"blob":"{x}"}}"#)).await;
    h.advance(STEP);
    let at_v1 = "1970-01-01T00:30:00.000000Z";
    concept(&db, "doc", "", &format!(r#"{{"blob":"{y}"}}"#)).await;

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_archived, 1);
    assert_eq!(report.blobs_restored, 0);
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Cold));
    assert_eq!(location(&db, &y).await, Some(BlobLocation::Hot));
    assert_eq!(hot_rows(&db, &x).await, 0);

    let state = db.reconstruct(at_v1).await.unwrap();
    assert!(
        state.concepts["doc"].extra.contains(&x),
        "reconstruct at version 1 must still name X: {}",
        state.concepts["doc"].extra
    );
    assert_eq!(
        db.blob_get(&x).await.unwrap().as_deref(),
        Some(&b"version one's attachment"[..]),
        "history names X and X is gone — the refcount failure"
    );
}

/// **Gate 4.** A fork between the two versions pins version 1's entry hot
/// ([D-269]), and the entry pins `X`. Forgetting the fork releases both.
///
/// [D-269]: ../docs/architecture/s13-decision-register.md#d-269
#[tokio::test]
async fn a_fork_between_versions_keeps_the_blob_hot_until_it_is_archived() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"pinned").await.unwrap();

    concept(&db, "doc", "", &format!(r#"{{"blob":"{x}"}}"#)).await;
    h.advance(STEP);
    db.fork(branch("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    concept(&db, "doc", "", r#"{"blob":null}"#).await;

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_archived, 0);
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Hot));

    db.archive_branch(branch("alt")).await.unwrap();
    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_archived, 1);
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Cold));
}

/// **Gate 5.** A reference that exists only on a live branch counts: the scan
/// reads every lineage's log, not the trunk's view.
#[tokio::test]
async fn a_reference_on_a_branch_alone_holds_the_blob() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"branch only").await.unwrap();

    h.advance(STEP);
    db.fork(branch("alt"), BranchId::main()).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new("on-alt", "Title")
            .content(format!("see {x}"))
            .valid_from(EPOCH)
            .on_branch(branch("alt")),
    )
    .await
    .unwrap();

    db.archive(CUTOFF).await.unwrap();
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Hot));

    db.archive_branch(branch("alt")).await.unwrap();
    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_archived, 1);
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Cold));
}

/// **Gate 6.** A reference is the digest's text anywhere in a hot payload:
/// link `properties`, concept `content`, inside a URL, inside a longer hex run.
/// An uppercase digest is not one — the documented negative ([D-281]
/// amendment 2): the address is lowercase, and the scan matches it exactly.
#[tokio::test]
async fn references_are_found_wherever_the_text_is_and_uppercase_is_not_one() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let in_props = db.blob_put(b"in link properties").await.unwrap();
    let in_content = db.blob_put(b"in concept content").await.unwrap();
    let in_url = db.blob_put(b"inside a url").await.unwrap();
    let in_hex = db.blob_put(b"inside a longer hex run").await.unwrap();
    let upper = db.blob_put(b"named only in uppercase").await.unwrap();

    concept(&db, "p", &format!("body {in_content} body"), "{}").await;
    concept(
        &db,
        "q",
        &format!("deadbeef{in_hex}cafe0123"),
        &format!(r#"{{"src":"https://example.org/blobs/{in_url}?v=1"}}"#),
    )
    .await;
    concept(&db, "r", &upper.to_uppercase(), "{}").await;
    db.assert_edge(
        EdgeAssertion::new("p", "q", "CITES")
            .valid_from(EPOCH)
            .properties(format!(r#"{{"attachment":"{in_props}"}}"#)),
    )
    .await
    .unwrap();

    let report = db.archive(CUTOFF).await.unwrap();
    for (what, d) in [
        ("link properties", &in_props),
        ("concept content", &in_content),
        ("a URL", &in_url),
        ("a longer hex run", &in_hex),
    ] {
        assert_eq!(
            location(&db, d).await,
            Some(BlobLocation::Hot),
            "a digest inside {what} did not hold its blob"
        );
    }
    assert_eq!(location(&db, &upper).await, Some(BlobLocation::Cold));
    assert_eq!(report.blobs_archived, 1);
}

/// **Gate 7.** Unreferenced is not enough: a blob put after the cutoff may be
/// about to be named by the write that put it there, and a re-put counts as a
/// put ([D-281] amendment 1).
#[tokio::test]
async fn the_age_guard_keeps_young_and_re_put_blobs_hot() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let old = db.blob_put(b"old").await.unwrap();
    let reput = db.blob_put(b"re-put").await.unwrap();

    h.advance(STEP * 30); // past CUTOFF
    let young = db.blob_put(b"young").await.unwrap();
    db.blob_put(b"re-put").await.unwrap();

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_archived, 1);
    assert_eq!(location(&db, &old).await, Some(BlobLocation::Cold));
    assert_eq!(location(&db, &young).await, Some(BlobLocation::Hot));
    assert_eq!(location(&db, &reput).await, Some(BlobLocation::Hot));
}

/// **Gate 8.** A hot entry that names a cold-only blob brings it back, and the
/// cold copy stays: the archive is append-only, and the next cold-side read
/// must not find a hole.
#[tokio::test]
async fn a_new_reference_to_a_cold_blob_copies_it_back() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"comes back").await.unwrap();

    db.archive(CUTOFF).await.unwrap();
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Cold));

    concept(&db, "doc", &format!("again: {x}"), "{}").await;
    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blobs_restored, 1);
    assert_eq!(report.blobs_archived, 0);
    assert_eq!(location(&db, &x).await, Some(BlobLocation::Hot));
    assert_eq!(hot_rows(&db, &x).await, 1);
    assert_eq!(cold_rows(&db, &x).await, 1, "the cold copy was removed");

    // Restored and still referenced: the next session leaves it alone.
    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!((report.blobs_archived, report.blobs_restored), (0, 0));
}

// ---------------------------------------------------------------------------
// gates 9 to 11: an old cold file, the guards in a session, the cost of nothing
// ---------------------------------------------------------------------------

/// **Gate 9.** A cold file written by 0.18 has no `blobs` table. A blob read
/// must answer `None` from it without changing it — the cold reader is opened
/// read-only, so it could not — and the next archive creates the table.
#[tokio::test]
async fn a_cold_file_without_a_blob_table_is_read_and_left_alone() {
    let h = TestHarness::new();
    let db = open(&h).await;
    db.archive(CUTOFF).await.unwrap(); // creates the cold file
    let cold_path = db.archive_path().to_path_buf();
    db.close().await.unwrap();

    {
        let conn = libsql::Builder::new_local(&cold_path)
            .build()
            .await
            .unwrap()
            .connect()
            .unwrap();
        conn.execute("DROP TABLE blobs", ()).await.unwrap();
        // Fold the WAL back so the main file holds the whole state, and a
        // byte comparison of it means something.
        conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await
            .unwrap();
    }
    let before = std::fs::read(&cold_path).unwrap();

    let db = open(&h).await;
    assert_eq!(db.blob_get(&"a".repeat(64)).await.unwrap(), None);
    assert_eq!(db.blob_stat(&"a".repeat(64)).await.unwrap(), None);
    assert_eq!(
        std::fs::read(&cold_path).unwrap(),
        before,
        "a blob read wrote to the cold file"
    );

    db.archive(CUTOFF).await.unwrap();
    assert_eq!(cold_rows(&db, &"a".repeat(64)).await, 0); // the table exists
}

/// **A missing cold file reads as absent, on purpose ([D-288]).** `reconstruct`
/// over the same loss is loud (R14), and the contrast is asserted here so that
/// neither half drifts unnoticed: the hot file keeps no record of which blobs
/// moved cold, so a blob read has nothing to tell *lost* from *never stored*.
///
/// [D-288]: ../docs/architecture/s13-decision-register.md#d-288
#[tokio::test]
async fn a_missing_cold_file_reads_as_absent_while_reconstruct_refuses() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"version one's attachment").await.unwrap();
    let y = db.blob_put(b"version two's attachment").await.unwrap();
    concept(&db, "doc", "", &format!(r#"{{"blob":"{x}"}}"#)).await;
    h.advance(STEP);
    let at_v1 = "1970-01-01T00:30:00.000000Z";
    concept(&db, "doc", "", &format!(r#"{{"blob":"{y}"}}"#)).await;

    assert_eq!(db.archive(CUTOFF).await.unwrap().blobs_archived, 1);
    let cold_path = db.archive_path().to_path_buf();
    db.close().await.unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let mut p = cold_path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    assert!(!cold_path.exists());

    let db = open(&h).await;
    assert_eq!(db.blob_get(&x).await.unwrap(), None);
    assert_eq!(db.blob_stat(&x).await.unwrap(), None);
    assert_eq!(location(&db, &y).await, Some(BlobLocation::Hot));
    assert!(
        !cold_path.exists(),
        "a blob read created the cold file it could not find"
    );
    match db.reconstruct(at_v1).await {
        Err(DbError::ReplayCorrupt { .. }) => {}
        other => panic!("R14: expected ReplayCorrupt, got {other:?}"),
    }
}

/// **Gate 10, the session half.** Inside a session the delete guard lets a
/// delete through and the update guard still refuses a rewrite of the bytes;
/// `put_at` moves either way. Outside a session, and `verify`, are asserted by
/// the v22 rung in `migration_tests`.
#[tokio::test]
async fn inside_a_session_a_blob_may_be_deleted_but_never_rewritten() {
    let h = TestHarness::new();
    let db = open(&h).await;
    let x = db.blob_put(b"guarded").await.unwrap();
    db.close().await.unwrap();

    let conn = libsql::Builder::new_local(&h.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    conn.execute(&format!("CREATE TABLE {ARCHIVE_SESSION_MARKER} (x)"), ())
        .await
        .unwrap();

    let err = conn
        .execute(
            "UPDATE blobs SET bytes = x'00', size = 1 WHERE sha256 = ?1",
            libsql::params![x.as_str()],
        )
        .await
        .expect_err("a session rewrote a blob's bytes under its address");
    assert_eq!(abort_kind(&err), AbortKind::BlobImmutable);

    conn.execute(
        "UPDATE blobs SET put_at = '1970-01-03T00:00:00.000000Z' WHERE sha256 = ?1",
        libsql::params![x.as_str()],
    )
    .await
    .expect("put_at must move inside a session too");
    let n = conn
        .execute(
            "DELETE FROM blobs WHERE sha256 = ?1",
            libsql::params![x.as_str()],
        )
        .await
        .expect("the marker must admit a delete");
    assert_eq!(n, 1);
    conn.execute("ROLLBACK", ()).await.unwrap();
}

/// **Gate 11.** A ledger with no blobs pays an indexed probe and no scan, and
/// the counter says so — timing could not. The positive control is what makes
/// the zero mean something: the same counter is non-zero once a blob is a
/// candidate.
#[tokio::test]
async fn a_ledger_without_blobs_scans_no_payload() {
    let h = TestHarness::new();
    let db = open(&h).await;
    for i in 0..20 {
        concept(&db, &format!("c{i}"), "a body", "{}").await;
        concept(&db, &format!("c{i}"), "a newer body", "{}").await;
    }

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.blob_scan_bytes, 0);
    assert_eq!((report.blobs_archived, report.blobs_restored), (0, 0));

    db.blob_put(b"a candidate").await.unwrap();
    let report = db.archive(CUTOFF).await.unwrap();
    assert!(
        report.blob_scan_bytes > 0,
        "the counter never moves, so its zero above proves nothing"
    );
    assert_eq!(report.blobs_archived, 1);
}
