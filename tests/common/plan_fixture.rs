//! The plan-pinning fixtures, in one place (W2.4, F-29; shared in W10.1).
//!
//! Two databases, both real states Macrame is in, both asserted against:
//!
//! * [`migrated`] — freshly migrated, empty, no statistics. Every process is in
//!   this state between `open()` and its first `ANALYZE`, and the plans it gets
//!   are real plans.
//! * [`populated_and_analysed`] — rows and `sqlite_stat1`, which is the shape
//!   production has had since [D-149]. Skewed out-degree on purpose: uniform
//!   data is exactly where measured statistics and SQLite's built-in guesses
//!   agree, so a uniform fixture would pass whether or not `ANALYZE` had ever
//!   run and would prove nothing about either.
//!
//! **`transaction_log` is empty in both.** The rows go into `concepts` and
//! `links` directly, which is right for the queries these fixtures were built
//! for and wrong for any question about the ledger itself. A plan on an empty
//! table is seekable in a way that proves nothing, so W10.6's questions use a
//! database written through the public API instead — see
//! `tests/bitemporal_plan_tests.rs`, which says so at its own top.
//!
//! **Why this is a module rather than a helper in one test file.** It was one,
//! in `index_plan_tests`, until `operation_count_tests` needed the same
//! database. A copied fixture is the defect D-088 names: two measurements
//! reported against "the populated fixture" that quietly stopped being the same
//! one. The costs pinned in the two files are only comparable because the rows
//! and the statistics behind them are identical by construction.
//!
//! [D-149]: ../../docs/architecture/s13-decision-register.md

// Two consumers use overlapping but different subsets; the alternative is a
// per-item allow on nearly every item here.
#![allow(dead_code)]

use libsql::Builder;
use macrame::schema::ddl;

pub const TS: &str = "2026-01-01T00:00:00.000000Z";
pub const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// One hub with 150 edges, against 60 single-edge leaves.
///
/// The skew is what D-059's 4.4 ms → 1.06 s spread was measured on, and it is
/// what a code graph actually is.
pub const HUB_EDGES: usize = 150;
pub const LEAF_EDGES: usize = 60;
pub const CONCEPTS: usize = 260;

/// A freshly migrated, entirely empty database.
pub async fn migrated(db_path: &std::path::Path) -> libsql::Connection {
    let db = Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    macrame::schema::run_migrations(&conn).await.unwrap();
    conn
}

/// A fixture with rows and statistics — the shape production has since D-149.
///
/// # Why this had to exist the moment `ANALYZE` shipped
///
/// Through 0.12.3 every plan assertion ran against [`migrated`]. That was
/// *faithful*, because production had no statistics either — nothing in the
/// crate ran `ANALYZE`, so `sqlite_stat1` existed nowhere and SQLite costed
/// every plan against built-in defaults on both sides.
///
/// D-149 ended that, and it ends it **silently**: the empty fixture still
/// passes, still asserts a plan, and no longer asserts anything about the
/// planner that actually runs. A gate that quietly stops testing the thing it
/// names is worse than no gate.
pub async fn populated_and_analysed(db_path: &std::path::Path) -> libsql::Connection {
    let conn = populated_without_statistics(db_path).await;
    let _ = conn.query(ddl::ANALYZE, ()).await.unwrap();
    conn
}

/// The same rows, with **no** `ANALYZE` (0.13.25, W10.2, [D-198]).
///
/// The third fixture, and it exists to isolate one variable. [`migrated`] and
/// [`populated_and_analysed`] differ in *two* things — rows and statistics —
/// so a plan that differs between them says nothing about which one caused it.
/// This one against that one differs in statistics alone, which is what
/// `tests/statistics_effect_tests.rs` needs in order to ask whether statistics
/// change any plan the crate has.
///
/// It is also a real state, not just an instrument: it is what a process holds
/// between a bulk load and its first `optimize()`.
///
/// [D-198]: ../../docs/architecture/s13-decision-register.md#d-198
pub async fn populated_without_statistics(db_path: &std::path::Path) -> libsql::Connection {
    let conn = migrated(db_path).await;

    // Matches `Database::configure`. A fixture analysed without the limit would
    // hold statistics production never computes.
    let _ = conn.query(ddl::ANALYSIS_LIMIT, ()).await.unwrap();

    conn.execute("BEGIN", ()).await.unwrap();
    for i in 0..CONCEPTS {
        conn.execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, \
             recorded_at, retired) VALUES (?1, ?2, '', ?3, ?4, ?3, 0)",
            libsql::params![format!("c{i:04}"), format!("C{i}"), TS, OPEN],
        )
        .await
        .unwrap();
    }
    // The hub.
    for i in 1..=HUB_EDGES {
        insert_edge(&conn, "c0000", &format!("c{i:04}")).await;
    }
    // The leaves.
    for i in (HUB_EDGES + 1)..=(HUB_EDGES + LEAF_EDGES) {
        insert_edge(&conn, &format!("c{i:04}"), &format!("c{:04}", i + 1)).await;
    }
    conn.execute("COMMIT", ()).await.unwrap();
    conn
}

