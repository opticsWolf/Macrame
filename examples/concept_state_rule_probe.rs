//! P1 probe (plan `Macrame Update Plan v0.21.0.md` §3.1): can the recorded folds
//! answer the state rule — *open belief, else latest-starting belief*, i.e.
//! `valid_from DESC, seq_id DESC`, take the first ([D-303]) — for wall-clock
//! within 10% of today's fold, with no plan shape the pins forbid?
//!
//! [D-254] retired a full window sort from `HOT_FOLD` and the pins forbid that
//! shape. Today's order (`seq_id DESC`) is the order of
//! `idx_txlog_fold_partition` for free. The new order is not. Three candidate
//! shapes are measured against it:
//!
//! | shape | what it is | what it risks |
//! |---|---|---|
//! | `s0_today` | the shipped recorded fold, copied | the baseline everything is compared to |
//! | `s1_uniform_rule` | the new order inside today's window, over the whole log | a sort the pinned plans forbid |
//! | `s2_concept_arm` | a links arm on today's order, a concepts arm on `idx_txlog_concept_starts` recreated with `valid_from DESC` (the v24 rung's premise) | two plans instead of one; the union doubles the cursor set |
//! | `s3_seq_then_correct` | today's window unchanged, then one seek per concept replacing its selected row with the latest row at that concept's greatest start | cost grows with concept count, not log size |
//!
//! `s2` and `s3` both need the v24 index variant, `json_extract(payload,
//! '$.valid_from') DESC, seq_id DESC` — today's partial index spells the
//! expression ascending, and *neither scan direction of a mixed-direction
//! index can deliver the new order* (reversing a scan negates every term's
//! direction). The probe creates both variants, so the seek is measured on
//! the v24 index and, as `s3m_mixed`, on the shipped one, to show whether a
//! rung would be needed for shape 3 too.
//!
//! # Fixtures
//!
//! - `mixed_starts` — a 20,000-row concept log of 400 entities × 20 distinct
//!   valid starts each, recorded stamps climbing one second a row. It is the
//!   [D-293] fixture with the start varied *independently* of the entity: on
//!   the D-293 assignment every entity holds one start, the candidates pick
//!   exactly the row `seq_id DESC` picks, and the probe would compare nothing.
//!   Plus 200 follow-up backfills at an older start, the §1 sequence in
//!   fixture form.
//! - `bench_ledger` — a file-backed database at the release-bench `replay`
//!   fixture's size, written through the real chunked write path: 5,000
//!   concepts, in order, where every shape must agree.
//! - `mixed_log` — the bench ledger plus 5,000 links written bulk. `s2`'s
//!   union arm is only meaningfully planned on a log holding both tables.
//!
//! # What each row reports
//!
//! The plan text (`EXPLAIN QUERY PLAN`), the VDBE triple (opens/seeks/rewinds,
//! read the way `tests/common/plan_fixture.rs` reads them), and the wall clock
//! of running the query and consuming every returned row — best, median and
//! mean over 25 runs after 2 warmups. The copies' hazard is the one
//! `txlog_fold_index_probe` documents: the shipped fold's text is copied here
//! and the pin tests bound that text against the private constant, and the
//! bench ledger's `reconstruct` row round-trips through the public read as a
//! checksum.
//!
//! `Δwinners` counts how many entities' winners differ from `s0_today`'s. It
//! is the probe's own sanity check, not a finding: on `mixed_starts` it is
//! expected to be large for every candidate, **equal between the three
//! candidates** (they claim to compute one rule), and 0 on the in-order
//! bench ledger.
//!
//! The fourth shape, `s4_full_log_index`, is not §3.1's list: it is the v24
//! rung **replacing** `idx_txlog_fold_partition` with the expression index
//! that carries the whole window order — `(table_name, entity_id,
//! branch_id, json_extract(payload, '$.valid_from') DESC, seq_id DESC)`. It
//! is the only shape whose read plan has no sort at all, and the only one
//! that serves the recorded fold's unbounded scan straight off the index. It
//! is priced for *reading* here: putting a `json_extract` on every log row
//! is a write-side trade that [D-301] refuses to make without the user's
//! say, so its write figure belongs in the same table as its read figure
//! before it is an option at all. It is a replacement, and the shipped index
//! is dropped for its station before anything is measured — measuring one
//! index against its own copy is the oldest fallacy in this repo.
//!
//! Run it:
//!
//! ```text
//! cargo run --release --example concept_state_rule_probe
//! ```
//!
//! [D-254]: ../docs/architecture/s13-decision-register.md#d-254
//! [D-293]: ../docs/architecture/s13-decision-register.md#d-293
//! [D-303]: ../docs/architecture/s13-decision-register.md#d-303

