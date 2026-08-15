//! The planner gets statistics, and they are bounded (W2, D-149).
//!
//! Until 0.12.4 nothing in this crate ran `ANALYZE`, so `sqlite_stat1` existed in
//! no database Macrame had ever created and every plan was costed against
//! SQLite's built-in defaults: assume ~1M rows, assume each bound equality
//! column divides by ten. That estimate is *structural* — a function of how many
//! columns a query binds, not of what the table holds — which is
//! [`index_plan_tests`]'s own summary of this schema's worst recurring defect
//! turned into a standing condition.
//!
//! These tests assert the three things that make the fix real rather than
//! nominal: the statistics **exist**, the bound that makes the write schedulable
//! is **in force on the connection**, and the incremental form is **a no-op when
//! nothing has changed** — which is the only reason it is safe to run from
//! `close()`.
//!
//! [`index_plan_tests`]: ../tests/index_plan_tests.rs

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

async fn db(harness: &TestHarness) -> Database {
    Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap()
}

/// Enough rows that `ANALYZE` has something to record, with skewed out-degree.
///
/// Skew is the point and not incidental: uniform data is exactly the case where
/// measured statistics and SQLite's default guesses agree, so a fixture built
/// from it would pass whether or not the statistics existed. One hub with many
/// edges beside many leaves with one is the shape a code graph actually has, and
/// the shape the defaults get wrong.
async fn populate(db: &Database) {
    db.write_concepts(
        (0..200)
            .map(|i| ConceptUpsert::new(format!("c{i:04}"), format!("C{i}")).valid_from(TS))
            .collect(),
    )
    .await
    .unwrap();

    let mut edges = Vec::new();
    // The hub.
    for i in 1..150 {
        edges.push(
            EdgeAssertion::new("c0000", format!("c{i:04}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    // The leaves.
    for i in 150..199 {
        edges.push(
            EdgeAssertion::new(format!("c{i:04}"), format!("c{:04}", i + 1), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    db.bulk_import(edges).await.unwrap();
}

async fn stat1_rows(db: &Database) -> Vec<(String, String)> {
    let conn = db.diagnostic_conn().await.unwrap();
    // `sqlite_stat1` does not exist until something has analysed, and querying a
    // missing table is an error rather than an empty result — so the absence is
    // read here as "no rows" deliberately, which is what makes the before/after
    // pair in `analyze_creates_statistics_that_did_not_exist` meaningful.
    let Ok(mut rows) = conn
        .query("SELECT tbl, idx FROM sqlite_stat1 ORDER BY tbl, idx", ())
        .await
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((
            r.get::<String>(0).unwrap_or_default(),
            r.get::<String>(1).unwrap_or_default(),
        ));
    }
    out
}

/// The before/after that says this changed something.
///
/// The "before" half is the load-bearing one. Asserting only that statistics
/// exist afterwards would pass in a world where libSQL had been writing them all
/// along, and the whole finding is that it had not.
#[tokio::test]
async fn analyze_creates_statistics_that_did_not_exist() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    populate(&db).await;

    let before = stat1_rows(&db).await;
    assert!(
        before.is_empty(),
        "sqlite_stat1 already had rows before any analyze() call — either the \
         engine now analyses on its own, or something else in this crate started \
         calling ANALYZE. Either way D-149's premise has changed and the entry \
         needs revisiting. Found: {before:?}"
    );

    db.analyze().await.unwrap();

    let after = stat1_rows(&db).await;
    assert!(
        !after.is_empty(),
        "analyze() returned Ok and sqlite_stat1 is still empty"
    );

    // Every declared index should be represented. This is the assertion that
    // would catch `analysis_limit` being set so low that ANALYZE skips indices
    // rather than merely sampling them.
    for idx in [
        "idx_lc_traversal_cover",
        "idx_lc_open_interval",
        "idx_txlog_time",
        "idx_txlog_entity",
    ] {
        assert!(
            after.iter().any(|(_, i)| i == idx),
            "{idx} has no row in sqlite_stat1 after analyze(); got {after:?}"
        );
    }

    db.close().await.unwrap();
}

/// The bound that makes `ANALYZE` schedulable is wired into `configure`.
///
/// `ANALYZE` is a write and takes the write lock, so unbounded on a populated
/// table it is precisely the unbudgeted hold `CHUNK_BUDGET` exists to prevent.
/// [`ddl::ANALYSIS_LIMIT`] is what bounds it, and it is applied in one line of
/// `configure` — easy to delete with no test noticing, leaving a crate that is
/// still correct and slower in a way nothing measures.
///
/// # Why this reads the source instead of asking the database
///
/// The obvious test — open a handle, `PRAGMA analysis_limit`, assert 400 — cannot
/// be written, because **the only connection a test can reach is the wrong one**.
/// `diagnostic_conn()` does not call `configure()` at all, and returns `0` here;
/// `read_conn()` is a reader. The connection that runs `ANALYZE` is the write
/// actor's, and it is private by design (D-068/D-091) — `raw()` exists and is
/// refused for reasons this test has no business overriding.
///
/// So it is checked the way `index_plan_tests` checks its reproduced queries:
/// `include_str!` the module and look for the fragment. Compile-time, no API
/// widened, and it goes red when the line moves.
///
/// **`diagnostic_conn()` skipping `configure()` is a real finding and not this
/// test's business** — it means diagnostic reads run with a different
/// `busy_timeout` than every other connection in the process. It is W5.5 in the
/// road map and it is left there.
#[test]
fn the_analysis_limit_is_applied_where_connections_are_configured() {
    let source = include_str!("../src/connection.rs");
    let configure = source
        .split("async fn configure(")
        .nth(1)
        .expect("`configure` has moved or been renamed");
    let body = configure
        .split("\n}")
        .next()
        .expect("`configure` body did not terminate");

    assert!(
        body.contains("ANALYSIS_LIMIT"),
        "`configure` no longer applies ddl::ANALYSIS_LIMIT. Without it ANALYZE \
         scans every index in full, which is a write-lock hold proportional to \
         the table rather than to the index count — the thing D-149 exists to \
         make schedulable."
    );
}

/// `optimize()` is safe to call when nothing has changed, which is why `close()`
/// can call it unconditionally.
///
/// Asserts the property `close()` relies on rather than a timing claim: running
/// it twice in a row leaves the statistics it already wrote intact and does not
/// error. If `PRAGMA optimize` ever became unconditional, `close()` would start
/// paying a full `ANALYZE` on every handle drop and nothing would say so.
#[tokio::test]
async fn optimize_is_repeatable_and_preserves_what_analyze_wrote() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    populate(&db).await;

    db.analyze().await.unwrap();
    let after_analyze = stat1_rows(&db).await;
    assert!(!after_analyze.is_empty());

    db.optimize().await.unwrap();
    db.optimize().await.unwrap();

    let after_optimize = stat1_rows(&db).await;
    assert_eq!(
        after_analyze, after_optimize,
        "optimize() changed the set of analysed indices on an idle database"
    );

    db.close().await.unwrap();
}

/// `close()` leaves a database whose next reader has statistics.
///
/// The end-to-end version of the two above, and the one that matches how this
/// is actually meant to be used: nobody calls `analyze()`, the handle is closed
/// normally, and the statistics are there for the next process.
#[tokio::test]
async fn close_leaves_statistics_behind_without_being_asked() {
    let harness = TestHarness::new();
    let first = db(&harness).await;
    populate(&first).await;
    first.close().await.unwrap();

    let reopened = db(&harness).await;
    let rows = stat1_rows(&reopened).await;
    assert!(
        !rows.is_empty(),
        "close() ran PRAGMA optimize on a database that had just been bulk \
         loaded, and sqlite_stat1 is still empty. The next process will plan on \
         built-in defaults (D-149)."
    );
    reopened.close().await.unwrap();
}
