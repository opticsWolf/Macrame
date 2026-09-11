//! Plan §9.1/§9.3: the vector build dissection — where does the build time live?
//!
//! ```text
//! cargo run --release --example vector_build_probe -- --dim 256 --n 2000 --sessions 3
//! ```
//!
//! Four arms, fresh file each, `register_model` seeds concepts + table:
//!
//! 1. **incremental** — the shipped path: table + DiskANN index, per-chunk
//!    upsert (`chunk_rows::EMBEDDINGS`).
//! 2. **incremental + WAL recipe** — same, opened at
//!    `wal_autocheckpoint = 10_000` (the D-275 recipe, never measured on the
//!    vector path).
//! 3. **drop → blob-only → full build** — the index dropped behind
//!    registration, all rows inserted without index maintenance, then one
//!    `CREATE INDEX` (a one-pass DiskANN build). Times the blob inserts and
//!    the build separately. The index is *always* recreated — it is
//!    load-bearing for correctness (ddl.rs `create_embeddings_index`), so no
//!    arm leaves a file without it.
//! 4. **query dissect** on the built file — `search_vector`'s full statement
//!    against bare `vector_top_k`, against the statement without the
//!    `concepts` join, to say where §9.3's 16.4 ms lives.

use macrame::prelude::*;
use macrame::vector::search_vector;
use std::time::{Duration, Instant};

const TS: &str = "2025-01-01T00:00:00.000000Z";
const MODEL: &str = "probe";

#[derive(Clone)]
struct Args {
    dim: usize,
    n: usize,
    sessions: usize,
}

fn parse_args() -> Args {
    let mut a = Args { dim: 256, n: 2000, sessions: 3 };
    let argv = std::env::args().collect::<Vec<_>>();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--dim" => { a.dim = argv[i + 1].parse().unwrap(); i += 2; }
            "--n" => { a.n = argv[i + 1].parse().unwrap(); i += 2; }
            "--sessions" => { a.sessions = argv[i + 1].parse().unwrap(); i += 2; }
            other => panic!("unknown arg {other}"),
        }
    }
    a
}

fn corpus(n: usize, dim: usize, seed: u64) -> Vec<(String, Vec<f32>)> {
    // xorshift; a seeded RNG keeps every arm's data identical.
    let mut s = seed | 1;
    let mut rng = move || {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17; s as f64 / u64::MAX as f64
    };
    (0..n)
        .map(|i| {
            (
                format!("c{i:06}"),
                (0..dim).map(|_| (rng() - 0.5) as f32).collect(),
            )
        })
        .collect()
}

async fn open_default(path: &std::path::Path) -> Database {
    Database::open(path).await.unwrap()
}

async fn open_wal_recipe(path: &std::path::Path) -> Database {
    let tuning = Tuning::default()
        .wal_autocheckpoint(WalCheckpointPolicy::EveryPages(10_000));
    Database::open_tuned(path, tuning).await.unwrap()
}

/// A fresh read-write connection on the actor's file, for the DDL arms.
/// Between actor commands the writer holds no transaction, so DDL from a
/// second connection lands (WAL mode). `connect()` is sync in libsql.
fn rw(db: &Database) -> libsql::Connection {
    db.raw().connect().unwrap()
}

struct ChunkTiming {
    wall: Duration,
    holds: Vec<(usize, Duration)>,
}

async fn upsert_all(db: &Database, rows: &[(String, Vec<f32>)]) -> ChunkTiming {
    let holds: std::sync::Arc<std::sync::Mutex<Vec<(usize, Duration)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = holds.clone();
    let t0 = Instant::now();
    let control = BulkControl::new().on_progress(move |p| {
        sink.lock().unwrap().push((p.rows, p.held));
    });
    let n = db
        .upsert_embeddings_with(&ModelName::new(MODEL).unwrap(), rows.to_vec(), control)
        .await
        .unwrap();
    assert_eq!(n, rows.len());
    let holds = std::sync::Arc::try_unwrap(holds).ok().unwrap().into_inner().unwrap();
    ChunkTiming { wall: t0.elapsed(), holds }
}

fn hold_profile(t: &ChunkTiming) -> (f64, Vec<f64>, Vec<f64>) {
    let per_row: Vec<f64> = t
        .holds
        .iter()
        .map(|(r, h)| h.as_secs_f64() * 1e6 / *r as f64)
        .collect();
    let head = per_row.iter().take(3).cloned().collect();
    let tail = per_row.iter().rev().take(3).rev().cloned().collect();
    (t.wall.as_secs_f64(), head, tail)
}

async fn wal_mb(path: &std::path::Path) -> f64 {
    let wal = path.with_extension("db-wal");
    if wal.exists() { wal.metadata().unwrap().len() as f64 / 1e6 } else { 0.0 }
}

async fn arm_incremental(args: &Args, recipe: bool, dir: &std::path::Path) -> String {
    let name = if recipe { "inc_wal" } else { "incremental" };
    let path = dir.join(format!("vbp_{}_d{}_n{}.db", name, args.dim, args.n));
    let _ = std::fs::remove_file(&path);
    let db = if recipe { open_wal_recipe(&path).await } else { open_default(&path).await };
    seed_concepts(&db, args.n).await;
    db.register_model(&ModelName::new(MODEL).unwrap(), args.dim).await.unwrap();
    let rows = corpus(args.n, args.dim, 1);

    let t = upsert_all(&db, &rows).await;
    let (wall, head, tail) = hold_profile(&t);
    let w = wal_mb(&path).await;
    db.close().await.unwrap();
    format!("{name:<14} insert={wall:7.2}s  holds first={head:?}  last={tail:?}  wal={w:6.1}MB")
}

