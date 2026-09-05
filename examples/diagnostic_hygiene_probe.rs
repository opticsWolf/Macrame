//! What it costs to notice that a shared diagnostic connection is dirty
//! (W15.5, D-257).
//!
//! [D-256] made `diagnostic_conn()` hand every caller the same connection, and
//! `diagnostic_query` is the one arbitrary-SQL surface this crate exposes. So
//! a `BEGIN`, a `CREATE TEMP TABLE`, an `ATTACH` or a `PRAGMA` left behind by
//! one caller is visible to the next. The worst of those is the `BEGIN`: a
//! read transaction on a WAL database pins a snapshot, which makes later
//! diagnostic reads answer stale *and* makes `checkpoint()` a no-op.
//!
//! Scrubbing costs something on **every** call, including the clean ones, and
//! a warm call was 18.6 µs of which all 18.6 was the `stat`. So the shape was
//! chosen from these numbers rather than from the design sketch:
//!
//!   1. `is_autocommit()` — a C call through libsql, no statement;
//!   2. one statement asking both dirtiness questions at once;
//!   3. the same two questions as separate statements, for the round-trip cost;
//!      3b. the same two asked as **pragmas**, which is what shipped;
//!   4. `PRAGMA busy_timeout` read back — the pragma [D-159] set, and the one
//!      neither `temp.sqlite_master` nor `pragma_database_list` can see;
//!   5. re-running `configure_common` unconditionally, as the alternative to
//!      detecting pragma dirt;
//!      5b. all of it in one block, then the whole call including the `stat`;
//!   6. the re-mint a dirty connection would pay: `connect()` plus configure.
//!
//! # What it answered
//!
//! The sketch asked for `temp.sqlite_master` and `pragma_database_list`, at
//! **7.8 µs**. The same two questions as `PRAGMA temp.schema_version` and
//! `PRAGMA database_list` are **2.4 µs** and shipped instead. `is_autocommit()`
//! is free at 0.04 µs, the `ROLLBACK` it gates is 2.4 µs and only on the calls
//! that leaked, and restating `busy_timeout` is 1.0 µs — cheaper than deciding
//! whether to.
//!
//! **The parts do not add, and that is the reason arms 5b and 5d exist.** The
//! scrub's statements measure 3.5 µs in a loop of their own and the `stat`
//! measures 18.3 in a loop of its own; the two in *one* loop are 27.6, and the
//! shipped call is 29.8. Interleaving a filesystem syscall with SQLite work
//! costs more than either alone, so the scrub's honest price is the **11 µs**
//! the whole call moved, not the 3.5 its parts sum to. A best-of of sums is not
//! a sum of best-ofs, and quoting the second would have understated this
//! release by 3×.
//!
//! Rejected on the strength of arm 5d: that the gap was the *future's* size —
//! the cold open's state machine inlined into the warm path's. `Box::pin`ning
//! the cold arm changed nothing measurable, and the simulation in 5d reproduces
//! the whole gap with no state machine of its own.
//!
//! Best-of rather than mean, for [D-055]'s reason, and every result unwrapped
//! rather than discarded — a `let _ =` in a timing loop measures the error
//! path and reports it as the happy one.
//!
//! Run with:  cargo run --release --example diagnostic_hygiene_probe
//!
//! [D-055]: ../docs/architecture/s13-decision-register.md#d-055
//! [D-159]: ../docs/architecture/s13-decision-register.md#d-159
//! [D-256]: ../docs/architecture/s13-decision-register.md#d-256

use libsql::{Builder, OpenFlags};
use macrame::prelude::*;
use std::time::{Duration, Instant};

const ROUNDS: usize = 200;

fn report(label: &str, best: Duration, total: Duration) {
    println!(
        "  {label:<44} best {:>9.3} us   mean {:>9.3} us",
        best.as_secs_f64() * 1e6,
        total.as_secs_f64() * 1e6 / ROUNDS as f64,
    );
}

