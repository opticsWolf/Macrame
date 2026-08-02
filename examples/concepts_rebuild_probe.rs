//! Can `concepts` be rebuilt inside a migration rung at all? (v0.8.0, D-084)
//!
//! The v6 → v7 rung rebuilds `links`, and `links` has **no inbound foreign
//! keys**. `concepts` does: `links.source_id` and `links.target_id` both
//! `REFERENCES concepts(id)`. Three things make that a different problem, and
//! all three have to be true at once for the rung to be writable as specified:
//!
//! 1. `PRAGMA foreign_keys = ON` is set on every connection this crate opens
//!    (`connection.rs:1701`), and it is a **no-op inside a transaction** — so a
//!    rung cannot turn it off, because `apply_step` wraps every rung in
//!    `BEGIN IMMEDIATE`.
//! 2. With FKs on, `DROP TABLE` performs an implicit `DELETE FROM` first. If
//!    that fires `trg_concepts_guard_delete`, the rung aborts on this crate's
//!    own delete guard.
//! 3. Even if no trigger fires, the implicit delete leaves every `links` row
//!    referencing nothing, which is an FK violation unless the constraint is
//!    deferred or unenforced during a schema change.
//!
//! Nothing here is exotic, but the answers differ between SQLite versions and
//! this project's standard is that a number is not a number until it is
//! measured on **libSQL 0.9.30**. Run before designing the rung, not after.
//!
//! ```text
//! cargo run --example concepts_rebuild_probe
//! ```

use libsql::Builder;

const CONCEPTS: &str = "CREATE TABLE concepts (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT ''
)";

const CONCEPTS_V8: &str = "CREATE TABLE concepts_v8 (
    rowid_pk INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT ''
)";

const LINKS: &str = "CREATE TABLE links (
    source_id TEXT NOT NULL REFERENCES concepts(id),
    target_id TEXT NOT NULL REFERENCES concepts(id),
    edge_type TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id, edge_type)
)";

const GUARD: &str = "CREATE TRIGGER trg_concepts_guard_delete
    BEFORE DELETE ON concepts
    BEGIN
        SELECT RAISE(ABORT, 'macrame: concepts are never physically archived (D-022)');
    END";

async fn seed(conn: &libsql::Connection, with_guard: bool) {
    conn.execute(CONCEPTS, ()).await.unwrap();
    conn.execute(LINKS, ()).await.unwrap();
    if with_guard {
        conn.execute(GUARD, ()).await.unwrap();
    }
    conn.execute("INSERT INTO concepts (id, title) VALUES ('a', 'A'), ('b', 'B')", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type) VALUES ('a', 'b', 'CITES')",
        (),
    )
    .await
    .unwrap();
}

