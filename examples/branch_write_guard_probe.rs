//! D-271's open finding: why does asserting an edge **on a branch** cost about
//! a thousand times the same assertion on the trunk, and which of the two costs
//! in the plan is the one to repair?
//!
//! 0.15.28 measured the symptom and stopped there on purpose. The same 200-edge
//! batch cost **55 ms on `main` and 75 s on a fork of it** against a 2,000-edge
//! trunk, and the plan named two suspects at once: the log arm of
//! `graph::lineage::links_cut_cte` reading `SEARCH transaction_log USING INDEX
//! idx_txlog_fold_partition (table_name=?)` — the whole links log, once per
//! asserted row — and an `AUTOMATIC PARTIAL COVERING INDEX` SQLite built over
//! the `churned` CTE on every execution. A repair that guesses between them is
//! a repair that cannot be argued for afterwards.
//!
//! # What it reports
//!
//! 1. **The fixture** — trunk edges, log rows, and how many of them carry
//!    `table_name = 'links'`, because the arm's cost is that count and not the
//!    batch's.
//! 2. **The plan, per candidate spelling**, from `EXPLAIN QUERY PLAN` with the
//!    same parameters the write path binds.
//! 3. **The guard alone**, timed: the resolved statement run once per edge key,
//!    which is exactly what `write_edges_atomic` does per row, without the
//!    insert, the triggers or the actor around it.
//! 4. **The trunk guard** beside it, because the repair must not move that one:
//!    the same statement compiles for every write in the crate.
//! 5. **End to end**, `bulk_import` of one batch on the trunk and on a fork,
//!    which is the figure D-271 quotes.
//!
//! # The candidates
//!
//! - **shipped** — what `overlap_candidates_resolved` emits today, copied.
//! - **cross** — the same statement with the log arm's join order forced:
//!   `churned` drives and `transaction_log` is the inner loop. `CROSS JOIN` is
//!   SQLite's documented way to say so.
//! - **indexed** — the shipped order with `INDEXED BY idx_txlog_entity`, which
//!   is the access path this arm had before 0.15.12 added
//!   `idx_txlog_fold_partition` and the planner moved.
//! - **materialized** — `churned AS MATERIALIZED`, which
//!   `overlap_candidates_resolved`'s rustdoc records as measured and refused in
//!   0.15.8. It is re-run here because it was refused against a table that did
//!   not yet carry the index the planner now prefers, and a refusal is only
//!   good against the schema it was measured on.
//!
//! # The reproduced-query hazard
//!
//! `overlap_candidates_resolved` is `pub(crate)`, so the four texts below are
//! **copies** and a copy can outlive its original — the same hazard
//! `txlog_fold_index_probe` carries and `index_plan_tests` bounds. The
//! end-to-end arm does not share it: it goes through `Database::bulk_import`,
//! which runs the real text.
//!
//! Run it:
//!
//! ```text
//! cargo run --release --example branch_write_guard_probe
//! cargo run --release --example branch_write_guard_probe -- --trunk 8000 --batch 200
//! cargo run --release --example branch_write_guard_probe -- --skip-end-to-end
//! ```

use std::time::Instant;

use libsql::Value;
use macrame::branch::BranchId;
use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database};

const GENESIS: &str = "2020-01-01T00:00:00.000000Z";

/// The three CTEs the four candidates share, verbatim from the generator.
const HEAD: &str = r#"WITH RECURSIVE lineage(branch_id, dist, cutoff) AS (VALUES (?6, ?7, ?8), (?9, ?10, ?11)),
"#;

const CHURNED: &str = r#"churned(entity_id, branch_id, cutoff) AS (
    SELECT lc.source_id || '|' || lc.target_id || '|' || lc.edge_type || '|' || lc.valid_from,
           lc.branch_id, g.cutoff
    FROM links_current lc
    JOIN lineage g ON g.branch_id = lc.branch_id
    WHERE lc.source_id = ?1 AND lc.target_id = ?2 AND lc.edge_type = ?3 AND g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff
),
"#;

const CHURNED_MAT: &str = r#"churned(entity_id, branch_id, cutoff) AS MATERIALIZED (
    SELECT lc.source_id || '|' || lc.target_id || '|' || lc.edge_type || '|' || lc.valid_from,
           lc.branch_id, g.cutoff
    FROM links_current lc
    JOIN lineage g ON g.branch_id = lc.branch_id
    WHERE lc.source_id = ?1 AND lc.target_id = ?2 AND lc.edge_type = ?3 AND g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff
),
"#;

