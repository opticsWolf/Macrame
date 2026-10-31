//! Shape-5 probe (P1, 0.21.0 — `docs/Macrame Update Plan v0.21.0.md` §3.5, option C):
//! can the state rule ([D-303]) be carried by today's fold plus a
//! write-maintained disorder table, with the fold's hot path within the §3.1
//! gate's 10%?
//!
//! # The shape
//!
//! Neither the plan's three shapes nor the measured extra (`s4`, the
//! full-order expression index) could carry the rule within 10% of today's
//! fold: the rule's extra work must be paid by *every* row the log holds. The
//! observation shape 5 builds on: **the rule and today's `seq_id DESC`
//! pick the same row for every partition that is not disordered**, and the
//! only way a partition becomes disordered is a write whose start is earlier
//! than the start the projection already shows — exactly the write that
//! §3.2's skipped projection upsert already detects, in its changed-row
//! count. The disorder is known to the writer the moment it happens:
//!
//! - the v24 rung adds `concept_disorder (entity_id, branch_id)`, filled on a
//!   backfill write (the projection's skip) and cleared when a write meets or
//!   passes the partition's greatest start (the write path answers that from
//!   data the D-291 guard read already holds when the asserted interval is
//!   open, and with one `LIMIT 1` seek otherwise);
//! - the fold keeps today's window untouched, filters its winners through
//!   `NOT EXISTS (… concept_disorder …)` — link rows always pass — and
//!   appends, per disordered partition, the rule row off `idx_txlog_concept_starts`
//!   recreated with `valid_from DESC` (one seek per *disordered* partition,
//!   not per concept).
//!
//! - the v24 rung adds `concept_disorder (entity_id, branch_id)`, filled on a
//!   backfill write (the projection's skip) and **never cleared**: the table
//!   lists every pair that was *ever* disordered. Clearing on a healing write
//!   would let a later write change what a past read returned — the ledger
//!   promises that never happens. The mark is applied the moment the write
//!   happens (the write path answers whether its start is behind the
//!   partition's greatest from data the D-291 guard already holds when the
//!   asserted interval is open, and with one `LIMIT 1` seek otherwise) and
//!   stays forever;
//! - the fold keeps today's window untouched, filters its winners through
//!   `NOT EXISTS (… concept_disorder …)` — link rows always pass — and
//!   appends, per ever-disordered partition, the rule row off
//!   `idx_txlog_concept_starts` recreated with `valid_from DESC` (one seek
//!   per *disordered* partition, not per concept). The correction carries
//!   the fold's own recorded bound, so a read at a past `ts` never admits a
//!   row written after it.
//!
//! A ledger that has never taken a backfill pays the filter probe per fold
//! row and nothing else. A ledger that ever took one pays the correction for
//! its ever-disordered pairs on every read thereafter — healing does not buy
//! the cost back; that is the price of past reads never changing. Both
//! economies are priced below.
//!
//! # Stations and epochs
//!
//! | station | why | epochs |
//! |---|---|---|
//! | `mixed_starts` (20,200 concept rows, 400 entities × 20 starts + 200 backfills) | the deep-history gate row, and the pathological extreme: every partition disordered | healthy (empty table), fully disordered, then a heal epoch with a bounded stability read |
//! | `bench_healthy` (5,000 concepts, real chunked writes) | the drive-size gate row | healthy |
//! | `bench_disordered` (the same, plus 250 real backfill writes past the guard) | the realistic 5% case | disordered, then the heal epoch |
//! | `mixed_log` (bench ledger + 5,000 links) | the links arm must pass the filter untouched | healthy |
//!
//! # Stations and epochs
//!
//! | station | why | epochs |
//! |---|---|---|
//! | `mixed_starts` (20,200 concept rows, 400 entities × 20 starts + 200 backfills) | the deep-history gate row, and the pathological extreme: every partition disordered | healthy (empty table), then fully disordered |
//! | `bench_healthy` (5,000 concepts, real chunked writes) | the drive-size gate row | healthy |
//! | `bench_disordered` (the same, plus 250 real backfill writes past the guard) | the realistic 5% case | disordered |
//! | `mixed_log` (bench ledger + 5,000 links) | the links arm must pass the filter untouched | healthy |
//!
//! Epoch handles are fresh `connect()`s per station-epoch, for the same
//! reason the state-rule probe fixed: a table created on one connection was
//! invisible to another connection's planner until its schema cache reloaded.
//! And the first sweep of any station is cache-cold from the fixture build;
//! bounded rows compare against bounded rows.
//!
//! # What a row reports
//!
//! Wall clock (medians of 25 runs after 2 warmups, rows consumed), plan text,
//! the VDBE triple read the way `common/plan_fixture::counts_of` reads it,
//! and winners (`branch|entity -> seq_id`). The gate comparison is bounded
//! shape-vs-shape: s5 against s0 (today's fold) in the same epoch. The
//! winner cross-checks are the probe's own correctness claim: s5-healthy
//! equals s0 everywhere; s5-disordered equals the rule window; and on
//! `bench_disordered` the winners that moved are exactly the 250 backfilled
//! pairs.
//!
//! The write rows price what can be priced without `src/`: the maintenance
//! decision itself (per pair: the partition's greatest start, one `LIMIT 1`
//! seek on the v24 index), and the 70-id batch of the same decision in one
//! prepared statement, against the budgets the release bench reports for
//! those paths (§9 upsert; `chunk_budget` concepts).
//!
//! Run it:
//!
//! ```text
//! cargo run --release --example concept_disorder_probe
//! ```
//!
//! [D-303]: ../docs/architecture/s13-decision-register.md#d-303

