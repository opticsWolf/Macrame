//! `writer_temp_store` moves where the writer journals, and nothing else
//! (0.21.0, D-306).
//!
//! The pragma is per-connection and the writer is unnameable from outside the
//! actor, so — as in `cache_size_tests` and `wal_policy_tests` — what can be
//! asserted is the half that would break: the readers keep SQLite's setting,
//! and the two policies leave byte-for-byte the same ledger behind, including
//! when a chunk is refused partway through and its statements have to be
//! undone. The speed it buys is a bench's to show (`chunk_budget/edges/90`),
//! not a test's.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Past the size at which one edge's statement journal outgrows 64 KiB on the
/// empty fixture, so the `File` arm really does journal to disk.
const ROWS: usize = 120;

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn open(harness: &TestHarness, policy: TempStorePolicy) -> Database {
    Database::open_tuned(
        &harness.db_path,
        Tuning::default()
            .cadence(CadencePolicy::Disabled)
            .writer_temp_store(policy),
    )
    .await
    .unwrap()
}

fn chunk(edge_type: &str, last_target: &str) -> Vec<EdgeAssertion> {
    (1..=ROWS)
        .map(|k| {
            let target = if k == ROWS {
                last_target.to_string()
            } else {
                format!("c{k:04}")
            };
            EdgeAssertion::new("c0000", target, edge_type)
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect()
}

/// What a ledger holds, as the three tables an edge write touches see it.
/// `recorded_at` is left out: it is the clock's, and the two arms run at
/// different instants.
async fn ledger(db: &Database) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT source_id || '|' || target_id || '|' || edge_type || '|' || valid_from \
         || '|' || valid_to FROM links ORDER BY 1",
        "SELECT source_id || '|' || target_id || '|' || edge_type || '|' || valid_from \
         || '|' || valid_to FROM links_current ORDER BY 1",
        "SELECT table_name || '|' || entity_id || '|' || operation || '|' || payload \
         FROM transaction_log WHERE table_name = 'links' ORDER BY seq_id",
    ] {
        let mut rows = db.read_conn().query(sql, ()).await.unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            out.push(row.get::<String>(0).unwrap());
        }
    }
    out
}

async fn run(policy: TempStorePolicy) -> (Vec<String>, i64) {
    let harness = TestHarness::new();
    let db = open(&harness, policy).await;
    let concepts: Vec<_> = (0..=ROWS)
        .map(|i| ConceptUpsert::new(format!("c{i:04}"), format!("Concept {i}")).valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();

    db.write_bulk_atomic(chunk("KEPT", &format!("c{ROWS:04}")))
        .await
        .unwrap();
    // The last edge names a concept that does not exist, so its foreign key
    // fails after the 119 before it — and their triggers — have written.
    let refused = db.write_bulk_atomic(chunk("REFUSED", "nowhere")).await;
    assert!(
        refused.is_err(),
        "{policy:?}: an edge to a missing concept landed"
    );

    let reader_temp_store = scalar(db.read_conn(), "PRAGMA temp_store").await;
    let held = ledger(&db).await;
    db.close().await.unwrap();
    (held, reader_temp_store)
}

/// **The two policies leave the same ledger**, and a refused chunk leaves
/// nothing of itself in either.
#[tokio::test]
async fn both_policies_write_the_same_ledger_and_refuse_the_same_chunk() {
    let (memory, _) = run(TempStorePolicy::Memory).await;
    let (file, _) = run(TempStorePolicy::File).await;

    assert_eq!(
        memory.len(),
        3 * ROWS,
        "one links row, one links_current row and one log row per kept edge"
    );
    assert!(
        !memory.iter().any(|row| row.contains("REFUSED")),
        "the refused chunk left rows behind under Memory"
    );
    assert_eq!(
        memory, file,
        "where the writer journals changed what it wrote"
    );
}

/// **The readers keep SQLite's setting** under either policy: the knob is the
/// writer's, and a reader's temporary sorts are not this knob's to move.
#[tokio::test]
async fn the_readers_temp_store_is_not_the_writers() {
    for policy in [TempStorePolicy::Memory, TempStorePolicy::File] {
        let (_, reader) = run(policy).await;
        assert_eq!(
            reader, 0,
            "{policy:?}: the shared reader reports temp_store = {reader}; \
             the writer's pragma reached a reader"
        );
    }
}
