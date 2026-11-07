//! 0.22.0: what `pointers(kind)` costs before and after the kind's partial
//! index (plan §4.3, on top of [D-309]).
//!
//! The draft pushes `(source, kind)` into the base scans as a prefix key for a
//! *single-source* pointer read — right for `pointer(source, kind)`, a scan
//! for `pointers(kind)` across all sources, which is Silk's ready-set query
//! ("every file's status right now"). Before adding an index the plan asks for
//! one to be probed: plan and time the listing on a 10,000-source fixture; add
//! `links_current (edge_type, source_id) WHERE valid_to = OPEN` only if it
//! scans, and price it on the 90-edge chunk the way D-304 priced the others —
//! that half lives in `edge_chunk_index_cost_probe`, which lists the new index
//! among the paths on `links_current` the day this one ships.
//!
//! The fixture is the workspace shape, not the empty one: 10,000 sources,
//! three moves each — `links_current` holds ~30,000 pointer rows of which
//! 10,000 are open and 20,000 are history the listing has to discard. A scan
//! pays for the history; the partial index never holds any.
//!
//! Sequence in one process, one file: the index dropped (when `--no-index`),
//! samples; created, samples; the statement does not differ between arms —
//! the same SQL finds whichever plan the schema offers, so the planner's own
//! choice is what each arm shows.
//!
//!   cargo run --release --example probe_pointers_kind            # with index
//!   cargo run --release --example probe_pointers_kind -- --no-index
//!
//! [D-309]: ../docs/architecture/s13-decision-register.md#d-309

use std::time::{Duration, Instant};

use macrame::{ConceptUpsert, Database, PointerSet, ReadPlan, WriteBatch};

/// The workspace: 10,000 files, three status moves each.
const SOURCES: usize = 10_000;
const MOVES: usize = 3;
/// [`macrame::batch::MAX_BATCH_ROWS`] — one act per chunk.
const CAP: usize = 2_000;
const SAMPLES: usize = 31;
const KIND: &str = "CUR";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The reader's own statement on the trunk shape, byte for byte as
/// `graph::pointer::open_edges` emits it for a whole-kind listing (no source,
/// no recorded instant): source unbound, kind bound, the open filter, then the
/// workspace order. The probe explains exactly this text, so the plan it
/// prints is the plan a caller pays.
fn listing_sql() -> String {
    format!(
        "SELECT l.source_id, l.target_id, l.valid_from, l.branch_id \
         FROM links_current l \
         WHERE (?1 IS NULL OR l.source_id = ?1) AND l.edge_type = ?2 \
           AND l.valid_to = '{OPEN}' \
         ORDER BY l.source_id, l.valid_from DESC, l.target_id"
    )
}

/// The index this probe is about, spelt the way the schema ships it.
const IDX: &str = "CREATE INDEX IF NOT EXISTS idx_lc_pointer_kind ON links_current \
     (edge_type, source_id) WHERE valid_to = '9999-12-31T23:59:59.999999Z';";

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// 10,000 sources × three moves, one act of [`CAP`] rows at a time — the
/// sources minted first in their own acts (`links`' foreign key reaches
/// `concepts`, so a pointer row's endpoints must exist first), then three
/// waves of moves a day apart, so every later move starts after the target
/// it closes.
async fn fixture(db: &Database) {
    db.register_edge_kind(KIND, false).await.unwrap();
    let mut first = WriteBatch::new();
    for h in ["h0", "h1", "h2"] {
        first = first.concept(ConceptUpsert::new(h, "h").valid_from("2025-01-01T00:00:00.000000Z"));
    }
    db.write_batch(first).await.unwrap();

    for batch in 0..(SOURCES / CAP) {
        let mut w = WriteBatch::new();
        for i in (batch * CAP)..((batch + 1) * CAP) {
            w = w.concept(
                ConceptUpsert::new(format!("f{i:05}"), "f")
                    .valid_from("2025-01-01T00:00:00.000000Z"),
            );
        }
        db.write_batch(w).await.unwrap();
    }
    for wave in 0..MOVES {
        let vf = format!("2026-01-{:02}T00:00:00.000000Z", 1 + wave);
        for batch in 0..(SOURCES / CAP) {
            let mut w = WriteBatch::new();
            for i in (batch * CAP)..((batch + 1) * CAP) {
                w = w.pointer(
                    PointerSet::new(format!("f{i:05}"), KIND, format!("h{wave}")).valid_from(&vf),
                );
            }
            db.write_batch(w).await.unwrap();
        }
    }
}

/// One timed listing through the reader, as a caller pays it.
async fn listing(db: &Database) -> Duration {
    let t = Instant::now();
    let got = db.pointers(KIND, ReadPlan::new()).await.unwrap();
    let d = t.elapsed();
    assert_eq!(got.len(), SOURCES, "one row per source, the ready set");
    d
}

/// Warm-up, then the recorded samples; returns the p50 in ms.
async fn samples(db: &Database, tag: &str) -> f64 {
    listing(db).await;
    let mut v = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        v.push(listing(db).await);
    }
    let ms = median(v).as_secs_f64() * 1000.0;
    println!("  {tag:<28} p50 {ms:.3} ms");
    ms
}

/// The plan the planner offers this statement right now.
async fn plan(conn: &libsql::Connection, tag: &str) {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {}", listing_sql()), ())
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let detail = row.get::<String>(3).unwrap();
        println!("    {tag:<28} {detail}");
    }
}

async fn count(conn: &libsql::Connection, sql: &str) -> usize {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get::<i64>(0).unwrap() as usize
}

#[tokio::main]
async fn main() {
    let no_index = std::env::args().any(|a| a == "--no-index");
    let dir = std::env::temp_dir().join(format!("pointer_kind_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open_with_cadence(dir.join("p.db"), None)
        .await
        .unwrap();
    println!("probe_pointers_kind — {SOURCES} sources x {MOVES} moves, p50 of {SAMPLES}");

    fixture(&db).await;
    let conn = db.raw().connect().unwrap();
    let total = count(&conn, "SELECT COUNT(*) FROM links_current").await;
    let open = count(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM links_current WHERE valid_to = '{OPEN}' AND edge_type = '{KIND}'"
        ),
    )
    .await;
    println!("  links_current: {total} rows, {open} open under '{KIND}'\n");

    if no_index {
        conn.execute("DROP INDEX IF EXISTS idx_lc_pointer_kind", ())
            .await
            .unwrap();
    }
    db.analyze().await.unwrap();
    let scan = samples(&db, "without the index").await;
    plan(&conn, "scan:").await;

    // The arm ends where every caller of the shipped schema starts: the index
    // (re-)created, stats refreshed so the planner can *name* the seek rather
    // than guess it, the same statement re-planned against the new schema.
    conn.execute(IDX, ()).await.unwrap();
    db.analyze().await.unwrap();
    let seek = samples(&db, "with the index").await;
    plan(&conn, "seek:").await;
    println!("\nlisting: {scan:.3} -> {seek:.3} ms ({:.1}x)", scan / seek);

    db.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
