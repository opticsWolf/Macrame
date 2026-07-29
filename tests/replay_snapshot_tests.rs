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
    // v2 as of D-054: the header gained the snapshot's own instant.
    assert_eq!(u16::from_le_bytes([raw[4], raw[5]]), 2, "format version");
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

/// **Composition is no longer disabled by an archive (0.5.5).**
///
/// It used to be, and the refusal was load-bearing: with the delta folded over
/// the hot log alone, a row above the anchor and at or before `ts` could be in
/// cold while a newer row for the same entity kept it out of hot, so the
/// snapshot answered with a stale value. `ANCHORED_COLD_FOLD` puts the cold log
/// in the delta, which removes the reason rather than the symptom.
///
/// This test is the old one inverted, and it keeps the old one's virtue: the
/// snapshot is planted *wrong*, claiming a concept the database never had, so
/// its presence in the result proves the anchor was actually consulted. A test
/// that only checked the answer was right could not tell composition from a
/// silent fall back to the full fold.
#[tokio::test]
async fn an_archive_no_longer_disables_composition() {
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
    let with_archive = reconstruct(db.read_conn(), &now, Some(&archive), Some(&snaps))
        .await
        .unwrap();
    assert!(
        with_archive.concepts.contains_key("GHOST"),
        "the anchor must still be consulted once an archive database exists; \
         if this is empty, composition has silently fallen back to a full fold"
    );

    db.close().await.unwrap();
}

/// **The acceptance gate for composing across the archive boundary.**
///
/// Two mechanisms, one question — the D-049 shape, now extended to the case
/// D-049 carved out. Composing a snapshot with an anchored hot+cold delta must
/// agree with folding hot+cold from genesis, at instants on both sides of the
/// archive cutoff. Instants *before* the cutoff are the ones that matter: that
/// is where rows have actually moved to the cold file, and where a delta folded
/// over hot alone would answer with a stale snapshot value.
#[tokio::test]
async fn composing_across_the_archive_boundary_equals_folding_from_genesis() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();
    let snaps = harness.temp_dir.path().join("snaps");

    // A history in which both entities are written repeatedly, so the archive
    // has superseded rows to move and every instant has a distinct answer.
    let mut stamps = Vec::new();
    for round in 0..4 {
        for id in ["A", "B"] {
            db.upsert_concept(
                ConceptUpsert::new(id, format!("{id} round {round}")).valid_from(CTS),
            )
            .await
            .unwrap();
            stamps.push(max_recorded_at(&db).await);
        }
    }

    // Anchor taken mid-history, so the delta spans the rows that later move.
    let mid = stamps[3].clone();
    let base = db.reconstruct(&mid).await.unwrap();
    save_snapshot(&snaps, &base).unwrap();

    let cutoff = stamps[5].clone();
    let report = db.archive(&cutoff).await.unwrap();
    assert!(
        report.log_entries_archived > 0,
        "the fixture must actually move rows to cold: {report:?}"
    );

    let archive = db.archive_path().to_path_buf();
    for ts in &stamps {
        let composed = reconstruct(db.read_conn(), ts, Some(&archive), Some(&snaps))
            .await
            .unwrap();
        let folded = reconstruct(db.read_conn(), ts, Some(&archive), None)
            .await
            .unwrap();

        let titles = |s: &MaterializedState| {
            s.concepts
                .iter()
                .map(|(k, v)| (k.clone(), v.title.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(
            titles(&composed),
            titles(&folded),
            "composed and full-fold disagree at {ts} (cutoff {cutoff})"
        );
    }

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

// ---------------------------------------------------------------------------
// The archive read path — `hot_log_covers` was not a completeness test
// ---------------------------------------------------------------------------

/// **A reconstruction of a pre-cutoff instant used to silently lose entities.**
///
/// `hot_log_covers` decided whether the cold file was needed by asking
/// `MIN(recorded_at) <= ts`. That is a test of how far back the hot log *reaches*,
/// not of whether it is *complete*, and `LOG_ARCHIVABLE` removes superseded rows
/// scattered through the sequence rather than a prefix. The two come apart as
/// soon as one entity is archived and another is not:
///
/// * `E` is written three times. The archive moves its first two log rows to
///   cold and keeps the third, because the newest per entity always stays hot.
/// * `F` is written once. Nothing supersedes it, so it stays hot — and it is the
///   *oldest* surviving row, so `MIN(recorded_at)` still points before the
///   archive cutoff.
///
/// Ask for belief at `E`'s second write. `MIN <= ts` says the hot log covers it,
/// so the fold runs over hot alone; `E`'s winning row is in cold and its third
/// row is later than `ts`, so **`E` disappears from the result entirely**. No
/// error, no missing file — just a state that has quietly forgotten a concept it
/// was asked about, which is the failure Doctrine II exists to prevent.
#[tokio::test]
async fn reconstructing_before_the_archive_cutoff_keeps_every_entity() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    // F first, so its single row is the oldest thing in the log.
    db.upsert_concept(ConceptUpsert::new("F", "F only ever written once").valid_from(CTS))
        .await
        .unwrap();

    let mut stamps = Vec::new();
    for title in ["E first", "E second", "E third"] {
        db.upsert_concept(ConceptUpsert::new("E", title).valid_from(CTS))
            .await
            .unwrap();
        stamps.push(max_recorded_at(&db).await);
    }
    let at_second_write = stamps[1].clone();

    let truth = db.reconstruct(&at_second_write).await.unwrap();
    assert_eq!(
        truth.concepts.get("E").map(|c| c.title.as_str()),
        Some("E second"),
        "before any archive, belief at the second write is the second title"
    );

    // Archive everything superseded. E's first two log rows go cold; E's third
    // and F's only row stay hot.
    let report = db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs superseded log rows to have moved: {report:?}"
    );

    let after = db.reconstruct(&at_second_write).await.unwrap();
    assert_eq!(
        after.concepts.get("E").map(|c| c.title.as_str()),
        Some("E second"),
        "archiving changed what was believed at {at_second_write}: reconstruct \
         answered from the hot log alone while E's winning row was in cold"
    );
    assert!(
        after.concepts.contains_key("F"),
        "the unsuperseded concept must survive too"
    );
    assert_eq!(
        truth.concepts.get("E"),
        after.concepts.get("E"),
        "an archive is a storage move, not a change of belief"
    );

    db.close().await.unwrap();
}

/// `reconstruct(now)` must still answer from the hot log after an archive — the
/// fast path `LOG_ARCHIVABLE` is designed around, and the one the fix must not
/// trade away in exchange for soundness.
#[tokio::test]
async fn reconstructing_now_still_answers_after_an_archive() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();
    for title in ["one", "two", "three"] {
        db.upsert_concept(ConceptUpsert::new("E", title).valid_from(CTS))
            .await
            .unwrap();
    }

    db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();

    let now = max_recorded_at(&db).await;
    let state = db.reconstruct(&now).await.unwrap();
    assert_eq!(
        state.concepts.get("E").map(|c| c.title.as_str()),
        Some("three"),
        "current belief is the newest write, and it never left the hot log"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The snapshot cadence (§5.5, D-053)
// ---------------------------------------------------------------------------

/// Wait for `f` to hold, or give up. Polls rather than sleeping a fixed time,
/// so the test is as fast as the machine allows and still deterministic about
/// the outcome — a fixed sleep is either flaky or slow, and usually both.
async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    false
}

fn snapshot_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "zst"))
                .count()
        })
        .unwrap_or(0)
}

