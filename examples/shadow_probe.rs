//! T1.2, step 0: does the swap work at all on libSQL 0.9.30?
//!
//! The plan states three mechanical facts, all probed on plain SQLite via
//! `node:sqlite`. This project does not carry another engine's numbers or
//! another engine's semantics, and D-078 is the precedent: the last time a
//! probe from a different engine was taken on trust, it was wrong in the
//! opposite direction from the one assumed.
//!
//! The three:
//!
//!   1. `ALTER TABLE … RENAME` reparses the whole schema (SQLite ≥ 3.25), so a
//!      rename fails while a trigger references the table being replaced.
//!   2. `DROP TRIGGER` → `DROP TABLE` → `RENAME` → recreate triggers works.
//!   3. DDL is transactional, so the whole swap can roll back.
//!
//! And a fourth the plan does not raise, which decides the design: after
//! `DROP TABLE links_current`, are its index **names** free for the swap
//! transaction to reuse? If they are, the shadow can be built unindexed and the
//! canonical indexes created inside the swap — which is what makes the
//! projection chunkable without the index names drifting from `CREATE_INDEXES`.
//!
//! Run with:  cargo run --release --example shadow_probe

use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

async fn report(label: &str, r: Result<u64, libsql::Error>) {
    match r {
        Ok(_) => println!("  {label:<52} OK"),
        Err(e) => println!("  {label:<52} ERR: {e}"),
    }
}

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("p.db");
    let db = Database::open_with_cadence(&path, None).await.unwrap();

    db.write_concepts(
        (0..50)
            .map(|i| ConceptUpsert::new(format!("c{i:03}"), "n").valid_from(TS))
            .collect(),
    )
    .await
    .unwrap();
    db.bulk_import(
        (0..49)
            .map(|i| {
                EdgeAssertion::new(format!("c{i:03}"), format!("c{:03}", i + 1), "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
            })
            .collect(),
    )
    .await
    .unwrap();

    let conn = db.raw().connect().unwrap();

    // ---- 1. the naive rename, with the triggers still in place ----
    println!("\n1. rename with triggers present (expected to fail):");
    conn.execute(
        "CREATE TABLE links_current_shadow AS SELECT * FROM links_current",
        (),
    )
    .await
    .unwrap();
    report(
        "DROP TABLE links_current",
        conn.execute("DROP TABLE links_current", ()).await,
    )
    .await;
    report(
        "ALTER TABLE links_current_shadow RENAME TO links_current",
        conn.execute(
            "ALTER TABLE links_current_shadow RENAME TO links_current",
            (),
        )
        .await,
    )
    .await;

    // Whatever state that left, start again from a clean file.
    db.close().await.unwrap();
    let dir2 = tempfile::TempDir::new().unwrap();
    let path2 = dir2.path().join("q.db");
    let db = Database::open_with_cadence(&path2, None).await.unwrap();
    db.write_concepts(
        (0..50)
            .map(|i| ConceptUpsert::new(format!("c{i:03}"), "n").valid_from(TS))
            .collect(),
    )
    .await
    .unwrap();
    db.bulk_import(
        (0..49)
            .map(|i| {
                EdgeAssertion::new(format!("c{i:03}"), format!("c{:03}", i + 1), "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
            })
            .collect(),
    )
    .await
    .unwrap();
    let conn = db.raw().connect().unwrap();

    // ---- 2 & 3 & 4. the full swap, inside one transaction ----
    println!("\n2. drop triggers -> drop table -> rename -> index -> recreate,");
    println!("   all inside one transaction:");
    conn.execute(
        "CREATE TABLE links_current_shadow AS SELECT * FROM links_current",
        (),
    )
    .await
    .unwrap();

    report("BEGIN IMMEDIATE", conn.execute("BEGIN IMMEDIATE", ()).await).await;
    for stmt in [
        "DROP TRIGGER trg_links_current_sync",
        "DROP TRIGGER trg_links_single_open",
        "DROP TABLE links_current",
        "ALTER TABLE links_current_shadow RENAME TO links_current",
        // The fourth question: the old table's index names, immediately reused.
        "CREATE INDEX idx_lc_traversal_cover ON links_current \
         (source_id, valid_from, valid_to, weight, edge_type, target_id)",
        "CREATE INDEX idx_lc_tgt_active ON links_current (target_id, valid_to)",
        "CREATE INDEX idx_lc_open_interval ON links_current \
         (source_id, target_id, edge_type, valid_to, valid_from)",
    ] {
        report(&stmt.chars().take(52).collect::<String>(), conn.execute(stmt, ()).await).await;
    }

    // The triggers come back from the crate's own DDL, so the recreated bodies
    // cannot drift from the declared ones.
    for trigger in macrame::schema::ddl::CREATE_TRIGGERS {
        if trigger.contains("links_current_sync") || trigger.contains("links_single_open") {
            report(
                trigger
                    .split_whitespace()
                    .nth(4)
                    .unwrap_or("trigger")
                    .to_string()
                    .as_str(),
                conn.execute(trigger, ()).await,
            )
            .await;
        }
    }
    report("COMMIT", conn.execute("COMMIT", ()).await).await;

    // ---- did it survive? ----
    println!("\n3. after the swap:");
    let n: i64 = conn
        .query("SELECT COUNT(*) FROM links_current", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    println!("  links_current holds {n} rows");

    // The triggers must be live again, or the next write silently stops
    // maintaining the projection.
    db.assert_edge(
        EdgeAssertion::new("c000", "c010", "LINKS")
            .valid_from(TS)
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    let n2: i64 = conn
        .query("SELECT COUNT(*) FROM links_current", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    println!("  after one assert_edge: {n2} rows (trigger is {})",
        if n2 == n + 1 { "live" } else { "DEAD" });

    println!(
        "  audit_current drift: {}",
        macrame::integrity::audit_current(db.read_conn())
            .await
            .map_or_else(|e| format!("{e}"), |d| d.to_string())
    );

    db.close().await.unwrap();
}
