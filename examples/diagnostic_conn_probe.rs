//! What one `diagnostic_conn()` costs, and where the cost is (W15.4, C-9,
//! D-256).
//!
//! C-9 asks for the file to be opened once per `Database` rather than once per
//! call. Before taking that, the question worth answering is how much of a
//! call the open actually is — a lazily cached handle is worth having if the
//! open dominates and is bookkeeping if it does not.
//!
//! The probe times the two halves separately against the same live database,
//! with the write actor running, because the numbers this crate quotes are
//! taken against a database that is being used rather than an idle file:
//!
//!   1. `Builder::new_local(..).flags(READ_ONLY).build()` — the open;
//!   2. `db.connect()` — minting a connection on an already-open handle;
//!   3. `Database::diagnostic_conn()` — the whole public call as shipped,
//!      which is (2) plus a `stat` plus `configure_common`.
//!
//! Best-of rather than mean, for [D-055]'s reason: the interesting number is
//! what the path costs when nothing else interferes, and a mean over a Windows
//! box measures the box. Every result is unwrapped rather than discarded —
//! a `let _ =` in a timing loop measures the error path and reports it as the
//! happy one.
//!
//! Run with:  cargo run --release --example diagnostic_conn_probe
//!
//! [D-055]: ../docs/architecture/s13-decision-register.md#d-055

use libsql::{Builder, OpenFlags};
use macrame::prelude::*;
use std::time::{Duration, Instant};

const ROUNDS: usize = 200;

fn report(label: &str, best: Duration, total: Duration) {
    println!(
        "  {label:<40} best {:>9.3} us   mean {:>9.3} us",
        best.as_secs_f64() * 1e6,
        total.as_secs_f64() * 1e6 / ROUNDS as f64,
    );
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_diag_probe_{}", std::process::id()));
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

    println!("diagnostic_conn, {ROUNDS} rounds, live WAL database, actor running\n");

    // 1. The open on its own.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let handle = Builder::new_local(&path)
            .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
            .build()
            .await
            .unwrap();
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(handle);
    }
    report("Builder::…build()  (the open)", best, total);

    // 2. `connect()` on a handle that is already open — what W15.4 leaves in
    //    the per-call path.
    let handle = Builder::new_local(&path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .unwrap();
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let conn = handle.connect().unwrap();
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(conn);
    }
    report("handle.connect()   (per call, after)", best, total);

    // 3. The shipped call, which is (2) plus a `stat` and `configure_common`.
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let conn = db.diagnostic_conn().await.unwrap();
        let d = t.elapsed();
        best = best.min(d);
        total += d;
        drop(conn);
    }
    report("Database::diagnostic_conn()", best, total);

    // What is left in the call after the connection stops being minted. The
    // `stat` stays per call on purpose: it is what keeps the documented
    // missing-file error a typed refusal rather than a stale connection.
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

    // The first call carries the open; every one after it does not. Reported
    // separately because a mean over 200 rounds hides it entirely, and it is
    // the only call that still pays what every call used to.
    let cold = Database::open_tuned(
        &dir.join("cold.db"),
        Tuning::default().cadence(CadencePolicy::Disabled),
    )
    .await
    .unwrap();
    let t = Instant::now();
    let conn = cold.diagnostic_conn().await.unwrap();
    let first = t.elapsed();
    drop(conn);
    let t = Instant::now();
    let conn = cold.diagnostic_conn().await.unwrap();
    let second = t.elapsed();
    drop(conn);
    println!(
        "\n  first call on a fresh handle             {:>8.3} ms\n  \
         second call on the same handle           {:>8.3} ms",
        first.as_secs_f64() * 1e3,
        second.as_secs_f64() * 1e3,
    );

    // Does `build()` touch the file at all? If it does not, a read-only
    // build against a path that does not exist must succeed and the failure
    // must arrive from `connect()`. This is the check that turns "the open is
    // free" from a suspicious timing into a mechanism.
    let missing = dir.join("no_such_file.db");
    let built = Builder::new_local(&missing)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await;
    println!(
        "
  build() against a missing file           {}",
        match &built {
            Ok(_) => "Ok  -- build() does not open anything".to_string(),
            Err(e) => format!("Err -- {e}"),
        }
    );
    if let Ok(h) = built {
        println!(
            "  connect() on that handle                 {}",
            match h.connect() {
                Ok(_) => "Ok".to_string(),
                Err(e) => format!("Err -- {e}"),
            }
        );
    }

    cold.close().await.unwrap();
    db.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