use std::collections::HashMap;
use std::time::Instant;

use macrame::graph::EdgeAssertion;
use macrame::schema::run_migrations;
use macrame::temporal::reconstruct;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";

/// The probe's `now`. One value for the whole run, so every shape is asked the
/// same question; the fixtures' recorded stamps all stay below it.
const NOW: &str = "2027-01-01T00:00:00.000000Z";

const BENCH_CONCEPTS: usize = 5_000;
const BENCH_LINKS: usize = 5_000;

const RUNS: usize = 25;
const WARMUPS: usize = 2;

// ---------------------------------------------------------------------------
// The shapes
// ---------------------------------------------------------------------------

/// The shipped recorded fold, copied (the same copy `txlog_fold_index_probe`
/// keeps; the pin tests bound this text against the private constant).
const S0_HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        {BOUND}
    ) WHERE rn = 1
"#;

/// The rule applied in the same window: one query over the whole log, with the
/// per-partition sort the question is whether the wall clock can live with.
const S1_UNIFORM_RULE: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY table_name, entity_id, branch_id
                   ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC
               ) as rn
        FROM transaction_log
        {BOUND}
    ) WHERE rn = 1
"#;

/// The two-arm fold: links win by today's order on `idx_txlog_fold_partition`;
/// concepts win by the rule on the v24 partial index. A link's `entity_id`
/// carries its own `valid_from`, so every link partition is single-start and
/// the two orders pick the same row there (plan §2.4).
///
/// The bound is spelled twice (`?1` in both arms), which is how the folds
/// themselves spell a parameter that appears once per arm.
const S2_CONCEPT_ARM: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE table_name = 'links' {LBOUND}
    ) WHERE rn = 1
    UNION ALL
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY entity_id, branch_id
                   ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC
               ) as rn
        FROM transaction_log
        WHERE table_name = 'concepts' {LBOUND}
    ) WHERE rn = 1
"#;

/// One seek per concept: the latest row at the concept's greatest start,
/// forward-scanned off the v24 partial index, which delivers the rule's order
/// directly. The stage-1 fold stays `S0_HOT_FOLD` — shape 3 changes only what
/// happens to the concept rows it returns.
const S3_PER_CONCEPT_SEEK: &str = "
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM transaction_log
    WHERE table_name = 'concepts' AND entity_id = ?1 AND branch_id = ?2
    ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC
    LIMIT 1";

/// The candidates for the concepts arm's index. `desc` is what the v24 rung
/// would recreate the shipped index as; `mixed` is today's shipped spelling,
/// under an own name so the shipped index and its pins stay untouched.
const V24_INDEX_DESC: &str = "CREATE INDEX IF NOT EXISTS probe_txlog_starts_desc \
     ON transaction_log \
     (entity_id, branch_id, json_extract(payload, '$.valid_from') DESC, seq_id DESC) \
     WHERE table_name = 'concepts'";
const SHIPPED_INDEX_MIXED: &str = "CREATE INDEX IF NOT EXISTS probe_txlog_starts_mixed \
     ON transaction_log \
     (entity_id, branch_id, json_extract(payload, '$.valid_from'), seq_id DESC) \
     WHERE table_name = 'concepts'";

/// The fourth shape's index: the fold partition's, with the valid start
/// inserted between the partition and the sequence. Replacement, not
/// addition: the shipped index's columns are a strict prefix of these, and
/// keeping both would be two index entries per log row for one ordering.
const FULL_LOG_INDEX_PREFIX: &str = "DROP INDEX IF EXISTS idx_txlog_fold_partition";
const FULL_LOG_INDEX: &str = "CREATE INDEX IF NOT EXISTS probe_txlog_fold_rule \
     ON transaction_log \
     (table_name, entity_id, branch_id, \
      json_extract(payload, '$.valid_from') DESC, seq_id DESC)";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The 20,000-row log, every entity holding 20 distinct starts in a recorded