use std::collections::HashMap;
use std::time::Instant;

use macrame::graph::EdgeAssertion;
use macrame::schema::run_migrations;
use macrame::temporal::reconstruct;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const NOW: &str = "2027-01-01T00:00:00.000000Z";
const BACKFILL_START: &str = "2025-12-15T00:00:00.000000Z";
const BACKFILL_END: &str = "2025-12-31T00:00:00.000000Z";

const BENCH_CONCEPTS: usize = 5_000;
const BENCH_BACKFILLS: usize = 250;
const BENCH_LINKS: usize = 5_000;

const RUNS: usize = 25;
const WARMUPS: usize = 2;

/// The batch size the release bench prices concept writes at.
const CHUNK: usize = 70;

/// The existence probe of the two-statement formulation: the fold statement
/// itself is never changed, and Rust asks the side table whether any work is
/// owed before running the correction. On a healthy ledger this is the fold's
/// whole shape-5 cost.
const DISORDER_PROBE: &str = "SELECT EXISTS (SELECT 1 FROM concept_disorder LIMIT 1)";

/// The correction statement of the two-statement formulation, run only when
/// the probe answers yes: per ever-disordered pair, the rule row on the v24
/// index. The rows it returns are merged over the fold's by Rust (they
/// replace the fold's own rows for the same pairs), so this statement must
/// never be run for a pair the fold did not return.
const DISORDER_CORRECTION: &str = r#"
    SELECT t.seq_id, t.table_name, t.entity_id, t.operation, t.payload, t.branch_id
    FROM concept_disorder cd
    JOIN transaction_log t
      ON t.entity_id = cd.entity_id AND t.branch_id = cd.branch_id
    WHERE t.table_name = 'concepts'
      AND t.seq_id = (
            SELECT t2.seq_id FROM transaction_log t2
            WHERE t2.table_name = 'concepts'
              AND t2.entity_id = t.entity_id AND t2.branch_id = t.branch_id
            ORDER BY json_extract(t2.payload, '$.valid_from') DESC, t2.seq_id DESC
            LIMIT 1
          )
"#;

/// The correction carrying the fold's own recorded bound: for a read at a
/// past `ts`, no admitted row may be written after it. The bound rides only
/// the subquery — `t` is the subquery's own winner by the join on `seq_id`,
/// so it cannot escape the bound on its own. A redundant bound on the join
/// (`AND t.recorded_at <= ?1`) made the healed ledger's read pay for it: on
/// libsql, once heal sits beyond the bound, the planner gave the join a
/// second sought predicate and the statement went from 0.5 ms to ~9 ms.
/// Keep the join predicate exactly the join.
const DISORDER_CORRECTION_BOUNDED: &str = r#"
    SELECT t.seq_id, t.table_name, t.entity_id, t.operation, t.payload, t.branch_id
    FROM concept_disorder cd
    JOIN transaction_log t
      ON t.entity_id = cd.entity_id AND t.branch_id = cd.branch_id
    WHERE t.table_name = 'concepts'
      AND t.seq_id = (
            SELECT t2.seq_id FROM transaction_log t2
            WHERE t2.table_name = 'concepts'
              AND t2.entity_id = t.entity_id AND t2.branch_id = t.branch_id
              AND t2.recorded_at <= ?1
            ORDER BY json_extract(t2.payload, '$.valid_from') DESC, t2.seq_id DESC
            LIMIT 1
          )
"#;

// ---------------------------------------------------------------------------
// The shape's statements
// ---------------------------------------------------------------------------

/// The shipped recorded fold, copied (the state-rule probe's `s0`).
const S0_HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        {BOUND}
    ) WHERE rn = 1
"#;

/// Shape 5 (single statement): today's window untouched; winners filtered by
/// the disorder table (link rows always pass); the rule row per disordered
/// partition appended from the v24 partial index. The outer derived table is
/// aliased `q` and every correlation names it: the first version let the bare
/// `entity_id`/`branch_id` bind to `cd`'s own columns, which made the filter
/// wrongly answer about the wrong scope and drop every concept row the
/// moment the table held rows.
const S5_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        {BOUND}
    ) AS q
    WHERE q.rn = 1
      AND (q.table_name = 'links' OR NOT EXISTS (
            SELECT 1 FROM concept_disorder cd
            WHERE cd.entity_id = q.entity_id AND cd.branch_id = q.branch_id))
    UNION ALL
    SELECT t.seq_id, t.table_name, t.entity_id, t.operation, t.payload, t.branch_id
    FROM concept_disorder cd
    JOIN transaction_log t
      ON t.entity_id = cd.entity_id AND t.branch_id = cd.branch_id
    WHERE t.table_name = 'concepts'
      AND t.seq_id = (
            SELECT t2.seq_id FROM transaction_log t2
            WHERE t2.table_name = 'concepts'
              AND t2.entity_id = t.entity_id AND t2.branch_id = t.branch_id
            ORDER BY json_extract(t2.payload, '$.valid_from') DESC, t2.seq_id DESC
            LIMIT 1
          )
