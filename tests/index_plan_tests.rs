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
#[path = "common/plan_fixture.rs"]
mod plan_fixture;

use harness::TestHarness;
use macrame::schema::ddl;
use plan_fixture::{assert_has_statistics, migrated, plan_of, populated_and_analysed};

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
        "idx_lc_lineage_cut",
        Query {
            label: "the lineage read's two base scans over the projection",
            // `churned_cte` and `links_cut_cte` both drive from the
            // materialised ancestry — `JOIN lineage g ON g.branch_id =
            // lc.branch_id` — and then compare the row's `recorded_at` to that
            // lineage's cutoff. Per ancestry row that reduces to exactly this,
            // which is what the index is shaped for and what the planner has to
            // pick for the branched read to stop building the index itself.
            sql: "SELECT source_id, target_id, edge_type, valid_from, valid_to, weight \
                  FROM links_current WHERE branch_id = ?1 AND recorded_at > ?2",
            source: Some((
                include_str!("../src/graph/lineage.rs"),
                // Without the `WHERE`: since 0.15.8 the clause carries an
                // optional key narrowing in front of it (W13.3, D-250),
                // and pinning the keyword would pin the interpolation
                // rather than the comparison this index serves.
                "g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff",
            )),
        },
    ),
    (
        "idx_txlog_time",
        Query {
            // **Was "the fold's recorded_at window" through 0.15.11, and the
            // fold has left.** `idx_txlog_fold_partition` (0.15.12, W15.2)
            // supplies the window function's partition *and* its order, which
            // the planner prefers to a seek on this one followed by a sort. An
            // index whose recorded justification is a query that no longer
            // reads it is precisely the shape D-089 was written about, so the
            // entry moves to the readers that remain rather than the entry
            // staying and quietly becoming false.
            //
            // Both are aggregates over `recorded_at` and both are served as an
            // index scan without touching the table: `newest_hot_stamp` /
            // `oldest_hot_stamp` in `temporal::replay`, and the reach guard's
            // four-value count beside them. The probe checked this index's
            // plan before and after the new one and it did not move.
            label: "the hot log's stamp aggregates",
            sql: "SELECT MAX(recorded_at) FROM transaction_log",
            source: Some((
                include_str!("../src/temporal/replay.rs"),
                "(recorded_at) FROM transaction_log",
            )),
        },
    ),
    (
        "idx_txlog_fold_partition",
        Query {
            // The whole fold, not its `WHERE`: the index is for the window
            // function, and a reproduction that dropped the `OVER` clause would
            // be asking about the filter — which is the *other* index's
            // question and is how this entry's predecessor came to name a
            // reader it did not have.
            label: "the log fold's partition and order",
            sql: "SELECT seq_id, table_name, entity_id, operation, payload, branch_id \
                  FROM ( \
                    SELECT seq_id, table_name, entity_id, operation, payload, branch_id, \
                           ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id \
                                              ORDER BY seq_id DESC) as rn \
                    FROM transaction_log WHERE recorded_at <= ?1) WHERE rn = 1",
            source: Some((
                include_str!("../src/temporal/replay.rs"),
                "PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC",
            )),
        },
    ),
    (
        "idx_txlog_entity",
        Query {
            // **The concept hydrate left this index at 0.15.12** and the
            // supersession test did not, which is the whole reason this entry
            // is unchanged. `hydrate_at_time` folds
            // `WHERE table_name = 'concepts' AND entity_id IN (…)` and used to
            // seek here on `(entity_id=?)`; `idx_txlog_fold_partition` binds
            // both of those columns, so the planner prefers it and this index
            // lost a reader it was not registered for.
            //
            // It gained nothing by moving. That fold partitions on `entity_id`
            // **alone** — see `temporal::as_of` for why, and it is correct —
            // so the new index's `branch_id` sits between the partition and
            // the order, and `USE TEMP B-TREE FOR RIGHT PART OF ORDER BY` is
            // in the plan before and after. Measured at +8% on a 0.10 ms call
            // (`examples/txlog_fold_index_probe.rs --arm other-folds`), which
            // is the honest cost of the rung and is recorded here rather than
            // rounded away.
            //
            // The entry stays because the archive's supersession test still
            // seeks here — the probe checked its plan across the change and it
            // did not move — so this is a reader leaving, not D-089's failure.
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
        "idx_links_recorded_at",
        Query {
            label: "the archive cutoff on the links ledger",
            sql: "SELECT source_id, target_id FROM links WHERE recorded_at < ?1 AND ( \
                    EXISTS ( \
                      SELECT 1 FROM links newer \
                      WHERE newer.source_id = links.source_id \
                        AND newer.target_id = links.target_id \
                        AND newer.edge_type = links.edge_type \
                        AND newer.valid_from = links.valid_from \
                        AND newer.recorded_at > links.recorded_at) \
                    OR (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= ?1))",
            source: Some((
                include_str!("../src/temporal/archive.rs"),
                "recorded_at < :cutoff AND (",
            )),
        },
    ),
    (
        "idx_links_target",
        Query {
            label: "the concept-archival reverse-reachability arm",
            sql: "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1 \
                  AND valid_to < ?1 AND NOT EXISTS ( \
                    SELECT 1 FROM links WHERE links.source_id = concepts.id \
                       OR links.target_id = concepts.id)",
            source: Some((
                include_str!("../src/temporal/archive.rs"),
                "OR links.target_id = concepts.id",
            )),
        },
    ),
    // `idx_annotations_label` and `idx_lc_tgt_active` were the two `NoReader`
    // entries this registry found (D-089). The v7 → v8 rung dropped them both
    // (D-118), so they are gone from `CREATE_INDICES` and gone from here.
    //
    // `idx_links_target` above is **not** `idx_lc_tgt_active` readmitted — it is
    // on `links`, not `links_current`, and it has the named seeking query D-089
    // asks for. `ddl::CREATE_INDICES` states the distinction at length.
];

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

