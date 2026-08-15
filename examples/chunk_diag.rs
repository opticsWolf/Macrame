//! Diagnostic for D-058's open question: why are the edge and embedding chunk
//! paths superlinear in chunk size?
//!
//! Not a benchmark — a set of controlled comparisons printed as a table.
//! One experiment per run, because opening dozens of local databases in one
//! process trips R15 (`STATUS_ACCESS_VIOLATION`) roughly half the time:
//!
//! ```text
//! cargo run --release --example chunk_diag -- <e2e|table|chunk|fanout|plan|lc|fix|split|idx>
//! ```

use std::time::{Duration, Instant};

use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

const INSERT_LINK_SQL: &str = "INSERT INTO links \
     (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const UPSERT_LC: &str = "INSERT INTO links_current \
     (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
     ON CONFLICT(source_id, target_id, edge_type, valid_from) DO UPDATE SET \
         valid_to = excluded.valid_to, weight = excluded.weight, \
         properties = excluded.properties, recorded_at = excluded.recorded_at \
     WHERE excluded.recorded_at > links_current.recorded_at";

/// The guard's own predicate as an index. `idx_lc_traversal_cover` leads with
/// `source_id` and then `valid_from`, so it can bind only the first column of
/// this predicate and must scan the rest.
///
/// `valid_from` is in this index not because the predicate can seek on it — it
/// is a `<>` — but because leaving it out stops the index being **covering**,
/// and the planner then keeps preferring the wrong index. That was the first
/// version of this and it changed nothing.
const GUARD_INDEX: &str = "CREATE INDEX idx_lc_open_interval ON links_current \
     (source_id, target_id, edge_type, valid_to, valid_from)";

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

async fn fresh(dir: &tempfile::TempDir, name: &str) -> Database {
    Database::open_with_cadence(dir.path().join(name), None)
        .await
        .unwrap()
}

/// A raw second connection, configured like the actor's.
async fn raw_conn(dir: &tempfile::TempDir, name: &str) -> (libsql::Database, libsql::Connection) {
    let raw = libsql::Builder::new_local(dir.path().join(name))
        .build()
        .await
        .unwrap();
    let conn = raw.connect().unwrap();
    conn.execute("PRAGMA journal_mode = WAL", ()).await.ok();
    conn.execute("PRAGMA synchronous = NORMAL", ())
        .await
        .unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    (raw, conn)
}

async fn seed(db: &Database, n: usize) {
    let all: Vec<ConceptUpsert> = (0..n)
        .map(|i| ConceptUpsert::new(format!("c{i:07}"), format!("Concept {i}")).valid_from(TS))
        .collect();
    for c in all.chunks(600) {
        db.write_concepts(c.to_vec()).await.unwrap();
    }
}

/// `n` edges out of one hub.
fn hub_edges(from: usize, n: usize, ty: &str) -> Vec<EdgeAssertion> {
    (0..n)
        .map(|k| {
            EdgeAssertion::new("c0000000", format!("c{:07}", from + k), ty)
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect()
}

async fn insert_raw(conn: &libsql::Connection, n: usize, ty: &str) -> (Duration, Duration) {
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let stmt = tx.prepare(INSERT_LINK_SQL).await.unwrap();
    let t = Instant::now();
    for k in 0..n {
        stmt.reset();
        stmt.execute(libsql::params![
            "c0000000",
            format!("c{:07}", k + 1),
            ty,
            TS,
            OPEN,
            1.0f64,
            "{}",
            TS
        ])
        .await
        .unwrap();
    }
    let loop_time = t.elapsed();
    drop(stmt);
    let t = Instant::now();
    tx.commit().await.unwrap();
    (loop_time, t.elapsed())
}

async fn drop_triggers(conn: &libsql::Connection, names: &[&str]) {
    for t in names {
        conn.execute(&format!("DROP TRIGGER IF EXISTS {t}"), ())
            .await
            .unwrap();
    }
}

#[tokio::main]
async fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "e2e".into());
    let dir = tempfile::TempDir::new().unwrap();

    match which.as_str() {
        // 1,000 edges as one transaction vs as chunks. The sweep in
        // budgets.rs measured each chunk size into a *fresh* database, so it
        // cannot tell chunk size from table size. This can: both arms end with
        // the same 1,000-row table and only the boundaries differ.
        "e2e" => {
            println!("== 1,000 edges: one transaction vs chunked ==");
            let db = fresh(&dir, "a.db").await;
            seed(&db, 1_001).await;
            let t = Instant::now();
            db.write_bulk_atomic(hub_edges(1, 1_000, "CHUNK"))
                .await
                .unwrap();
            println!("  one 1000-row transaction : {:>8.2} ms", ms(t.elapsed()));
            db.close().await.unwrap();

            let db = fresh(&dir, "b.db").await;
            seed(&db, 1_001).await;
            let t = Instant::now();
            db.bulk_import(hub_edges(1, 1_000, "CHUNK")).await.unwrap();
            println!(
                "  bulk_import @ {:<4}       : {:>8.2} ms",
                chunk_rows::EDGES,
                ms(t.elapsed())
            );
            db.close().await.unwrap();
        }

        // Chunk size held constant, table size varied, per trigger config.
        //
        // 0.11.0 (Appendix C item 1) turned this into the attribution rig for
        // D-136's unexplained residual, which needed one more column. The drops
        // are **cumulative**, left to right, so each column subtracts exactly
        // one component and the difference between two neighbouring columns is
        // that component's cost. `none` is the bare `INSERT INTO links` — the
        // floor this path could ever reach — and without it "log only" was the
        // last column, leaving the log trigger and the base insert summed into
        // one figure that could not be split.
        //
        // What attribution reads is not a column but the *growth* down a
        // column: the residual is whatever gets more expensive as the table
        // fills, so a component that costs the same at 0 and at 8,000 is not
        // the answer however large it is.
        "table" => {
            println!("== fixed 90-row chunk vs table size ==");
            println!(
                "  {:>7}  {:>10}  {:>10}  {:>10}  {:>10}",
                "preload", "all three", "no guard", "log only", "none"
            );
            let mut baseline: Option<Vec<f64>> = None;
            for preload in [0usize, 2_000, 8_000] {
                let mut cells = Vec::new();
                for drops in [
                    vec![],
                    vec!["trg_links_single_open"],
                    vec!["trg_links_single_open", "trg_links_current_sync"],
                    vec![
                        "trg_links_single_open",
                        "trg_links_current_sync",
                        "trg_links_log_insert",
                    ],
                ] {
                    let name = format!("t{preload}_{}.db", drops.len());
                    let db = fresh(&dir, &name).await;
                    seed(&db, preload + 200).await;
                    for c in (0..preload).collect::<Vec<_>>().chunks(500) {
                        db.write_bulk_atomic(hub_edges(c[0] + 1, c.len(), "PRE"))
                            .await
                            .unwrap();
                    }
                    db.close().await.unwrap();
                    let (_raw, conn) = raw_conn(&dir, &name).await;
                    drop_triggers(&conn, &drops).await;
                    let (loop_t, commit_t) = insert_raw(&conn, 90, "MEASURED").await;
                    cells.push(ms(loop_t + commit_t));
                }
                println!(
                    "  {:>7}  {:>9.2}  {:>9.2}  {:>9.2}  {:>9.2}",
                    preload, cells[0], cells[1], cells[2], cells[3]
                );
                if baseline.is_none() {
                    baseline = Some(cells.clone());
                }
                if preload == 8_000 {
                    let z = baseline.as_ref().unwrap();
                    println!(
                        "\n  growth 0 -> 8,000 (ms) : {:>9.2}  {:>9.2}  {:>9.2}  {:>9.2}",
                        cells[0] - z[0],
                        cells[1] - z[1],
                        cells[2] - z[2],
                        cells[3] - z[3]
                    );
                    println!(
                        "  attributed to          : guard {:>6.2}   sync {:>6.2}   log {:>6.2}   insert {:>6.2}",
                        (cells[0] - z[0]) - (cells[1] - z[1]),
                        (cells[1] - z[1]) - (cells[2] - z[2]),
                        (cells[2] - z[2]) - (cells[3] - z[3]),
                        cells[3] - z[3]
                    );
                }
            }
        }

        // Chunk size varied, fresh database each time, per trigger config.
        "chunk" => {
            println!("== us/row by chunk size and configuration ==");
            println!(
                "  {:>6}  {:>10}  {:>10}  {:>10}  {:>13}",
                "n", "all three", "no guard", "log only", "noguard+cache"
            );
            for n in [90usize, 250, 500, 1_000] {
                let mut cells = Vec::new();
                for (drops, big_cache) in [
                    (vec![], false),
                    (vec!["trg_links_single_open"], false),
                    (
                        vec!["trg_links_single_open", "trg_links_current_sync"],
                        false,
                    ),
                    (vec!["trg_links_single_open"], true),
                ] {
                    let name = format!("c{n}_{}_{}.db", drops.len(), big_cache);
                    let db = fresh(&dir, &name).await;
                    seed(&db, n + 2).await;
                    db.close().await.unwrap();
                    let (_raw, conn) = raw_conn(&dir, &name).await;
                    if big_cache {
                        conn.execute("PRAGMA cache_size = -524288", ())
                            .await
                            .unwrap();
                    }
                    drop_triggers(&conn, &drops).await;
                    let (loop_t, commit_t) = insert_raw(&conn, n, "CHUNK").await;
                    cells.push(ms(loop_t + commit_t) * 1e3 / n as f64);
                }
                println!(
                    "  {:>6}  {:>9.1}  {:>9.1}  {:>9.1}  {:>12.1}",
                    n, cells[0], cells[1], cells[2], cells[3]
                );
            }
        }

        // Distinct source per edge. The guard scans rows sharing NEW.source_id,
        // so if the guard were the whole story this shape would not degrade.
        "fanout" => {
            println!("== us/row by chunk size, distinct source per edge ==");
            for n in [90usize, 500, 1_000] {
                let name = format!("f{n}.db");
                let db = fresh(&dir, &name).await;
                seed(&db, 2 * n + 4).await;
                let batch: Vec<EdgeAssertion> = (0..n)
                    .map(|k| {
                        EdgeAssertion::new(
                            format!("c{:07}", 2 * k),
                            format!("c{:07}", 2 * k + 1),
                            "CHUNK",
                        )
                        .valid_from(TS)
                        .valid_to(OPEN)
                    })
                    .collect();
                let t = Instant::now();
                db.write_bulk_atomic(batch).await.unwrap();
                let e = t.elapsed();
                println!(
                    "  n = {:>5} : {:>8.2} ms  ({:>6.1} us/row)",
                    n,
                    ms(e),
                    ms(e) * 1e3 / n as f64
                );
                db.close().await.unwrap();
            }
        }

        // What the guard's EXISTS actually does, per the planner.
        "plan" => {
            let db = fresh(&dir, "p.db").await;
            seed(&db, 2_002).await;
            db.bulk_import(hub_edges(1, 2_000, "CHUNK")).await.unwrap();
            db.close().await.unwrap();
            println!("== query plan, single-open guard's EXISTS ==");
            for (label, extra) in [("as shipped", false), ("with idx_lc_open_interval", true)] {
                // A *fresh* connection each time: a connection that has already
                // loaded the schema does not see a later CREATE INDEX, and the
                // first version of this printed the same plan twice for that
                // reason rather than because the index was ignored.
                let (_raw, conn) = raw_conn(&dir, "p.db").await;
                if extra {
                    conn.execute(GUARD_INDEX, ()).await.unwrap();
                }
                let mut rows = conn
                    .query(
                        "EXPLAIN QUERY PLAN SELECT 1 FROM links_current \
                           WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
                             AND valid_from <> ?4 AND valid_to = ?5",
                        libsql::params!["c0000000", "c0000001", "CHUNK", TS, OPEN],
                    )
                    .await
                    .unwrap();
                while let Some(row) = rows.next().await.unwrap() {
                    println!("  {label:<26} {}", row.get::<String>(3).unwrap());
                }
            }
        }

        // Inside `trg_links_current_sync`, and what it is paying for (0.11.0,
        // Appendix C item 1). `table` attributes the whole residual to this one
        // trigger, and `lc` shows the statement it runs is *not* expensive:
        // 90 direct upserts into an 8,000-row `links_current` cost ~0.5 ms
        // against the ~6 ms the trigger costs on the same table. So the cost is
        // in the environment the statement runs in, and this splits that.
        //
        // Guard and log trigger are dropped in every arm, so the only thing
        // being measured is the base insert plus the sync trigger. Each arm
        // removes one obligation from the upsert:
        //
        // * `fk off`   — the two `concepts` foreign keys on `links_current`,
        //   which `lc` had switched off and `table` had on. That difference is
        //   the first suspect precisely because it was never controlled for.
        // * `-cover` / `-open` — the two secondary indexes, dropped one at a
        //   time. Both lead on `source_id`, and this fixture gives every row
        //   the same one, so they are asked to maintain 8,000 entries under a
        //   single leading key.
        // * `-both`    — the floor.
        "sync" => {
            // Every arm is one connection, and the only difference between arms
            // is the SQL run on it between opening and measuring. That makes
            // the *preparation* a variable rather than a fixture detail, which
            // is what the first version of this experiment got wrong.
            //
            // `+cache` tests cache pressure, because the per-index arms came
            // out *super-additive*: dropping either index alone recovered ~80%
            // of the cost, and two indexes cannot cost eight times what one
            // costs unless something crosses a threshold between the two cases.
            // The actor sets no `cache_size` (`apply_pragmas` in
            // `connection.rs`), so the default ~2 MB is the threshold.
            //
            // `warm` and `ckpt` test the alternative reading of that same
            // super-additivity, and it is a reading about the *instrument*:
            // the three fast arms are exactly the three that ran a `DROP INDEX`
            // before measuring, and a drop is a write. `warm` performs a schema
            // write that touches no data — so if it reads fast, the indexes are
            // exonerated and what is being measured is the first substantial
            // write after opening a connection on a populated file. `ckpt`
            // separates the WAL from the page cache as the thing that state
            // lives in.
            const ARMS: [(&str, &[&str]); 8] = [
                ("as shipped", &[]),
                ("fk off", &["PRAGMA foreign_keys = OFF"]),
                ("-cover", &["DROP INDEX idx_lc_traversal_cover"]),
                ("-open", &["DROP INDEX idx_lc_open_interval"]),
                (
                    "-both",
                    &[
                        "DROP INDEX idx_lc_traversal_cover",
                        "DROP INDEX idx_lc_open_interval",
                    ],
                ),
                ("+cache", &["PRAGMA cache_size = -524288"]),
                (
                    "warm",
                    &["CREATE TABLE zz_warm (x INTEGER)", "DROP TABLE zz_warm"],
                ),
                ("ckpt", &["PRAGMA wal_checkpoint(TRUNCATE)"]),
            ];
            println!("== inside trg_links_current_sync, 90-row chunk ==");
            print!("  {:>7}", "preload");
            for (label, _) in ARMS {
                print!("  {label:>10}");
            }
            println!();
            for preload in [0usize, 8_000] {
                let mut cells = Vec::new();
                for (i, (_, prep)) in ARMS.iter().enumerate() {
                    let name = format!("y{preload}_{i}.db");
                    let db = fresh(&dir, &name).await;
                    seed(&db, preload + 200).await;
                    for c in (0..preload).collect::<Vec<_>>().chunks(500) {
                        db.write_bulk_atomic(hub_edges(c[0] + 1, c.len(), "PRE"))
                            .await
                            .unwrap();
                    }
                    db.close().await.unwrap();
                    let (_raw, conn) = raw_conn(&dir, &name).await;
                    drop_triggers(&conn, &["trg_links_single_open", "trg_links_log_insert"]).await;
                    for sql in *prep {
                        // `wal_checkpoint` returns rows, so it is a query.
                        conn.query(sql, ()).await.unwrap();
                    }
                    let (loop_t, commit_t) = insert_raw(&conn, 90, "MEASURED").await;
                    cells.push(ms(loop_t + commit_t));
                }
                print!("  {preload:>7}");
                for c in &cells {
                    print!("  {c:>10.2}");
                }
                println!();
            }
        }

        // Is the residual a property of the index, or of the fixture's key
        // distribution? (0.11.0, Appendix C item 1 → item 2.)
        //
        // `sync` attributes the growth to `links_current`'s two secondary
        // indexes: dropping them takes a 90-row chunk into an 8,000-edge table
        // from ~5 ms to ~1 ms, and the alternatives are refuted — a larger
        // cache does not help, and neither does a schema write or a checkpoint
        // before measuring, which rules out the "any write warms it" reading of
        // the same numbers.
        //
        // That leaves *why*. `idx_lc_traversal_cover` is
        // `(source_id, valid_from, valid_to, weight, edge_type, target_id)`,
        // and the star fixture gives all 8,000 rows the same source, the same
        // two timestamps and the same weight — four of them 27-character
        // strings. Every key comparison walks five identical columns before it
        // reaches the one that discriminates.
        //
        // So this holds *everything* constant except the key distribution:
        // both arms preload the same edge count into the same schema over the
        // same 8,200 concepts, and differ only in whether the edges share a
        // source. `chain` is D-088's shape, out-degree 1.
        "shape" => {
            println!("== 90-row chunk into an 8,000-edge table, by key distribution ==");
            println!(
                "  {:>18}  {:>12}  {:>12}  {:>10}",
                "preload shape", "as shipped", "-cover", "ratio"
            );
            for hub in [true, false] {
                let mut cells = Vec::new();
                for drop_cover in [false, true] {
                    let name = format!("h{hub}_{drop_cover}.db");
                    let db = fresh(&dir, &name).await;
                    seed(&db, 8_300).await;
                    for c in (0..8_000usize).collect::<Vec<_>>().chunks(500) {
                        let batch: Vec<EdgeAssertion> = c
                            .iter()
                            .map(|&k| {
                                let (s, t) = if hub {
                                    (0, k + 1)
                                } else {
                                    // A chain: one edge per source, so the
                                    // index's leading column discriminates on
                                    // the first comparison.
                                    (k, k + 1)
                                };
                                EdgeAssertion::new(format!("c{s:07}"), format!("c{t:07}"), "PRE")
                                    .valid_from(TS)
                                    .valid_to(OPEN)
                            })
                            .collect();
                        db.write_bulk_atomic(batch).await.unwrap();
                    }
                    db.close().await.unwrap();
                    let (_raw, conn) = raw_conn(&dir, &name).await;
                    if drop_cover {
                        conn.query("DROP INDEX idx_lc_traversal_cover", ())
                            .await
                            .unwrap();
                    }
                    // The measured chunk follows the same distribution as the
                    // table it lands in, because a hub chunk into a chain would
                    // be measuring a third thing.
                    let tx = conn
                        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                        .await
                        .unwrap();
                    let stmt = tx.prepare(INSERT_LINK_SQL).await.unwrap();
                    let t = Instant::now();
                    for k in 0..90usize {
                        let (s, t_id) = if hub {
                            (0, 8_100 + k)
                        } else {
                            (8_100 + k, 8_101 + k)
                        };
                        stmt.reset();
                        stmt.execute(libsql::params![
                            format!("c{s:07}"),
                            format!("c{t_id:07}"),
                            "MEASURED",
                            TS,
                            OPEN,
                            1.0f64,
                            "{}",
                            TS
                        ])
                        .await
                        .unwrap();
                    }
                    let loop_t = t.elapsed();
                    drop(stmt);
                    let t = Instant::now();
                    tx.commit().await.unwrap();
                    cells.push(ms(loop_t + t.elapsed()));
                }
                println!(
                    "  {:>18}  {:>11.2}  {:>11.2}  {:>9.2}x",
                    if hub {
                        "star (one source)"
                    } else {
                        "chain (8,000 srcs)"
                    },
                    cells[0],
                    cells[1],
                    cells[0] / cells[1]
                );
            }
        }

        // The sync trigger's upsert alone, no trigger machinery at all.
        "lc" => {
            println!("== 90 direct upserts into links_current vs its size ==");
            for preload in [0usize, 2_000, 8_000, 32_000] {
                let name = format!("l{preload}.db");
                let db = fresh(&dir, &name).await;
                seed(&db, 200).await;
                db.close().await.unwrap();
                let (_raw, conn) = raw_conn(&dir, &name).await;
                conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
                for (count, ty, measure) in [(preload, "PRE", false), (90, "MEASURED", true)] {
                    let tx = conn
                        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                        .await
                        .unwrap();
                    let stmt = tx.prepare(UPSERT_LC).await.unwrap();
                    let t = Instant::now();
                    for k in 0..count {
                        stmt.reset();
                        stmt.execute(libsql::params![
                            "c0000000",
                            format!("c{:07}", k + 1),
                            ty,
                            TS,
                            OPEN,
                            1.0f64,
                            "{}",
                            TS
                        ])
                        .await
                        .unwrap();
                    }
                    let e = t.elapsed();
                    drop(stmt);
                    tx.commit().await.unwrap();
                    if measure {
                        println!(
                            "  links_current has {preload:>6} rows -> 90 upserts : {:>8.2} ms",
                            ms(e)
                        );
                    }
                }
            }
        }

        // Proof by fix: an index matching the guard's predicate.
        "fix" => {
            println!("== guard cost vs table size, with a matching index ==");
            println!(
                "  {:>7}  {:>12}  {:>13}",
                "preload", "as shipped", "+guard index"
            );
            for preload in [0usize, 2_000, 8_000] {
                let mut cells = Vec::new();
                for add_index in [false, true] {
                    let name = format!("x{preload}_{add_index}.db");
                    let db = fresh(&dir, &name).await;
                    seed(&db, preload + 200).await;
                    if add_index {
                        let (_raw, conn) = raw_conn(&dir, &name).await;
                        conn.execute(GUARD_INDEX, ()).await.unwrap();
                    }
                    for c in (0..preload).collect::<Vec<_>>().chunks(500) {
                        db.write_bulk_atomic(hub_edges(c[0] + 1, c.len(), "PRE"))
                            .await
                            .unwrap();
                    }
                    let t = Instant::now();
                    db.write_bulk_atomic(hub_edges(1, 90, "MEASURED"))
                        .await
                        .unwrap();
                    cells.push(ms(t.elapsed()));
                    db.close().await.unwrap();
                }
                println!("  {:>7}  {:>11.2}  {:>12.2}", preload, cells[0], cells[1]);
            }
        }

        // Where the residual cost lands: in the statements or in the commit?
        "split" => {
            println!("== loop vs commit, guard dropped ==");
            for n in [90usize, 1_000] {
                let name = format!("s{n}.db");
                let db = fresh(&dir, &name).await;
                seed(&db, n + 2).await;
                db.close().await.unwrap();
                let (_raw, conn) = raw_conn(&dir, &name).await;
                drop_triggers(&conn, &["trg_links_single_open"]).await;
                let (loop_t, commit_t) = insert_raw(&conn, n, "CHUNK").await;
                println!(
                    "  n = {:>5} : loop {:>8.2} ms ({:>5.1} us/row), commit {:>7.2} ms",
                    n,
                    ms(loop_t),
                    ms(loop_t) * 1e3 / n as f64,
                    ms(commit_t)
                );
            }
        }

        // Is the residual index maintenance on links_current? Drop its two
        // secondary indexes and re-measure the chunk-size shape.
        "idx" => {
            println!("== us/row by chunk size, guard dropped, links_current indexes varied ==");
            println!("  {:>6}  {:>12}  {:>14}", "n", "3 indexes", "1 index (PK)");
            for n in [90usize, 500, 1_000] {
                let mut cells = Vec::new();
                for drop_idx in [false, true] {
                    let name = format!("i{n}_{drop_idx}.db");
                    let db = fresh(&dir, &name).await;
                    seed(&db, n + 2).await;
                    db.close().await.unwrap();
                    let (_raw, conn) = raw_conn(&dir, &name).await;
                    drop_triggers(&conn, &["trg_links_single_open"]).await;
                    if drop_idx {
                        // idx_lc_tgt_active was the third here until v8 dropped
                        // it (D-118); it never had a reader to disturb.
                        for i in ["idx_lc_traversal_cover"] {
                            conn.execute(&format!("DROP INDEX IF EXISTS {i}"), ())
                                .await
                                .unwrap();
                        }
                    }
                    let (loop_t, commit_t) = insert_raw(&conn, n, "CHUNK").await;
                    cells.push(ms(loop_t + commit_t) * 1e3 / n as f64);
                }
                println!("  {:>6}  {:>11.1}  {:>13.1}", n, cells[0], cells[1]);
            }
        }

        // The other superlinear path. Same question: is the cost a function of
        // the chunk, or of the corpus the chunk lands in?
        "embed" => {
            let vecs = |from: usize, n: usize| -> Vec<(String, Vec<f32>)> {
                (from..from + n)
                    .map(|i| {
                        let t = i as f32 / 500.0;
                        (
                            format!("c{i:07}"),
                            (0..8).map(|k| ((t + k as f32) * 0.37).sin()).collect(),
                        )
                    })
                    .collect()
            };

            println!("== fixed 30-vector chunk vs corpus size ==");
            for preload in [0usize, 2_000, 8_000] {
                let name = format!("v{preload}.db");
                let db = fresh(&dir, &name).await;
                seed(&db, preload + 100).await;
                let m = ModelName::new("diag_v1").unwrap();
                db.register_model(&m, 8).await.unwrap();
                if preload > 0 {
                    db.upsert_embeddings(&m, vecs(0, preload)).await.unwrap();
                }
                let t = Instant::now();
                db.upsert_embeddings(&m, vecs(preload, 30)).await.unwrap();
                println!(
                    "  corpus {:>6} -> 30 vectors : {:>8.2} ms ({:>6.1} us/row)",
                    preload,
                    ms(t.elapsed()),
                    ms(t.elapsed()) * 1e3 / 30.0
                );
                db.close().await.unwrap();
            }

            println!("\n== one chunk of n vectors into an empty corpus ==");
            for n in [30usize, 250, 1_000] {
                let name = format!("w{n}.db");
                let db = fresh(&dir, &name).await;
                seed(&db, n + 2).await;
                let m = ModelName::new("diag_v1").unwrap();
                db.register_model(&m, 8).await.unwrap();
                // One transaction of n vectors, bypassing the chunking loop —
                // the chunk function itself is crate-private, so this restates
                // its statement the way the trigger diagnostic in budgets.rs
                // restates INSERT_LINK.
                let (_raw, conn) = raw_conn(&dir, &name).await;
                let sql = format!(
                    "INSERT INTO {t} (concept_id, embedding) VALUES (?1, ?2) \
                     ON CONFLICT(concept_id) DO UPDATE SET embedding = excluded.embedding",
                    t = m.table()
                );
                let rows = vecs(0, n);
                let tx = conn
                    .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                    .await
                    .unwrap();
                let stmt = tx.prepare(&sql).await.unwrap();
                let t = Instant::now();
                for (id, v) in &rows {
                    let blob = EmbeddingCodec::encode(v, 8, m.as_str()).unwrap();
                    stmt.reset();
                    stmt.execute(libsql::params![id.as_str(), blob])
                        .await
                        .unwrap();
                }
                drop(stmt);
                tx.commit().await.unwrap();
                let e = t.elapsed();
                println!(
                    "  n = {:>5} : {:>8.2} ms  ({:>6.1} us/row)",
                    n,
                    ms(e),
                    ms(e) * 1e3 / n as f64
                );
                drop(conn);
                db.close().await.unwrap();
            }
        }

        other => println!("unknown experiment: {other}"),
    }
}
