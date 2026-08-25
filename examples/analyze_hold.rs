//! §8 acceptance item 2: `analysis_limit` bounds `ANALYZE`'s hold — **measured**.
//!
//! [D-149] argues the bound by construction: `PRAGMA analysis_limit = 400` caps
//! rows examined per index, "making the cost a function of the index count
//! (four) rather than the table size". §8 asks for that measured rather than
//! asserted, and the difference is the point — a bound that holds by
//! construction and is never observed is a bound nobody notices losing.
//!
//! ```text
//! cargo run --release --example analyze_hold
//! ```
//!
//! # Two questions, and they have different answers
//!
//! 1. **Is the pragma in force on the connection that runs `ANALYZE`?** Yes.
//!    That connection is the write actor's and no test can reach it (see
//!    `tests/analyze_tests.rs`), so this is established indirectly: the same
//!    file is re-opened as a plain libSQL connection and `ANALYZE` timed with
//!    the limit off and on. The crate's own hold matches the *on* arm.
//!
//! 2. **Does it make the hold independent of the table?** No. It is a constant
//!    factor of roughly 3–4×, and what is left still grows about linearly. So
//!    D-149's mechanism is real and its stated strength is not: `ANALYZE` on a
//!    40,000-edge ledger holds the write lock for ~19 ms, which is ~6×
//!    [`macrame::CHUNK_BUDGET`].
//!
//! That is why `analyze()` is low-priority work rather than a background timer,
//! and why `optimize()` — which does nothing when nothing has moved — is what
//! `close()` calls.
//!
//! [D-149]: ../docs/architecture/s13-decision-register.md#d-149

use macrame::metrics::CommandKind;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Build a ledger with skewed out-degree and return its path.
///
/// Skew is load-bearing for the same reason as in `tests/analyze_tests.rs`:
/// uniform data is exactly where measured statistics and SQLite's defaults
/// agree, so a uniform fixture measures the wrong thing (D-088).
async fn build(dir: &std::path::Path, edges: usize) -> (std::path::PathBuf, Database) {
    let path = dir.join(format!("analyze_{edges}.db"));
    let db = Database::open_with_cadence(&path, None).await.unwrap();

    let concepts: Vec<_> = (0..=edges)
        .map(|i| ConceptUpsert::new(format!("c{i:06}"), format!("C{i}")).valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();

    let hub = edges / 4;
    let mut assertions = Vec::with_capacity(edges);
    for i in 1..=hub {
        assertions.push(
            EdgeAssertion::new("c000000", format!("c{i:06}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for i in hub + 1..edges {
        assertions.push(
            EdgeAssertion::new(format!("c{i:06}"), format!("c{:06}", i + 1), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    db.bulk_import(assertions).await.unwrap();
    (path, db)
}

/// The actor's own hold for one `analyze()`, with the turn counts that say it
/// is *this* call.
///
/// The turn counts are still checked even though `CommandKind::Analyze` stopped
/// covering `optimize()` in 0.13.24 (W10.5, D-197): the guard is about *this
/// call* being the only `analyze()`, which the split does not establish.
async fn crate_hold(db: &Database) -> (u64, u64, std::time::Duration) {
    let before = analyze_turns(db);
    db.analyze().await.unwrap();
    let snap = db.metrics();
    let k = snap.kinds.iter().find(|k| k.kind == CommandKind::Analyze);
    (
        before,
        k.map(|k| k.turns).unwrap_or(0),
        k.map(|k| k.longest).unwrap_or_default(),
    )
}

fn analyze_turns(db: &Database) -> u64 {
    db.metrics()
        .kinds
        .iter()
        .find(|k| k.kind == CommandKind::Analyze)
        .map(|k| k.turns)
        .unwrap_or(0)
}

/// `ANALYZE` on a plain connection over the same file, at a chosen limit.
///
/// The control arm. `0` is SQLite's own default and means *no limit*.
async fn bare_analyze(path: &std::path::Path, limit: u32) -> (i64, std::time::Duration) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let _ = conn.query("PRAGMA journal_mode = WAL", ()).await.unwrap();
    // Set the way `configure_writable` sets it: `query`, result dropped.
    let _ = conn
        .query(&format!("PRAGMA analysis_limit = {limit}"), ())
        .await
        .unwrap();
    let readback: i64 = conn
        .query("PRAGMA analysis_limit", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .map(|r| r.get(0).unwrap())
        .unwrap_or(-1);
    // Each arm is a cold ANALYZE: statistics left by the previous one would
    // make the second arm measure a re-analysis rather than an analysis.
    let _ = conn.execute("DROP TABLE IF EXISTS sqlite_stat1", ()).await;
    let t = std::time::Instant::now();
    conn.execute("ANALYZE", ()).await.unwrap();
    (readback, t.elapsed())
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_analyze_hold_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    println!("edges    crate hold   limit=off   limit=400   readback");
    let mut on = Vec::new();
    for edges in [10_000usize, 40_000] {
        let (path, db) = build(&dir, edges).await;
        let (before, after, held) = crate_hold(&db).await;
        assert_eq!(
            (before, after),
            (0, 1),
            "the Analyze counter moved {before} -> {after}: something other than \
             this call ran ANALYZE or optimize, so `longest` is not this hold"
        );
        let snap = db.metrics();
        let violations: Vec<(String, u64)> = snap
            .budget_violations()
            .iter()
            .map(|k| (k.kind.as_str().to_string(), k.over_budget))
            .collect();
        println!("  over budget after analyze(): {violations:?}");
        db.close().await.unwrap();

        let (_, off) = bare_analyze(&path, 0).await;
        let (readback, capped) = bare_analyze(&path, 400).await;
        println!("{edges:<8} {held:<12?} {off:<11?} {capped:<11?} {readback}");
        on.push((edges, held, off, capped));
    }

    let (n0, held0, off0, cap0) = on[0];
    let (n1, held1, off1, cap1) = on[1];
    let rows = n1 as f64 / n0 as f64;
    println!();
    println!("table grew {rows:.0}x over this range:");
    println!(
        "  uncapped ANALYZE grew {:.1}x, capped grew {:.1}x, the crate's hold grew {:.1}x",
        off1.as_secs_f64() / off0.as_secs_f64(),
        cap1.as_secs_f64() / cap0.as_secs_f64(),
        held1.as_secs_f64() / held0.as_secs_f64(),
    );
    println!(
        "  the cap is worth {:.1}x at {n0} and {:.1}x at {n1}",
        off0.as_secs_f64() / cap0.as_secs_f64(),
        off1.as_secs_f64() / cap1.as_secs_f64(),
    );
    println!();
    println!(
        "The crate's hold tracks the capped arm, not the uncapped one, so the \
         pragma is in force on the write connection.\nIt is a constant factor, \
         not a bound: what is left still grows with the table, and at {n1} edges \
         the hold is {held1:?} against a {:?} budget.",
        macrame::CHUNK_BUDGET
    );

    let _ = std::fs::remove_dir_all(&dir);
}
