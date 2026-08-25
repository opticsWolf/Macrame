//! W10.1 reconnaissance: what runtime counters does the bundled engine expose,
//! and what do the pinned queries cost in cursors and seeks?
//!
//! Scratch probe. The assertions it produced live in
//! `tests/operation_count_tests.rs`; this stays so the sweep can be re-run when
//! a query, an index or the vendored engine changes — which is what that file's
//! failure message tells you to do.
//!
//! Two questions, in order:
//!
//! 1. **Is there a rows-scanned counter?** `PRAGMA compile_options` answers it.
//!    `sqlite3_stmt_scanstatus` needs `SQLITE_ENABLE_STMT_SCANSTATUS`, and the
//!    list this prints is the evidence for whichever way that goes.
//! 2. **What does `EXPLAIN` say each pinned query does?** The VDBE program is a
//!    deterministic function of the chosen plan, so cursors opened, seeks
//!    issued and b-tree rewinds are integers that move when the plan does and
//!    not when the machine is busy.
//!
//! It reads the **same** fixture the gate does, by including the test module
//! rather than copying it: a probe measuring a different database from the one
//! the numbers are pinned against would produce a sweep nobody can act on.
//!
//! Run with:  cargo run --example opcode_probe

#[path = "../tests/common/plan_fixture.rs"]
mod plan_fixture;

/// The six queries `tests/index_plan_tests.rs` pins, plus one that must scan.
const PROBES: &[(&str, &str)] = &[
    (
        "traversal recursive step",
        "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
         AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
    ),
    (
        "overlap guard",
        "SELECT valid_from, valid_to FROM links_current \
         WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
           AND valid_from <> ?4",
    ),
    (
        "fold: recorded_at window",
        "SELECT seq_id, table_name, entity_id, operation, payload \
         FROM transaction_log WHERE recorded_at <= ?1",
    ),
    (
        "archive: supersession test",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
           SELECT 1 FROM transaction_log newer \
           WHERE newer.entity_id = transaction_log.entity_id \
             AND newer.seq_id > transaction_log.seq_id)",
    ),
    (
        "archive: links cutoff",
        "SELECT source_id, target_id FROM links WHERE recorded_at < ?1 AND ( \
           EXISTS ( \
             SELECT 1 FROM links newer \
             WHERE newer.source_id = links.source_id \
               AND newer.target_id = links.target_id \
               AND newer.edge_type = links.edge_type \
               AND newer.valid_from = links.valid_from \
               AND newer.recorded_at > links.recorded_at) \
           OR (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= ?1))",
    ),
    (
        "archive: concept reverse reachability",
        "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1 \
         AND valid_to < ?1 AND NOT EXISTS ( \
           SELECT 1 FROM links WHERE links.source_id = concepts.id \
              OR links.target_id = concepts.id)",
    ),
    (
        "CONTROL: no index serves it",
        "SELECT id FROM concepts WHERE content LIKE ?1",
    ),
    // Same plan as the first row, one column the covering index does not carry.
    // The pair is the point: `EXPLAIN QUERY PLAN` cannot tell them apart.
    (
        "traversal step, uncovered",
        "SELECT l.target_id, l.properties FROM links_current l \
         WHERE l.source_id = ?1 AND l.valid_from <= ?3 AND ?3 < l.valid_to \
           AND l.weight >= ?4",
    ),
];

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = plan_fixture::populated_and_analysed(&dir.path().join("p.db")).await;

    let mut rows = conn.query("PRAGMA compile_options", ()).await.unwrap();
    let mut opts = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        opts.push(r.get::<String>(0).unwrap());
    }
    println!("compile_options ({}):", opts.len());
    for o in &opts {
        println!("  {o}");
    }
    println!(
        "\nSTMT_SCANSTATUS: {}",
        if opts.iter().any(|o| o.contains("STMT_SCANSTATUS")) {
            "present"
        } else {
            "ABSENT -- no rows-scanned counter exists on this engine"
        }
    );

    println!(
        "\n{:>40}  {:>5} {:>5} {:>7}  plan",
        "query", "opens", "seeks", "rewinds"
    );
    for (label, sql) in PROBES {
        let mut r = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
        let (mut opens, mut seeks, mut rewinds) = (0usize, 0usize, 0usize);
        while let Some(row) = r.next().await.unwrap() {
            let op: String = row.get(1).unwrap();
            if op == "OpenRead" {
                opens += 1;
            } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
                seeks += 1;
            } else if op == "Rewind" || op == "Last" {
                rewinds += 1;
            }
        }
        let plan = plan_fixture::plan_of(&conn, sql).await;
        println!("{label:>40}  {opens:>5} {seeks:>5} {rewinds:>7}  {plan}");
    }
}
