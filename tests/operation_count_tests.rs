//! Operation counts, which do not move when the machine is busy (W10.1, D-055).
//!
//! [D-055] keeps the benches out of CI as gates and [D-070]'s ~29%
//! session-to-session spread is why. That argument is about **timings**. It
//! does not reach everything worth gating: the D-042 / D-059 / D-064 bug class —
//! *a covering index captures a query because it contains the columns, not
//! because it discriminates* — shows up as work done, and work done is an
//! integer.
//!
//! `index_plan_tests` already pins the *plan* against the same fixture. This
//! file pins what the plan compiles to, which is a strictly finer statement: two
//! different plans can both say `USING INDEX`, and the one that opens a second
//! cursor to fetch the columns it did not cover is doing more work per row.
//!
//! # Rows scanned is not among these, and that is measured rather than assumed
//!
//! The obvious counter is `sqlite3_stmt_scanstatus`, which reports rows visited
//! per loop. It requires `SQLITE_ENABLE_STMT_SCANSTATUS` at compile time, and
//! `PRAGMA compile_options` on the vendored engine does not list it —
//! `examples/opcode_probe.rs` prints the list. So no rows-scanned counter
//! exists here to gate, at any level of the stack, and the honest substitute is
//! the *program* that would do the scanning.
//!
//! # What one number does not tell you
//!
//! `rewinds` is not "this query scans a table". An index range scan with an
//! open-ended bound rewinds its index cursor too, and the fold's
//! `recorded_at <= ?1` is exactly that shape. The control at the bottom of
//! [`PINNED`] is a genuine full table scan and carries `(1, 0, 1)`; the fold
//! carries `(2, 0, 1)`. **The triple separates them and no single component
//! does** — which is why these are pinned as a fingerprint rather than as three
//! independent thresholds, and why `a_full_scan_and_an_index_range_scan_are_
//! distinguishable` asserts that separation instead of leaving it implied.
//!
//! # When this goes red
//!
//! A changed query, a changed index, or a changed engine. The first two are the
//! point. The third is a plan review and not a bug: re-run
//! `cargo run --example opcode_probe`, read the sweep, and change the numbers
//! deliberately. A fingerprint nobody re-derives is a fingerprint of whatever
//! the code did last.
//!
//! # Where the counter lives
//!
//! `Counts` and `counts_of` are in `tests/common/plan_fixture.rs`, next to
//! the fixture they are read against. They were declared here until W10.6's
//! cross-axis read needed the same three integers against a *different*
//! database — one with a populated `transaction_log` — and two definitions of
//! "a seek" agree only until one of them is edited.
//!
//! [D-055]: ../docs/architecture/s13-decision-register.md
//! [D-070]: ../docs/architecture/s13-decision-register.md

#[path = "common/harness.rs"]
mod harness;
#[path = "common/plan_fixture.rs"]
mod plan_fixture;

use harness::TestHarness;
use plan_fixture::{
    assert_has_statistics, counts, counts_of, plan_of, populated_and_analysed, Counts,
};

/// The queries `index_plan_tests` justifies an index with, plus one control.
///
/// Same statements, same fixture, deliberately: the two files are the same
/// claim at two depths, and a query pinned here but not there would be a plan
/// nobody has looked at.
const PINNED: &[(&str, &str, Counts)] = &[
    (
        "the traversal CTE's recursive step",
        "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1 \
         AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4",
        // One cursor: `idx_lc_traversal_cover` answers it without touching the
        // table. That is what "covering" means, and it is D-042's whole point.
        counts(1, 1, 0),
    ),
    (
        "the overlap guard and the single-open probe",
        "SELECT valid_from, valid_to FROM links_current \
         WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
           AND valid_from <> ?4",
        counts(1, 1, 0),
    ),
    (
        "the fold's recorded_at window",
        "SELECT seq_id, table_name, entity_id, operation, payload \
         FROM transaction_log WHERE recorded_at <= ?1",
        // Two cursors — the index and the row lookup — and a rewind, because
        // `<= ?1` is open at the bottom. Both are correct here and both are the
        // reason the module note says one number proves nothing.
        counts(2, 0, 1),
    ),
    (
        "the archive's supersession test",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
           SELECT 1 FROM transaction_log newer \
           WHERE newer.entity_id = transaction_log.entity_id \
             AND newer.seq_id > transaction_log.seq_id)",
        counts(3, 1, 1),
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
        counts(3, 1, 1),
    ),
    (
        "the concept-archival reverse-reachability arm",
        "SELECT id FROM concepts WHERE retired = 1 AND recorded_at < ?1 \
         AND valid_to < ?1 AND NOT EXISTS ( \
           SELECT 1 FROM links WHERE links.source_id = concepts.id \
              OR links.target_id = concepts.id)",
        counts(2, 2, 1),
    ),
    (
        "CONTROL: a query with no index to use",
        "SELECT id FROM concepts WHERE content LIKE ?1",
        // Nothing in the schema indexes `content`, and nothing should. This row
        // is here so the gate has a known-bad shape to be different from.
        counts(1, 0, 1),
    ),
];

