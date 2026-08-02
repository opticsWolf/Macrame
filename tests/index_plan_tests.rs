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
//! **Running the registry found exactly that, twice** (D-089). Both were dropped
//! by the v7 → v8 rung (D-118), so [`the_unread_index_set_is_empty`] now asserts
//! the standard rather than tallying the exceptions.
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

#[path = "common/harness.rs"]
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
    /// Nothing in the crate reads it.
    ///
    /// **Currently unconstructed, and that is the assertion** — see
    /// [`the_unread_index_set_is_empty`]. The variant stays because the category
    /// has to outlive its instances: it is what a future unjustifiable index
    /// would have to be recorded as, and recording one is what makes the test go
    /// red. Deleting the variant would turn a red test into a compile error at
    /// the wrong place, and then into a temptation to skip the entry entirely.
    #[allow(dead_code)]
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
    // `idx_annotations_label` and `idx_lc_tgt_active` were the two `NoReader`
    // entries this registry found (D-089). The v7 → v8 rung dropped them both
    // (D-118), so they are gone from `CREATE_INDICES` and gone from here.
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

/// **No index in this schema is unread**, and that is now the standard rather
/// than a tally (D-089, completed by D-118).
///
/// Through 0.7.0 this test pinned the unread *set* — `["idx_annotations_label",
/// "idx_lc_tgt_active"]` — because removing an index needs a `DROP INDEX` rung
/// and no release had one to put it in. That form is a tripwire against a
/// *third* joining them; it accepts the two. The v7 → v8 rung dropped both, so
/// the assertion can be the one D-089 was actually arguing for: an index with no
/// reader is a red test, full stop.
///
/// The `NoReader` variant deliberately survives its last instance. Recording a
/// new index as unread is how this test is made to fail, so the category has to
/// remain available for that failure to be expressible.
#[test]
fn the_unread_index_set_is_empty() {
    let unread: Vec<String> = REGISTRY
        .iter()
        .filter_map(|(n, j)| match j {
            NoReader { why } => Some(format!("  {n}: {why}")),
            _ => None,
        })
        .collect();

    assert!(
        unread.is_empty(),
        "an index in `ddl::CREATE_INDICES` has no reader in the crate. That is \
         an index write on every insert into its table, forever, buying nothing \
         — and one of the two v8 removed was on the hottest write path (D-089). \
         Either name the query that seeks on it, or drop it in a rung.\
         \nUnread:\n{}",
        unread.join("\n")
    );

    // The registry is not empty, or the assertion above holds vacuously.
    assert!(
        REGISTRY.len() >= 4,
        "the registry has shrunk to {} entries; an empty unread set means \
         nothing if there is nothing to be unread",
        REGISTRY.len()
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
