//! W10.5 reconnaissance: what does `optimize()` hold, as distinct from
//! `analyze()`?
//!
//! [D-168] left the budget-exemption question undecided **because the kind is
//! shared**: `CommandKind::Analyze` covers [`Database::optimize`] as well as
//! [`Database::analyze`], and `close()` calls `optimize()` unconditionally. So
//! an exemption argued about the explicit call would land on the automatic one
//! without ever being argued about it. W10.5 splits the kind; this measures the
//! half nobody has measured, because "then decide each on its merits" needs a
//! number for the second one.
//!
//! ```text
//! cargo run --release --example optimize_hold
//! ```
//!
//! # The question
//!
//! `analyze()` is 19.1 ms at 40,000 edges and every call is a permanent budget
//! violation (`examples/analyze_hold.rs`, [D-166]). `optimize()` is documented
//! as "a no-op when nothing has moved" — but nobody has checked what *nothing
//! has moved* costs, or what the first call after a bulk load costs, and those
//! are the two states `close()` is actually in.
//!
//! Four arms, in the order a process meets them:
//!
//! 1. **Cold** — `optimize()` on a database that has never been analysed.
//! 2. **Idle** — immediately after, with nothing changed in between.
//! 3. **Drifted a little** — after a small write.
//! 4. **Drifted a lot** — after doubling the ledger.
//!
//! [D-166]: ../docs/architecture/s13-decision-register.md#d-166
//! [D-168]: ../docs/architecture/s13-decision-register.md#d-168