"#;

/// The bounded form: the recorded bound rides both arms, and the correction
/// seek admits only rows the bound keeps. Pairs with nothing admitted answer
/// nothing from either arm.
const S5_FOLD_BOUNDED: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1
    ) AS q
    WHERE q.rn = 1
      AND (q.table_name = 'links' OR NOT EXISTS (
            SELECT 1 FROM concept_disorder cd
            WHERE cd.entity_id = q.entity_id AND cd.branch_id = q.branch_id))
    UNION ALL
    SELECT t.seq_id, t.table_name, t.entity_id, t.operation, t.payload, t.branch_id
    FROM concept_disorder cd
    JOIN transaction_log t
      ON t.entity_id = cd.entity_id AND t.branch_id = cd.branch_id
    WHERE t.table_name = 'concepts'
      AND t.seq_id = (
            SELECT t2.seq_id FROM transaction_log t2
            WHERE t2.table_name = 'concepts'
              AND t2.entity_id = t.entity_id AND t2.branch_id = t.branch_id
              AND t2.recorded_at <= ?1
            ORDER BY json_extract(t2.payload, '$.valid_from') DESC, t2.seq_id DESC
            LIMIT 1
          )
"#;

/// The two-statement form's combined read, for `RUNS`: fold, probe, and —
/// where the probe answers yes — the correction run with its rows merged over
/// the fold's. The measured cost is the production cost path. `ts` rides both
/// statements: the production fold's bound and the bounded correction's are
/// the same bound, which is what keeps a past read honest.
async fn time_two_statement(
    conn: &libsql::Connection,
    fold_sql: &str,
    ts: Option<&str>,
    corr_sql: &str,
) -> (Vec<f64>, Winners) {
    let mut winners = Winners::new();
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..WARMUPS {
        run_two_statement(conn, fold_sql, ts, corr_sql, &mut winners).await;
        winners.clear();
    }
    for _ in 0..RUNS {
        let start = Instant::now();
        run_two_statement(conn, fold_sql, ts, corr_sql, &mut winners).await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    (sorted(times), winners)
}

async fn run_two_statement(
    conn: &libsql::Connection,
    fold_sql: &str,
    ts: Option<&str>,
    corr_sql: &str,
    winners: &mut Winners,
) {
    consume(conn, fold_sql, ts, winners).await;
    let mut rows = conn
        .query(DISORDER_PROBE, ())
        .await
        .unwrap_or_else(|e| panic!("probe failed: {e}"));
    let owed: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    if owed == 1 {
        // The merge is an overwrite: correction rows replace the fold's rows
        // for their pairs, keyed identically.
        consume(conn, corr_sql, ts, winners).await;
    }
}

/// The rule window, for the winner cross-checks. Six columns, because the
/// consumer reads all of them. `{BOUND}` rides it too: the bounded claim is
/// that the two-statement read at `ts` equals this window over rows recorded
/// `<= ts` — the same statement with the same bound, nothing re-derived.
const RULE_WINNERS: &str = r#"
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

/// The v24 index the rung recreates (the state-rule probe's spelling of it),
/// which the fold's correction arm rides.
const V24_INDEX_DESC: &str = "CREATE INDEX IF NOT EXISTS probe_txlog_starts_desc \
     ON transaction_log \
     (entity_id, branch_id, json_extract(payload, '$.valid_from') DESC, seq_id DESC) \
     WHERE table_name = 'concepts'";

/// The side table the v24 rung adds. Its key covers the fold's probe, and its
/// row count is the fold's correction arm's seek count.
const DISORDER_TABLE: &str = "CREATE TABLE IF NOT EXISTS concept_disorder (\
     entity_id TEXT NOT NULL, \
     branch_id TEXT NOT NULL, \
     PRIMARY KEY (entity_id, branch_id))";

/// The truth, computed analytically for the probe: pairs *ever* disordered.
/// Production marks them at the write (§3.2's skipped projection upsert is
/// the same event) and never unmarks — clearing on a healing write would let
/// a later write change what a past read returns. The analytic fill reads the
/// run-order history for exactly that definition: a row starts before the
/// greatest start the partition already showed when it was written, so each
/// row meets the starts that came before it in `seq_id` order.
const EVER_DISORDER: &str = r#"
    INSERT INTO concept_disorder (entity_id, branch_id)
    SELECT DISTINCT entity_id, branch_id FROM (
        SELECT entity_id, branch_id,
               json_extract(payload, '$.valid_from') AS vf,
               MAX(json_extract(payload, '$.valid_from')) OVER (
                   PARTITION BY entity_id, branch_id
                   ORDER BY seq_id
                   ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS maxvf_before
        FROM transaction_log
        WHERE table_name = 'concepts'
    ) WHERE maxvf_before IS NOT NULL AND vf < maxvf_before
"#;

/// The write-side decision the rung's paths need per pair: the greatest
/// start, one seek, which marks a write as a backfill. A healing write does
/// not unmark: the marks are permanent, that is what makes past reads stable.
const MAINTAIN_SEEK_ONE: &str = "
    SELECT json_extract(payload, '$.valid_from')
    FROM transaction_log
    WHERE table_name = 'concepts' AND entity_id = ?1 AND branch_id = ?2
    ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC
    LIMIT 1";

/// The batch form, one prepared statement per chunk, exactly what the rung's
/// batch fixup needs: the partition's greatest start for each of the chunk's
/// ids. `?1` binds the ids as a JSON text array; `json_each` parses text.
const MAINTAIN_SEEK_BATCH: &str = "
    WITH want(id) AS (SELECT value FROM json_each(?1))
    SELECT want.id,
      (SELECT json_extract(payload, '$.valid_from')
       FROM transaction_log
       WHERE table_name = 'concepts' AND entity_id = want.id AND branch_id = ?2
       ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC
       LIMIT 1)
    FROM want";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn probe_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("macrame-disorder-probe");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The deep-history log: 20,000 concept rows, 400 entities × 20 distinct
/// starts written out of order, plus 200 §1 backfills. File-backed: the
/// station's later epochs connect freshly, and each fresh `connect()` on a
/// `:memory:` builder is a new blank database.
async fn mixed_starts() -> libsql::Database {
    let db = libsql::Builder::new_local(probe_dir().join("mixed_starts.db"))
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
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 199) \
         INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         SELECT 'concepts', 'c' || printf('%03d', i), 'upsert', \
                json_object('valid_from', '2025-12-15T00:00:00.000000Z', \
                            'valid_to', '2025-12-31T00:00:00.000000Z', \
                            'title', 'backfill-' || i, 'content', '', \
                            'retired', 0, 'extra', '{}'), \
                strftime('2025-12-25T12:%M:%S', i, 'unixepoch') || '.000000Z' \
         FROM n",
        (),
    )
    .await
    .unwrap();
    conn.execute("ANALYZE", ()).await.unwrap();
    db
}