fn fast_cadence(every: i64) -> macrame::temporal::SnapshotCadence {
    macrame::temporal::SnapshotCadence {
        every_entries: every,
        poll_interval: std::time::Duration::from_millis(20),
    }
}

/// **The cadence writes an anchor without anyone asking.**
///
/// Before this, `close()` was the only thing that ever wrote one, so a
/// long-running process accumulated an unbounded delta and §9's "≤200 ms with
/// snapshot" described a mechanism that only ran at shutdown.
#[tokio::test]
async fn the_cadence_anchors_a_running_database() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open_with_cadence(&harness.db_path, Some(fast_cadence(3)))
        .await
        .unwrap();
    let dir = db.snapshots_dir().to_path_buf();

    for i in 0..8 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), "N").valid_from(CTS))
            .await
            .unwrap();
    }

    assert!(
        eventually(|| snapshot_count(&dir) > 0).await,
        "the cadence never wrote an anchor"
    );

    db.close().await.unwrap();
}

/// An idle database writes nothing, however long it is left open.
///
/// The cadence is a *distance* in log entries, not a schedule — the thing worth
/// bounding is how much delta a reconstruction folds, and an idle database adds
/// none. A time-based cadence would rewrite an identical snapshot forever and
/// call it maintenance.
#[tokio::test]
async fn an_idle_database_is_never_anchored() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open_with_cadence(&harness.db_path, Some(fast_cadence(3)))
        .await
        .unwrap();
    let dir = db.snapshots_dir().to_path_buf();

    // Two writes, one short of the threshold, then nothing at all.
    for i in 0..2 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), "N").valid_from(CTS))
            .await
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert_eq!(
        snapshot_count(&dir),
        0,
        "an idle database below the threshold must not be anchored"
    );

    db.close().await.unwrap();
}

