//! Probe: does the v28 import trigger fire on the takeover's version row?
#[path = "../tests/common/harness.rs"]
mod harness_db;

#[tokio::main]
async fn main() {
    let harness = harness_db::TestHarness::new();
    let db0: std::sync::Arc<macrame::Database> =
        std::sync::Arc::new(macrame::Database::open(&harness.db_path).await.unwrap());
    use macrame::prelude::*;
    db0.upsert_concept(
        ConceptUpsert::new("x", "the trunk's belief")
            .valid_from("2026-01-01T00:00:00.000000Z")
            .valid_to("9999-12-31T23:59:59.999999Z"),
    )
    .await
    .unwrap();
    db0.fork(
        macrame::BranchId::new("b0").unwrap(),
        macrame::BranchId::main(),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    db0.view(macrame::BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("x", "the branch's belief")
                .supersede(true)
                .valid_from("2026-01-10T00:00:00.000000Z")
                .valid_to("9999-12-31T23:59:59.999999Z"),
        )
        .await
        .unwrap();

    async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
        let mut r = conn.query(sql, ()).await.unwrap();
        r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }
    let raw = db0.raw().connect().unwrap();
    println!(
        "versions: {}",
        scalar(
            &raw,
            "SELECT COUNT(*) FROM concept_versions WHERE id='x' AND branch_id='b0'",
        )
        .await
    );
    println!(
        "shadow: {}",
        scalar(
            &raw,
            "SELECT COUNT(*) FROM concept_version_fts_rows WHERE id='x'"
        )
        .await
    );
    let mut rows = raw
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'trg_cversion%'",
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        let n: String = r.get(0).unwrap();
        println!("  trigger: {n}");
    }
    println!(
        "fts match 'branch': {}",
        scalar(
            &raw,
            "SELECT COUNT(*) FROM concept_versions_fts WHERE concept_versions_fts MATCH 'branch'",
        )
        .await
    );

    let model = macrame::vector::ModelName::new("m6c_v2").unwrap();
    println!("register: {:?}", db0.register_model(&model, 2).await);
    println!(
        "VE raw: {:?}",
        macrame::vector::upsert_version_embedding(
            &raw,
            &model,
            "x",
            "b0",
            "2026-01-10T00:00:00.000000Z",
            &[0.0, 1.0],
        )
        .await
    );
    println!(
        "VE bulk: {:?}",
        db0.upsert_version_embeddings(
            &model,
            vec![(
                "a".to_string(),
                "main".to_string(),
                "2026-01-01T00:00:00.000000Z".to_string(),
                vec![1.0, 0.0],
            )],
        )
        .await
    );
    let mut r = raw
        .query("SELECT COUNT(*) FROM embeddings_m6c_v2_versions", ())
        .await
        .unwrap();
    println!(
        "version rows: {:?}",
        r.next().await.unwrap().map(|row| row.get::<i64>(0))
    );
}
