mod harness;

use std::path::{Path, PathBuf};

use harness::TestHarness;
use macrame::error::DbError;
use macrame::schema::migrations;
use macrame::temporal::snapshot::{cleanup_expired_snapshots, save_snapshot};
use macrame::temporal::{reconstruct, MaterializedState};

/// Build a cold archive database holding the given log rows.
///
/// Shaped like the cold schema `archive()` creates: a plain `INTEGER PRIMARY
/// KEY` for `seq_id` (never AUTOINCREMENT — that would renumber history), no
/// triggers, no foreign keys.
async fn make_cold_archive(path: &Path, rows: &[(i64, &str, &str, &str, &str, &str)]) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE transaction_log (
             seq_id      INTEGER PRIMARY KEY,
             table_name  TEXT NOT NULL,
             entity_id   TEXT NOT NULL,
             operation   TEXT NOT NULL,
             payload     TEXT NOT NULL,
             recorded_at TEXT NOT NULL
         )",
        (),
    )
    .await
    .unwrap();

    for (seq, table, entity, op, payload, recorded_at) in rows {
        conn.execute(
            "INSERT INTO transaction_log
                 (seq_id, table_name, entity_id, operation, payload, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![*seq, *table, *entity, *op, *payload, *recorded_at],
        )
        .await
        .unwrap();
    }
}

async fn hot_db(harness: &TestHarness) -> libsql::Connection {
    let conn = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    migrations::run(&conn).await.unwrap();
    conn
}

/// The defect this guards: `DETACH` used to sit after the row loop, so every `?`
/// inside the loop returned early and skipped it. ATTACH is not transactional
/// and survives ROLLBACK, so one corrupt payload leaked the handle permanently —
/// poisoning every later `reconstruct` *and* every later `archive` on that
/// connection with "database cold is already in use". A single bad row taking
/// out the archive path is a far worse outcome than the bad row itself.
#[tokio::test]
async fn a_failed_cold_reconstruct_still_detaches() {
    let harness = TestHarness::new();
    let conn = hot_db(&harness).await;
    let archive_path = harness.temp_dir.path().join("cold.db");

    make_cold_archive(
        &archive_path,
        &[(
            1,
            "concepts",
            "c1",
            "I",
            "{ this is not json",
            "2026-01-01T00:00:00.000000Z",
        )],
    )
    .await;

    // A hot row newer than the query instant, so the hot log cannot cover it and
    // the cold path is taken.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) \
         VALUES ('c2', 'T', '2026-06-01T00:00:00.000000Z', '2026-06-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    let err = reconstruct(
        &conn,
        "2026-03-01T00:00:00.000000Z",
        Some(&archive_path),
        None,
    )
    .await
    .expect_err("a corrupt payload must surface as an error");
    assert!(
        matches!(err, DbError::ReplayCorrupt { .. }),
        "expected ReplayCorrupt, got {err:?}"
    );

    // The probe: if the handle leaked, this fails with "database cold is
    // already in use".
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive_path.to_string_lossy().as_ref()],
    )
    .await
    .expect("cold must be free to attach again after a failed reconstruct");
}

/// An empty hot log used to be read as "the hot log covers everything", so a
/// database whose entire log had been archived reconstructed to the empty state:
/// no error, no missing file, just a confident wrong answer.
#[tokio::test]
async fn an_empty_hot_log_consults_the_archive() {
    let harness = TestHarness::new();
    let conn = hot_db(&harness).await;
    let archive_path = harness.temp_dir.path().join("cold.db");

    make_cold_archive(
        &archive_path,
        &[(
            1,
            "concepts",
            "c1",
            "I",
            r#"{"v":1,"title":"Archived","content":"","retired":0}"#,
            "2026-01-01T00:00:00.000000Z",
        )],
    )
    .await;

    let state = reconstruct(
        &conn,
        "2026-03-01T00:00:00.000000Z",
        Some(&archive_path),
        None,
    )
    .await
    .unwrap();

    assert_eq!(state.concepts.len(), 1, "archived state must be recovered");
    assert_eq!(state.concepts["c1"].title, "Archived");
    assert_eq!(state.seq_anchor, 1);
}

