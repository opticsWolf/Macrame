//! Probe for the two retire stories in `tests/m6b_law_tests.rs`: what do the
//! overlay's tables hold after the third act, and what does the branch read?
use macrame::prelude::*;
use macrame::BranchId;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const T5: &str = "2026-01-10T00:00:00.000000Z";
const AFTER: &str = "2026-01-11T00:00:00.000000Z";
const V9: &str = "2026-01-20T00:00:00.000000Z";
const V9_AFTER: &str = "2026-01-21T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const OLD: &str = "the trunk's belief";
const NEW: &str = "the branch's belief";

async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("m6b-law-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = std::sync::Arc::new(Database::open(dir.join("probe.db")).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("x", OLD)
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
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    view.upsert_concept(
        ConceptUpsert::new("x", NEW)
            .supersede(true)
            .valid_from(AFTER)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    println!("-- before the retire --");
    dump(&db).await;
    let got = view
        .upsert_concept(
            ConceptUpsert::new("x", NEW)
                .retired(true)
                .valid_from(V9)
                .valid_to(FOREVER),
        )
        .await;
    println!("retire -> {got:?}");
    tick().await;
    println!("-- after the retire --");
    dump(&db).await;

    let rows = db
        .concepts_by_id(
            &["x".to_string()],
            ReadPlan::new()
                .on(BranchId::new("b0").unwrap())
                .valid_at(V9_AFTER),
        )
        .await
        .unwrap();
    match rows.into_iter().next() {
        Some(c) => println!(
            "read at V9_AFTER -> title={:?} vt={:?}",
            c.title, c.valid_to
        ),
        None => println!("read at V9_AFTER -> absent"),
    }
}

async fn dump(db: &std::sync::Arc<Database>) {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT id, branch_id, valid_from, valid_to, title FROM concept_versions ORDER BY valid_from",
            (),
        )
        .await
        .unwrap();
    println!("versions:");
    while let Some(r) = rows.next().await.unwrap() {
        println!(
            "  {} {} [{} {}) {}",
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
            r.get::<String>(3).unwrap(),
            r.get::<String>(4).unwrap()
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
}