/// A release-bench ledger through the crate's chunked write: `concepts`
/// alone, or with links, and — for the disordered station — real backfill
/// writes through the public path (their ends fall before the open starts
/// they follow, which the interval guard admits).
async fn bench_ledger(path: &std::path::Path, links: bool, backfills: usize) -> Database {
    let db = Database::open_with_cadence(path, None).await.unwrap();
    let concepts: Vec<ConceptUpsert> = (0..BENCH_CONCEPTS)
        .map(|i| ConceptUpsert::new(format!("c{i:05}"), format!("t{i}")).valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();
    if links {
        let edges: Vec<EdgeAssertion> = (0..BENCH_LINKS)
            .map(|i| EdgeAssertion::new("c00000", format!("c{i:05}"), "LINKS").valid_from(TS))
            .collect();
        db.write_bulk_atomic(edges).await.unwrap();
    }
    for i in 0..backfills {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i:05}"), format!("backfill-{i}"))
                .valid_from(BACKFILL_START)
                .valid_to(BACKFILL_END),
        )
        .await
        .unwrap();
    }
    db
}

// ---------------------------------------------------------------------------
// Measurement helpers
// ---------------------------------------------------------------------------

type Winners = HashMap<String, u64>;

/// The plan for `sql`, with `NOW` bound where the statement carries `?1`.
async fn plan_of(conn: &libsql::Connection, sql: &str, ts_bound: bool) -> String {
    let mut out = Vec::new();
    if ts_bound {
        let params: Vec<libsql::Value> = vec![libsql::Value::Text(NOW.to_string())];
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {sql}"),
                libsql::params_from_iter(params.into_iter()),
            )
            .await
            .unwrap_or_else(|e| panic!("plan failed: {e}: {sql}"));
        while let Some(row) = rows.next().await.unwrap() {
            out.push(row.get::<String>(3).unwrap());
        }
    } else {
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap_or_else(|e| panic!("plan failed: {e}: {sql}"));
        while let Some(row) = rows.next().await.unwrap() {
            out.push(row.get::<String>(3).unwrap());
        }
    }
    out.join(" | ")
}

fn plan_lines(plan: &str) -> Vec<String> {
    plan.split(" | ")
        .map(str::to_owned)
        .filter(|l| l.contains("INDEX") || l.contains("B-TREE") || l.contains("SCAN"))
        .collect()
}

/// The VDBE triple, read the way `common/plan_fixture::counts_of` reads it.
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

/// One run of `sql`, rows into `winners`. All fold statements report six
/// columns in the fixed order.
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