/// `links_cut` up to the log arm's `FROM`, which is the only part that differs.
const CUT_OPEN: &str = r#"links_cut(source_id, target_id, edge_type, valid_from, valid_to, weight, properties, branch_id) AS (
    SELECT lc.source_id, lc.target_id, lc.edge_type, lc.valid_from, lc.valid_to,
           lc.weight, lc.properties, lc.branch_id
    FROM links_current lc
    JOIN lineage g ON g.branch_id = lc.branch_id
    WHERE lc.source_id = ?1 AND lc.target_id = ?2 AND lc.edge_type = ?3 AND (g.cutoff IS NULL OR lc.recorded_at <= g.cutoff)
    UNION ALL
    SELECT json_extract(payload, '$.source_id'),
           json_extract(payload, '$.target_id'),
           json_extract(payload, '$.edge_type'),
           json_extract(payload, '$.valid_from'),
           json_extract(payload, '$.valid_to'),
           json_extract(payload, '$.weight'),
           json_extract(payload, '$.properties'),
           branch_id
    FROM (
        SELECT transaction_log.payload, transaction_log.branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY transaction_log.entity_id, transaction_log.branch_id
                   ORDER BY transaction_log.seq_id DESC
               ) AS rn
"#;

const CUT_CLOSE: &str = r#"    ) WHERE rn = 1
),
"#;

/// The shipped log-arm source: `transaction_log` outer, `churned` inner.
const ARM_SHIPPED: &str = r#"        FROM transaction_log
        JOIN churned k ON k.entity_id = transaction_log.entity_id
                      AND k.branch_id = transaction_log.branch_id
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= k.cutoff
"#;

/// The same relation with the loops the other way round. `CROSS JOIN` does not
/// change the answer — `links_current`'s primary key makes `churned` unique on
/// `(entity_id, branch_id)`, so the join is one-to-many in exactly one
/// direction — it only stops SQLite reordering it.
const ARM_CROSS: &str = r#"        FROM churned k
        CROSS JOIN transaction_log ON transaction_log.entity_id = k.entity_id
                      AND transaction_log.branch_id = k.branch_id
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= k.cutoff
"#;

/// The pre-0.15.12 access path, asked for by name.
const ARM_INDEXED: &str = r#"        FROM transaction_log INDEXED BY idx_txlog_entity
        JOIN churned k ON k.entity_id = transaction_log.entity_id
                      AND k.branch_id = transaction_log.branch_id
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= k.cutoff
"#;

const VISIBLE: &str = r#"visible(source_id, target_id, edge_type, valid_from, valid_to, weight, properties, branch_id) AS (
    SELECT source_id, target_id, edge_type, valid_from, valid_to, weight, properties, branch_id FROM (
        SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.weight,
               l.properties, l.branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY l.source_id, l.target_id, l.edge_type, l.valid_from
                   ORDER BY g.dist
               ) AS rn
        FROM links_cut l
        JOIN lineage g ON g.branch_id = l.branch_id
    ) WHERE rn = 1
)
SELECT l.valid_from, l.valid_to FROM visible l WHERE l.valid_from <> ?4"#;

/// The trunk's guard, which is not a resolution at all.
const TRUNK_GUARD: &str =
    "SELECT l.valid_from, l.valid_to FROM links_current l \
     WHERE l.valid_from <> ?4 AND l.source_id = ?1 AND l.target_id = ?2 AND l.edge_type = ?3";

fn resolved(churned: &str, arm: &str) -> String {
    format!("{HEAD}{churned}{CUT_OPEN}{arm}{CUT_CLOSE}{VISIBLE}")
}

fn edge(i: usize) -> EdgeAssertion {
    EdgeAssertion::new("hub", format!("t{i}"), "LINKS")
        .valid_from(format!("2026-01-01T00:00:00.{:06}Z", i % 1_000_000))
        .valid_to(format!("2026-01-01T00:00:00.{:06}Z", (i % 1_000_000) + 1))
}

/// The eleven values `write_edges_atomic` binds, for the `i`th key of the batch.
fn params(i: usize, branch: &str, cutoff: &str) -> Vec<Value> {
    vec![
        Value::Text("hub".into()),
        Value::Text(format!("t{i}")),
        Value::Text("LINKS".into()),
        Value::Text(format!("2026-01-01T00:00:00.{:06}Z", i % 1_000_000)),
        Value::Text(branch.into()),
        Value::Text(branch.into()),
        Value::Integer(0),
        Value::Null,
        Value::Text("main".into()),
        Value::Integer(1),
        Value::Text(cutoff.into()),
    ]
}

async fn plan(conn: &libsql::Connection, sql: &str, p: Vec<Value>) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), p)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(3).unwrap());
    }
    out
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap()
}

/// Milliseconds to run the guard once per key of a `batch`-sized batch, which
/// is the per-row cost `write_edges_atomic` pays and nothing else.
async fn time_guard(
    conn: &libsql::Connection,
    sql: &str,
    batch: usize,
    branch: &str,
    cutoff: &str,
) -> f64 {
    let stmt_start = Instant::now();
    let stmt = conn.prepare(sql).await.unwrap();
    let compile = stmt_start.elapsed().as_secs_f64() * 1e3;
    let start = Instant::now();
    for i in 0..batch {
        let mut rows = stmt.query(params(i, branch, cutoff)).await.unwrap();
        while rows.next().await.unwrap().is_some() {}
        stmt.reset();
    }
    let ms = start.elapsed().as_secs_f64() * 1e3;
    println!("      (compile {compile:.3} ms)");
    ms
}

