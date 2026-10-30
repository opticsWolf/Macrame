//! 0.20.0: what each index on the edge write path costs a 90-row chunk.
//!
//! The 0.20.0 release bench read `chunk_budget/edges/90` at **6.06 ms**
//! against a ≤ 3 ms budget, where 0.10.0 published 2.39–2.71 ms. A bisect by
//! release found no single defect: 0.12.6 ([D-151], two indexes on `links`)
//! took the empty-table chunk from 2.53 to 3.81 ms, and the 0.15.x index
//! rungs took it the rest of the way. Each index was accepted for a reader
//! and none was priced on the write, which is what this probe does.
//!
//! One arm per index: a fresh file, the index dropped, the same 90-edge
//! `write_bulk_atomic` the bench times, median of [`SAMPLES`] files. The
//! `baseline` arm drops nothing. A dropped index's saving is the price every
//! edge write pays for it.
//!
//!   cargo run --release --example edge_chunk_index_cost_probe
//!
//! [D-151]: ../docs/architecture/s13-decision-register.md#d-151

use std::time::{Duration, Instant};

use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const ROWS: usize = 90;
const SAMPLES: usize = 21;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// One timed chunk on a fresh file with `drop` removed first.
async fn one(dir: &std::path::Path, n: usize, drop: Option<&str>) -> Duration {
    let path = dir.join(format!("s{n}.db"));
    let _ = std::fs::remove_file(&path);
    let db = Database::open_with_cadence(&path, None).await.unwrap();
    if let Some(names) = drop {
        let conn = db.raw().connect().unwrap();
        for name in names.split(',') {
            conn.execute(&format!("DROP INDEX {name}"), ()).await.unwrap();
        }
    }
    for i in 0..=ROWS {
        db.upsert_concept(ConceptUpsert::new(format!("c{i:07}"), "c").valid_from(TS))
            .await
            .unwrap();
    }
    let edges: Vec<EdgeAssertion> = (0..ROWS)
        .map(|k| {
            EdgeAssertion::new("c0000000", format!("c{:07}", k + 1), "CHUNK")
                .valid_from(TS)
                .valid_to(OPEN)
        })
        .collect();
    let t = Instant::now();
    db.write_bulk_atomic(edges).await.unwrap();
    let elapsed = t.elapsed();
    db.close().await.unwrap();
    elapsed
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("edge_chunk_cost_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // The indexes on the three tables an edge write touches, as the schema
    // declares them today.
    let names: Vec<String> = {
        let db = Database::open_with_cadence(&dir.join("list.db"), None).await.unwrap();
        let conn = db.raw().connect().unwrap();
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL \
                 AND tbl_name IN ('links', 'links_current', 'transaction_log') ORDER BY tbl_name, name",
                (),
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            out.push(row.get::<String>(0).unwrap());
        }
        db.close().await.unwrap();
        out
    };

    println!("edge_chunk_index_cost_probe — {ROWS}-edge write_bulk_atomic, median of {SAMPLES} fresh files\n");
    // One arm per process when names are given (`baseline` for none, or a
    // comma-separated set): a long run of fresh files in one process is what
    // R15 needs (D-148), and the full sweep met it four arms in.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        for name in &names {
            println!("{name}");
        }
        return;
    }
    let mut arms: Vec<Option<String>> = vec![None];
    if args.is_empty() {
        arms.extend(names.into_iter().map(Some));
    } else {
        arms.extend(args.into_iter().filter(|a| a != "baseline").map(Some));
    }

    let mut n = 0;
    let mut base = 0.0;
    for arm in &arms {
        let mut v = Vec::with_capacity(SAMPLES + 1);
        for i in 0..=SAMPLES {
            n += 1;
            let d = one(&dir, n, arm.as_deref()).await;
            if i > 0 {
                v.push(d);
            }
        }
        let ms = median(v).as_secs_f64() * 1000.0;
        match arm {
            None => {
                base = ms;
                println!("    {:<32} {:>8.3} ms", "baseline (all indexes)", ms);
            }
            Some(name) => println!(
                "    {:<32} {:>8.3} ms   saves {:>6.3} ms",
                format!("without {name}"),
                ms,
                base - ms
            ),
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