/// With no archive file, an empty hot log really does mean an empty database.
#[tokio::test]
async fn an_empty_database_reconstructs_to_nothing() {
    let harness = TestHarness::new();
    let conn = hot_db(&harness).await;

    let state = reconstruct(&conn, "2026-03-01T00:00:00.000000Z", None, None)
        .await
        .unwrap();

    assert!(state.concepts.is_empty());
    assert_eq!(state.seq_anchor, 0);
}

fn empty_state(seq_anchor: i64) -> MaterializedState {
    MaterializedState {
        seq_anchor,
        timestamp: "2026-01-01T00:00:00.000000Z".to_string(),
        concepts: Default::default(),
        edges: Vec::new(),
    }
}

fn surviving_anchors(dir: &Path) -> Vec<i64> {
    let mut anchors: Vec<i64> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".snap.zst")?
                .parse()
                .ok()
        })
        .collect();
    anchors.sort();
    anchors
}

/// Retention used to sort filenames as strings under an `{:08}` format. Past
/// 10^8 the names change length, and `"100000000" < "99999997"` lexicographically
/// — so "delete the oldest" deleted the *newest* snapshots first. The anchors
/// here straddle that boundary deliberately.
#[tokio::test]
async fn retention_orders_by_sequence_not_by_filename() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    let anchors = [
        99_999_997,
        99_999_998,
        99_999_999,
        100_000_000,
        100_000_001,
        100_000_002,
        100_000_003,
    ];
    for a in anchors {
        save_snapshot(&dir, &empty_state(a)).unwrap();
    }

    let removed = cleanup_expired_snapshots(&dir).unwrap();

    assert_eq!(removed, 2);
    assert_eq!(
        surviving_anchors(&dir),
        vec![
            99_999_999,
            100_000_000,
            100_000_001,
            100_000_002,
            100_000_003
        ],
        "the five highest anchors must survive"
    );
}

/// A snapshot is loaded with no integrity check beyond what zstd and bincode
/// happen to notice, so a half-written file under the final name looks loadable
/// and is not — and it is the newest one, the one a restart reaches for first.
/// The write goes to a temp name and is renamed in, which also means no `.tmp`
/// may survive a successful save.
#[tokio::test]
async fn a_successful_save_leaves_no_temporary_file() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    let path = save_snapshot(&dir, &empty_state(42)).unwrap();

    assert!(path.exists());
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
}

/// A save interrupted before the rename leaves a `.tmp` nothing will ever read.
/// Left alone they accumulate for the life of the database.
#[tokio::test]
async fn cleanup_removes_orphaned_temporary_files() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");
    save_snapshot(&dir, &empty_state(1)).unwrap();
    std::fs::write(dir.join("interrupted.tmp"), b"partial").unwrap();

    cleanup_expired_snapshots(&dir).unwrap();

    assert!(!dir.join("interrupted.tmp").exists());
    assert_eq!(surviving_anchors(&dir), vec![1], "real snapshots untouched");
}

/// `write_final` was a `Ok(())` stub and `close()` never called it, so the
/// shutdown anchor §5.1.7 specifies was never written and every restart replayed
/// from whatever snapshot happened to be lying around.
#[tokio::test]
async fn close_writes_the_final_snapshot() {
    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    db.close().await.unwrap();

    let dir = harness.temp_dir.path().join("test_macrame_snapshots");
    assert!(dir.exists(), "close() must write a snapshot");
    let anchors = surviving_anchors(&dir);
    assert_eq!(anchors.len(), 1, "expected exactly one snapshot, got {anchors:?}");

    let path = dir.join(format!("{:019}.snap.zst", anchors[0]));
    macrame::temporal::load_snapshot(&path).expect("the final snapshot must load");
}

/// A file whose name we cannot order is not a file we may delete on a guess.
#[tokio::test]
async fn cleanup_skips_files_it_cannot_order() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), b"keep me").unwrap();
    std::fs::write(dir.join("mystery.snap.zst"), b"unparseable anchor").unwrap();

    let removed = cleanup_expired_snapshots(&dir).unwrap();

    assert_eq!(removed, 0);
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join("mystery.snap.zst").exists());
}

// -- D-043: the snapshot container is versioned -----------------------------

