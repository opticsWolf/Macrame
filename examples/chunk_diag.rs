//! Scratch diagnostic for D-058's open question: why are the edge and embedding
//! chunk paths superlinear in chunk size?
//!
//! Not a benchmark — a set of controlled comparisons printed as a table, run
//! with `cargo run --release --example chunk_diag`.

use std::time::{Duration, Instant};

use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

async fn fresh(dir: &tempfile::TempDir, name: &str) -> Database {
    Database::open_with_cadence(dir.path().join(name), None)
        .await
        .unwrap()
}

fn concepts(range: std::ops::Range<usize>) -> Vec<ConceptUpsert> {
    range
        .map(|i| {
            ConceptUpsert::new(format!("c{i:07}"), format!("Concept {i}")).valid_from(TS)
        })
        .collect()
}

/// `n` edges out of one hub, targets `from..from+n`.
fn edges(from: usize, n: usize) -> Vec<EdgeAssertion> {
    (0..n)
        .map(|k| {
            EdgeAssertion::new("c0000000", format!("c{:07}", from + k), "CHUNK")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect()
}

async fn seed(db: &Database, n: usize) {
    for c in concepts(0..n).chunks(600) {
        db.write_annotations(c.to_vec()).await.unwrap();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();

    // ---------------------------------------------------------------------
    // 1. End to end: 1,000 edges as one transaction vs as chunks of 90.
    //
    // The sweep measured each chunk size into a *fresh* database, so it cannot
    // tell chunk size from table size. This can: both arms end with the same
    // 1,000-row table, and only the transaction boundaries differ.
    // ---------------------------------------------------------------------
    println!("== 1. 1,000 edges, one transaction vs eleven chunks ==");
    {
        let db = fresh(&dir, "e2e_atomic.db").await;
        seed(&db, 1_001).await;
        let t = Instant::now();
        db.write_bulk_atomic(edges(1, 1_000)).await.unwrap();
        println!("  one 1000-row transaction : {:>8.2} ms", ms(t.elapsed()));
        db.close().await.unwrap();
    }
    {
        let db = fresh(&dir, "e2e_chunked.db").await;
        seed(&db, 1_001).await;
        let t = Instant::now();
        // bulk_import chunks at chunk_rows::EDGES internally.
        db.bulk_import(edges(1, 1_000)).await.unwrap();
        println!(
            "  bulk_import @ {:<4}        : {:>8.2} ms",
            chunk_rows::EDGES,
            ms(t.elapsed())
        );
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 2. Chunk size held constant, table size varied.
    //
    // If a 90-row chunk costs the same into an empty table and into a
    // 90,000-row one, the effect is chunk size. If it grows, the effect is
    // table size and the sweep was measuring the wrong variable.
    // ---------------------------------------------------------------------
    println!("\n== 2. fixed 90-row chunk into a table of N existing edges ==");
    for preload in [0usize, 900, 9_000, 90_000] {
        let db = fresh(&dir, &format!("tbl_{preload}.db")).await;
        seed(&db, preload + 200).await;
        if preload > 0 {
            // Distinct edge_type per preload batch so the hub's (source,
            // target, type) keys stay unique and the single-open guard is not
            // what we are measuring.
            for chunk in (0..preload).collect::<Vec<_>>().chunks(90) {
                let batch: Vec<EdgeAssertion> = chunk
                    .iter()
                    .map(|k| {
                        EdgeAssertion::new("c0000000", format!("c{:07}", k + 1), "PRE")
                            .valid_from(TS)
                            .valid_to(OPEN)
                    })
                    .collect();
                db.write_bulk_atomic(batch).await.unwrap();
            }
        }
        // The measured chunk: 90 fresh edges under a type nothing else uses.
        let batch: Vec<EdgeAssertion> = (0..90)
            .map(|k| {
                EdgeAssertion::new("c0000000", format!("c{:07}", k + 1), "MEASURED")
                    .valid_from(TS)
                    .valid_to(OPEN)
            })
            .collect();
        let t = Instant::now();
        db.write_bulk_atomic(batch).await.unwrap();
        println!(
            "  preload {:>6} edges -> 90-row chunk : {:>8.2} ms",
            preload,
            ms(t.elapsed())
        );
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 3. The sweep again, but every point into the *same* database, so the
    //    table grows across points. Compared against the fresh-database sweep
    //    this separates the two variables from the other side.
    // ---------------------------------------------------------------------
    println!("\n== 3. chunk sizes into a fresh database each time (repeat of the sweep) ==");
    for n in [10usize, 50, 100, 500, 1_000] {
        let db = fresh(&dir, &format!("sweep_{n}.db")).await;
        seed(&db, n + 2).await;
        let t = Instant::now();
        db.write_bulk_atomic(edges(1, n)).await.unwrap();
        let e = t.elapsed();
        println!(
            "  n = {:>5} : {:>8.2} ms  ({:>6.1} us/row)",
            n,
            ms(e),
            ms(e) * 1e3 / n as f64
        );
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 4. Page cache. If the shape is dirty pages spilling to the WAL, a large
    //    cache_size should flatten it. Default is 2,000 pages (~8 MB).
    // ---------------------------------------------------------------------
    println!("\n== 4. n = 1000 under a 512 MB page cache ==");
    {
        let db = fresh(&dir, "cache_big.db").await;
        seed(&db, 1_002).await;
        // The write connection lives in the actor; this reaches it through the
        // same pragma surface the actor's connection was configured with.
        // cache_size is per-connection, so this is applied to a raw handle and
        // the insert issued there, mirroring the diagnostic in budgets.rs.
        let raw = libsql::Builder::new_local(dir.path().join("cache_big.db"))
            .build()
            .await
            .unwrap();
        let conn = raw.connect().unwrap();
        conn.execute("PRAGMA journal_mode = WAL", ()).await.ok();
        conn.execute("PRAGMA synchronous = NORMAL", ()).await.unwrap();
        conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
        conn.execute("PRAGMA cache_size = -524288", ()).await.unwrap();
        let t = Instant::now();
        insert_raw(&conn, 1, 1_000).await;
        println!("  1000 rows, 512 MB cache  : {:>8.2} ms", ms(t.elapsed()));
        drop(conn);
        db.close().await.unwrap();
    }
    println!("\n== 5. n = 1000, triggers dropped, same raw path (control) ==");
    {
        let db = fresh(&dir, "no_trig.db").await;
        seed(&db, 1_002).await;
        let raw = libsql::Builder::new_local(dir.path().join("no_trig.db"))
            .build()
            .await
            .unwrap();
        let conn = raw.connect().unwrap();
        conn.execute("PRAGMA journal_mode = WAL", ()).await.ok();
        conn.execute("PRAGMA synchronous = NORMAL", ()).await.unwrap();
        for t in ["trg_links_log_insert", "trg_links_current_sync"] {
            conn.execute(&format!("DROP TRIGGER IF EXISTS {t}"), ())
                .await
                .unwrap();
        }
        let t = Instant::now();
        insert_raw(&conn, 1, 1_000).await;
        println!("  1000 rows, no triggers   : {:>8.2} ms", ms(t.elapsed()));
        drop(conn);
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 6. Which trigger. Same 1,000 rows with one trigger at a time.
    // ---------------------------------------------------------------------
    println!("\n== 6. n = 1000, one trigger at a time ==");
    for (label, drops) in [
        ("log only      ", vec!["trg_links_current_sync"]),
        ("current only  ", vec!["trg_links_log_insert"]),
        ("both (default)", vec![]),
    ] {
        let name = format!("one_{}.db", label.trim().replace(' ', "_"));
        let db = fresh(&dir, &name).await;
        seed(&db, 1_002).await;
        let raw = libsql::Builder::new_local(dir.path().join(&name))
            .build()
            .await
            .unwrap();
        let conn = raw.connect().unwrap();
        conn.execute("PRAGMA journal_mode = WAL", ()).await.ok();
        conn.execute("PRAGMA synchronous = NORMAL", ()).await.unwrap();
        for t in &drops {
            conn.execute(&format!("DROP TRIGGER IF EXISTS {t}"), ())
                .await
                .unwrap();
        }
        let t = Instant::now();
        insert_raw(&conn, 1, 1_000).await;
        println!("  {label} : {:>8.2} ms", ms(t.elapsed()));
        drop(conn);
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 7. Experiment 6 cannot separate the two triggers, and this is why:
    //    dropping trg_links_current_sync leaves links_current EMPTY, which
    //    also makes trg_links_single_open's EXISTS free. The cheap arm was
    //    cheap for two reasons and 6 credits it to one.
    //
    //    So: keep the sync trigger (links_current fills up as it should) and
    //    drop the *guard* instead.
    // ---------------------------------------------------------------------
    println!("\n== 7. n = 1000, links_current populated either way ==");
    for (label, drops) in [
        ("guard dropped ", vec!["trg_links_single_open"]),
        ("all three     ", vec![]),
    ] {
        let name = format!("guard_{}.db", label.trim().replace(' ', "_"));
        let db = fresh(&dir, &name).await;
        seed(&db, 1_002).await;
        let raw = libsql::Builder::new_local(dir.path().join(&name))
            .build()
            .await
            .unwrap();
        let conn = raw.connect().unwrap();
        conn.execute("PRAGMA journal_mode = WAL", ()).await.ok();
        conn.execute("PRAGMA synchronous = NORMAL", ()).await.unwrap();
        for t in &drops {
            conn.execute(&format!("DROP TRIGGER IF EXISTS {t}"), ())
                .await
                .unwrap();
        }
        let t = Instant::now();
        insert_raw(&conn, 1, 1_000).await;
        println!("  {label} : {:>8.2} ms", ms(t.elapsed()));
        drop(conn);
        db.close().await.unwrap();
    }

    // ---------------------------------------------------------------------
    // 8. What the guard's EXISTS actually does, per the planner.
    // ---------------------------------------------------------------------
    println!("\n== 8. query plan for the single-open guard's EXISTS ==");
    {
        let db = fresh(&dir, "plan.db").await;
        seed(&db, 2_002).await;
        db.bulk_import(edges(1, 2_000)).await.unwrap();
        let mut rows = db
            .read_conn()
            .query(
                "EXPLAIN QUERY PLAN SELECT 1 FROM links_current \
                   WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
                     AND valid_from <> ?4 AND valid_to = ?5",
                libsql::params!["c0000000", "c0000001", "CHUNK", TS, OPEN],
            )
            .await
            .unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            println!("  {}", row.get::<String>(3).unwrap());
        }
        db.close().await.unwrap();
    }
}

const INSERT_LINK_SQL: &str = "INSERT INTO links \
     (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

async fn insert_raw(conn: &libsql::Connection, from: usize, n: usize) {
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let stmt = tx.prepare(INSERT_LINK_SQL).await.unwrap();
    for k in 0..n {
        stmt.reset();
        stmt.execute(libsql::params![
            "c0000000",
            format!("c{:07}", from + k),
            "CHUNK",
            TS,
            OPEN,
            1.0f64,
            "{}",
            TS
        ])
        .await
        .unwrap();
    }
    drop(stmt);
    tx.commit().await.unwrap();
}