/// order that does not follow the starts — the [D-293] fixture's mechanism
/// (SQL-generated, then analysed) with the start varied independently of the
/// entity.
///
/// `i = 400 * k + e`: entity `e`'s k-th row sits at recorded stamp `k`
/// seconds + `e` microseconds and holds start `k % 20`. Every entity's newest
/// write (k = 49) is its start 9, not its greatest start (19), so the rule
/// and `seq_id DESC` differ on all 400 entities, and each entity's *late*
/// rows carry starts that wrap.
async fn mixed_starts() -> libsql::Database {
    // A file, not `:memory:`: the station's later epochs connect freshly, and
    // each `:memory:` connect is its own blank database with nothing to fold.
    let dir = std::env::temp_dir().join("macrame-state-rule-probe");
    std::fs::create_dir_all(&dir).unwrap();
    let db = libsql::Builder::new_local(dir.join("mixed_starts.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    run_migrations(&conn).await.unwrap();
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 19999) \
         INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         SELECT 'concepts', 'c' || printf('%03d', i % 400), 'upsert', \
                json_object(\
                    'valid_from', strftime('2026-01-%dT00:00:00.000000Z', \
                                       1 + (i / 400) % 20), \
                    'valid_to', '9999-12-31T23:59:59.999999Z', \
                    'title', 'v' || i, 'content', '', 'retired', 0, 'extra', '{}'), \
                strftime('2025-01-01T%H:%M:%S', i, 'unixepoch') || printf('.%06dZ', 1000 * (i % 400)) \
         FROM n",
        (),
    )
    .await
    .unwrap();

    // 200 backfills at an older start than anything above, after the main
    // fixture's stamps — the §1 sequence, at fixture scale.
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 199) \
         INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         SELECT 'concepts', 'c' || printf('%03d', i), 'upsert', \
                json_object('valid_from', '2024-01-01T00:00:00.000000Z', \
                            'valid_to', '2026-01-01T00:00:00.000000Z', \
                            'title', 'backfill-' || i, 'content', '', 'retired', 0, 'extra', '{}'), \
                strftime('2025-12-25T12:%M:%S', i, 'unixepoch') || '.000000Z' \
         FROM n",
        (),
    )
    .await
    .unwrap();

    conn.execute("ANALYZE", ()).await.unwrap();
    db
}

/// The release-bench ledger shape: file-backed, written through the crate's
/// chunked concept write, in order. This is what the benchmark's
/// `reconstruct_full_fold` row folds.
async fn bench_ledger(path: &std::path::Path, with_links: bool) -> Database {
    let db = Database::open_with_cadence(path, None).await.unwrap();
    let concepts: Vec<ConceptUpsert> = (0..BENCH_CONCEPTS)
        .map(|i| ConceptUpsert::new(format!("c{i:05}"), format!("t{i}")).valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();
    if with_links {
        let edges: Vec<EdgeAssertion> = (0..BENCH_LINKS)
            .map(|i| EdgeAssertion::new("c00000", format!("c{i:05}"), "LINKS").valid_from(TS))
            .collect();
        db.write_bulk_atomic(edges).await.unwrap();
    }
    db
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// The plan the planner prints for `sql` with `ts` bound (bound where the SQL
/// carries `?1`, unbound otherwise).
async fn plan_of(conn: &libsql::Connection, sql: &str, ts: Option<&str>) -> String {
    let mut rows = match ts {
        Some(ts) => {
            let params: Vec<libsql::Value> = vec![libsql::Value::Text(ts.to_string())];
            conn.query(
                &format!("EXPLAIN QUERY PLAN {sql}"),
                libsql::params_from_iter(params.into_iter()),
            )
            .await
            .unwrap_or_else(|e| panic!("plan failed: {e}: {sql}"))
        }
        None => conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap_or_else(|e| panic!("plan failed: {e}: {sql}")),
    };
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(3).unwrap());
    }
    out.join(" | ")
}

/// The informative lines of one plan.
fn plan_lines(plan: &str) -> Vec<String> {
    plan.split(" | ")
        .map(str::to_owned)
        .filter(|l| l.contains("INDEX") || l.contains("B-TREE") || l.contains("SCAN"))
        .collect()
}

/// The VDBE triple, read the same way `common/plan_fixture::counts_of` reads
/// it, because two definitions of "a seek" is [D-088]'s defect in a probe.
async fn counts_of(conn: &libsql::Connection, sql: &str) -> (usize, usize, usize) {
    let mut rows = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
    let (mut opens, mut seeks, mut rewinds) = (0usize, 0usize, 0usize);
    while let Some(row) = rows.next().await.unwrap() {
        let op: String = row.get(1).unwrap();
        if op == "OpenRead" {
            opens += 1;
        } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
            seeks += 1;
        } else if op == "Rewind" || op == "Last" {
            rewinds += 1;
        }
    }
    (opens, seeks, rewinds)
}