use macrame::metrics::CommandKind;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The same skewed shape `analyze_hold` uses, so the two numbers are about the
/// same database (D-088).
fn edges_from(lo: usize, hi: usize) -> Vec<EdgeAssertion> {
    let hub = hi / 4;
    let mut out = Vec::new();
    for i in lo.max(1)..=hub.min(hi) {
        out.push(
            EdgeAssertion::new("c000000", format!("c{i:06}"), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    for i in lo.max(hub + 1)..hi {
        out.push(
            EdgeAssertion::new(format!("c{i:06}"), format!("c{:06}", i + 1), "LINKS")
                .valid_from(TS)
                .valid_to(OPEN),
        );
    }
    out
}

/// `sqlite_stat1` as one string, read off a second connection.
///
/// The actor's connection is not reachable and the file is, which is the same
/// route `analyze_hold` takes to establish `analysis_limit` is in force.
async fn stat1(path: &std::path::Path) -> String {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = match conn
        .query(
            "SELECT tbl, idx, stat FROM sqlite_stat1 ORDER BY tbl, idx",
            (),
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return String::from("<no sqlite_stat1>"),
    };
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(format!(
            "{}/{}={}",
            r.get::<String>(0).unwrap_or_default(),
            r.get::<String>(1).unwrap_or_default(),
            r.get::<String>(2).unwrap_or_default()
        ));
    }
    out.join(" ")
}

/// Turns and longest hold for one kind, or zeroes if it has never run.
fn stats(db: &Database, kind: CommandKind) -> (u64, u64, std::time::Duration) {
    let snap = db.metrics();
    snap.kinds
        .iter()
        .find(|k| k.kind == kind)
        .map(|k| (k.turns, k.over_budget, k.longest))
        .unwrap_or((0, 0, std::time::Duration::ZERO))
}

/// One `optimize()`, timed from the caller's side *and* read off the actor.
///
/// Both, because they answer different questions: the caller's elapsed time
/// includes the queue, and the budget is about the hold. A large gap between
/// them is the scheduler working, not a defect.
async fn one_optimize(db: &Database, label: &str) {
    let (turns_before, _, _) = stats(db, CommandKind::Optimize);
    let t = std::time::Instant::now();
    db.optimize().await.unwrap();
    let wall = t.elapsed();
    let (turns_after, over, longest) = stats(db, CommandKind::Optimize);
    assert_eq!(
        turns_after,
        turns_before + 1,
        "the Optimize counter moved {turns_before} -> {turns_after}: something \
         else ran one, so `longest` is not this hold"
    );
    println!("  {label:<22} wall={wall:<12?} longest_hold={longest:<12?} over_budget={over}");
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_optimize_hold_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("optimize.db");

    let half = 20_000usize;
    let full = 40_000usize;

    let db = Database::open_with_cadence(&path, None).await.unwrap();
    let concepts: Vec<_> = (0..=full)
        .map(|i| ConceptUpsert::new(format!("c{i:06}"), format!("C{i}")).valid_from(TS))
        .collect();
    db.write_concepts(concepts).await.unwrap();
    db.bulk_import(edges_from(1, half)).await.unwrap();

    println!("budget = {:?}", macrame::CHUNK_BUDGET);
    println!("\n===== optimize(), by how far the statistics have drifted =====");

    one_optimize(&db, "cold (never analysed)").await;
    one_optimize(&db, "idle (nothing moved)").await;

    db.bulk_import(edges_from(half, half + 200)).await.unwrap();
    one_optimize(&db, "after a small write").await;

    db.bulk_import(edges_from(half + 200, full)).await.unwrap();
    one_optimize(&db, "after doubling").await;

    one_optimize(&db, "idle again").await;

    // The arm that matters to W10.2. `PRAGMA optimize` does not re-analyse
    // because rows arrived; it re-analyses when its own staleness heuristic
    // says the statistics no longer describe the table, and that heuristic is a
    // *ratio*. Doubling is not enough. So the question "how much growth does it
    // take" gets an answer here rather than an assumption, by reading
    // `sqlite_stat1` across the call instead of only timing it.
    println!(
        "
===== does optimize() actually rewrite the statistics? ====="
    );
    let mut grown = full;
    for factor in [2usize, 5, 25] {
        let target = full * factor;
        let before = stat1(&path).await;
        db.write_concepts(
            (grown..=target)
                .map(|i| ConceptUpsert::new(format!("c{i:06}"), format!("C{i}")).valid_from(TS))
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        db.bulk_import(edges_from(grown, target)).await.unwrap();
        grown = target;

        let (t0, _, _) = stats(&db, CommandKind::Optimize);
        db.optimize().await.unwrap();
        let (t1, _, longest) = stats(&db, CommandKind::Optimize);
        assert_eq!(t1, t0 + 1, "somebody else ran an optimize");
        let after = stat1(&path).await;
        println!(
            "  {factor:>2}x the original ledger: stat1 {} (longest hold now {longest:?})",
            if after == before {
                "UNCHANGED -- optimize() declined"
            } else {
                "rewritten"
            }
        );
    }

    // The comparison the exemption question turns on: the explicit call does
    // the work unconditionally, at the same ledger size, right after an
    // optimize() has just declared nothing stale.
    println!("\n===== analyze(), same ledger, for scale =====");
    let (t_before, _, _) = stats(&db, CommandKind::Analyze);
    db.analyze().await.unwrap();
    let (t_after, over, longest) = stats(&db, CommandKind::Analyze);
    assert_eq!((t_before, t_after), (0, 1), "somebody else ran ANALYZE");
    println!("  analyze()              longest_hold={longest:<12?} over_budget={over}");

    println!("\n===== what budget_violations() says now =====");
    for k in db.metrics().budget_violations() {
        println!(
            "  {:<10} turns={:<4} over={:<4} longest={:?}",
            k.kind.as_str(),
            k.turns,
            k.over_budget,
            k.longest
        );
    }

    // close() runs one more optimize(), on a database whose statistics were
    // just rebuilt. That is the automatic call D-168 refused to exempt blind.
    let (closing_turns, _, _) = stats(&db, CommandKind::Optimize);
    db.close().await.unwrap();
    println!("\noptimize() turns before close(): {closing_turns}");

    let _ = std::fs::remove_dir_all(&dir);
}
