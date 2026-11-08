//! A narrow probe: one store, one takeover-supersede on b0, dump everything.
use macrame::prelude::*;
use macrame::BranchId;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const T5: &str = "2026-01-10T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("m6b-route-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = std::sync::Arc::new(Database::open(dir.join("probe.db")).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("x", OLD2)
            .valid_from(TS)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    tick().await;
    let view = db.view(BranchId::new("b0").unwrap());
    let got = view
        .upsert_concept(
            ConceptUpsert::new("x", "the branch's belief")
                .supersede(true)
                .valid_from(T5)
                .valid_to(FOREVER),
        )
        .await;
    println!("upsert -> {got:?}");
    // the ancestry + the effective-state statement, read directly
    let mut rows = db
        .read_conn()
        .query("SELECT branch_id, forked_at FROM branches", ())
        .await
        .unwrap();
    println!("branches:");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} forked_at={:?}",
            r.get::<String>(0).unwrap(),
            r.get::<Option<String>>(1).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT valid_from, valid_to, recorded_at FROM concepts WHERE id='x' AND branch_id='main'",
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "main row: [{}) rec={}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(2).unwrap()
        );
    }
    let cutoff: Option<String> = db
        .read_conn()
        .query("SELECT forked_at FROM branches WHERE branch_id='main'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    println!("main forked_at: {cutoff:?}");
    // the effective-state statement, verbatim, at the T5 instant with the
    // fork-time cap (the route's exact read for the takeover decision),
    // read in-process so the cap cannot lag a run
    let cap: String = db
        .read_conn()
        .query("SELECT forked_at FROM branches WHERE branch_id='b0'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    println!("cap = {cap}");
    let mut rows = db
        .read_conn()
        .query(
            "SELECT valid_from, valid_to FROM ( \
               SELECT valid_from, valid_to FROM concept_versions \
                WHERE id = ?1 AND branch_id = ?2 \
                  AND valid_from <= ?3 AND valid_to > ?3 \
                  AND recorded_at <= ?4 \
                UNION ALL \
               SELECT valid_from, valid_to FROM ( \
                 SELECT json_extract(j.payload, '$.valid_from') AS valid_from, \
                        json_extract(j.payload, '$.valid_to') AS valid_to, \
                        ROW_NUMBER() OVER ( \
                            PARTITION BY json_extract(j.payload, '$.valid_from') \
                            ORDER BY j.seq_id DESC \
                         ) AS rn \
                 FROM transaction_log j \
                 WHERE j.branch_id = ?2 AND j.entity_id = ?1 \
                   AND j.table_name = 'concepts' AND j.recorded_at <= ?4 \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM concepts w \
                      WHERE w.id = ?1 AND w.branch_id = ?2 \
                        AND w.valid_from = json_extract(j.payload, '$.valid_from') \
                        AND w.recorded_at <= ?4 \
                   ) \
               ) WHERE rn = 1 AND valid_from <= ?3 AND valid_to > ?3 \
             ) \
             ORDER BY valid_from DESC LIMIT 1",
            libsql::params!["x", "main", "2026-01-10T00:00:00.000000Z", cap.as_str()],
        )
        .await
        .unwrap();
    match rows.next().await.unwrap() {
        Some(r) => println!(
            "effective row: [{} {})",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap()
        ),
        None => println!("effective row: NONE"),
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT valid_from, valid_to FROM concepts \
              WHERE id = ?1 AND branch_id = ?2 AND valid_from <= ?3 AND valid_to > ?3 AND recorded_at <= ?4",
            libsql::params!["x", "main", "2026-01-10T00:00:00.000000Z", cap.as_str()],
        )
        .await
        .unwrap();
    match rows.next().await.unwrap() {
        Some(r) => println!(
            "concepts arm: [{} {})",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap()
        ),
        None => println!("concepts arm: NONE"),
    }
    for (tag, sql) in [
        ("id-branch", "SELECT COUNT(*) FROM concepts WHERE id='x' AND branch_id='main'"),
        ("valid", "SELECT COUNT(*) FROM concepts WHERE id='x' AND branch_id='main' AND valid_from <= '2026-01-10T00:00:00.000000Z'"),
        ("valid2", "SELECT COUNT(*) FROM concepts WHERE id='x' AND branch_id='main' AND valid_from <= '2026-01-10T00:00:00.000000Z' AND valid_to > '2026-01-10T00:00:00.000000Z'"),
        ("cap", "SELECT COUNT(*) FROM concepts WHERE id='x' AND branch_id='main' AND valid_from <= '2026-01-10T00:00:00.000000Z' AND valid_to > '2026-01-10T00:00:00.000000Z' AND recorded_at <= '2026-11-08T13:34:27.691689Z'"),
    ] {
        let mut rows = db.read_conn().query(sql, ()).await.unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        println!("{tag}: {n}");
    }
    dump(&db).await;
}

const OLD2: &str = "the trunk's belief";

async fn dump(db: &std::sync::Arc<Database>) {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT id, title, valid_from, valid_to, branch_id FROM concepts ORDER BY id",
            (),
        )
        .await
        .unwrap();
    println!("concepts:");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {:?} [{} {}) {}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap(),
            r.get::<String>(4).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT id, branch_id, valid_from, valid_to FROM concept_versions",
            (),
        )
        .await
        .unwrap();
    println!("versions:");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {} [{} {})",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query("SELECT id, branch_id, cut_at FROM concept_cuts", ())
        .await
        .unwrap();
    println!("cuts:");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {} @{}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap()
        );
    }
    let mut rows = db
        .read_conn()
        .query(
            "SELECT table_name, entity_id, operation, branch_id FROM transaction_log ORDER BY seq_id DESC LIMIT 6",
            (),
        )
        .await
        .unwrap();
    println!("log (newest 6):");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {} op={} on {}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap()
        );
    }
}
