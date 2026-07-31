//! T5.1 reconnaissance: does `SQLITE_OPEN_READONLY` hold, on this engine, on a
//! live WAL database that the write actor is using at the same time?
//!
//! T5.1 proposes `diagnostic_conn()` opening the file read-only "at the OS
//! level, which is a boundary rather than a pragma". That is the right shape
//! *if* three things are true on libSQL 0.9.30, and none of them is obvious:
//!
//!   1. a read-only open succeeds at all against a **WAL** database — SQLite
//!      needs a writable `-shm` for WAL, and a read-only connection is normally
//!      permitted only because the *file* is writable even though the
//!      *connection* is not;
//!   2. writes through it are actually refused, rather than refused by
//!      `query_only` semantics that a holder can turn off;
//!   3. `PRAGMA query_only = OFF` does **not** rescue it — which is the whole
//!      claim, since `read_conn()`'s pragma is reversible in one statement and
//!      that is T5.1's stated reason for wanting something stronger.
//!
//! It also checks the case the proposal does not mention: opening read-only
//! against a path that **does not exist yet**, since `SQLITE_OPEN_CREATE` is
//! dropped along with write access.
//!
//! Run with:  cargo run --example readonly_open_probe

use libsql::{Builder, OpenFlags};
use macrame::prelude::*;

// Generic over the error because `macrame::prelude` puts its own `Result` alias
// in scope: the raw connections here return `libsql::Error`, `read_conn()`'s
// callers return `DbError`, and the probe wants both in one table.
async fn report<E: std::fmt::Display>(label: &str, r: std::result::Result<(), E>) {
    match r {
        Ok(()) => println!("  {label:<44} ALLOWED"),
        Err(e) => println!("  {label:<44} refused: {e}"),
    }
}

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("probe.db");

    // A live database with the actor running and a real write behind it.
    let db = Database::open_with_cadence(&path, None).await.unwrap();
    db.write_concepts(vec![ConceptUpsert::new("a", "A")
        .content("body")
        .valid_from("2026-01-01T00:00:00.000000Z")])
        .await
        .unwrap();

    println!("journal mode: {}", {
        let mut rows = db
            .read_conn()
            .query("PRAGMA journal_mode", ())
            .await
            .unwrap();
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap()
    });

    // ---- 1. does a read-only open succeed against the live WAL file? ----
    let ro = Builder::new_local(&path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await;
    let ro = match ro {
        Ok(d) => {
            println!("\nread-only open: OK");
            d
        }
        Err(e) => {
            println!("\nread-only open FAILED: {e}");
            return;
        }
    };
    let conn = ro.connect().unwrap();

    println!("\nthrough a SQLITE_OPEN_READ_ONLY connection:");

    // Reads must still work, or the connection is useless as a diagnostic.
    let read = conn
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .map(|_| ());
    report("SELECT COUNT(*) FROM concepts", read).await;

    let explain = conn
        .query("EXPLAIN QUERY PLAN SELECT * FROM links_current", ())
        .await
        .map(|_| ());
    report("EXPLAIN QUERY PLAN", explain).await;

    // ---- 2. are writes refused? ----
    let ins = conn
        .execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, \
             recorded_at, retired) VALUES ('x','X','','2026-01-01T00:00:00.000000Z', \
             '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)",
            (),
        )
        .await
        .map(|_| ());
    report("INSERT", ins).await;

    let ddl = conn
        .execute("CREATE TABLE t (x INTEGER)", ())
        .await
        .map(|_| ());
    report("CREATE TABLE", ddl).await;

    let tmp = conn
        .execute("CREATE TEMP TABLE t (x INTEGER)", ())
        .await
        .map(|_| ());
    report("CREATE TEMP TABLE", tmp).await;

    // ---- 3. can the holder turn it off, the way query_only can be? ----
    let off = conn
        .execute("PRAGMA query_only = OFF", ())
        .await
        .map(|_| ());
    report("PRAGMA query_only = OFF", off).await;

    let ins2 = conn
        .execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, \
             recorded_at, retired) VALUES ('y','Y','','2026-01-01T00:00:00.000000Z', \
             '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)",
            (),
        )
        .await
        .map(|_| ());
    report("INSERT after query_only = OFF", ins2).await;

    // ---- the same three, through `read_conn()`, for comparison ----
    println!("\nthrough read_conn() (PRAGMA query_only = ON), for comparison:");
    let rc = db.read_conn();
    let ins3 = rc
        .execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, \
             recorded_at, retired) VALUES ('z','Z','','2026-01-01T00:00:00.000000Z', \
             '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)",
            (),
        )
        .await
        .map(|_| ());
    report("INSERT", ins3).await;
    let off2 = rc.execute("PRAGMA query_only = OFF", ()).await.map(|_| ());
    report("PRAGMA query_only = OFF", off2).await;
    let ins4 = rc
        .execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, \
             recorded_at, retired) VALUES ('w','W','','2026-01-01T00:00:00.000000Z', \
             '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)",
            (),
        )
        .await
        .map(|_| ());
    report("INSERT after query_only = OFF", ins4).await;
    // Put it back, so `close()` is not run against a reader the probe disarmed.
    let _ = rc.execute("PRAGMA query_only = ON", ()).await;

    // ---- 4. read-only against a path that does not exist ----
    let missing = dir.path().join("nope.db");
    let r = Builder::new_local(&missing)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await;
    println!(
        "\nread-only open of a nonexistent path: {}",
        match r {
            Ok(d) => match d.connect() {
                Ok(_) => "build OK, connect OK (file created?)".to_string(),
                Err(e) => format!("build OK, connect failed: {e}"),
            },
            Err(e) => format!("failed: {e}"),
        }
    );
    println!("  file now exists: {}", missing.exists());

    drop(conn);
    drop(ro);
    db.close().await.unwrap();
}