/// A snapshot written by this build must carry the header and round-trip.
#[tokio::test]
async fn a_snapshot_carries_its_header_and_reloads() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");
    let path = save_snapshot(&dir, &empty_state(7)).unwrap();

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[0..4], b"MACR", "missing magic: {:?}", &raw[0..4]);
    assert_eq!(u16::from_le_bytes([raw[4], raw[5]]), 1, "format version");
    assert_eq!(
        u32::from_le_bytes([raw[6], raw[7], raw[8], raw[9]]),
        migrations::SCHEMA_VERSION,
        "schema version"
    );

    let loaded = macrame::temporal::load_snapshot(&path).unwrap();
    assert_eq!(loaded.seq_anchor, 7);
}

/// **The failure the header exists to prevent.**
///
/// `bincode` is not self-describing, so a snapshot written against a different
/// `MaterializedState` does not reliably fail to parse — it parses into wrong
/// values, and a snapshot is the first thing a restart reaches for. A header
/// mismatch must be refused *before* the payload is decompressed, and refused
/// with a variant distinct from corruption, because the correct response is to
/// discard the file and fold from the log rather than to report a fault.
#[tokio::test]
async fn a_snapshot_from_another_schema_version_is_refused_not_parsed() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");
    let path = save_snapshot(&dir, &empty_state(9)).unwrap();

    // Bump the recorded schema version; leave the payload byte-identical, so
    // the only thing that can reject it is the header check.
    let mut raw = std::fs::read(&path).unwrap();
    raw[6..10].copy_from_slice(&(migrations::SCHEMA_VERSION + 1).to_le_bytes());
    std::fs::write(&path, &raw).unwrap();

    match macrame::temporal::load_snapshot(&path).unwrap_err() {
        DbError::SnapshotIncompatible { reason, .. } => {
            assert!(reason.contains("schema v"), "reason should name it: {reason}");
        }
        other => panic!("expected SnapshotIncompatible, got {other:?}"),
    }
}

/// Files written before the container existed begin with zstd's magic, so the
/// same check catches them without a special case.
#[tokio::test]
async fn a_headerless_legacy_snapshot_is_refused() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("0000000000000000042.snap.zst");

    // Exactly what 0.5.4 wrote: zstd(bincode(state)), no header.
    let payload = bincode::serialize(&empty_state(42)).unwrap();
    std::fs::write(&path, zstd::encode_all(&payload[..], 3).unwrap()).unwrap();

    match macrame::temporal::load_snapshot(&path).unwrap_err() {
        DbError::SnapshotIncompatible { reason, .. } => {
            assert!(reason.contains("0.5.4"), "reason should say why: {reason}");
        }
        other => panic!("expected SnapshotIncompatible, got {other:?}"),
    }
}

// -- D-044: a leaked cold handle heals on the way in ------------------------

/// A `cold` handle left attached by an earlier call must not poison the
/// connection.
///
/// Both ATTACH sites pair with an unconditional DETACH, so the only way to get
/// here is a panic unwinding between the two — which no `Result` path and no
/// `Drop` guard can cover, since `execute` is `async` and `Drop` cannot await.
/// Simulated directly by attaching and not detaching, which is exactly the
/// state such a panic leaves behind.
#[tokio::test]
async fn a_leaked_cold_attachment_does_not_poison_the_connection() {
    let harness = TestHarness::new();
    let cold_path = harness.temp_dir.path().join("archive.db");
    make_cold_archive(&cold_path, &[]).await;

    let db = libsql::Builder::new_local(&harness.db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // The state a panic between ATTACH and DETACH leaves behind.
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![cold_path.to_string_lossy().as_ref()],
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "ATTACH DATABASE ?1 AS cold",
            libsql::params![cold_path.to_string_lossy().as_ref()]
        )
        .await
        .is_err(),
        "precondition: a second ATTACH under the same name must fail"
    );

    // Both entry points must recover rather than inherit the leak.
    reconstruct(&conn, "2026-06-01T00:00:00.000000Z", Some(&cold_path), None)
        .await
        .expect("reconstruct must survive a leaked cold handle");
    macrame::temporal::archive(&conn, "2026-06-01T00:00:00.000000Z", &cold_path)
        .await
        .expect("archive must survive a leaked cold handle");
}

// -- D-049: snapshot composition -------------------------------------------