/// `bulk_import` of one batch on the trunk and on a fork of it, through the
/// public call, so this arm measures the shipped statement and not a copy.
async fn run_end_to_end(
    db: &Database,
    trunk: usize,
    batch: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("end to end, bulk_import of one {batch}-edge batch");
    let start = Instant::now();
    db.bulk_import((trunk..trunk + batch).map(edge).collect())
        .await?;
    println!(
        "  {:>12}  {:>12.2} ms",
        "main",
        start.elapsed().as_secs_f64() * 1e3
    );
    let start = Instant::now();
    db.bulk_import(
        (trunk..trunk + batch)
            .map(|i| edge(i).on_branch(BranchId::new("alt").unwrap()))
            .collect(),
    )
    .await?;
    println!(
        "  {:>12}  {:>12.2} ms",
        "alt",
        start.elapsed().as_secs_f64() * 1e3
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let trunk = flag("--trunk", 2_000);
    let batch = flag("--batch", 200);
    let skip_end = args.iter().any(|a| a == "--skip-end-to-end");
    let skip_guards = args.iter().any(|a| a == "--skip-guards");

    let dir = tempfile::tempdir()?;
    let db = Database::open(dir.path().join("probe.db")).await?;

    println!("branch write guard, trunk {trunk} edges, batch {batch} edges\n");

    db.upsert_concept(ConceptUpsert::new("hub", "hub").valid_from(GENESIS))
        .await?;
    for i in 0..(trunk + batch) {
        db.upsert_concept(ConceptUpsert::new(format!("t{i}"), "t").valid_from(GENESIS))
            .await?;
    }
    // The trunk the branch will inherit. The batch's keys start above it, so
    // nothing in the batch is a second write to a key: this measures the guard
    // finding nothing, which is the ordinary case and the expensive one.
    db.bulk_import((0..trunk).map(edge).collect()).await?;
    db.fork(BranchId::new("alt")?, BranchId::new("main")?)
        .await?;

    let conn = db.read_conn();
    let cutoff: String = conn
        .query("SELECT forked_at FROM branches WHERE branch_id = 'alt'", ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;

    println!("fixture");
    println!(
        "  links_current {:>8}   transaction_log {:>8}   of those table_name='links' {:>8}",
        scalar(conn, "SELECT count(*) FROM links_current").await,
        scalar(conn, "SELECT count(*) FROM transaction_log").await,
        scalar(
            conn,
            "SELECT count(*) FROM transaction_log WHERE table_name = 'links'"
        )
        .await,
    );
    println!("  fork cutoff {cutoff}\n");

    let candidates: Vec<(&str, String)> = vec![
        ("shipped", resolved(CHURNED, ARM_SHIPPED)),
        ("cross", resolved(CHURNED, ARM_CROSS)),
        ("indexed", resolved(CHURNED, ARM_INDEXED)),
        ("materialized", resolved(CHURNED_MAT, ARM_SHIPPED)),
        ("mat+cross", resolved(CHURNED_MAT, ARM_CROSS)),
    ];

    if skip_guards {
        // The end-to-end arm alone, for the before/after pair: it runs the real
        // statement through `bulk_import`, so it is the half of this probe that
        // measures the tree rather than a copy.
        run_end_to_end(&db, trunk, batch).await?;
        db.close().await?;
        return Ok(());
    }

    println!("plans (branch shape, the parameters the write path binds)");
    for (name, sql) in &candidates {
        println!("  {name}");
        for step in plan(conn, sql, params(0, "alt", &cutoff)).await {
            println!("    {step}");
        }
    }
    println!("  trunk");
    for step in plan(conn, TRUNK_GUARD, params(0, "main", &cutoff)).await {
        println!("    {step}");
    }
    println!();

    println!("the guard alone, {batch} executions (one per row of the batch)");
    for (name, sql) in &candidates {
        let ms = time_guard(conn, sql, batch, "alt", &cutoff).await;
        println!(
            "  {name:>12}  {ms:>12.2} ms   {:>9.4} ms/row",
            ms / batch as f64
        );
    }
    let ms = time_guard(conn, TRUNK_GUARD, batch, "main", &cutoff).await;
    println!(
        "  {:>12}  {ms:>12.2} ms   {:>9.4} ms/row",
        "trunk",
        ms / batch as f64
    );
    println!();

    if !skip_end {
        run_end_to_end(&db, trunk, batch).await?;
    }

    db.close().await?;
    Ok(())
}
