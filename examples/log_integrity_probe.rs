//! W14.5 (review C-5) — what the reach guard's intactness test costs, and what
//! writing it down instead costs the archive.
//!
//! [`crate::temporal::replay::hot_log_is_intact`] answered one bit —
//! *has anything been deleted from `transaction_log`?* — with
//! `MIN(seq_id) = 1 AND COUNT(*) = MAX(seq_id)`. The comparison is exact and
//! was argued for at length. It is also a scan: `MIN` and `MAX` on the rowid
//! are index seeks, `COUNT(*)` is not, and the whole thing ran on every
//! recorded-time read below the newest surviving stamp, in front of a
//! hydration D-247 measured at a flat 0.14 ms.
//!
//! Schema v16 keeps the bit in a one-row table maintained by a trigger. That
//! is a trade, not a free win, and this probe prices both sides of it:
//!
//! 1. **The read.** The old scan against the new one-row read, at log sizes
//!    from a toy to §9's working ledger.
//! 2. **The write.** `trg_txlog_mark_gap` is `FOR EACH ROW` — SQLite has no
//!    statement-level triggers — so an archive session that removes N rows
//!    fires it N times, each firing a one-page `UPDATE` of the same value. The
//!    question is not whether that is wasteful in the abstract; it is whether
//!    it is visible against the delete it rides on.
//!
//! Run it with `--release`, or the numbers are the debug build's, not
//! SQLite's:
//!
//! ```text
//! cargo run --release --features metrics --example log_integrity_probe
//! ```

use std::time::Instant;

use libsql::Connection;

/// Log sizes for the read comparison. The last is what §9 calls a working
/// ledger, where the scan this replaces costs 32.6 ms.
const SIZES: &[usize] = &[2_000, 50_000, 500_000];

/// Rows in the archive-session measurement, and how many of them go. Two
/// thirds is roughly what `LOG_ARCHIVABLE` takes off a log whose entities have
/// been restated a few times each.
const ARCHIVE_ROWS: usize = 500_000;
const ARCHIVE_REMOVED: usize = 333_000;

const TS: &str = "2026-01-01T00:00:00.000000Z";

/// The v15 intactness test, verbatim.
const OLD: &str = "SELECT COUNT(*), MIN(seq_id), MAX(seq_id) FROM transaction_log";

/// The v16 one.
const NEW: &str = "SELECT rows_removed FROM log_integrity WHERE id = 1";

async fn open(dir: &tempfile::TempDir, name: &str) -> Connection {
    let conn = libsql::Builder::new_local(dir.path().join(name))
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    macrame::schema::run_migrations(&conn).await.unwrap();
    conn
}

/// Fill the log by writing concepts, which `trg_concepts_log_insert` logs.
///
/// Raw SQL rather than the public write path on purpose: this is measuring a
/// read against a log of a given size, and half a million round trips through
/// the actor would price the fixture instead.
async fn fill(conn: &Connection, rows: usize) {
    conn.execute("BEGIN", ()).await.unwrap();
    for i in 0..rows {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 't', ?2, ?2)",
            libsql::params![format!("c{i}"), TS],
        )
        .await
        .unwrap();
    }
    conn.execute("COMMIT", ()).await.unwrap();
}

/// Best and mean of `n` runs of `sql`, in milliseconds.
///
/// Every row is consumed and unwrapped. A best-of statistic selects for the
/// fastest iteration, which is exactly the one that would have done no work if
/// the result were discarded.
async fn time(conn: &Connection, sql: &str, n: usize) -> (f64, f64) {
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..n {
        let start = Instant::now();
        let mut rows = conn.query(sql, ()).await.unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("the query returned no row");
        let _: i64 = row.get(0).unwrap();
        let ms = start.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        total += ms;
    }
    (best, total / n as f64)
}

/// One archive-session delete, timed, with the trigger either present or not.
async fn archive_session(dir: &tempfile::TempDir, name: &str, with_trigger: bool) -> f64 {
    let conn = open(dir, name).await;
    fill(&conn, ARCHIVE_ROWS).await;
    if !with_trigger {
        conn.execute("DROP TRIGGER trg_txlog_mark_gap", ())
            .await
            .unwrap();
    }

    // `trg_txlog_guard_delete` refuses a delete unless this table exists
    // (D-126), so the measurement takes the route the archive takes.
    conn.execute("CREATE TABLE macrame_archive_session (marker INTEGER)", ())
        .await
        .unwrap();
    let start = Instant::now();
    conn.execute(
        "DELETE FROM transaction_log WHERE seq_id <= ?1",
        libsql::params![ARCHIVE_REMOVED as i64],
    )
    .await
    .unwrap();
    let ms = start.elapsed().as_secs_f64() * 1e3;
    conn.execute("DROP TABLE macrame_archive_session", ())
        .await
        .unwrap();

    if with_trigger {
        let bit: i64 = conn
            .query(NEW, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(bit, 1, "the session deleted rows and the bit did not move");
    }
    ms
}

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().unwrap();

    println!("== 1. the guard's intactness test, per log size ==");
    println!(
        "{:>10}  {:>18}  {:>18}",
        "log rows", "v15 COUNT(*)", "v16 one row"
    );
    for &size in SIZES {
        let conn = open(&dir, &format!("read{size}.db")).await;
        fill(&conn, size).await;

        // Warm both plans before timing either, so the first number measured
        // is not also the one paying for the page cache.
        let _ = time(&conn, OLD, 3).await;
        let _ = time(&conn, NEW, 3).await;

        let iterations = if size >= 500_000 { 20 } else { 200 };
        let (old_best, old_mean) = time(&conn, OLD, iterations).await;
        let (new_best, new_mean) = time(&conn, NEW, iterations).await;
        println!(
            "{size:>10}  {old_best:>8.4} / {old_mean:<7.4}  {new_best:>8.4} / {new_mean:<7.4}",
        );
    }
    println!("(best / mean, ms)");

    println!();
    println!("== 2. what the per-row trigger costs one archive session ==");
    let without = archive_session(&dir, "arch_without.db", false).await;
    let with = archive_session(&dir, "arch_with.db", true).await;
    println!("{ARCHIVE_REMOVED} of {ARCHIVE_ROWS} rows deleted in one statement:");
    println!("  without trg_txlog_mark_gap: {without:.1} ms");
    println!("     with trg_txlog_mark_gap: {with:.1} ms");
    println!(
        "  delta: {:.1} ms, {:.3} us per row deleted",
        with - without,
        (with - without) * 1e3 / ARCHIVE_REMOVED as f64
    );
}
