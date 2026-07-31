//! T4.2 reconnaissance: which declared index does each query actually get?
//!
//! Scratch probe. The assertions it produced live in
//! `tests/index_plan_tests.rs`; this stays so the sweep can be re-run when an
//! index or a query changes.
//!
//! Run with:  cargo run --example index_coverage_probe

use macrame::prelude::*;

const PROBES: &[(&str, &str)] = &[
    (
        "fold: recorded_at window",
        "SELECT seq_id, table_name, entity_id, operation, payload FROM transaction_log \
         WHERE recorded_at <= ?1",
    ),
    (
        "archive: oldest hot stamp",
        "SELECT MIN(recorded_at) FROM transaction_log WHERE recorded_at < ?1",
    ),
    (
        "archive: log archivable EXISTS",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
           SELECT 1 FROM transaction_log newer \
           WHERE newer.entity_id = transaction_log.entity_id \
             AND newer.seq_id > transaction_log.seq_id)",
    ),
    (
        "as_of: latest per entity",
        "SELECT entity_id, seq_id, payload FROM ( \
           SELECT entity_id, seq_id, payload, \
                  ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) as rn \
           FROM transaction_log WHERE recorded_at <= ?1) WHERE rn = 1",
    ),
    (
        "annotations by label",
        "SELECT concept_id, value FROM analytics_annotations WHERE label = ?1",
    ),
    (
        "in-edges by target",
        "SELECT source_id FROM links_current WHERE target_id = ?1 AND valid_to = ?2",
    ),
    (
        "traversal recursive step",
        "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
           AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
    ),
    (
        "overlap guard",
        "SELECT valid_from, valid_to FROM links_current \
         WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 AND valid_from <> ?4",
    ),
];

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open_with_cadence(&dir.path().join("p.db"), None)
        .await
        .unwrap();
    let conn = db.read_conn();

    for (label, sql) in PROBES {
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            plan.push(r.get::<String>(3).unwrap());
        }
        println!("{label:>32}  {}", plan.join(" | "));
    }
}
