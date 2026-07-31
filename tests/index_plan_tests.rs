//! Plan-pinning as a category, not as a reaction (T4.2, D-089).
//!
//! D-042, D-059 and D-064 are one bug three times: **a covering index captures
//! a query because it contains the columns, not because it discriminates**. Each
//! was found by measurement after it shipped, and each produced one more
//! `EXPLAIN`-asserting test written reactively at the place it hurt. Three
//! instances is a category.
//!
//! So this file inverts the direction. Rather than "here is a query we were
//! burned by, assert its plan", it holds a **registry keyed by index**: every
//! entry in `ddl::CREATE_INDICES` names the query that justifies its existence,
//! and [`every_index_is_justified`] fails if an index is added without one.
//!
//! That direction matters because it catches the failure the reactive tests
//! cannot. A query that quietly leaves its index is caught by an assertion on
//! that query — which is what the reactive tests do, and they are kept. An index
//! that no query ever seeks on is invisible to every such assertion, because
//! there is no query to write one against. It is pure cost: an index write on
//! every insert into its table, forever, and nothing reads it.
//!
//! **Running the registry found exactly that, twice** — see
//! [`the_unread_indices_are_the_two_already_known`].
//!
//! # The reproduced-query hazard, and what bounds it
//!
//! Most of these queries are private `const`s or trigger bodies, neither of
//! which `EXPLAIN QUERY PLAN` can reach, so the registry holds copies. A copy
//! can outlive its original and go on proving something about a query nobody
//! runs — the same defect one layer up. `migration_tests` bounds its copies by
//! checking the trigger DDL still contains the predicate; this file bounds its
//! own with [`every_reproduced_query_still_exists_in_its_source`], which
//! `include_str!`s the module each query came from and looks for a fragment of
//! it. Compile-time, no API change, and it goes red when the original moves.

mod harness;

use harness::TestHarness;
use libsql::Builder;
use macrame::schema::{ddl, migrations};

/// Why an index exists.
enum Justification {
    /// A query in the crate seeks on it. `sql` is reproduced for `EXPLAIN`;
    /// `source` and `fragment` bound the copy (see the module note).
    Query {
        label: &'static str,
        sql: &'static str,
        source: Option<(&'static str, &'static str)>,
    },
    /// Nothing in the crate reads it. Recorded rather than removed: dropping an
    /// index is a schema change and needs a migration rung, which is not a
    /// 0.6.0 hardening item. See D-089.
    NoReader { why: &'static str },
}

use Justification::{NoReader, Query};

const REGISTRY: &[(&str, Justification)] = &[
    (
        "idx_lc_traversal_cover",
        Query {
            label: "the traversal CTE's recursive step",
            // Deeper assertions on this one — that it stays *covering*, with and
            // without an edge-type filter, against the exact string
            // `TraversalBuilder::build_sql` emits — live in `migration_tests`
            // (D-042, D-064). This entry exists so the registry is complete.
            sql: "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
                  AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
            source: None,
        },
    ),
    (
        "idx_lc_open_interval",
        Query {
            label: "the overlap guard and the single-open probe",
            // Likewise: `migration_tests` asserts the column-binding depth that
            // D-059 exists for.
            sql: "SELECT valid_from, valid_to FROM links_current \
                  WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
                    AND valid_from <> ?4",
            source: None,
        },
    ),
    (
        "idx_txlog_time",
        Query {
            label: "the fold's recorded_at window",
            sql: "SELECT seq_id, table_name, entity_id, operation, payload \
                  FROM transaction_log WHERE recorded_at <= ?1",
            source: Some((
                include_str!("../src/temporal/replay.rs"),
                "FROM transaction_log\n        WHERE recorded_at <= ?1",
            )),
        },
    ),
    (
        "idx_txlog_entity",
        Query {
            label: "the archive's supersession test",
            sql: "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
                    SELECT 1 FROM transaction_log newer \
                    WHERE newer.entity_id = transaction_log.entity_id \
                      AND newer.seq_id > transaction_log.seq_id)",
            source: Some((
                include_str!("../src/temporal/archive.rs"),
                "newer.entity_id = transaction_log.entity_id",
            )),
        },
    ),
    (
        "idx_annotations_label",
        NoReader {
            why: "nothing in the crate selects from analytics_annotations at all. \
                  The table is written by `write_analytics_annotations` and read \
                  only by callers, through their own connection.",
        },
    ),
    (
        "idx_lc_tgt_active",
        NoReader {
            why: "no query in the crate seeks on target_id as a leading column. \
                  Every links_current predicate leads on source_id (the walk) or \
                  binds all three key columns (the guards); `Subgraph::in_adj` is \
                  built in Rust from the forward rows the walk already returned.",
        },
    ),
];

