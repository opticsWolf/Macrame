//! Standalone probe for [D-316]: the trunk walk's pasted fold row.
//!
//! Prints the `concepts` log rows at each step (the folds' one source
//! beside the tables), then reads through `concepts_by_id` at a pre-cut
//! instant and past the cut. Red before [D-316]'s paste both ways: T1
//! answered `[]` after either a forward retirement or a plain supersede.
use macrame::prelude::*;
use std::sync::Arc;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-01-15T00:00:00.000000Z";
const T2: &str = "2026-02-01T00:00:00.000000Z";
const T3: &str = "2026-03-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

async fn dump(db: &Arc<Database>, label: &str) {
    println!("== {label} ==");
    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT seq_id, entity_id, operation, \
             json_extract(payload,'$.valid_from'), \
             json_extract(payload,'$.valid_to'), \
             json_extract(payload,'$.retired'), recorded_at \
             FROM transaction_log WHERE table_name='concepts' ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        println!(
            "{} {} {} vf={} vt={} r={} @{}",
            row.get::<i64>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
            row.get::<Option<String>>(3)
                .unwrap_or(None)
                .unwrap_or_default(),
            row.get::<Option<String>>(4)
                .unwrap_or(None)
                .unwrap_or_default(),
            row.get::<i64>(5).unwrap_or(0),
            row.get::<String>(6).unwrap()
        );
    }
}

async fn read(db: &Arc<Database>, at: &str, label: &str) {
    let rows = db
        .concepts_by_id(
            &["g".to_string()],
            ReadPlan::new().on(BranchId::main()).valid_at(at),
        )
        .await
        .unwrap();
    println!(
        "read {label} ({at}): {:?}",
        rows.iter()
            .map(|r| (&r.title, &r.valid_from, &r.valid_to))
            .collect::<Vec<_>>()
    );
}

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("probe.db");
    let db: Arc<Database> = Arc::new(Database::open(&path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("g", "live g")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    dump(&db, "after the mint").await;

    // The plain supersede: closes [T0,T2), opens [T2,FOREVER).
    db.upsert_concept_superseding(
        ConceptUpsert::new("g", "new text")
            .valid_from(T2)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    dump(&db, "after the supersede").await;
    read(&db, T1, "T1 (pre-cut)").await;
    read(&db, T2, "T2 (the write's own start)").await;
    read(&db, T3, "T3 (past)").await;

    // The forward retirement on top: tombstone [T2,T2).
    db.upsert_concept(
        ConceptUpsert::new("g", "new text")
            .valid_from(T2)
            .valid_to(T2)
            .retired(true),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    dump(&db, "after the retirement").await;
    read(&db, T1, "T1 (below the retirement's cut)").await;
    read(&db, T3, "T3 (past the retirement)").await;
}