/// **A cadence anchor changes no answer.**
///
/// The composed and full-fold paths must agree at every instant, exactly as
/// D-049 requires — the difference here is that the anchor was written by a
/// background task mid-history rather than by a test that chose the moment. If
/// the cadence anchored at a `ts` later than anything it actually reflects, or
/// left the delta straddling its own write, this is where it would show.
#[tokio::test]
async fn a_cadence_anchor_does_not_change_what_reconstruct_answers() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open_with_cadence(&harness.db_path, Some(fast_cadence(3)))
        .await
        .unwrap();
    let dir = db.snapshots_dir().to_path_buf();

    let mut stamps = Vec::new();
    for i in 0..6 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), format!("title {i}")).valid_from(CTS))
            .await
            .unwrap();
        stamps.push(max_recorded_at(&db).await);
    }
    assert!(
        eventually(|| snapshot_count(&dir) > 0).await,
        "no anchor was written, so this asserts nothing"
    );

    // More history *after* the anchor, so composition has a real delta to apply.
    for i in 6..10 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), format!("title {i}")).valid_from(CTS))
            .await
            .unwrap();
        stamps.push(max_recorded_at(&db).await);
    }

    for ts in &stamps {
        let composed = reconstruct(db.read_conn(), ts, None, Some(&dir)).await.unwrap();
        let folded = reconstruct(db.read_conn(), ts, None, None).await.unwrap();
        let titles = |s: &MaterializedState| {
            s.concepts
                .iter()
                .map(|(k, v)| (k.clone(), v.title.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(
            titles(&composed),
            titles(&folded),
            "a cadence-written anchor changed the answer at {ts}"
        );
    }

    db.close().await.unwrap();
}

/// **`close()` stops the task, and stops it before taking the final snapshot.**
///
/// The lifecycle question this feature was left open on. A task nobody stops
/// outlives the handle that spawned it, holding a connection whose database is
/// being torn down; and one still running during `write_final` has both of them
/// enumerating and deleting in the same directory.
///
/// **The obvious form of this test asserts nothing**, and did: close the handle,
/// wait, check no new snapshot appeared. Nothing appears either way, because
/// nothing is *writing* after close — an idle-but-alive task is indistinguishable
/// from a stopped one. Verified by mutation: with `close()` leaking its stop
/// signal, that version still passed.
///
/// So the log has to keep growing after `close()` returns. The writes go through
/// a raw connection rather than a second `Database`: the trigger fires either
/// way, and a second handle would mean a second actor and a second cadence task
/// alive in one process, which is precisely the churn R15 punishes.
#[tokio::test]
async fn closing_the_handle_stops_the_cadence() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open_with_cadence(&harness.db_path, Some(fast_cadence(1)))
        .await
        .unwrap();
    let dir = db.snapshots_dir().to_path_buf();

    for i in 0..4 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), "N").valid_from(CTS))
            .await
            .unwrap();
    }
    assert!(eventually(|| snapshot_count(&dir) > 0).await, "cadence never ran");

    db.close().await.unwrap();
    let settled = snapshot_count(&dir);

    // Keep the log growing. A live task would see the head advance past its
    // anchor by more than the threshold and write within a few ticks.
    let outside = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = outside.connect().unwrap();
    for i in 4..12 {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 'N', ?2, ?2)",
            libsql::params![format!("c{i}"), format!("2027-01-01T00:00:{i:02}.000000Z")],
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert_eq!(
        snapshot_count(&dir),
        settled,
        "a snapshot appeared after close() returned, while the log was still \
         growing: the cadence outlived the handle that spawned it"
    );
}

/// Opting out restores the pre-0.5.5 behaviour exactly: `close()` is the only
/// thing that writes an anchor.
#[tokio::test]
async fn a_disabled_cadence_writes_nothing_until_close() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();
    let dir = db.snapshots_dir().to_path_buf();

    for i in 0..8 {
        db.upsert_concept(ConceptUpsert::new(format!("c{i}"), "N").valid_from(CTS))
            .await
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(snapshot_count(&dir), 0, "no cadence was asked for");

    db.close().await.unwrap();
    assert_eq!(snapshot_count(&dir), 1, "close() still writes the final anchor");
}

// ---------------------------------------------------------------------------
// Retention: the newest five, plus one per day (§5.5, D-054)
// ---------------------------------------------------------------------------

/// A state anchored at `seq`, reflecting midday on epoch-day `day`.
///
/// Built by epoch arithmetic rather than by formatting a day number into a
/// date string. The first version of this helper wrote
/// `format!("2026-01-{:02}", day + 1)`, which produces `2026-01-41` past day 39
/// — a shape-valid, calendar-invalid timestamp that `parse` correctly refuses.
/// Those snapshots landed with *no* instant in their header, so the two tests
/// that used them were measuring the dateless path instead of the daily one, and
/// one of them passed for the wrong reason.
fn state_on_day(seq: i64, day: u64) -> MaterializedState {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(day * 86_400 + 12 * 3_600);
    MaterializedState {
        seq_anchor: seq,
        timestamp: macrame::util::timestamp::format(at),
        concepts: Default::default(),
        edges: Vec::new(),
    }
}