/// The control's label, so the two tests below cannot drift apart.
const CONTROL: &str = "CONTROL: a query with no index to use";

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// **Every pinned query compiles to the same work it compiled to when the
/// number was written down** (0.13.22, W10.1).
///
/// The fixture has rows and statistics, so this is the planner callers get. A
/// change here is a change in what the database does per row of input, and it
/// arrives as an integer rather than as a support ticket — which is the whole
/// of what D-055 left available once timings were ruled out.
#[tokio::test]
async fn every_pinned_query_does_the_work_it_is_pinned_to() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;
    assert_has_statistics(&conn).await;

    for (label, sql, expected) in PINNED {
        let got = counts_of(&conn, sql).await;
        assert_eq!(
            &got, expected,
            "{label}: the work changed. Re-run `cargo run --example \
             opcode_probe`, decide whether the new plan is the one you want, \
             and change the number deliberately"
        );
    }
}

/// **A full table scan and an index range scan are told apart by the triple and
/// by no one component of it.**
///
/// Without this, the fingerprints above look like three thresholds with obvious
/// meanings — "seeks good, rewinds bad" — and someone would eventually simplify
/// the gate to assert `rewinds == 0` on queries where a rewind is correct. The
/// fold and the control both carry `seeks = 0, rewinds = 1`; they differ only
/// in cursors opened.
#[tokio::test]
async fn a_full_scan_and_an_index_range_scan_are_distinguishable() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;

    let scan = PINNED.iter().find(|(l, _, _)| *l == CONTROL).unwrap();
    let ranged = PINNED
        .iter()
        .find(|(l, _, _)| *l == "the fold's recorded_at window")
        .unwrap();

    let scan_counts = counts_of(&conn, scan.1).await;
    let ranged_counts = counts_of(&conn, ranged.1).await;

    assert_eq!(
        (scan_counts.seeks, scan_counts.rewinds),
        (ranged_counts.seeks, ranged_counts.rewinds),
        "the premise of this test has stopped holding: the two shapes now \
         differ in seeks or rewinds, so the module note about one number \
         proving nothing needs rewriting"
    );
    assert_ne!(
        scan_counts.opens, ranged_counts.opens,
        "a full table scan and an index range scan are now indistinguishable \
         to this gate, which means the gate has stopped gating"
    );
}

/// **This gate is strictly finer than the plan gate, demonstrated rather than
/// claimed** (0.13.22, W10.1).
///
/// The traversal's recursive step, with one column added that
/// `idx_lc_traversal_cover` does not carry. Same index, same seek, and
/// `index_plan_tests`' assertion — `plan.contains(name)` — still passes,
/// because the index it justifies is still the one the planner picks. The work
/// is not unchanged: the index no longer answers the query on its own and a
/// second cursor opens to fetch the row.
///
/// **`EXPLAIN QUERY PLAN` does say `COVERING INDEX` where it applies**, so a
/// plan assertion *could* have caught this by matching that word. Two reasons
/// this is the better gate anyway, and both are worth stating rather than
/// implying: the registry keys on the index name on purpose — its question is
/// "what reads this index" — and a word in a prose string is a weaker thing to
/// pin than an integer, because it says a plan is covering without saying how
/// much is read when it is not.
#[tokio::test]
async fn losing_coverage_moves_the_count_and_not_the_registry_assertion() {
    let harness = TestHarness::new();
    let conn = populated_and_analysed(&harness.db_path).await;

    let covered = "SELECT l.target_id FROM links_current l WHERE l.source_id = ?1                    AND l.valid_from <= ?3 AND ?3 < l.valid_to AND l.weight >= ?4";
    // `properties` is in no index, so the row itself has to be read.
    let uncovered = "SELECT l.target_id, l.properties FROM links_current l                      WHERE l.source_id = ?1 AND l.valid_from <= ?3 AND ?3 < l.valid_to                        AND l.weight >= ?4";

    // The registry's assertion, run on both: it keys on the index name, and
    // the name is what does not move.
    for sql in [covered, uncovered] {
        assert!(
            plan_of(&conn, sql).await.contains("idx_lc_traversal_cover"),
            "the premise has stopped holding: this variant no longer uses the              index at all, so it demonstrates nothing about coverage"
        );
    }

    assert_eq!(counts_of(&conn, covered).await.opens, 1, "covered");
    assert_eq!(
        counts_of(&conn, uncovered).await.opens,
        2,
        "an uncovered query must open the table as well as the index, or the          two shapes are indistinguishable here and this gate adds nothing to          the plan gate"
    );
}

/// **Every query pinned here is also plan-pinned there** (W10.1).
///
/// The two files must not drift into pinning different sets. An operation count
/// with no plan behind it is a number nobody can interpret; a plan with no
/// operation count is the coarser of the two claims left standing alone.
///
/// Textual, because `index_plan_tests`' registry is a private `const` in
/// another test crate — the same bound its own reproduced queries carry, for
/// the same reason (D-089).
#[test]
fn every_pinned_query_is_also_plan_pinned() {
    const PLAN_TESTS: &str = include_str!("index_plan_tests.rs");

    for (label, _, _) in PINNED {
        if *label == CONTROL {
            // The control exists precisely because nothing justifies an index
            // for it, so it has no registry entry and must not gain one.
            assert!(
                !PLAN_TESTS.contains(label),
                "the control has acquired a plan-pinning entry; it is supposed \
                 to be the query no index justifies"
            );
            continue;
        }
        assert!(
            PLAN_TESTS.contains(label),
            "{label} is pinned here and named nowhere in index_plan_tests: \
             either the label drifted or the query lost the index that \
             justifies it"
        );
    }
}