/// Each justified index is the one its query actually gets, **on a database
/// that has rows and statistics** — which is what production has since D-149.
///
/// This is the arm that matters. Before D-149 the empty fixture below was the
/// only one and was faithful; the moment `ANALYZE` shipped, a planner with
/// `sqlite_stat1` became the one callers get, and pinning plans against a
/// planner nobody runs is a gate that has quietly stopped gating.
#[tokio::test]
async fn every_justified_index_is_the_one_the_planner_picks_with_statistics() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;

    // Guard the fixture itself: without this, every assertion below would
    // still pass while testing the empty-database planner under a name
    // claiming otherwise.
    assert_has_statistics(&conn).await;

    for (name, j) in REGISTRY {
        let Query { label, sql, .. } = j else {
            continue;
        };
        let plan = plan_of(&conn, sql).await;
        assert!(
            plan.contains(name),
            "{label}: expected {name} on a populated, analysed database — \
             planner chose: {plan}"
        );
    }
}

/// The registry's other direction: queries that must not silently start
/// scanning (W2.3, D-150).
///
/// # The hole this closes
///
/// [`REGISTRY`] is keyed by **index**, and that catches an index nothing reads.
/// It cannot catch the inverse — *a query that quietly leaves its index* — for
/// any query no entry happens to name, because there is nothing to write an
/// assertion against. Both halves are needed and neither implies the other.
///
/// # A `Scan` expectation is a recorded defect, not an endorsement
///
/// Two of these rows expect a scan today. That is [§2.1 and §2.2 of the codebase
/// review](../docs/Macrame%20Codebase%20Review%20v0.12.0.md) written down where a
/// change has to walk past it: `links` carries a primary key and nothing else, so
/// the archive predicates and the clock floor have nothing to seek on.
///
/// Recording the defect rather than asserting the fix means W3 **cannot land
/// quietly** — the indexes it adds turn these rows red, and the commit that adds
/// them has to come here and say so. A test that already expected the good plan
/// would go green on its own and nobody would see the improvement.
const QUERY_REGISTRY: &[(&str, &str, Expect)] = &[
    (
        "the concept-archival predicate's link check",
        "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1          AND valid_to < ?1 AND NOT EXISTS (              SELECT 1 FROM links WHERE links.source_id = concepts.id                 OR links.target_id = concepts.id)",
        Expect {
            // Was, through 0.12.5:
            //   SCAN concepts | CORRELATED SCALAR SUBQUERY 1
            //     | SCAN links USING COVERING INDEX sqlite_autoindex_links_1
            //
            // Now, with idx_links_target (0.12.6, W3.2, D-151):
            //   SCAN concepts USING INDEX sqlite_autoindex_concepts_1
            //     | CORRELATED SCALAR SUBQUERY 1 | MULTI-INDEX OR
            //     | INDEX 1 | SEARCH links USING COVERING INDEX
            //                  sqlite_autoindex_links_1 (source_id=?)
            //     | INDEX 2 | SEARCH links USING INDEX idx_links_target
            //                  (target_id=?)
            //
            // The correlation is still there and is *supposed* to be — the
            // subquery is per-concept by construction. What changed is what it
            // costs: both arms of the `OR` now seek instead of the right one
            // scanning. So the fragment moved off `CORRELATED SCALAR SUBQUERY`,
            // which was true before and after and therefore proved nothing, and
            // onto the plan shape that is actually the fix.
            fragment: "MULTI-INDEX OR",
            forbidden: "",
            note: "Review §2.2, closed in 0.12.6 by `idx_links_target`. If this                    reverts to a bare `SCAN links` inside the subquery, concept                    archival is O(concepts × links) again.",
        },
    ),
    (
        "the link-archival supersession probe",
        "SELECT rowid FROM links WHERE recorded_at < ?1 AND EXISTS (              SELECT 1 FROM links newer              WHERE newer.source_id = links.source_id                AND newer.target_id = links.target_id                AND newer.edge_type = links.edge_type                AND newer.valid_from = links.valid_from                AND newer.recorded_at > links.recorded_at)",
        Expect {
            // Was, through 0.12.5:
            //   SCAN links | CORRELATED SCALAR SUBQUERY 1
            //     | SEARCH newer ... (full PK prefix)
            //
            // Now, with idx_links_recorded_at (0.12.6, W3.1, D-151):
            //   SEARCH links USING INDEX idx_links_recorded_at (recorded_at<?)
            //     | CORRELATED SCALAR SUBQUERY 1 | SEARCH newer ...
            //
            // The inner probe was always fine — it binds the whole primary-key
            // prefix. It was the OUTER `recorded_at <` that had nothing to seek
            // on, because the primary key leads on `source_id`.
            fragment: "SEARCH links USING INDEX idx_links_recorded_at",
            forbidden: "",
            note: "Review §2.1, closed in 0.12.6 by `idx_links_recorded_at`.                    The outer `recorded_at <` filter used to scan every row of                    `links`; it now seeks. The inner probe was always served by                    the primary key and was never the problem.",
        },
    ),
    (
        "the clock floor read on every open()",
        "SELECT MAX(recorded_at) FROM (              SELECT MAX(recorded_at) AS recorded_at FROM concepts              UNION ALL              SELECT MAX(recorded_at) AS recorded_at FROM links)",
        Expect {
            // ... | UNION ALL | SEARCH links USING COVERING INDEX
            //         sqlite_autoindex_links_1 | ...
            //
            // **NOT a scan, and this contradicts review §2.1**, which counted
            // this among the "four full scans" an index on `links.recorded_at`
            // would close. The planner already serves the bare `MAX()` from the
            // primary key's covering index without traversing the table.
            //
            // **W3.1 landed and this query did not improve, as predicted here.**
            // With `idx_links_recorded_at` in the schema the plan reads
            // `SEARCH links USING COVERING INDEX idx_links_recorded_at` — the
            // planner swapped which covering index answers the bare `MAX()`, and
            // a covering-index seek it already had is what it already had. The
            // entry is kept unnamed on purpose: pinning the index *name* here
            // would assert that the new index serves this query, which is the
            // claim the entry exists to deny.
            //
            // So `idx_links_recorded_at` is justified on the two archive queries
            // and on nothing else, and this row is the record of that scope.
            // D-089 exists because an index bought on a believed benefit is an
            // index write per insert forever; this is the belief being checked
            // before the purchase rather than after it.
            fragment: "SEARCH links USING COVERING INDEX",
            forbidden: "",
            note: "Served from a covering index before and after W3.1 — no                    traversal of the table either way. Contradicts review                    §2.1's claim that this is a full scan closed by                    `idx_links_recorded_at`; that index is justified on the                    archive path alone (D-150, D-151).",
        },
    ),
    (
        "the log fold does not sort",
        "SELECT seq_id, table_name, entity_id, operation, payload, branch_id           FROM (            SELECT seq_id, table_name, entity_id, operation, payload, branch_id,                   ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id                                      ORDER BY seq_id DESC) as rn            FROM transaction_log WHERE recorded_at <= ?1) WHERE rn = 1",
        Expect {
            // Was, through 0.15.11:
            //   SEARCH transaction_log USING INDEX idx_txlog_time (recorded_at<?)
            //     | USE TEMP B-TREE FOR ORDER BY
            //
            // Now, with idx_txlog_fold_partition (0.15.12, W15.2, D-254):
            //   SCAN transaction_log USING INDEX idx_txlog_fold_partition
            //
            // The entry above pins that the index is *used*. This pins the
            // property it was bought for, which is not the same claim: the
            // **ascending** form of the identical columns is also used, and
            // still sorts — `USE TEMP B-TREE FOR RIGHT PART OF ORDER BY` — at
            // 60.2 ms against 46.2, for the same file and the same write cost.
            // An edit that drops the `DESC` would keep the entry above green
            // and give back most of the improvement, and this is the assertion
            // that would notice.
            fragment: "SCAN transaction_log USING INDEX idx_txlog_fold_partition",
            forbidden: "TEMP B-TREE",
            note: "0.15.12, W15.2, D-254. The fold is a window function and                    wants its input in partition-then-order sequence. If a temp                    B-tree comes back, every reconstruction is sorting the whole                    log again — 64 ms against 46 at 30,000 rows.",
        },
    ),
    (
        "the concept hydrate still seeks",
        "SELECT entity_id, seq_id, payload FROM (              SELECT entity_id, seq_id, payload,                     ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) as rn              FROM transaction_log              WHERE table_name = 'concepts'                AND recorded_at <= ?1                AND entity_id IN ('a', 'b', 'c')) WHERE rn = 1",
        Expect {
            // Was, through 0.15.11:
            //   SEARCH transaction_log USING INDEX idx_txlog_entity (entity_id=?)
            //     | USE TEMP B-TREE FOR RIGHT PART OF ORDER BY
            //
            // Now, with idx_txlog_fold_partition (0.15.12, W15.2, D-254):
            //   SEARCH transaction_log USING INDEX idx_txlog_fold_partition
            //     (table_name=? AND entity_id=?)
            //     | USE TEMP B-TREE FOR RIGHT PART OF ORDER BY
            //
            // **The sort survives on purpose and the entry does not forbid
            // it.** This fold partitions on `entity_id` alone, so `branch_id`
            // sits between the partition and the order in every index the
            // table has, and no index the crate declares can supply it. What
            // is worth pinning is that the `IN` list still *seeks* — a
            // `SCAN transaction_log` here would make attribute hydration
            // O(log) per read — and that is what the forbidden half says.
            //
            // Recorded rather than fixed: an index on
            // `(table_name, entity_id, seq_id DESC)` would remove this sort
            // and would be a third index on the hottest write path in the
            // crate, bought for 8% of a 0.10 ms call. Measure it before
            // building it (D-089).
            fragment: "SEARCH transaction_log USING INDEX idx_txlog_fold_partition",
            forbidden: "SCAN transaction_log",
            note: "0.15.12, W15.2, D-254. The concept hydrate reads the log by                    entity for AttributeMode::AtTime. It moved off idx_txlog_entity                    when the fold index arrived; what must not change is that it                    seeks at all.",
        },
    ),
];