/// **The daily tier keeps history the flat rule threw away.**
///
/// Ten snapshots across ten days. Newest-five alone keeps days 5–9 and deletes
/// the rest, so every instant older than five anchors folds the whole log — which
/// is precisely the cost the cadence was added to avoid, defeated by the
/// retention rule it inherited.
#[tokio::test]
async fn retention_keeps_one_snapshot_per_day_beyond_the_newest_five() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    for day in 0..10u64 {
        save_snapshot(&dir, &state_on_day(day as i64 + 1, day)).unwrap();
    }

    cleanup_expired_snapshots(&dir).unwrap();

    assert_eq!(
        surviving_anchors(&dir),
        (1..=10).collect::<Vec<i64>>(),
        "each of the ten days is inside the thirty-day window, so all ten survive"
    );
}

/// Several snapshots on one day collapse to one — the newest — while the daily
/// coverage either side is untouched.
#[tokio::test]
async fn retention_collapses_a_busy_day_to_its_newest_snapshot() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    // Day 0: four anchors. Days 1..8: one each. The newest five are all on the
    // later days, so day 0's survivor is decided by the daily rule alone.
    for seq in 1..=4 {
        save_snapshot(&dir, &state_on_day(seq, 0)).unwrap();
    }
    for day in 1..9u64 {
        save_snapshot(&dir, &state_on_day(4 + day as i64, day)).unwrap();
    }

    cleanup_expired_snapshots(&dir).unwrap();

    let survivors = surviving_anchors(&dir);
    assert!(
        survivors.contains(&4),
        "day 0 must keep its newest anchor: {survivors:?}"
    );
    for gone in [1, 2, 3] {
        assert!(
            !survivors.contains(&gone),
            "day 0's superseded anchor {gone} should have been collapsed: {survivors:?}"
        );
    }
    assert_eq!(survivors.len(), 9, "one per day, nine days: {survivors:?}");
}

/// Beyond the window, the daily tier stops protecting anything — otherwise
/// "thirty days" would mean "forever" and the directory would grow without
/// bound, which is the failure retention exists to prevent.
#[tokio::test]
async fn retention_drops_days_past_the_window() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    // One snapshot 40 days before the newest, well outside the thirty-day
    // window, and six recent ones so the newest-five rule cannot rescue it.
    save_snapshot(&dir, &state_on_day(1, 0)).unwrap();
    for i in 0..6u64 {
        save_snapshot(&dir, &state_on_day(10 + i as i64, 40 + i)).unwrap();
    }

    cleanup_expired_snapshots(&dir).unwrap();

    let survivors = surviving_anchors(&dir);
    assert!(
        !survivors.contains(&1),
        "a snapshot 40 days older than the newest is outside the window: {survivors:?}"
    );
    assert_eq!(survivors.len(), 6, "the six recent days survive: {survivors:?}");
}

/// The newest five survive regardless of date, so a burst inside a single day
/// still leaves a usable ladder of recent anchors — which is the tier the
/// cadence actually depends on.
#[tokio::test]
async fn the_newest_five_survive_even_within_one_day() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    for seq in 1..=8 {
        save_snapshot(&dir, &state_on_day(seq, 0)).unwrap();
    }

    cleanup_expired_snapshots(&dir).unwrap();

    assert_eq!(
        surviving_anchors(&dir),
        vec![4, 5, 6, 7, 8],
        "the newest five, all on the same day"
    );
}

/// The header carries the snapshot's own instant so retention can bucket by day
/// without decompressing anything — and it must agree with the payload, or the
/// two descriptions have already drifted.
#[tokio::test]
async fn the_header_instant_matches_the_payload() {
    let harness = TestHarness::new();
    let dir = harness.temp_dir.path().join("snapshots");

    let state = state_on_day(7, 3);
    let path = save_snapshot(&dir, &state).unwrap();

    let loaded = macrame::temporal::load_snapshot(&path).unwrap();
    assert_eq!(loaded.timestamp, state.timestamp);

    // Read the header alone: magic, format, schema, then the instant.
    let raw = std::fs::read(&path).unwrap();
    let micros = u64::from_le_bytes(raw[10..18].try_into().unwrap());
    let expected = macrame::util::timestamp::parse(&state.timestamp)
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    assert_eq!(
        micros, expected,
        "the header instant must be the payload's timestamp, not an approximation"
    );
}