const CTS: &str = "2026-01-01T00:00:00.000000Z";

/// A `Database` with two concepts, ready to hang edges off.
async fn composed_db(harness: &TestHarness) -> macrame::Database {
    use macrame::prelude::*;
    let db = macrame::Database::open(&harness.db_path).await.unwrap();
    for id in ["A", "B", "C"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(CTS))
            .await
            .unwrap();
    }
    db
}

async fn max_recorded_at(db: &macrame::Database) -> String {
    db.read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// **The gap-tolerance test D-024 named in §8 and could not have.**
///
/// Until snapshot composition there was no anchored fold, so there was no
/// `seq_id > :anchor` for gap tolerance to be a property *of* — the rule was
/// vacuous rather than satisfied. An anchored fold written with
/// `seq_id = :anchor + 1`, or any successor arithmetic, stops at the first hole
/// and silently truncates the delta; the inequality steps over it.
///
/// **The hole is made by deleting a log row, not by rolling a write back**, and
/// that is a correction to D-024 rather than a convenience. D-024 asserts that
/// "a rolled-back transaction still increments the `sqlite_sequence` counter,
/// leaving a gap". Measured against libSQL 0.9.30 it does not: `sqlite_sequence`
/// is written inside the transaction, so a rollback undoes it and the number is
/// reused. Gaps in the *hot* log are real all the same — the archive path
/// deletes superseded rows from `transaction_log` (§5.7), scattered through the
/// sequence rather than as a prefix — so the rule D-024 states is right and its
/// stated mechanism is wrong. This test builds the state the real mechanism
/// produces.
#[tokio::test]
async fn the_anchored_fold_steps_over_a_seq_id_gap() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = composed_db(&harness).await;
    let snaps = harness.temp_dir.path().join("snaps");

    db.assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from(CTS))
        .await
        .unwrap();

    let mid = max_recorded_at(&db).await;
    let base = reconstruct(db.read_conn(), &mid, None, None).await.unwrap();
    save_snapshot(&snaps, &base).unwrap();

    db.assert_edge(EdgeAssertion::new("B", "C", "KNOWS").valid_from(CTS))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("A", "C", "KNOWS").valid_from(CTS))
        .await
        .unwrap();

    // Punch the hole the way the archive does: delete a log row above the
    // anchor, inside a session marker so the delete guard permits it.
    {
        let raw = libsql::Builder::new_local(&harness.db_path)
            .build()
            .await
            .unwrap()
            .connect()
            .unwrap();
        let victim: i64 = raw
            .query(
                "SELECT MIN(seq_id) FROM transaction_log WHERE seq_id > ?1",
                libsql::params![base.seq_anchor],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        raw.execute("CREATE TABLE macrame_archive_session (x)", ())
            .await
            .unwrap();
        raw.execute(
            "DELETE FROM transaction_log WHERE seq_id = ?1",
            libsql::params![victim],
        )
        .await
        .unwrap();
        raw.execute("DROP TABLE macrame_archive_session", ())
            .await
            .unwrap();
    }

    // The gap is real: the newest seq_id has outrun the number of rows.
    let count: i64 = count_of(&db, "SELECT COUNT(*) FROM transaction_log").await;
    let max_seq: i64 = count_of(&db, "SELECT MAX(seq_id) FROM transaction_log").await;
    assert!(max_seq > count, "expected a gap; max_seq={max_seq} rows={count}");

    let now = max_recorded_at(&db).await;
    let composed = reconstruct(db.read_conn(), &now, None, Some(&snaps))
        .await
        .unwrap();
    let folded = reconstruct(db.read_conn(), &now, None, None).await.unwrap();

    assert_eq!(composed.edges, folded.edges, "the delta was truncated at the gap");
    assert!(
        composed.edges.iter().any(|(s, t, ..)| s == "A" && t == "C"),
        "the edge written after the gap is missing: {:?}",
        composed.edges
    );

    db.close().await.unwrap();
}