async fn seed_concepts(db: &Database, n: usize) {
    let t0 = Instant::now();
    let concepts: Vec<_> = (0..n)
        .map(|i| ConceptUpsert::new(format!("c{i:06}"), "text").valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();
    println!("           concepts n={} in {:.3}s", n, t0.elapsed().as_secs_f64());
}

async fn arm_full_build(args: &Args, dir: &std::path::Path) -> String {
    let path = dir.join(format!("vbp_fullbuild_d{}_n{}.db", args.dim, args.n));
    let _ = std::fs::remove_file(&path);
    let db = open_default(&path).await;
    seed_concepts(&db, args.n).await;
    let model = ModelName::new(MODEL).unwrap();
    db.register_model(&model, args.dim).await.unwrap();
    let rows = corpus(args.n, args.dim, 1);

    // Drop the index behind registration (actor idle), insert without it.
    let conn = rw(&db);
    conn.execute(
        &format!("DROP INDEX {}", model.index()),
        (),
    ).await.unwrap();
    drop(conn);

    let t = upsert_all(&db, &rows).await;
    let (blob_wall, head, tail) = hold_profile(&t);
    let wal_after_blob = wal_mb(&path).await;

    // One-pass build.
    let conn = rw(&db);
    let t1 = Instant::now();
    conn.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {} (libsql_vector_idx(embedding))",
            model.index(),
            model.table()
        ),
        (),
    ).await.unwrap();
    let build = t1.elapsed();
    drop(conn);

    let w = wal_mb(&path).await;
    db.close().await.unwrap();
    format!(
        "full_build     blob={blob_wall:.2}s first={head:?} last={tail:?}  build(1-pass)={build:.2}s  wal blob/after={wal_after_blob:.1}/{w:.1}MB",
        build = build.as_secs_f64(),
    )
}

async fn query_dissect(args: &Args, dir: &std::path::Path) -> String {
    // Reuse the most recent incremental file if present, else build one.
    let path = dir.join(format!("vbp_query_d{}_n{}.db", args.dim, args.n));
    let _ = std::fs::remove_file(&path);
    let db = open_default(&path).await;
    seed_concepts(&db, args.n).await;
    let model = ModelName::new(MODEL).unwrap();
    db.register_model(&model, args.dim).await.unwrap();
    let rows = corpus(args.n, args.dim, 1);
    let _ = upsert_all(&db, &rows).await;

    let read = db.read_conn();
    let query: Vec<f32> = corpus(1, args.dim, 7).pop().unwrap().1;
    let blob = macrame::vector::EmbeddingCodec::encode(&query, args.dim, MODEL).unwrap();

    // (1) the full search_vector path.
    let t0 = Instant::now();
    for _ in 0..50 {
        let hits = search_vector(read, &query, &model, 10, None, None).await.unwrap();
        assert_eq!(hits.len(), 10);
    }
    let full = t0.elapsed().as_secs_f64() / 50.0;

    // (2) bare vector_top_k — the index alone, no joins, no distance recompute.
    let sql = format!("SELECT id FROM vector_top_k('{}', ?1, 10)", model.index());
    let t0 = Instant::now();
    for _ in 0..50 {
        let _ = read.query(&sql, libsql::params![blob.clone()]).await.unwrap();
    }
    let topk = t0.elapsed().as_secs_f64() / 50.0;

    // (3) the statement without the concepts join (distance recompute kept).
    let sql3 = format!(
        "SELECT e.concept_id, vector_distance_cos(e.embedding, ?1) \
           FROM vector_top_k('{}', ?1, 10) AS t \
           JOIN {} AS e ON e.rowid = t.id \
          ORDER BY 2 ASC LIMIT 10",
        model.index(),
        model.table()
    );
    let t0 = Instant::now();
    for _ in 0..50 {
        let _ = read.query(&sql3, libsql::params![blob.clone()]).await.unwrap();
    }
    let nojoin = t0.elapsed().as_secs_f64() / 50.0;

    db.close().await.unwrap();
    let full_ms = full * 1e3;
    let topk_ms = topk * 1e3;
    let nojoin_ms = nojoin * 1e3;
    format!(
        "query dissect  full={full_ms:.2}ms  top_k_only={topk_ms:.2}ms  no_concepts_join={nojoin_ms:.2}ms"
    )
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let dir = std::env::temp_dir().join(format!("macrame_vector_build_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    println!(
        "vector build probe: n={} dim={} sessions={} (medians of {} runs per arm)",
        args.n, args.dim, args.sessions, args.sessions
    );

    for arm in ["incremental", "inc_wal", "full_build"] {
        let mut lines = Vec::new();
        for _ in 0..args.sessions {
            let line = match arm {
                "incremental" => arm_incremental(&args, false, &dir).await,
                "inc_wal" => arm_incremental(&args, true, &dir).await,
                _ => arm_full_build(&args, &dir).await,
            };
            lines.push(line);
        }
        lines.sort();
        let mid = lines[lines.len() / 2].clone();
        println!("{mid}");
    }

    // Query dissect runs on its own fresh build, once per dim.
    println!("{}", query_dissect(&args, &dir).await);

    std::fs::remove_dir_all(&dir).ok();
}