async fn fresh(with_guard: bool) -> (tempfile::TempDir, libsql::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let db = Builder::new_local(dir.path().join("probe.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    seed(&conn, with_guard).await;
    (dir, conn)
}

async fn fk_state(conn: &libsql::Connection, label: &str) {
    let on: i64 = conn
        .query("PRAGMA foreign_keys", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .and_then(|r| r.get(0).ok())
        .unwrap_or(-1);
    println!("    {label}: PRAGMA foreign_keys = {on}");
}

#[tokio::main]
async fn main() {
    println!("== 1. does `PRAGMA foreign_keys = OFF` take effect inside a transaction? ==");
    {
        let (_d, conn) = fresh(true).await;
        fk_state(&conn, "before").await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        let r = tx.execute("PRAGMA foreign_keys = OFF", ()).await;
        println!("    inside tx, execute -> {r:?}");
        let on: i64 = tx
            .query("PRAGMA foreign_keys", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get(0).ok())
            .unwrap_or(-1);
        println!("    inside tx, reads back = {on}   (0 = the rung could do this)");
        tx.rollback().await.unwrap();
    }

    println!("\n== 2. DROP TABLE concepts inside a tx, guard present, FKs on ==");
    {
        let (_d, conn) = fresh(true).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        match tx.execute("DROP TABLE concepts", ()).await {
            Ok(_) => println!("    DROP TABLE succeeded — no implicit-delete trigger fire"),
            Err(e) => println!("    DROP TABLE failed: {e}"),
        }
        let _ = tx.rollback().await;
    }

    println!("\n== 3. the same, with the delete guard absent ==");
    {
        let (_d, conn) = fresh(false).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        match tx.execute("DROP TABLE concepts", ()).await {
            Ok(_) => println!("    DROP TABLE succeeded"),
            Err(e) => println!("    DROP TABLE failed: {e}"),
        }
        let _ = tx.rollback().await;
    }

    println!("\n== 4. the whole rung, as it would actually be written ==");
    {
        let (_d, conn) = fresh(true).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();

        let steps: &[(&str, &str)] = &[
            ("create concepts_v8", CONCEPTS_V8),
            (
                "copy rows",
                "INSERT INTO concepts_v8 (id, title, content) \
                 SELECT id, title, content FROM concepts ORDER BY rowid",
            ),
            ("drop guard first", "DROP TRIGGER trg_concepts_guard_delete"),
            ("drop concepts", "DROP TABLE concepts"),
            ("rename", "ALTER TABLE concepts_v8 RENAME TO concepts"),
            ("recreate guard", GUARD),
        ];

        let mut ok = true;
        for (label, sql) in steps {
            match tx.execute(sql, ()).await {
                Ok(_) => println!("    ok   {label}"),
                Err(e) => {
                    println!("    FAIL {label}: {e}");
                    ok = false;
                    break;
                }
            }
        }

        if ok {
            // Did the rename fix up links' FK clause, and do the rows survive?
            let sql: Option<String> = tx
                .query(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='links'",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .and_then(|r| r.get(0).ok());
            println!("    links DDL after rename:\n      {}",
                sql.unwrap_or_default().replace('\n', "\n      "));

            let n: i64 = tx
                .query("SELECT COUNT(*) FROM links", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .and_then(|r| r.get(0).ok())
                .unwrap_or(-1);
            println!("    links rows surviving: {n}");

            let mut rows = tx.query("PRAGMA foreign_key_check", ()).await.unwrap();
            let mut violations = 0;
            while rows.next().await.unwrap().is_some() {
                violations += 1;
            }
            println!("    foreign_key_check violations: {violations}");

            match tx.commit().await {
                Ok(()) => println!("    COMMIT ok — the rung is writable as specified"),
                Err(e) => println!("    COMMIT failed: {e}"),
            }
        } else {
            let _ = tx.rollback().await;
        }
    }

    println!("\n== 4b. the same rung, with `PRAGMA defer_foreign_keys = ON` ==");
    println!("    (per-transaction, auto-cleared at COMMIT — designed for this)");
    {
        let (_d, conn) = fresh(true).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();

        let steps: &[(&str, &str)] = &[
            ("defer FKs", "PRAGMA defer_foreign_keys = ON"),
            ("create concepts_v8", CONCEPTS_V8),
            (
                "copy rows",
                "INSERT INTO concepts_v8 (id, title, content) \
                 SELECT id, title, content FROM concepts ORDER BY rowid",
            ),
            ("drop guard first", "DROP TRIGGER trg_concepts_guard_delete"),
            ("drop concepts", "DROP TABLE concepts"),
            ("rename", "ALTER TABLE concepts_v8 RENAME TO concepts"),
            ("recreate guard", GUARD),
        ];

        let mut ok = true;
        for (label, sql) in steps {
            match tx.execute(sql, ()).await {
                Ok(_) => println!("    ok   {label}"),
                Err(e) => {
                    println!("    FAIL {label}: {e}");
                    ok = false;
                    break;
                }
            }
        }

        if ok {
            let sql: Option<String> = tx
                .query(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='links'",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .and_then(|r| r.get(0).ok());
            println!(
                "    links DDL after rename:\n      {}",
                sql.unwrap_or_default().replace('\n', "\n      ")
            );

            let mut rows = tx.query("PRAGMA foreign_key_check", ()).await.unwrap();
            let mut violations = 0;
            while rows.next().await.unwrap().is_some() {
                violations += 1;
            }
            println!("    foreign_key_check violations before commit: {violations}");

            match tx.commit().await {
                Ok(()) => {
                    println!("    COMMIT ok");
                    let n: i64 = conn
                        .query("SELECT COUNT(*) FROM links", ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .and_then(|r| r.get(0).ok())
                        .unwrap_or(-1);
                    let c: i64 = conn
                        .query("SELECT COUNT(*) FROM concepts", ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .and_then(|r| r.get(0).ok())
                        .unwrap_or(-1);
                    let pk: i64 = conn
                        .query("SELECT rowid_pk FROM concepts WHERE id='b'", ())
                        .await
                        .unwrap()
                        .next()
                        .await
                        .unwrap()
                        .and_then(|r| r.get(0).ok())
                        .unwrap_or(-1);
                    println!("    after commit: {c} concepts, {n} links, b.rowid_pk = {pk}");

                    // The guard must still refuse an ad-hoc delete.
                    let guarded = conn.execute("DELETE FROM concepts WHERE id='a'", ()).await;
                    println!("    ad-hoc DELETE after rung -> {}",
                        match guarded { Ok(_) => "ACCEPTED (guard lost!)".into(),
                                        Err(e) => format!("refused: {e}") });
                }
                Err(e) => println!("    COMMIT failed: {e}"),
            }
        } else {
            let _ = tx.rollback().await;
        }
    }

    println!("\n== 4c. is `PRAGMA legacy_alter_table` settable inside a tx? (fallback) ==");
    {
        let (_d, conn) = fresh(true).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        let _ = tx.execute("PRAGMA legacy_alter_table = ON", ()).await;
        let on: i64 = tx
            .query("PRAGMA legacy_alter_table", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get(0).ok())
            .unwrap_or(-1);
        println!("    reads back = {on}   (1 = the rename-around fallback is available)");
        let _ = tx.rollback().await;
    }

    println!("\n== 4d. rename-around: move the old table out of the way, never drop a parent ==");
    for legacy in [true, false] {
        println!("    --- legacy_alter_table = {} ---", if legacy { "ON" } else { "OFF (default)" });
        let (_d, conn) = fresh(true).await;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();

        let mut steps: Vec<(&str, String)> = Vec::new();
        if legacy {
            steps.push(("legacy_alter_table", "PRAGMA legacy_alter_table = ON".into()));
        }
        steps.extend([
            ("create concepts_v8", CONCEPTS_V8.to_string()),
            (
                "copy rows",
                "INSERT INTO concepts_v8 (id, title, content) \
                 SELECT id, title, content FROM concepts ORDER BY rowid"
                    .into(),
            ),
            ("drop guard", "DROP TRIGGER trg_concepts_guard_delete".into()),
            ("rename old aside", "ALTER TABLE concepts RENAME TO concepts_old".into()),
            ("rename new in", "ALTER TABLE concepts_v8 RENAME TO concepts".into()),
            ("drop the orphan", "DROP TABLE concepts_old".into()),
            ("recreate guard", GUARD.to_string()),
        ]);

        let mut ok = true;
        for (label, sql) in &steps {
            match tx.execute(sql.as_str(), ()).await {
                Ok(_) => println!("        ok   {label}"),
                Err(e) => {
                    println!("        FAIL {label}: {e}");
                    ok = false;
                    break;
                }
            }
        }

        if !ok {
            let _ = tx.rollback().await;
            continue;
        }

        let links_ddl: Option<String> = tx
            .query("SELECT sql FROM sqlite_master WHERE type='table' AND name='links'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get(0).ok());
        let refs_concepts = links_ddl
            .as_deref()
            .map(|s| s.contains("REFERENCES concepts(id)"))
            .unwrap_or(false);
        println!("        links still REFERENCES concepts(id): {refs_concepts}");

        match tx.commit().await {
            Ok(()) => {
                let c: i64 = conn.query("SELECT COUNT(*) FROM concepts", ()).await.unwrap()
                    .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
                let n: i64 = conn.query("SELECT COUNT(*) FROM links", ()).await.unwrap()
                    .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
                let pk: i64 = conn.query("SELECT rowid_pk FROM concepts WHERE id='b'", ()).await.unwrap()
                    .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
                println!("        COMMIT ok — {c} concepts, {n} links, b.rowid_pk = {pk}");

                let mut rows = conn.query("PRAGMA foreign_key_check", ()).await.unwrap();
                let mut v = 0;
                while rows.next().await.unwrap().is_some() { v += 1; }
                println!("        foreign_key_check after commit: {v} violations");

                // The FK must still bite on a genuinely bad insert.
                let bad = conn.execute(
                    "INSERT INTO links (source_id, target_id, edge_type) VALUES ('a','ghost','X')", ()).await;
                println!("        insert referencing a missing concept -> {}",
                    match bad { Ok(_) => "ACCEPTED (FK lost!)".to_string(),
                                Err(e) => format!("refused: {e}") });

                let guarded = conn.execute("DELETE FROM concepts WHERE id='a'", ()).await;
                println!("        ad-hoc DELETE -> {}",
                    match guarded { Ok(_) => "ACCEPTED (guard lost!)".to_string(),
                                    Err(e) => format!("refused: {e}") });
            }
            Err(e) => println!("        COMMIT failed: {e}"),
        }
    }

    println!("\n== 4e. the procedure SQLite actually prescribes: pragma OUTSIDE the tx ==");
    {
        let (_d, conn) = fresh(true).await;

        // Outside any transaction — this is the part `apply_step` cannot do today.
        conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
        fk_state(&conn, "after OFF, outside tx").await;

        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();

        let steps: &[(&str, &str)] = &[
            ("create concepts_v8", CONCEPTS_V8),
            (
                "copy rows",
                "INSERT INTO concepts_v8 (id, title, content) \
                 SELECT id, title, content FROM concepts ORDER BY rowid",
            ),
            ("drop guard", "DROP TRIGGER trg_concepts_guard_delete"),
            ("drop concepts", "DROP TABLE concepts"),
            ("rename", "ALTER TABLE concepts_v8 RENAME TO concepts"),
            ("recreate guard", GUARD),
        ];

        let mut ok = true;
        for (label, sql) in steps {
            match tx.execute(sql, ()).await {
                Ok(_) => println!("        ok   {label}"),
                Err(e) => {
                    println!("        FAIL {label}: {e}");
                    ok = false;
                    break;
                }
            }
        }

        if ok {
            let mut rows = tx.query("PRAGMA foreign_key_check", ()).await.unwrap();
            let mut v = 0;
            while rows.next().await.unwrap().is_some() {
                v += 1;
            }
            println!("        foreign_key_check inside tx: {v} violations");
            match tx.commit().await {
                Ok(()) => println!("        COMMIT ok"),
                Err(e) => println!("        COMMIT failed: {e}"),
            }
        } else {
            let _ = tx.rollback().await;
        }

        conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
        fk_state(&conn, "after ON, outside tx").await;

        let c: i64 = conn.query("SELECT COUNT(*) FROM concepts", ()).await.unwrap()
            .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
        let n: i64 = conn.query("SELECT COUNT(*) FROM links", ()).await.unwrap()
            .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
        let pk: i64 = conn.query("SELECT rowid_pk FROM concepts WHERE id='b'", ()).await.unwrap()
            .next().await.unwrap().and_then(|r| r.get(0).ok()).unwrap_or(-1);
        println!("        result: {c} concepts, {n} links, b.rowid_pk = {pk}");

        let mut rows = conn.query("PRAGMA foreign_key_check", ()).await.unwrap();
        let mut v = 0;
        while rows.next().await.unwrap().is_some() { v += 1; }
        println!("        foreign_key_check after: {v} violations");

        let bad = conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type) VALUES ('a','ghost','X')", ()).await;
        println!("        insert referencing a missing concept -> {}",
            match bad { Ok(_) => "ACCEPTED (FK lost!)".to_string(), Err(e) => format!("refused: {e}") });

        let guarded = conn.execute("DELETE FROM concepts WHERE id='a'", ()).await;
        println!("        ad-hoc DELETE -> {}",
            match guarded { Ok(_) => "ACCEPTED (guard lost!)".to_string(), Err(e) => format!("refused: {e}") });
    }

    println!("\n== 5. does VACUUM preserve an explicit INTEGER PRIMARY KEY? ==");
    {
        let dir = tempfile::tempdir().unwrap();
        let db = Builder::new_local(dir.path().join("vac.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(CONCEPTS_V8, ()).await.unwrap();
        for i in 0..6 {
            conn.execute(
                "INSERT INTO concepts_v8 (id, title) VALUES (?1, ?2)",
                libsql::params![format!("c{i}"), format!("T{i}")],
            )
            .await
            .unwrap();
        }
        // Make the numbering sparse, which is what archival would do.
        conn.execute("DELETE FROM concepts_v8 WHERE id IN ('c1','c3')", ())
            .await
            .unwrap();

        let read = |conn: libsql::Connection| async move {
            let mut rows = conn
                .query("SELECT rowid_pk, id FROM concepts_v8 ORDER BY rowid_pk", ())
                .await
                .unwrap();
            let mut out = Vec::new();
            while let Some(r) = rows.next().await.unwrap() {
                out.push(format!(
                    "{}:{}",
                    r.get::<i64>(0).unwrap(),
                    r.get::<String>(1).unwrap()
                ));
            }
            out.join(" ")
        };

        let before = read(conn.clone()).await;
        conn.execute("VACUUM", ()).await.unwrap();
        let after = read(conn.clone()).await;
        println!("    before VACUUM: {before}");
        println!("    after  VACUUM: {after}");
        println!(
            "    preserved: {}",
            if before == after { "YES" } else { "NO" }
        );
    }
}
