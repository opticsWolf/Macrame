//! Is C-13's failure reachable, and is the cause the one the review names?
//!
//! The review says `COLD_SCHEMA` running before `BEGIN IMMEDIATE` leaves a cold
//! file with schema and no horizon row when a session fails, and that
//! `hot_log_reach` then sees "an archive exists" for a file nothing was written
//! to. There is a step in that worth checking rather than assuming: every
//! reader keys on `archive_path.exists()`, and ATTACH runs before the DDL.
//!
//! Part 1 walks the file into existence, statement by statement.
//! Part 2 asks what a leftover file does to an ordinary read.
use macrame::{ConceptUpsert, Database};
use std::path::Path;

async fn raw(path: &Path) -> libsql::Connection {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    db.connect().unwrap()
}

async fn tables(c: &libsql::Connection, schema: &str) -> Vec<String> {
    let mut rows = c
        .query(
            &format!("SELECT name FROM {schema}.sqlite_master WHERE type='table' ORDER BY name"),
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("macrame_cold_reach_probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // ---------------------------------------------------------- part 1
    println!("--- when does the cold file appear? ---");
    let cold = dir.join("cold.db");
    let c = raw(&dir.join("raw.db")).await;
    println!("before ATTACH:  exists = {}", cold.exists());
    c.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![cold.to_string_lossy().as_ref()],
    )
    .await
    .unwrap();
    println!(
        "after ATTACH:   exists = {}, tables = {:?}",
        cold.exists(),
        tables(&c, "cold").await
    );
    c.execute(
        "CREATE TABLE IF NOT EXISTS cold.archive_horizon (archived_at TEXT, cutoff TEXT, horizon INTEGER)",
        (),
    )
    .await
    .unwrap();
    println!(
        "after cold DDL: exists = {}, tables = {:?}",
        cold.exists(),
        tables(&c, "cold").await
    );
    let _ = c.execute("DETACH DATABASE cold", ()).await;
    drop(c);

    // ---------------------------------------------------------- part 2
    println!();
    println!("--- what a leftover cold file does to an ordinary read ---");
    let hot = dir.join("ledger.db");
    let db = Database::open(&hot).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("a", "a").valid_from("2020-01-01T00:00:00.000000Z"))
        .await
        .unwrap();
    let archive = db.archive_path().to_path_buf();
    let now = "2099-01-01T00:00:00.000000Z";

    let before = db.reconstruct(now).await;
    println!(
        "no cold file:   reconstruct -> {}",
        match &before {
            Ok(s) => format!("ok, {} concepts", s.concepts.len()),
            Err(e) => format!("{e}"),
        }
    );

    // Exactly what an archive session that failed at its first statement leaves
    // behind: ATTACH created the file, nothing else ran.
    let c2 = raw(&hot).await;
    c2.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive.to_string_lossy().as_ref()],
    )
    .await
    .unwrap();
    let _ = c2.execute("DETACH DATABASE cold", ()).await;
    drop(c2);
    println!(
        "left behind:    {:?}, {} bytes",
        archive.file_name().unwrap(),
        std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0)
    );

    let after = db.reconstruct(now).await;
    println!(
        "0-byte cold:    reconstruct -> {}",
        match &after {
            Ok(s) => format!("ok, {} concepts", s.concepts.len()),
            Err(e) => format!("{e}"),
        }
    );

    // The instant that forces the cold arm: below the newest hot stamp, so the
    // `MAX <= ts` short-circuit does not fire and the archive path is taken.
    let early = "2021-01-01T00:00:00.000000Z";
    let deep = db.reconstruct(early).await;
    println!(
        "0-byte cold:    reconstruct(early) -> {}",
        match &deep {
            Ok(s) => format!("ok, {} concepts", s.concepts.len()),
            Err(e) => format!("{e}"),
        }
    );

    db.close().await.unwrap();

    // ------------------------------------------------------------- part 3
    // The same state, reached through the public API alone and with nothing
    // failing unexpectedly: `rehydrate` on a ledger that has never been
    // archived. It ATTACHes (which creates the file), asks `cold.concepts` a
    // question, is told the table does not exist, and returns that error --
    // leaving the file behind.
    println!();
    println!("--- rehydrate on a never-archived ledger ---");
    let dir3 = dir.join("three");
    std::fs::create_dir_all(&dir3).unwrap();
    let hot3 = dir3.join("ledger.db");
    let db3 = Database::open(&hot3).await.unwrap();
    db3.upsert_concept(ConceptUpsert::new("a", "a").valid_from("2020-01-01T00:00:00.000000Z"))
        .await
        .unwrap();
    let arch3 = db3.archive_path().to_path_buf();
    println!("before:         archive file exists = {}", arch3.exists());
    println!(
        "before:         reconstruct(early) -> {}",
        match db3.reconstruct(early).await {
            Ok(s) => format!("ok, {} concepts", s.concepts.len()),
            Err(e) => format!("{e}"),
        }
    );
    let r = db3.rehydrate(&["a"]).await;
    println!(
        "rehydrate:      -> {}",
        match &r {
            Ok(rep) => format!("ok, {} rehydrated", rep.concepts_rehydrated),
            Err(e) => format!("{e}"),
        }
    );
    println!(
        "after:          archive file exists = {}, {} bytes",
        arch3.exists(),
        std::fs::metadata(&arch3).map(|m| m.len()).unwrap_or(0)
    );
    println!(
        "after:          reconstruct(early) -> {}",
        match db3.reconstruct(early).await {
            Ok(s) => format!("ok, {} concepts", s.concepts.len()),
            Err(e) => format!("{e}"),
        }
    );
    db3.close().await.unwrap();
}