async fn time_and_winners(
    conn: &libsql::Connection,
    sql: &str,
    ts: Option<&str>,
) -> (Vec<f64>, Winners) {
    let mut winners = Winners::new();
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..WARMUPS {
        consume(conn, sql, ts, &mut winners).await;
        winners.clear();
    }
    for _ in 0..RUNS {
        let start = Instant::now();
        consume(conn, sql, ts, &mut winners).await;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    (sorted(times), winners)
}

fn sorted(mut times: Vec<f64>) -> Vec<f64> {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times
}

fn stat(times: &[f64]) -> String {
    format!(
        "best {:6.3}  med {:6.3}  mean {:6.3} (ms)",
        times[0],
        times[times.len() / 2],
        times.iter().sum::<f64>() / times.len() as f64
    )
}

/// Entities whose winners differ between two result rows.
fn delta(a: &Winners, b: &Winners) -> usize {
    a.iter()
        .filter(|(k, v)| b.get(k.as_str()) != Some(v))
        .count()
}

/// `(branch, entity)` concept pairs of the station: the population the write
/// maintenance walks, and the pool the batch statement draws `CHUNK` ids from.
async fn pairs_of(conn: &libsql::Connection) -> Vec<(String, String)> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT branch_id, entity_id FROM transaction_log \
             WHERE table_name = 'concepts'",
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

/// The write-side costs, priced on the station's own ledger: per pair (the
/// greatest start, one sought seek), and per 70-id batch (the same decision in
/// one prepared statement, measured as a whole).
async fn write_costs(conn: &libsql::Connection, label: &str, pairs: &[(String, String)]) {
    // Per write: the full pair list driven by one prepared statement.
    let mut per_times = Vec::with_capacity(RUNS);
    let stmt = conn.prepare(MAINTAIN_SEEK_ONE).await.unwrap();
    for _ in 0..RUNS {
        let start = Instant::now();
        for (branch, entity) in pairs {
            stmt.reset();
            let mut rows = stmt
                .query(libsql::params![entity.as_str(), branch.as_str()])
                .await
                .unwrap_or_else(|e| panic!("maintain seek failed for {entity}: {e}"));
            while let Some(row) = rows.next().await.unwrap() {
                let _greatest: Option<String> = row.get(0).unwrap();
            }
        }
        per_times.push(start.elapsed().as_secs_f64() * 1_000_000.0);
    }
    per_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let per_pair_us = per_times[per_times.len() / 2] / pairs.len().max(1) as f64;

    // Per batch: one prepared statement per repeat over CHUNK ids.
    let ids: Vec<String> = pairs
        .iter()
        .take(CHUNK)
        .map(|(_, id)| format!("\"{id}\""))
        .collect();
    let arr = format!("[{}]", ids.join(","));
    let mut batch_times = Vec::with_capacity(RUNS);
    let bstmt = conn.prepare(MAINTAIN_SEEK_BATCH).await.unwrap();
    for _ in 0..RUNS {
        bstmt.reset();
        let start = Instant::now();
        let mut rows = bstmt
            .query(libsql::params![arr.as_str(), "main"])
            .await
            .unwrap_or_else(|e| panic!("batch maintain failed: {e}"));
        let mut n = 0;
        while let Some(row) = rows.next().await.unwrap() {
            let _id: String = row.get(0).unwrap();
            let _greatest: Option<String> = row.get(1).unwrap();
            n += 1;
        }
        let expected = CHUNK.min(pairs.len());
        assert_eq!(n, expected, "the batch returned {n} of {expected}");
        batch_times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    batch_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{label}: per-write decision ≈ {per_pair_us:.1} µs/pair over {} pairs; \
         per {CHUNK}-id batch statement ≈ {} ms (median)",
        pairs.len(),
        batch_times[batch_times.len() / 2]
    );
}

/// The public fold's total, once per fixture, printed as a checksum row.
/// The heal epoch's writes: heal-count rows over the station's leading
/// entity ids, each a true healing write (its start is at or ahead of the
/// partition's greatest start, so the projection's own update would run) and
/// each stamped *after* the stability bound, so the bounded read at `ts_b`
/// cannot see it and the bound does the bounding.
///
/// `?1` count, `?2` valid_from, `?3` valid_to, `?4` recorded_at, `?5` the
/// full entity printf format (`c%03d` / `c%05d` — the prefix included; the
/// first version bound the digits only and healed foreign partitions).
const HEAL_INSERT: &str = r#"
    WITH RECURSIVE n(i) AS (
        SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?1
    )
    INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
    SELECT 'concepts', printf(?5, i), 'upsert',
           json_object('valid_from', ?2,
                       'valid_to', ?3,
                       'title', 'heal-' || i,
                       'content', '', 'retired', 0, 'extra', '{}'),
           ?4
    FROM n
"#;

/// The never-unmarked mark, asserted: the mark set before vs after the heals
/// must be identical, and separately, pairs that no longer *have* a
/// currently-disordered winner are recomputed for the printout.
const CURRENTLY_DISORDERED_COUNT: &str = "
    SELECT COUNT(*) FROM (
        SELECT entity_id, branch_id,
               json_extract(payload, '$.valid_from') AS vf,
               ROW_NUMBER() OVER (
                   PARTITION BY entity_id, branch_id ORDER BY seq_id DESC) AS rb,
               MAX(json_extract(payload, '$.valid_from')) OVER (
                   PARTITION BY entity_id, branch_id) AS maxvf
        FROM transaction_log
        WHERE table_name = 'concepts'
    ) WHERE rb = 1 AND vf < maxvf
";

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

struct Station {
    name: String,
    db: libsql::Database,
    /// True when the station's ledger itself carries backfills, so the
    /// disordered epoch is a claim about the data, not an empty set.
    residential_disorder: bool,
    /// The heal epoch's configuration, only where the ledger carries
    /// backfills to heal.
    heal: Option<HealSpec>,
}

#[derive(Clone, Copy)]
struct HealSpec {
    count: usize,
    valid_from: &'static str,
    recorded_at: &'static str,
    /// The stability read's bound: after the backfills, before the heal.
    ts_b: &'static str,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("Shape-5 probe — the rule carried by a write-maintained disorder table.");
    println!("runs {RUNS} after {WARMUPS} warmups; compare bounded rows to bounded rows;");
    println!("Δv is measured against s0 (today's fold) in the same epoch.");
    println!();

    // The probe directory exists before any fixture: the first fixture built
    // into it must survive.
    let dir = probe_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let heal_deep = HealSpec {
        count: 200,
        valid_from: "2026-03-01T00:00:00.000000Z",
        recorded_at: "2026-02-01T00:00:00.000000Z",
        ts_b: "2026-01-05T00:00:00.000000Z",
    };
    let heal_bench = HealSpec {
        count: BENCH_BACKFILLS,
        valid_from: "2026-06-01T00:00:00.000000Z",
        recorded_at: "2027-01-02T00:00:00.000000Z",
        ts_b: "2026-12-15T00:00:00.000000Z",
    };
    let mut stations: Vec<Station> = Vec::new();
    stations.push(Station {
        name: "mixed_starts (20,200 rows / 400 entities x 20 starts + 200 backfills)".into(),
        db: mixed_starts().await,
        residential_disorder: true,
        heal: Some(heal_deep),
    });
    for (name, file, links, backfills, disorder, heal) in [
        (
            "bench_healthy (5,000 concepts, real writes)",
            "bench_healthy.db",
            false,
            0usize,
            false,
            None,
        ),
        (
            "bench_disordered (5,000 concepts + 250 real backfills)",
            "bench_disordered.db",
            false,
            BENCH_BACKFILLS,
            true,
            Some(heal_bench),
        ),
        (
            "mixed_log (5,000 concepts + 5,000 links)",
            "mixed_log.db",
            true,
            0,
            false,
            None,
        ),
    ] {
        let path = probe_dir().join(file);
        let db = bench_ledger(&path, links, backfills).await;
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
            residential_disorder: disorder,
            heal,
        });
    }

    println!();
    for Station {
        name,
        db,
        residential_disorder,
        heal,
    } in &stations
    {
        // --- healthy epoch: the side table exists and is empty (the gate row).
        let (conn, writes) = (db.connect().unwrap(), db.connect().unwrap());
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
        println!("    rows {rows}, ts = {NOW}");

        writes.execute(DISORDER_TABLE, ()).await.unwrap();
        writes.execute("ANALYZE", ()).await.unwrap();

        let s0u = S0_HOT_FOLD.replace("{BOUND}", "");
        let s0b = S0_HOT_FOLD.replace("{BOUND}", "WHERE recorded_at <= ?1");
        let s5u = S5_FOLD.replace("{BOUND}", "");
        let s5b = S5_FOLD.replace("{BOUND}", "WHERE recorded_at <= ?1");

        let (_, w0b) = time_and_winners(&conn, &s0b, Some(NOW)).await;
        for (label, sql, bound) in [
            ("s0 healthy   unbounded", &s0u, false),
            ("s0 healthy   bounded$max", &s0b, true),
            ("s5-single    unbounded", &s5u, false),
            ("s5-single    bounded$max", &s5b, true),
        ] {
            let plan = plan_of(&conn, sql, bound).await;
            let counts = counts_of(&conn, sql).await;
            let (times, winners) = time_and_winners(&conn, sql, bound.then_some(NOW)).await;
            println!(
                "{label}  {}  {:?}  Δv vs s0-bounded {} (rows {})",
                stat(&times),
                counts,
                delta(&w0b, &winners),
                winners.len()
            );
            for line in plan_lines(&plan) {
                println!("      {line}");
            }
        }

        // The two-statement form on the healthy epoch: fold unchanged, the
        // probe answers no, nothing else runs. Its bounded row is the
        // gate row for this formulation.
        let (two_times, two_winners) =
            time_two_statement(&conn, &s0b, Some(NOW), DISORDER_CORRECTION_BOUNDED).await;
        println!(
            "s5-two       bounded$max  {}  Δv vs s0-bounded {} (rows {})",
            stat(&two_times),
            delta(&w0b, &two_winners),
            two_winners.len()
        );
        // And the probe alone, priced in µs: 1,000 runs median.
        let mut probe_us = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let start = Instant::now();
            for _ in 0..1000 {
                let mut rows = conn
                    .query(DISORDER_PROBE, ())
                    .await
                    .unwrap_or_else(|e| panic!("probe failed: {e}"));
                let _r: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
            }
            probe_us.push(start.elapsed().as_secs_f64() * 100.0);
        }
        probe_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "    disorder probe alone: {:.1} µs per 1,000 calls (median of {} sweeps)",
            probe_us[probe_us.len() / 2],
            RUNS
        );
        drop(writes);
        drop(conn);

        // --- disordered epoch: this ledger's true disorder, computed from the
        // log, filled into the side table, and the fold asked for the rule
        // through the correction arm.
        if *residential_disorder {
            let (conn, writes) = (db.connect().unwrap(), db.connect().unwrap());
            writes.execute(V24_INDEX_DESC, ()).await.unwrap();
            writes.execute(EVER_DISORDER, ()).await.unwrap();
            writes.execute("ANALYZE", ()).await.unwrap();

            let disorder_rows: i64 = conn
                .query("SELECT COUNT(*) FROM concept_disorder", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();

            let s0b2 = S0_HOT_FOLD.replace("{BOUND}", "WHERE recorded_at <= ?1");
            let s5u2 = S5_FOLD.replace("{BOUND}", "");
            let s5b2 = S5_FOLD_BOUNDED.to_string();

            let s1_rule_u = RULE_WINNERS.replace("{BOUND}", "");
            let (w0_times, w0b) = time_and_winners(&conn, &s0b2, Some(NOW)).await;
            let (_, w_rule) = time_and_winners(&conn, &s1_rule_u, None).await;
            println!(
                "s0 itself, disordered epoch  {}  (the epoch-bound reference for the rows below)",
                stat(&w0_times)
            );
            for (label, sql, bound) in [
                ("s5-single disordered unbounded", &s5u2, false),
                ("s5-single disordered bounded$max", &s5b2, true),
            ] {
                let plan = plan_of(&conn, sql, bound).await;
                let counts = counts_of(&conn, sql).await;
                let (times, winners) = time_and_winners(&conn, sql, bound.then_some(NOW)).await;
                println!(
                    "{label}  {}  {:?}  Δv vs s0 {} | rule-identical {} (Δ{}); disorder pairs {disorder_rows}",
                    stat(&times),
                    counts,
                    delta(&w0b, &winners),
                    winners == w_rule,
                    delta(&w_rule, &winners)
                );
                for line in plan_lines(&plan) {
                    println!("      {line}");
                }
            }

            // The two-statement form, disordered: fold + probe + correction
            // merge, one combined production cost path.
            let (two_times, two_winners) =
                time_two_statement(&conn, &s0b2, Some(NOW), DISORDER_CORRECTION).await;
            println!(
                "s5-two disordered bounded$max  {}  Δv vs s0 {} | rule-identical {} (Δ{})",
                stat(&two_times),
                delta(&w0b, &two_winners),
                two_winners == w_rule,
                delta(&w_rule, &two_winners)
            );
            // The correction statement alone (the delta on top of the fold).
            let corr_plan = plan_of(&conn, DISORDER_CORRECTION, false).await;
            let corr_counts = counts_of(&conn, DISORDER_CORRECTION).await;
            let (corr_times, _) = time_and_winners(&conn, DISORDER_CORRECTION, None).await;
            println!(
                "    correction alone ({} pairs)  {}  {:?}",
                disorder_rows,
                stat(&corr_times),
                corr_counts
            );
            for line in plan_lines(&corr_plan) {
                println!("      {line}");
            }
            drop(writes);
            drop(conn);
        }

        // --- heal epoch: write one later version over some ever-disordered
        // pairs, never unmarking, then ask the three cases the plan now
        // demands. (1) The bounded read at ts_b returns the same winners
        // before and after the heals — a later write must never change what
        // a past read answered. (2) The bounded read equals the bounded rule
        // window wherever it reads — the bounded identity.
        // (3) The never-clear economy: the correction stays owed for healed
        // pairs; the fold's cost does not go back to healthy.
        if let Some(h) = *heal {
            let (conn, writes) = (db.connect().unwrap(), db.connect().unwrap());
            // The station's entity-id spelling: the fixtures name ids as
            // 'c' || printf('%0Nd', i), so the format carries the prefix too.
            // (The first heal bound the digits only and wrote ids like
            // `00000` — stray partitions, not heals; caught a run later by
            // the drop assertion below.)
            let fmt = if name.contains("mixed_starts") {
                "c%03d"
            } else {
                "c%05d"
            };
            let s0b3 = S0_HOT_FOLD.replace("{BOUND}", "WHERE recorded_at <= ?1");
            let s1_rule_b3 = RULE_WINNERS.replace("{BOUND}", "WHERE recorded_at <= ?1");

            let marks_before: i64 = conn
                .query("SELECT COUNT(*) FROM concept_disorder", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();

            // Reads at ts_b *before* the heal exists.
            let (pre_times, two_pre) =
                time_two_statement(&conn, &s0b3, Some(h.ts_b), DISORDER_CORRECTION_BOUNDED).await;
            let (_, rule_pre) = time_and_winners(&conn, &s1_rule_b3, Some(h.ts_b)).await;

            // The heal writes: one later version per pair, stamped after ts_b
            // so the bounded read cannot see them, each start high enough that
            // the projection's own update would run (a true healing write).
            let healed: u64 = writes
                .execute(
                    HEAL_INSERT,
                    libsql::params![
                        h.count as i64,
                        h.valid_from,
                        "9999-12-31T23:59:59.999999Z",
                        h.recorded_at,
                        fmt,
                    ],
                )
                .await
                .unwrap_or_else(|e| panic!("heal insert failed: {e}"));
            writes.execute("ANALYZE", ()).await.unwrap();
            // Both handles go away before any post-heal read: a reader opened
            // before writes ran could miss what the writer added (libsql local
            // keeps per-connection state; the earlier DROP INDEX lesson's data
            // edition). Fresh handles see the healed ledger.
            drop(writes);
            drop(conn);
            let conn = db.connect().unwrap();

            let heals_on_disk: i64 = conn
                .query(
                    "SELECT COUNT(*) FROM transaction_log WHERE payload LIKE '%heal-%'",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();
            assert_eq!(
                heals_on_disk as u64, healed,
                "heal rows not readable on a fresh handle"
            );
            let marks_after: i64 = conn
                .query("SELECT COUNT(*) FROM concept_disorder", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();
            let still_disordered: i64 = conn
                .query(CURRENTLY_DISORDERED_COUNT, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();

            let (post_ts_times, two_ts_post) =
                time_two_statement(&conn, &s0b3, Some(h.ts_b), DISORDER_CORRECTION_BOUNDED).await;
            let (post_now_times, two_now) =
                time_two_statement(&conn, &s0b3, Some(NOW), DISORDER_CORRECTION_BOUNDED).await;
            let (_, rule_b_now) = time_and_winners(&conn, &s1_rule_b3, Some(NOW)).await;
            let (s0_now_times, w0b_now) = time_and_winners(&conn, &s0b3, Some(NOW)).await;

            // (1) stability: a later write changed nothing at ts_b.
            assert_eq!(two_pre, two_ts_post, "a later write changed a past read");
            // (2) the bounded identity holds before and after the heals.
            assert_eq!(
                two_pre, rule_pre,
                "the bounded identity broke before the heals"
            );
            assert_eq!(
                two_now, rule_b_now,
                "the bounded identity broke on a healed ledger"
            );
            // (3) never-unmarking: a later write must not change what a past
            // read answered.
            assert_eq!(marks_before, marks_after, "marks were cleared by heals");
            // The heals must land on the disordered partitions themselves: a
            // healed pair's newest row is its greatest-start row, so every
            // heal takes exactly one pair out of currently-disordered. (The
            // first heal run broke this silently: its rows went to stranger
            // entities and the count stood.)
            assert_eq!(
                still_disordered as u64,
                marks_before as u64 - healed,
                "heal rows did not heal the disordered partitions"
            );

            println!(
                "  heal epoch: {healed} heal rows written over {} pairs (fmt {fmt}); \
                 ever-disorder marks {marks_before} -> {marks_after} (must not shrink); \
                 currently disordered: {marks_before} -> {still_disordered}",
                h.count
            );
            println!("  heal epoch  bounded read at ts_b = {}", h.ts_b);
            println!(
                "    two-statement before heal {} ; after heal {} ; Δ pre/post {} ✓ (rule-identical Δ{})",
                stat(&pre_times),
                stat(&post_ts_times),
                delta(&two_pre, &two_ts_post),
                delta(&rule_pre, &two_ts_post)
            );
            println!(
                "    after the heal at NOW: two-statement {} ; today's fold {} ; → two{} s0 (correction stays owed: {} pairs)",
                stat(&post_now_times),
                stat(&s0_now_times),
                if delta(&two_now, &w0b_now) > 0 { "≠" } else { "=" },
                marks_after
            );
            // The arm's own cost and plan on the healed ledger: the healing
            // epoch's ANALYZE is a planner-world change, and the shape's
            // production promise is plan-pinned statements.
            let (corr_healed_times, _) =
                time_and_winners(&conn, DISORDER_CORRECTION_BOUNDED, Some(NOW)).await;
            let corr_healed_plan = plan_of(&conn, DISORDER_CORRECTION_BOUNDED, true).await;
            println!(
                "    correction alone (healed ledger, bounded) {}  {:?}",
                stat(&corr_healed_times),
                counts_of(&conn, DISORDER_CORRECTION_BOUNDED).await
            );
            for line in plan_lines(&corr_healed_plan) {
                println!("      {line}");
            }
        }

        // Write-side prices, after the fold rows of both epochs.
        {
            let conn = db.connect().unwrap();
            write_costs(&conn, "  write-maintenance", &pairs_of(&conn).await).await;
        }
        println!();
    }
}