/// The plan a registered query is **measured** to get, with what that means.
///
/// `fragment` is deliberately a substring rather than a whole-plan equality:
/// pinning the entire `EXPLAIN QUERY PLAN` string would go red on any SQLite
/// wording change and teach people to re-bless it without reading.
struct Expect {
    fragment: &'static str,
    /// A substring the plan must **not** contain, or `""` for no such claim.
    ///
    /// Added for `idx_txlog_fold_partition` (0.15.12), where the property being
    /// defended is the absence of a step rather than the presence of one. Every
    /// other entry here asserts that a query reaches an index; that one asserts
    /// it reaches it *in the right order*, and the only evidence of the
    /// difference in `EXPLAIN QUERY PLAN` is whether a sort is listed.
    forbidden: &'static str,
    note: &'static str,
}

/// Every query-keyed entry gets the plan it is recorded as getting.
///
/// Run against the populated, analysed fixture: a scan and a seek are only
/// distinguishable once there are rows and statistics to choose between them.
#[tokio::test]
async fn every_registered_query_gets_the_plan_it_is_recorded_as_getting() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;

    for (label, sql, expect) in QUERY_REGISTRY {
        let plan = plan_of(&conn, sql).await;
        assert!(
            plan.contains(expect.fragment),
            "{label}: expected the plan to contain {:?}
             note: {}
             planner chose: {plan}

             If an index you just added changed this, that is the point of this              test — update the entry and say what the new plan is.",
            expect.fragment,
            expect.note
        );
        assert!(
            expect.forbidden.is_empty() || !plan.contains(expect.forbidden),
            "{label}: the plan must not contain {:?}
             note: {}
             planner chose: {plan}",
            expect.forbidden,
            expect.note
        );
    }
}

/// The same assertions on an **empty, unanalysed** database.
///
/// Kept deliberately, and not as a leftover. A fresh database before its first
/// `ANALYZE` is a real state Macrame is in — every process is in it between
/// `open()` and the first `optimize()` — and the plans it gets are real plans a
/// caller runs. Two fixtures, both asserted, each labelled with which planner it
/// is describing.
#[tokio::test]
async fn every_justified_index_is_the_one_the_planner_picks_when_empty() {
    let harness = TestHarness::new();
    let conn = migrated(&harness.db_path).await;

    for (name, j) in REGISTRY {
        let Query { label, sql, .. } = j else {
            continue;
        };
        let plan = plan_of(&conn, sql).await;
        assert!(
            plan.contains(name),
            "{label}: expected {name} on an empty database — planner chose: {plan}"
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