/// Both dirtiness questions in one statement: does this connection carry temp
/// objects, and is anything attached beyond `main` and `temp`?
const DIRTY_ONE: &str = "SELECT (SELECT count(*) FROM temp.sqlite_master) \
                         + (SELECT count(*) FROM pragma_database_list) - 2";

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_hyg_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("probe.db");

    let db = Database::open_tuned(&path, Tuning::default().cadence(CadencePolicy::Disabled))
        .await
        .unwrap();
    db.write_concepts(vec![ConceptUpsert::new("a", "A")
        .content("body")
        .valid_from("2026-01-01T00:00:00.000000Z")])
        .await
        .unwrap();

    let conn = db.diagnostic_conn().await.unwrap();

    println!("diagnostic hygiene, {ROUNDS} rounds, live WAL database, actor running");
    println!();

    // 0. The two numbers every candidate below is measured against.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let c = db.diagnostic_conn().await.unwrap();
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(c);
    }
    report("Database::diagnostic_conn()  (warm, today)", best, total);

    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        assert!(path.exists());
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("  of which: path.exists()", best, total);
    println!();

    // 1. The free one.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        assert!(conn.is_autocommit());
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("is_autocommit()", best, total);

    // The floor for anything that has to run a statement at all.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query("SELECT 1", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let v: i64 = row.get(0).unwrap();
        let d = t.elapsed();
        assert_eq!(v, 1);
        best = best.min(d);
        total += d;
    }
    report("  floor: one trivial statement", best, total);

    // 2. Both questions in one statement.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query(DIRTY_ONE, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let dirt: i64 = row.get(0).unwrap();
        let d = t.elapsed();
        assert_eq!(dirt, 0);
        best = best.min(d);
        total += d;
    }
    report("dirt check, one statement", best, total);

    // 3. The same, as two.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn
            .query("SELECT count(*) FROM temp.sqlite_master", ())
            .await
            .unwrap();
        let a: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let mut rows = conn
            .query("SELECT count(*) FROM pragma_database_list", ())
            .await
            .unwrap();
        let b: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let d = t.elapsed();
        assert_eq!((a, b), (0, 2));
        best = best.min(d);
        total += d;
    }
    report("dirt check, two statements", best, total);

    // 3b. The same two questions asked as pragmas rather than as a query over
    //     the table-valued functions, which is where the 7 us above goes.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query("PRAGMA temp.schema_version", ()).await.unwrap();
        let sv: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let mut rows = conn.query("PRAGMA database_list", ()).await.unwrap();
        let mut n = 0;
        while rows.next().await.unwrap().is_some() {
            n += 1;
        }
        let d = t.elapsed();
        assert_eq!(n, 2, "database_list should be main + temp");
        let _ = sv;
        best = best.min(d);
        total += d;
    }
    report("dirt check, two pragmas", best, total);

    // 4. The pragma neither of those can see.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query("PRAGMA busy_timeout", ()).await.unwrap();
        let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let d = t.elapsed();
        assert_eq!(v, 5000);
        best = best.min(d);
        total += d;
    }
    report("PRAGMA busy_timeout  (read back)", best, total);

    // 5. Restating the crate's pragmas instead of detecting that they moved.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query("PRAGMA busy_timeout = 5000", ()).await.unwrap();
        let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let d = t.elapsed();
        assert_eq!(v, 5000);
        best = best.min(d);
        total += d;
    }
    report("PRAGMA busy_timeout = 5000  (restate)", best, total);

    // 5b. The three parts above run back to back, on the connection they will
    //     actually run on, plus the mutex that guards the slot. Measured as one
    //     block because the sum of the arms above did not account for what the
    //     shipped call costs, and a sum of best-ofs is not a best-of a sum.
    let slot = tokio::sync::Mutex::new(Some(conn.clone()));
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let guard = slot.lock().await;
        let c = guard.as_ref().unwrap();
        assert!(c.is_autocommit());
        let mut rows = c.query("PRAGMA temp.schema_version", ()).await.unwrap();
        let _sv: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let mut rows = c.query("PRAGMA database_list", ()).await.unwrap();
        let mut n = 0;
        while rows.next().await.unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 2);
        let mut rows = c.query("PRAGMA busy_timeout = 5000", ()).await.unwrap();
        let _v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let out = c.clone();
        drop(guard);
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(out);
    }
    report("the whole scrub, in one block", best, total);

    // Just the mutex, so the block above can be attributed.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let guard = slot.lock().await;
        let out = guard.as_ref().unwrap().clone();
        drop(guard);
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(out);
    }
    report("  of which: lock + Connection::clone", best, total);

    // 5c. `configure_common` drops its `Rows` without stepping it (`let _ =`),
    //     which is how it has always been written. Measured against the stepped
    //     form because the scrub as shipped cost 10.5 us in situ against 3.7 us
    //     for the same statements inline, and this is the only difference
    //     between the two.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let _ = conn.query("PRAGMA busy_timeout = 5000", ()).await.unwrap();
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("PRAGMA busy_timeout = 5000, Rows dropped", best, total);

    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let mut rows = conn.query("PRAGMA temp.schema_version", ()).await.unwrap();
        let _sv: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        drop(rows);
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("  one pragma, stepped then dropped", best, total);

    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let rows = conn.query("PRAGMA temp.schema_version", ()).await.unwrap();
        drop(rows);
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("  one pragma, dropped unstepped", best, total);

    // 5d. The `stat` and the scrub in one loop — the shipped call, simulated.
    //     The two measured apart are 18.3 + 3.7; the shipped call is 29.8. If
    //     this arm lands on 29.8 the composition is the cost and the parts
    //     simply do not add; if it lands on 22 the crate is doing something the
    //     simulation is not.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        assert!(path.exists());
        let guard = slot.lock().await;
        let c = guard.as_ref().unwrap();
        assert!(c.is_autocommit());
        let mut rows = c.query("PRAGMA temp.schema_version", ()).await.unwrap();
        let _sv: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let mut rows = c.query("PRAGMA database_list", ()).await.unwrap();
        let mut n = 0;
        while rows.next().await.unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 2);
        let _ = c.query("PRAGMA busy_timeout = 5000", ()).await.unwrap();
        let out = c.clone();
        drop(guard);
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(out);
    }
    report("stat + scrub, one loop (the whole call)", best, total);

    // And the `stat` alone, measured again *here* rather than 200 lines up, in
    // case what it costs depends on what ran just before it.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        assert!(path.exists());
        let d = t.elapsed();
        best = best.min(d);
        total += d;
    }
    report("  path.exists(), measured again here", best, total);

    // 6. What a dirty call would pay to start over.
    let handle = Builder::new_local(&path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .unwrap();
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let fresh = handle.connect().unwrap();
        let mut rows = fresh.query("PRAGMA busy_timeout = 5000", ()).await.unwrap();
        let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let d = t.elapsed();
        assert_eq!(v, 5000);
        best = best.min(d);
        total += d;
        drop(fresh);
    }
    report("re-mint: connect() + configure_common", best, total);

    // Does a rollback on an already-clean connection cost anything worth
    // avoiding? Only reached when `is_autocommit()` says false, but the price
    // decides whether the guard needs the check in front of it at all.
    println!();
    let _ = conn.query("BEGIN", ()).await.unwrap();
    println!(
        "  after BEGIN, is_autocommit()               {}",
        conn.is_autocommit()
    );
    let mut rows = conn.query(DIRTY_ONE, ()).await.unwrap();
    let dirt: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  after BEGIN, dirt counter                  {dirt}");
    let t = Instant::now();
    let _ = conn.query("ROLLBACK", ()).await.unwrap();
    println!(
        "  ROLLBACK of a leaked read transaction      {:>9.3} us",
        t.elapsed().as_secs_f64() * 1e6
    );
    println!(
        "  after ROLLBACK, is_autocommit()            {}",
        conn.is_autocommit()
    );

    // And the dirt counter against each hazard, so the check is shown to
    // detect what it claims to rather than assumed to.
    println!();
    let _ = conn
        .query("CREATE TEMP TABLE probe_t(x)", ())
        .await
        .unwrap();
    let mut rows = conn.query(DIRTY_ONE, ()).await.unwrap();
    let dirt: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  dirt counter after CREATE TEMP TABLE       {dirt}");
    let mut rows = conn.query("PRAGMA temp.schema_version", ()).await.unwrap();
    let sv: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  temp.schema_version after TEMP TABLE       {sv}");
    let aux = dir.join("aux.db");
    Database::open_tuned(&aux, Tuning::default().cadence(CadencePolicy::Disabled))
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let _ = conn
        .query(&format!("ATTACH DATABASE '{}' AS aux", aux.display()), ())
        .await
        .unwrap();
    let mut rows = conn.query(DIRTY_ONE, ()).await.unwrap();
    let dirt: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  dirt counter after ATTACH                  {dirt}");
    let mut rows = conn.query("PRAGMA database_list", ()).await.unwrap();
    let mut n = 0;
    while rows.next().await.unwrap().is_some() {
        n += 1;
    }
    println!("  database_list rows after ATTACH            {n}");
    let _ = conn
        .query("PRAGMA case_sensitive_like = ON", ())
        .await
        .unwrap();
    let mut rows = conn.query(DIRTY_ONE, ()).await.unwrap();
    let dirt: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  dirt counter after a PRAGMA                {dirt}   <-- invisible");

    drop(conn);
    db.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