/// The row winner: `branch|entity -> seq_id`, what all shapes report one way
/// or the other.
type Winners = HashMap<String, u64>;

/// The `(branch, entity)` pairs the log's concept rows household, read of the
/// partial index. Both single-log fixtures and a union's concepts arm.
async fn concept_pairs(conn: &libsql::Connection) -> Vec<(String, String)> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT branch_id, entity_id FROM transaction_log WHERE table_name = 'concepts'",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    out
}

/// Run `sql` as many times as it takes, consuming every returned row, and
/// report `RUNS` timings and the winner map. Bound where the SQL carries `?1`.
async fn time_and_winners(
    conn: &libsql::Connection,
    _label: &str,
    sql: &str,
    ts_bound: Option<&str>,
) -> (Vec<f64>, Winners) {
    let mut winners = Winners::new();
    let mut times = Vec::with_capacity(RUNS);

    for _ in 0..WARMUPS {
        consume(conn, sql, ts_bound, &mut winners).await;
        winners.clear();
    }
    for _ in 0..RUNS {
        let start = Instant::now();
        consume(conn, sql, ts_bound, &mut winners).await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    (sorted(times), winners)
}

/// One run of `sql`, rows into `winners`.
async fn consume(conn: &libsql::Connection, sql: &str, ts: Option<&str>, winners: &mut Winners) {
    let mut rows = match ts {
        Some(ts) => {
            let params: Vec<libsql::Value> = vec![libsql::Value::Text(ts.to_string())];
            conn.query(sql, libsql::params_from_iter(params.into_iter()))
                .await
                .unwrap_or_else(|e| panic!("{sql} run failed: {e}"))
        }
        None => conn
            .query(sql, ())
            .await
            .unwrap_or_else(|e| panic!("{sql} run failed: {e}")),
    };
    while let Some(row) = rows.next().await.unwrap() {
        let seq: i64 = row.get(0).unwrap();
        let _table: String = row.get(1).unwrap();
        let entity: String = row.get(2).unwrap();
        let _operation: String = row.get(3).unwrap();
        let _payload: String = row.get(4).unwrap();
        let branch: String = row.get(5).unwrap();
        winners.insert(format!("{branch}|{entity}"), seq as u64);
    }
}

/// The same consumption, for the per-concept seek statements of shape 3: the
/// seek per `(branch, entity)` pair of a stage-1's output. Timed as a whole —
/// the shape's cost is the stage-1 window plus one seek and one row read per
/// concept.
async fn time_and_winners_s3(
    conn: &libsql::Connection,
    sql: &str,
    pairs: &[(String, String)],
) -> (Vec<f64>, Winners) {
    let mut winners = Winners::new();
    let mut times = Vec::with_capacity(RUNS);

    for _ in 0..WARMUPS {
        consume_s3(conn, sql, pairs, &mut winners).await;
        winners.clear();
    }
    for _ in 0..RUNS {
        let start = Instant::now();
        consume_s3(conn, sql, pairs, &mut winners).await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    (sorted(times), winners)
}

/// One run of stage-1 + per-concept seeks, rows into `winners`.
async fn consume_s3(
    conn: &libsql::Connection,
    sql: &str,
    pairs: &[(String, String)],
    winners: &mut Winners,
) {
    // Stage 1, as shipped: its rows are this shape's non-concept rows and its
    // starting point for the correction.
    consume(conn, sql, Some(NOW), winners).await;
    let stmt = conn.prepare(S3_PER_CONCEPT_SEEK).await.unwrap();
    for (branch, entity) in pairs {
        stmt.reset();
        let mut rows = stmt
            .query(libsql::params![entity.as_str(), branch.as_str()])
            .await
            .unwrap_or_else(|e| panic!("per-concept seek failed for {entity}: {e}"));
        while let Some(row) = rows.next().await.unwrap() {
            let seq: i64 = row.get(0).unwrap();
            let _table: String = row.get(1).unwrap();
            let entity: String = row.get(2).unwrap();
            let _operation: String = row.get(3).unwrap();
            let _payload: String = row.get(4).unwrap();
            let branch: String = row.get(5).unwrap();
            winners.insert(format!("{branch}|{entity}"), seq as u64);
        }
    }
}

fn sorted(mut times: Vec<f64>) -> Vec<f64> {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times
}

fn stat(times: &[f64]) -> String {
    let best = times[0];
    let median = times[times.len() / 2];
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    format!("best {best:7.3}  med {median:7.3}  mean {mean:7.3} (ms)")
}

/// Entities whose winners differ between two result rows.
fn delta(a: &Winners, b: &Winners) -> usize {
    a.iter()
        .filter(|(k, v)| b.get(k.as_str()) != Some(v))
        .count()
}

/// One fixture under measurement. Epochs take fresh handles from the same
/// `Database`: an index dropped on one connection stayed visible to another
/// connection's planner until its schema cache reloaded, which turned the
/// s4 epoch into a lie (the plan named the dropped index while timing the
/// new one) — fresh handles per epoch is the cure, and it is also why the
/// bench fixture's handles are opened after the crate `Database` closes.
struct Station {
    name: String,
    db: libsql::Database,
}

/// Record the run through the crate's public fold: the checksum that the wall
/// clock of the copies is not measuring something the real path does not do.
async fn public_reconstruct(conn: &libsql::Connection) -> Vec<f64> {
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..WARMUPS {
        reconstruct(conn, NOW, None, None).await.unwrap();
    }
    for _ in 0..RUNS {
        let start = Instant::now();
        let _ = reconstruct(conn, NOW, None, None).await.unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    sorted(times)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("P1 probe — can the recorded fold take the state rule without a sort?");
    println!("runs {RUNS} after {WARMUPS} warmups. The fold is consumed row by row,");
    println!("winner key branch|entity. 'Δv' = winners differing from s0.");
    println!();

    let mut stations: Vec<Station> = Vec::new();

    // The probe directory exists before any fixture: it is wiped, and the
    // first fixture built into it must survive that.
    let bench_dir = std::env::temp_dir().join("macrame-state-rule-probe");
    let _ = std::fs::remove_dir_all(&bench_dir);
    std::fs::create_dir_all(&bench_dir).unwrap();

    stations.push(Station {
        name: "mixed_starts (20,000 rows / 400 entities x 20 starts / now)".into(),
        db: mixed_starts().await,
    });
    // The bench fixture builds through the crate (chunked writes, triggers,
    // cadence disabled), is checksummed against the public read once, and is
    // then closed; the shapes measure on the probe's own handles, opened
    // after the close so no connection carries a stale schema across the
    // epochs.
    for (name, file, links) in [
        (
            "bench_ledger concepts_only (5,000 concepts, in order)",
            "concepts_only.db",
            false,
        ),
        (
            "mixed_log (5,000 concepts + 5,000 links)",
            "mixed_log.db",
            true,
        ),
    ] {
        let path = bench_dir.join(file);
        let db = bench_ledger(&path, links).await;
        let checksum = public_reconstruct(db.read_conn()).await;
        println!(
            "  [checksum] public `reconstruct` on {file}: {}",
            stat(&checksum)
        );
        db.close().await.unwrap();
        let hdb = libsql::Builder::new_local(&path).build().await.unwrap();
        stations.push(Station {
            name: name.into(),
            db: hdb,
        });
    }

    println!();
    for Station { name, db } in &stations {
        let conn = db.connect().unwrap();
        let writes = db.connect().unwrap();
        let max_ts: String = conn
            .query("SELECT MAX(recorded_at) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let rows: i64 = conn
            .query("SELECT COUNT(*) FROM transaction_log", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        println!("=== {name} ===");
        println!("    rows {rows}, max = {max_ts}, ts = {NOW}");

        let s0u = S0_HOT_FOLD.replace("{BOUND}", "");
        let s0b = S0_HOT_FOLD.replace("{BOUND}", "WHERE recorded_at <= ?1");
        let s1u = S1_UNIFORM_RULE.replace("{BOUND}", "");
        let s1b = S1_UNIFORM_RULE.replace("{BOUND}", "WHERE recorded_at <= ?1");
        let s2b = S2_CONCEPT_ARM.replace("{LBOUND}", "AND recorded_at <= ?1");
        let pairs = concept_pairs(&conn).await;

        // --- epoch 1: today. The shipped mixed-direction index is the only
        // concepts-arm index, so s3m measures here (it is the shape as it
        // would be without a rung). s2/s3 assume the rung's epoch.
        writes.execute(SHIPPED_INDEX_MIXED, ()).await.unwrap();
        writes.execute("ANALYZE", ()).await.unwrap();

        let (_, w0) = time_and_winners(&conn, "s0 bounded", &s0b, Some(NOW)).await;
        let (_, w1) = time_and_winners(&conn, "s1 bounded", &s1b, Some(NOW)).await;

        for (label, sql, bound) in [
            ("s0_today    unbounded", &s0u, None),
            ("s0_today    bounded$max", &s0b, Some(NOW)),
            ("s1_uniform  unbounded", &s1u, None),
            ("s1_uniform  bounded$max", &s1b, Some(NOW)),
        ] {
            let plan = plan_of(&conn, sql, bound).await;
            let counts = counts_of(&conn, sql).await;
            let (times, winners) = time_and_winners(&conn, label, sql, bound).await;
            println!(
                "{}  {}  {:?}  Δv vs s0 {}",
                label,
                stat(&times),
                counts,
                delta(&w0, &winners)
            );
            for line in plan_lines(&plan) {
                println!("      {line}");
            }
        }

        let (t3m, w3m) = time_and_winners_s3(&conn, &s0b, &pairs).await;
        let plan = plan_of(&conn, S3_PER_CONCEPT_SEEK, None).await;
        let counts = counts_of(&conn, S3_PER_CONCEPT_SEEK).await;
        println!(
            "s3m (stage1 + seek, pre-rung index only)  {}  {:?}  Δv vs s0 {}",
            stat(&t3m),
            counts,
            delta(&w0, &w3m)
        );
        for line in plan_lines(&plan) {
            println!("      {line}");
        }

        // --- epoch 2: the v24 rung's premise. The DESC index appears; s2's
        // arm and s3's seek are measured in it.
        writes.execute(V24_INDEX_DESC, ()).await.unwrap();
        writes.execute("ANALYZE", ()).await.unwrap();

        let plan = plan_of(&conn, &s2b, Some(NOW)).await;
        let counts = counts_of(&conn, &s2b).await;
        let (t2, w2) = time_and_winners(&conn, "s2", &s2b, Some(NOW)).await;
        println!(
            "s2_concept_arm (bounded$max, v24 index)  {}  {:?}  Δv vs s0 {}",
            stat(&t2),
            counts,
            delta(&w0, &w2)
        );
        for line in plan_lines(&plan) {
            println!("      {line}");
        }

        let (t3, w3) = time_and_winners_s3(&conn, &s0b, &pairs).await;
        let plan3 = plan_of(&conn, S3_PER_CONCEPT_SEEK, None).await;
        let counts3 = counts_of(&conn, S3_PER_CONCEPT_SEEK).await;
        println!(
            "s3_seq_then_correct (stage1 + seek, v24 index)  {}  {:?}  Δv vs s0 {}",
            stat(&t3),
            counts3,
            delta(&w0, &w3)
        );
        for line in plan_lines(&plan3) {
            println!("      {line}");
        }

        // The candidates must agree with each other: one rule, three shapes.
        println!(
            "    cross-check: s1==s2 {} (Δ{}), s1==s3 {} (Δ{}), s3==s3m {} (Δ{}), mixed-vs-v24 identical {} (Δ{})",
            w1 == w2,
            delta(&w1, &w2),
            w1 == w3,
            delta(&w1, &w3),
            w3 == w3m,
            delta(&w3, &w3m),
            w3m == w3,
            delta(&w3m, &w3)
        );

        // --- s4: the fold partition index becomes the full-order expression
        // index. A replacement, so the shipped one is dropped: measuring a
        // new index against its own still-living predecessor reports the
        // difference between two indexes sharing one key and is not a
        // finding. The epoch takes its own handles: an index dropped on one
        // connection stayed visible to another connection's planner.
        let conn = db.connect().unwrap();
        let writes = db.connect().unwrap();
        writes.execute(FULL_LOG_INDEX_PREFIX, ()).await.unwrap();
        writes.execute(FULL_LOG_INDEX, ()).await.unwrap();
        writes.execute("ANALYZE", ()).await.unwrap();

        for (label, sql, bound) in [
            ("s4 fold on full-order index  unbounded", &s0u, None),
            ("s4 fold on full-order index  bounded$max", &s0b, Some(NOW)),
            ("s4 s1 on full-order index    unbounded", &s1u, None),
            ("s4 s2 arms on full-order     bounded$max", &s2b, Some(NOW)),
        ] {
            let plan = plan_of(&conn, sql, bound).await;
            let counts = counts_of(&conn, sql).await;
            let (times, winners) = time_and_winners(&conn, label, sql, bound).await;
            println!(
                "{}  {}  {:?}  Δv vs s0 {}",
                label,
                stat(&times),
                counts,
                delta(&w0, &winners)
            );
            for line in plan_lines(&plan) {
                println!("      {line}");
            }
        }
        println!();
    }
}