async fn count_of(db: &macrame::Database, sql: &str) -> i64 {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// A snapshot this build cannot read is skipped, not raised (D-043).
///
/// The variant exists precisely so that an upgrade is not an outage: the fold
/// from genesis is slower and always available, so the right response to an
/// incompatible anchor is to carry on without it.
#[tokio::test]
async fn an_incompatible_snapshot_is_skipped_and_the_fold_still_answers() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = composed_db(&harness).await;
    let snaps = harness.temp_dir.path().join("snaps");

    db.assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from(CTS))
        .await
        .unwrap();
    let now = max_recorded_at(&db).await;
    let base = reconstruct(db.read_conn(), &now, None, None).await.unwrap();
    let path = save_snapshot(&snaps, &base).unwrap();

    // Same bytes, a schema version this build does not read.
    let mut raw = std::fs::read(&path).unwrap();
    raw[6..10].copy_from_slice(&(migrations::SCHEMA_VERSION + 1).to_le_bytes());
    std::fs::write(&path, &raw).unwrap();

    let composed = reconstruct(db.read_conn(), &now, None, Some(&snaps))
        .await
        .unwrap();
    let folded = reconstruct(db.read_conn(), &now, None, None).await.unwrap();
    assert_eq!(composed.edges, folded.edges);

    db.close().await.unwrap();
}

/// **Composition is off once an archive exists, and that is correctness.**
///
/// `LOG_ARCHIVABLE` removes *superseded* rows, which are scattered through the
/// sequence rather than forming a prefix, so a row above the anchor and at or
/// before `ts` can be gone from the hot log while a newer row for the same
/// entity — recorded after `ts`, and so invisible to the fold — keeps it out.
/// The delta would miss it and the snapshot would answer with the older value.
///
/// Observed by planting a snapshot that is *wrong*: it claims a concept the
/// database never had. With no archive the anchor is used and the ghost shows
/// through; with an archive file present composition is skipped and it does
/// not. Testing the switch rather than the consequence, because the consequence
/// is a wrong answer that only a specific archive interleaving produces.
#[tokio::test]
async fn an_existing_archive_disables_composition() {
    use macrame::prelude::*;
    use macrame::temporal::as_of::NodeAttributes;

    let harness = TestHarness::new();
    let db = composed_db(&harness).await;
    let snaps = harness.temp_dir.path().join("snaps");
    let archive = harness.temp_dir.path().join("cold.db");

    db.assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from(CTS))
        .await
        .unwrap();
    let now = max_recorded_at(&db).await;

    let mut planted = reconstruct(db.read_conn(), &now, None, None).await.unwrap();
    planted.concepts.insert(
        "GHOST".to_string(),
        NodeAttributes {
            id: "GHOST".to_string(),
            title: "not in the ledger".to_string(),
            content: String::new(),
            embedding_model: None,
        },
    );
    save_snapshot(&snaps, &planted).unwrap();

    let used = reconstruct(db.read_conn(), &now, None, Some(&snaps))
        .await
        .unwrap();
    assert!(
        used.concepts.contains_key("GHOST"),
        "precondition: with no archive the snapshot must be the base"
    );

    make_cold_archive(&archive, &[]).await;
    let skipped = reconstruct(db.read_conn(), &now, Some(&archive), Some(&snaps))
        .await
        .unwrap();
    assert!(
        !skipped.concepts.contains_key("GHOST"),
        "composition must be skipped once an archive database exists"
    );

    db.close().await.unwrap();
}

/// The handle's `reconstruct` wires both paths in, so composition is the
/// default rather than something a caller has to remember to switch on.
#[tokio::test]
async fn the_handle_reconstructs_through_its_own_snapshot_directory() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = composed_db(&harness).await;
    db.assert_edge(EdgeAssertion::new("A", "B", "KNOWS").valid_from(CTS))
        .await
        .unwrap();

    // close() writes the shutdown anchor into the handle's snapshots dir.
    let path = harness.db_path.clone();
    db.close().await.unwrap();

    let db = macrame::Database::open(&path).await.unwrap();
    let now = max_recorded_at(&db).await;
    let state = db.reconstruct(&now).await.unwrap();
    assert!(
        state.edges.iter().any(|(s, t, ..)| s == "A" && t == "B"),
        "{:?}",
        state.edges
    );
    assert!(
        std::fs::read_dir(db.snapshots_dir()).unwrap().count() > 0,
        "close() should have left an anchor for this to compose from"
    );
    db.close().await.unwrap();
}
