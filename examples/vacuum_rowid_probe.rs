//! Does `VACUUM` renumber an implicit sparse rowid on this engine? (B4, D-119)
//!
//! D-071 recorded the hazard `rowid_pk` exists to close: `concepts_fts` is
//! external-content keyed on `concepts`'s rowid, that rowid was implicit through
//! v7, and SQLite's documentation says `VACUUM` **may** change the rowids of a
//! table with no explicit `INTEGER PRIMARY KEY`.
//!
//! "May" is not "does", and the control arm of
//! `vacuum_preserves_a_sparse_rowid_pk` found the difference: on this build the
//! sparse implicit rowids came back unchanged. That makes the v7 test's premise
//! unmeasured, so this probe measures it rather than arguing from the manual.
//!
//! Run: `cargo run --example vacuum_rowid_probe`

use libsql::Builder;

async fn rowids(conn: &libsql::Connection, table: &str, col: &str) -> Vec<i64> {
    let mut rows = conn
        .query(&format!("SELECT {col} FROM {table} ORDER BY {col}"), ())
        .await
        .unwrap();
    let mut v = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        v.push(r.get(0).unwrap());
    }
    v
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("vacuum_rowid_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Four shapes, so the answer is not confounded by column count, by whether
    // a free page exists to reclaim, or by the file being a `:memory:` one.
    let cases: &[(&str, &str, &str)] = &[
        (
            "one column, implicit rowid",
            "one_col",
            "id TEXT PRIMARY KEY",
        ),
        (
            "several columns, implicit rowid",
            "multi_col",
            "id TEXT PRIMARY KEY, a TEXT, b INTEGER",
        ),
        ("no primary key at all", "no_pk", "id TEXT NOT NULL, a TEXT"),
        // These two separate "has a PRIMARY KEY" from "has any index at all",
        // which the first three cases confound.
        (
            "UNIQUE but no primary key",
            "uniq_no_pk",
            "id TEXT UNIQUE, a TEXT",
        ),
        (
            "no constraints, one secondary index",
            "sec_index",
            "id TEXT NOT NULL, a TEXT",
        ),
        (
            "explicit INTEGER PRIMARY KEY (the v8 shape)",
            "explicit_pk",
            "rowid_pk INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE",
        ),
    ];

    for (label, table, cols) in cases {
        let path = dir.join(format!("{table}.db"));
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(&format!("CREATE TABLE {table} ({cols})"), ())
            .await
            .unwrap();
        if *table == "sec_index" {
            conn.execute(&format!("CREATE INDEX ix_{table} ON {table} (a)"), ())
                .await
                .unwrap();
        }

        // Sparse from the start, and sparse by deletion — a gap that was never
        // occupied and a gap that was freed are not obviously the same case to
        // a page allocator.
        for pk in [1i64, 2, 3, 4, 5, 9, 100, 1_000_000] {
            let sql = if *table == "explicit_pk" {
                format!("INSERT INTO {table} (rowid_pk, id) VALUES (?1, ?2)")
            } else {
                format!("INSERT INTO {table} (rowid, id) VALUES (?1, ?2)")
            };
            conn.execute(&sql, libsql::params![pk, format!("c{pk:07}")])
                .await
                .unwrap();
        }
        conn.execute(&format!("DELETE FROM {table} WHERE rowid IN (2, 3)"), ())
            .await
            .unwrap();

        let col = if *table == "explicit_pk" {
            "rowid_pk"
        } else {
            "rowid"
        };
        let before = rowids(&conn, table, col).await;
        let pages_before = scalar(&conn, "PRAGMA page_count").await;
        let free_before = scalar(&conn, "PRAGMA freelist_count").await;

        conn.execute("VACUUM", ()).await.unwrap();

        let after = rowids(&conn, table, col).await;
        let pages_after = scalar(&conn, "PRAGMA page_count").await;

        println!("{label}");
        println!("    before: {before:?}  ({pages_before} pages, {free_before} free)");
        println!("    after:  {after:?}  ({pages_after} pages)");
        println!(
            "    -> VACUUM {}\n",
            if before == after {
                "PRESERVED the numbering"
            } else {
                "RENUMBERED"
            }
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
