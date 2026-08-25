//! Do statistics change any plan this crate has? (W10.2, D-198)
//!
//! W2.2 scheduled `PRAGMA optimize` at two call sites. `close()` got one in
//! 0.12.4 ([D-149]); the other — "at the end of `write_concepts` / the bulk edge
//! path when a run exceeds a threshold worth picking by measurement" — was left
//! to W10.2 with an instruction to measure the threshold rather than guess it.
//!
//! The measurement went past the threshold to the question underneath it, and
//! **W10.2 closed as "no call site"**: on a freshly loaded database, running
//! `optimize()` writes seven rows of `sqlite_stat1` and moves **no plan and no
//! opcode count**, at 90, 500, 5,000 and 40,000 edges alike
//! (`examples/bulk_optimize.rs`). A maintenance call whose output nothing reads
//! is [D-089]'s unread index in a different costume.
//!
//! # What this file pins, and why it is not `index_plan_tests`
//!
//! That file already asserts the registry's index is chosen **both** on an empty
//! database and on a populated, analysed one ([D-150]). Those two fixtures
//! differ in *two* things — rows and statistics — so agreement between them
//! cannot say which one did not matter. This file isolates the variable:
//! [`plan_fixture::populated_without_statistics`] against
//! [`plan_fixture::populated_and_analysed`] is the same rows with and without
//! `ANALYZE`, and nothing else.
//!
//! **The fingerprint is the plan text and W10.1's opcode triple together**
//! ([D-195]). A plan can read identically while the program does not, and
//! "statistics changed nothing" is exactly the claim that needs the finer
//! instrument rather than the coarser one.
//!
//! **This is a tripwire, not a celebration.** The day a query in this crate
//! plans differently with statistics than without, W10.2's decision stops being
//! true and this test says so by name. That is the whole reason a closure of the
//! form "we measured and there was nothing to build" gets a gate.
//!
//! [D-089]: ../docs/architecture/s13-decision-register.md#d-089
//! [D-149]: ../docs/architecture/s13-decision-register.md#d-149
//! [D-150]: ../docs/architecture/s13-decision-register.md#d-150
//! [D-195]: ../docs/architecture/s13-decision-register.md#d-195

#[path = "common/harness.rs"]
mod harness;
#[path = "common/plan_fixture.rs"]
mod plan_fixture;

use harness::TestHarness;
use plan_fixture::{
    assert_has_statistics, plan_of, populated_and_analysed, populated_without_statistics,
};

/// The queries `tests/operation_count_tests.rs` pins, plus a join whose order
/// the planner is free to choose.
///
/// The pinned six are the set an index in `ddl::CREATE_INDICES` is justified by,
/// so they are where a change in `sqlite_stat1` has any business showing up. The
/// join is here because all six are single-table seeks with one candidate path,
/// and a set made only of those would be answering an easier question than the
/// one asked: statistics decide *between* alternatives, so at least one query
/// has to have some.
const QUERIES: &[(&str, &str)] = &[
    (
        "the traversal CTE's recursive step",
        "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
         AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
    ),
    (
        "the overlap guard and the single-open probe",
        "SELECT valid_from, valid_to FROM links_current \
         WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
           AND valid_from <> ?4",
    ),
    (
        "the fold's recorded_at window",
        "SELECT seq_id, table_name, entity_id, operation, payload \
         FROM transaction_log WHERE recorded_at <= ?1",
    ),
    (
        "the archive's supersession test",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
           SELECT 1 FROM transaction_log newer \
           WHERE newer.entity_id = transaction_log.entity_id \
             AND newer.seq_id > transaction_log.seq_id)",
    ),
    (
        "the archive cutoff on the links ledger",
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
        "the concept-archival reverse-reachability arm",
        "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1 \
         AND valid_to < ?1 AND NOT EXISTS ( \
           SELECT 1 FROM links WHERE links.source_id = concepts.id \
              OR links.target_id = concepts.id)",
    ),
    (
        "a join whose order the planner chooses",
        "SELECT c.id, l.target_id FROM concepts c JOIN links_current l \
         ON l.source_id = c.id WHERE c.retired = 0 AND l.weight >= ?1",
    ),
];

/// The plan text and the opcode triple, as one string.
///
/// Both, because either alone is answerable by the wrong thing: the plan is a
/// summary that can hide a program change, and the counts are integers that can
/// coincide (D-195).
async fn fingerprint(conn: &libsql::Connection, sql: &str) -> String {
    let plan = plan_of(conn, sql).await;

    let mut rows = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
    let (mut opens, mut seeks, mut rewinds, mut sorts) = (0usize, 0usize, 0usize, 0usize);
    while let Some(r) = rows.next().await.unwrap() {
        let op: String = r.get(1).unwrap();
        if op == "OpenRead" || op == "OpenEphemeral" || op == "OpenAutoindex" {
            opens += 1;
        } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
            seeks += 1;
        } else if op == "Rewind" || op == "Last" {
            rewinds += 1;
        } else if op == "SorterOpen" || op == "SorterSort" {
            sorts += 1;
        }
    }
    format!("({opens},{seeks},{rewinds},{sorts}) {plan}")
}

/// **No query in this crate plans differently with statistics than without**
/// (0.13.25, W10.2, D-198).
///
/// Which is why W10.2 ships no automatic `optimize()` call site: there is
/// nothing on the other end of one. If this ever goes red, that reasoning has
/// expired — the failure message says so rather than inviting a fix.
#[tokio::test]
async fn statistics_do_not_change_any_plan_the_crate_pins() {
    let without = TestHarness::new();
    let with = TestHarness::new();
    let bare = populated_without_statistics(&without.db_path).await;
    let analysed = populated_and_analysed(&with.db_path).await;

    // Without this the test passes vacuously on two identical databases, which
    // is D-150's failure exactly: a gate that stops testing what it names.
    assert_has_statistics(&analysed).await;
    assert!(
        bare.query("SELECT COUNT(*) FROM sqlite_stat1", ())
            .await
            .is_err(),
        "the no-statistics fixture has a `sqlite_stat1`, so the two sides differ \
         in nothing and this test is comparing a database with itself"
    );

    for (label, sql) in QUERIES {
        let a = fingerprint(&bare, sql).await;
        let b = fingerprint(&analysed, sql).await;
        assert_eq!(
            a, b,
            "{label}: the plan or the program changed when `ANALYZE` ran.\n\
             That is not a regression. It means statistics have started \
             mattering to a query this crate actually runs, and W10.2's \
             decision — no automatic `optimize()` after a bulk load, because \
             nothing reads what it writes (D-198) — is due for review.\n\
             without statistics: {a}\n\
             with statistics:    {b}"
        );
    }
}

/// **And the fixture can tell a difference when there is one** (0.13.25, W10.2).
///
/// The test above asserts a negative, and a negative is passed by an instrument
/// that cannot detect anything. So the same fingerprint is taken across a change
/// it *must* see: the traversal's recursive step with one column added that
/// `idx_lc_traversal_cover` does not carry, which W10.1 measured as `opens`
/// 1 → 2 (D-195).
#[tokio::test]
async fn the_fingerprint_moves_when_the_work_does() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;

    let covered = "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
                   AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4";
    let uncovered = "SELECT l.target_id, l.properties FROM links_current l \
                     WHERE l.source_id = ?1 \
                     AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4";

    assert_ne!(
        fingerprint(&conn, covered).await,
        fingerprint(&conn, uncovered).await,
        "the fingerprint cannot distinguish a covered read from an uncovered \
         one, so the equality asserted above is worth nothing"
    );
}