async fn insert_edge(conn: &libsql::Connection, source: &str, target: &str) {
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
         valid_to, weight, properties, recorded_at) \
         VALUES (?1, ?2, 'LINKS', ?3, ?4, 1.0, '{}', ?3)",
        libsql::params![source, target, TS, OPEN],
    )
    .await
    .unwrap();
}

/// The statistics guard, as an assertion rather than a comment.
///
/// If `ANALYZE` silently did nothing, every plan assertion downstream would
/// still pass and would be testing the empty-database planner under a name
/// claiming otherwise (D-150).
pub async fn assert_has_statistics(conn: &libsql::Connection) {
    let mut rows = conn
        .query("SELECT COUNT(*) FROM sqlite_stat1", ())
        .await
        .expect("sqlite_stat1 missing: ANALYZE did not run on this fixture");
    let stats: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert!(
        stats > 0,
        "the fixture analysed to zero statistics rows, so this is the \
         empty-database case wearing the populated one's name (D-150)"
    );
}

/// `EXPLAIN QUERY PLAN`, flattened to one line.
pub async fn plan_of(conn: &libsql::Connection, sql: &str) -> String {
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

// ---------------------------------------------------------------------------
// Operation counts (W10.1, D-195; shared with `bitemporal_plan_tests` in
// 0.13.39, §14 item 11)
// ---------------------------------------------------------------------------

/// What a statement's VDBE program does, in the three quantities that move with
/// the plan and with nothing else.
///
/// **Here rather than in `operation_count_tests.rs`, where it was written,**
/// because a second file needed it and copying it would have produced two
/// definitions of "a seek" that agree until one of them is edited. That is
/// [D-088]'s defect in a helper instead of in a fixture, and it is the same
/// argument that moved `populated_and_analysed` into this file in the first
/// place.
///
/// [D-088]: ../../docs/architecture/s13-decision-register.md
#[derive(Debug, PartialEq, Eq)]
pub struct Counts {
    /// Cursors opened for reading. One per table or index the statement
    /// touches, so a covering index shows one where an index-plus-lookup shows
    /// two. This is the number D-042's class moves.
    pub opens: usize,
    /// Positioning operations: the b-tree is entered at a computed key rather
    /// than at its start.
    pub seeks: usize,
    /// Cursors started at one end of a b-tree. A full scan has one; so does an
    /// index range scan whose bound is open on that side.
    pub rewinds: usize,
}

pub const fn counts(opens: usize, seeks: usize, rewinds: usize) -> Counts {
    Counts {
        opens,
        seeks,
        rewinds,
    }
}

/// The three integers, read out of `EXPLAIN`.
///
/// `EXPLAIN` compiles the statement and prints the VDBE program without running
/// it, so this is a function of the chosen plan and of nothing else — not of
/// how busy the machine is, which is the whole reason these are gated and
/// timings are not ([D-055], [D-070]).
///
/// [D-055]: ../../docs/architecture/s13-decision-register.md
/// [D-070]: ../../docs/architecture/s13-decision-register.md
pub async fn counts_of(conn: &libsql::Connection, sql: &str) -> Counts {
    let mut rows = conn.query(&format!("EXPLAIN {sql}"), ()).await.unwrap();
    let mut c = counts(0, 0, 0);
    while let Some(row) = rows.next().await.unwrap() {
        let op: String = row.get(1).unwrap();
        if op == "OpenRead" {
            c.opens += 1;
        } else if op.starts_with("Seek") || op == "NotExists" || op == "NotFound" {
            c.seeks += 1;
        } else if op == "Rewind" || op == "Last" {
            c.rewinds += 1;
        }
    }
    c
}
