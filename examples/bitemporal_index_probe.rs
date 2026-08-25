//! W10.6 reconnaissance: what does a cross-axis read cost, and can an index
//! serve it? (F-33)
//!
//! W7.1 made bitemporal predicates expressible — a traversal can state a
//! valid-time instant and a transaction-time instant at once. F-33 is that
//! nobody had asked what that costs, and that the answer the literature is
//! usually cited for (an R\*Tree over the two axes) does not fit this schema.
//! **The plan says measure before building**, and this is the measurement.
//!
//! The fixture is a real `Database` written through the public API rather than
//! hand-rolled rows: the transaction-time half reads `transaction_log`, and the
//! log is only populated the way production populates it if the writes go
//! through the actor. `analyze()` is the crate's own, so the statistics are the
//! ones a caller gets.
//!
//! Run with:  cargo run --example bitemporal_index_probe

use libsql::Builder;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const VALID_AT: &str = "2026-06-01T00:00:00.000000Z";
const RECORDED_AT: &str = "2026-06-01T00:00:00.000000Z";

/// One hub of 150 against 60 leaves, the shape `tests/common/plan_fixture.rs`
/// uses, so the two measurements are about the same graph.
const HUB_EDGES: usize = 150;
const LEAF_EDGES: usize = 60;
const CONCEPTS: usize = 260;

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open_with_cadence(&dir.path().join("p.db"), None)
        .await
        .unwrap();

    let concepts: Vec<ConceptUpsert> = (0..CONCEPTS)
        .map(|i| ConceptUpsert::new(format!("c{i:04}"), "N").valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();

    let mut edges = Vec::new();
    for i in 1..=HUB_EDGES {
        edges.push(EdgeAssertion::new("c0000", format!("c{i:04}"), "LINKS").valid_from(TS));
    }
    for i in (HUB_EDGES + 1)..=(HUB_EDGES + LEAF_EDGES) {
        edges.push(
            EdgeAssertion::new(format!("c{i:04}"), format!("c{:04}", i + 1), "LINKS")
                .valid_from(TS),
        );
    }
    db.write_bulk_atomic(edges).await.unwrap();
    db.analyze().await.unwrap();

    let conn = db.read_conn();
    let log_rows: i64 = {
        let mut r = conn
            .query("SELECT COUNT(*) FROM transaction_log", ())
            .await
            .unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };
    println!("transaction_log rows: {log_rows}");

    let arms: [(&str, TraversalBuilder); 3] = [
        (
            "valid time only",
            TraversalBuilder::new("c0000")
                .max_depth(2)
                .as_of_valid(VALID_AT),
        ),
        (
            "transaction time only",
            TraversalBuilder::new("c0000")
                .max_depth(2)
                .as_of_recorded(RECORDED_AT),
        ),
        (
            "both axes",
            TraversalBuilder::new("c0000")
                .max_depth(2)
                .as_of_valid(VALID_AT)
                .as_of_recorded(RECORDED_AT),
        ),
    ];

    for (label, walk) in arms {
        let sql = walk.build_sql();
        println!("\n===== {label} =====");

        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            println!("  {}", r.get::<String>(3).unwrap());
        }

        let mut r = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
        let (mut opens, mut seeks, mut rewinds, mut sorts) = (0usize, 0usize, 0usize, 0usize);
        while let Some(row) = r.next().await.unwrap() {
            let op: String = row.get(1).unwrap();
            if op == "OpenRead" {
                opens += 1;
            } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
                seeks += 1;
            } else if op == "Rewind" || op == "Last" {
                rewinds += 1;
            } else if op == "SorterOpen" || op == "SorterSort" {
                sorts += 1;
            }
        }
        println!("  opens={opens} seeks={seeks} rewinds={rewinds} sorter_ops={sorts}");
    }

    // Option 1 from the plan, made concrete: a one-dimensional index per
    // temporal domain. The transaction-time domain has a column and is already
    // indexed. The valid-time domain, *in the log*, does not -- `valid_from`
    // exists only inside the JSON payload -- so the only shape option 1 can
    // take here is an expression index over `json_extract`.
    // The cost the plans above are actually dominated by is not a temporal
    // predicate at all: it is `ROW_NUMBER() OVER (PARTITION BY entity_id ORDER
    // BY seq_id DESC)`, which sorts the whole `recorded_at`-bounded slice. This
    // arm asks whether the ordering could come from an index instead -- the
    // only lever any of the plan's three options could pull that the arms above
    // do not already reach.
    println!(
        "
===== the fold's window, and where its ordering comes from ====="
    );
    for (label, from) in [
        ("planner's choice", "transaction_log"),
        (
            "forced onto the entity index",
            "transaction_log INDEXED BY idx_txlog_entity",
        ),
    ] {
        let sql = format!(
            "SELECT payload, ROW_NUMBER() OVER (PARTITION BY entity_id              ORDER BY seq_id DESC) AS rn FROM {from}              WHERE table_name = 'links' AND recorded_at <= ?1"
        );
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            plan.push(r.get::<String>(3).unwrap());
        }
        println!("  {label:>28}: {}", plan.join(" | "));
    }

    //
    // A second, writable connection: the handle's own reader is read-only by
    // construction, and the point of the arm is a schema change the crate does
    // not ship.
    let raw = Builder::new_local(dir.path().join("p.db"))
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    for ddl in [
        "CREATE INDEX probe_txlog_valid_from ON transaction_log (json_extract(payload, '$.valid_from'))",
        "CREATE INDEX probe_txlog_two_d ON transaction_log (recorded_at, json_extract(payload, '$.valid_from'))",
    ] {
        raw.execute(ddl, ()).await.unwrap();
    }
    let _ = raw.query("ANALYZE", ()).await.unwrap();

    let both = TraversalBuilder::new("c0000")
        .max_depth(2)
        .as_of_valid(VALID_AT)
        .as_of_recorded(RECORDED_AT)
        .build_sql();
    println!("\n===== both axes, with the candidate indexes present =====");
    let mut rows = raw
        .query(&format!("EXPLAIN QUERY PLAN {both}"), ())
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        println!("  {}", r.get::<String>(3).unwrap());
    }

    drop(raw);
    db.close().await.unwrap();
}