async fn migrated(harness: &TestHarness) -> libsql::Connection {
    let db = Builder::new_local(&harness.db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();
    conn
}

async fn plan_of(conn: &libsql::Connection, sql: &str) -> String {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut lines = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        lines.push(r.get::<String>(3).unwrap());
    }
    lines.join(" | ")
}

/// Every declared index appears in the registry, and nothing else does.
///
/// The test that makes this a category. Adding an index without stating what
/// reads it is now a red test rather than a line of DDL nobody revisits — and
/// D-059's own note argues the cost explicitly ("a fourth index write per
/// assertion"), so an unjustified index is a known price paid for an unknown
/// return.
#[test]
fn every_index_is_justified() {
    let declared: Vec<String> = ddl::CREATE_INDICES
        .iter()
        .map(|sql| {
            let after = sql.split("IF NOT EXISTS ").nth(1).expect("index DDL shape");
            after.split_whitespace().next().unwrap().to_string()
        })
        .collect();

    for name in &declared {
        assert!(
            REGISTRY.iter().any(|(n, _)| n == name),
            "{name} is declared in ddl::CREATE_INDICES and has no registry entry. \
             State the query that seeks on it, or record it as NoReader — see D-089."
        );
    }
    for (name, _) in REGISTRY {
        assert!(
            declared.iter().any(|d| d == name),
            "{name} is in the registry and no longer declared; drop the entry"
        );
    }
    assert_eq!(declared.len(), REGISTRY.len());
}

/// Each justified index is the one its query actually gets.
#[tokio::test]
async fn every_justified_index_is_the_one_the_planner_picks() {
    let harness = TestHarness::new();
    let conn = migrated(&harness).await;

    for (name, j) in REGISTRY {
        let Query { label, sql, .. } = j else {
            continue;
        };
        let plan = plan_of(&conn, sql).await;
        assert!(
            plan.contains(name),
            "{label}: expected {name}, planner chose: {plan}"
        );
    }
}

/// The two unread indexes are exactly the two already known.
///
/// A tripwire rather than a fix. Removing them is a `DROP INDEX` migration
/// rung, which is a schema change in a release whose other items are additive —
/// the same reason T3.3's interning was deferred — so D-089 schedules it and
/// this keeps a third from joining them unnoticed.
#[test]
fn the_unread_indices_are_the_two_already_known() {
    let unread: Vec<&str> = REGISTRY
        .iter()
        .filter(|(_, j)| matches!(j, NoReader { .. }))
        .map(|(n, _)| *n)
        .collect();
    let reasons = REGISTRY
        .iter()
        .filter_map(|(n, j)| match j {
            NoReader { why } => Some(format!("  {n}: {why}")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        unread,
        vec!["idx_annotations_label", "idx_lc_tgt_active"],
        "the set of indexes with no reader in the crate has changed. If one \
         gained a reader, move it to a Query entry. If a new one lost its \
         reader, that is an index write per insert buying nothing — see D-089.\
         \nCurrently recorded as unread:\n{reasons}"
    );
}

/// Every reproduced query still exists where it was copied from.
///
/// Bounds the one weakness of testing a copy. `include_str!` is compile-time, so
/// this costs nothing at runtime and needs no API surface widened to reach a
/// private `const`.
#[test]
fn every_reproduced_query_still_exists_in_its_source() {
    for (name, j) in REGISTRY {
        let Query {
            source: Some((text, fragment)),
            ..
        } = j
        else {
            continue;
        };
        // Whitespace-normalised on both sides. The sources are CRLF and the
        // fragments are written LF, so a byte-exact `contains` fails for a
        // reason that has nothing to do with the query — which would make this
        // guard a nuisance test, and nuisance tests get deleted.
        let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat(text).contains(&flat(fragment)),
            "{name}: the source no longer contains {fragment:?}, so the query \
             this file explains is a query nobody runs"
        );
    }
}
