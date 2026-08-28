//! W12.2, step 0: what did the **v11** schema refuse when a second lineage
//! asserted into it?
//!
//! Kept runnable, and kept honest about its tense. The answers below were
//! folded into `ddl` at v12, so the live constants are no longer the thing this
//! probe is asking about — it builds the pre-v12 shapes from
//! `tests/common/v11_schema.rs` instead. A probe re-pointed at today's schema
//! would report that `branch_id` adds cleanly because `branch_id` is already
//! there, which is not a measurement.
//!
//! §15.2 says the storage model is *shared ledger, logical versions*, and that
//! ledger tables gain `branch_id TEXT NOT NULL DEFAULT 'main'` — *"the default
//! is what makes the migration a rung and not a rewrite."* That sentence is
//! true of `ALTER TABLE` and says nothing about the constraints already on the
//! tables, three of which decide the shape of the whole wave:
//!
//! 1. **`concepts.id` is `NOT NULL UNIQUE`** and the table is written with
//!    `ON CONFLICT(id) DO UPDATE` — it is a *current-state projection keyed by
//!    identity*, not an append-only ledger. Two lineages holding different
//!    beliefs about one concept is two rows with one `id`.
//! 2. **`links.source_id` and `links.target_id` are declared foreign keys into
//!    `concepts(id)`.** SQLite requires the parent column to carry a unique
//!    index *on exactly those columns*, so widening the uniqueness to
//!    `(id, branch_id)` is not a free choice.
//! 3. **Copy-on-write is the whole design.** A branch that has asserted nothing
//!    costs one row and reads its parent's, so a link asserted on branch `b`
//!    routinely names a concept row carrying `branch_id = 'main'`.
//!
//! (2) and (3) point in opposite directions and that is the finding: a
//! composite foreign key expresses (2) and forbids (3).
//!
//! Probed rather than reasoned about, on libSQL rather than on SQLite, for the
//! reason [D-078] made expensive: a probe on a different engine is a probe on a
//! different engine.
//!
//! Run with:  cargo run --release --example branch_identity_probe
//!
//! [D-078]: ../docs/architecture/s13-decision-register.md

use std::time::Instant;

#[path = "../tests/common/v11_schema.rs"]
mod v11_schema;

use macrame::schema::ddl;

/// Run `sql` and report whether the engine took it, with the reason if not.
async fn attempt(conn: &libsql::Connection, label: &str, sql: &str) -> bool {
    match conn.execute(sql, ()).await {
        Ok(_) => {
            println!("  {label:<44} ACCEPTED");
            true
        }
        Err(e) => {
            let msg = e.to_string();
            let short = msg
                .split(':')
                .next_back()
                .unwrap_or(&msg)
                .trim()
                .to_string();
            println!("  {label:<44} refused ({short})");
            false
        }
    }
}

async fn fresh() -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    conn
}

const TS: &str = "'2026-01-01T00:00:00.000000Z'";
const FOREVER: &str = "'9999-12-31T23:59:59.999999Z'";

async fn cols(conn: &libsql::Connection, schema: &str, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA {schema}.table_info({table})"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(1).unwrap());
    }
    out
}

#[tokio::main]
async fn main() {
    // ---- 1. ADD COLUMN on the real tables ----
    //
    // The uncontested half. SQLite records a new column with a constant
    // default in the schema header and touches no row, so the cost should not
    // depend on the row count. Measured on 20,000 concepts because a rung that
    // is O(rows) is a rung that changes what an upgrade costs.
    println!("\n1. ALTER TABLE ... ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'");
    let conn = fresh().await;
    conn.execute(&v11_schema::concepts_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_current_v11(), ())
        .await
        .unwrap();
    conn.execute(&v11_schema::transaction_log_v11(), ())
        .await
        .unwrap();

    conn.execute("BEGIN", ()).await.unwrap();
    for i in 0..20_000 {
        conn.execute(
            &format!(
                "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at) \
                 VALUES ('c{i}', 't', {TS}, {FOREVER}, {TS})"
            ),
            (),
        )
        .await
        .unwrap();
    }
    conn.execute("COMMIT", ()).await.unwrap();
    println!("  20,000 concepts loaded");

    for table in ["concepts", "links", "links_current", "transaction_log"] {
        let t = Instant::now();
        let ok = attempt(
            &conn,
            &format!("ADD COLUMN on {table}"),
            &format!("ALTER TABLE {table} ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'"),
        )
        .await;
        if ok {
            println!("  {:>50}", format!("{:?}", t.elapsed()));
        }
    }

    // ---- 2. does UNIQUE(id) refuse a second lineage's belief? ----
    //
    // The question the whole wave turns on. If this is ACCEPTED, `branch_id`
    // on `concepts` is enough and §15.2 is complete as written.
    println!("\n2. two lineages, one concept id (UNIQUE(id) still in force):");
    attempt(
        &conn,
        "same id, branch 'b'",
        &format!(
            "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, branch_id) \
             VALUES ('c0', 'branch b believes otherwise', {TS}, {FOREVER}, {TS}, 'b')"
        ),
    )
    .await;

    // ---- 3. can the uniqueness widen while the foreign keys stand? ----
    //
    // A table built the way v13 would have to build it: UNIQUE(id, branch_id)
    // instead of UNIQUE(id), with `links` declaring the same single-column
    // foreign key it declares today.
    println!("\n3. UNIQUE(id, branch_id) on the parent, single-column FK on the child:");
    let conn = fresh().await;
    conn.execute(
        "CREATE TABLE concepts (
             rowid_pk INTEGER PRIMARY KEY,
             id TEXT NOT NULL,
             branch_id TEXT NOT NULL DEFAULT 'main',
             UNIQUE (id, branch_id)
         )",
        (),
    )
    .await
    .unwrap();
    attempt(
        &conn,
        "CREATE TABLE links ... REFERENCES concepts(id)",
        "CREATE TABLE links (
             source_id TEXT NOT NULL REFERENCES concepts(id),
             branch_id TEXT NOT NULL DEFAULT 'main'
         )",
    )
    .await;
    conn.execute(
        "INSERT INTO concepts (id, branch_id) VALUES ('c0', 'main')",
        (),
    )
    .await
    .unwrap();
    // The table is created either way — SQLite resolves a foreign key when a
    // row is written, not when the table is declared. This is the statement
    // that actually asks the engine.
    attempt(
        &conn,
        "INSERT a link naming that concept",
        "INSERT INTO links (source_id) VALUES ('c0')",
    )
    .await;

    // ---- 4. does a composite foreign key permit copy-on-write? ----
    //
    // The composite key is the only shape that expresses the widened
    // uniqueness. §15.2's inheritance is a link on branch 'b' naming a concept
    // row that exists only on 'main'.
    println!("\n4. composite FK (source_id, branch_id) -> concepts(id, branch_id):");
    let conn = fresh().await;
    conn.execute(
        "CREATE TABLE concepts (
             rowid_pk INTEGER PRIMARY KEY,
             id TEXT NOT NULL,
             branch_id TEXT NOT NULL DEFAULT 'main',
             UNIQUE (id, branch_id)
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE links (
             source_id TEXT NOT NULL,
             branch_id TEXT NOT NULL DEFAULT 'main',
             FOREIGN KEY (source_id, branch_id) REFERENCES concepts(id, branch_id)
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO concepts (id, branch_id) VALUES ('c0', 'main')",
        (),
    )
    .await
    .unwrap();
    attempt(
        &conn,
        "link on 'main' -> concept on 'main'",
        "INSERT INTO links (source_id, branch_id) VALUES ('c0', 'main')",
    )
    .await;
    attempt(
        &conn,
        "link on 'b' -> inherited concept on 'main'",
        "INSERT INTO links (source_id, branch_id) VALUES ('c0', 'b')",
    )
    .await;

    // ---- 5. the two primary keys, asked the same question ----
    //
    // `links` carries `recorded_at` in its primary key and `links_current` does
    // not, which is the difference between a ledger and a projection. Whether
    // that difference is enough to keep two lineages apart is not a matter of
    // opinion.
    println!("\n5. the same edge asserted on two lineages:");
    let conn = fresh().await;
    conn.execute(&v11_schema::concepts_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_current_v11(), ())
        .await
        .unwrap();
    for t in ["links", "links_current"] {
        conn.execute(
            &format!("ALTER TABLE {t} ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'"),
            (),
        )
        .await
        .unwrap();
    }
    conn.execute(
        &format!(
            "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at) \
             VALUES ('a', 't', {TS}, {FOREVER}, {TS}), ('b', 't', {TS}, {FOREVER}, {TS})"
        ),
        (),
    )
    .await
    .unwrap();

    let link = |branch: &str, recorded: &str| {
        format!(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             recorded_at, branch_id) VALUES ('a', 'b', 'rel', {TS}, {FOREVER}, '{recorded}', \
             '{branch}')"
        )
    };
    let current = |branch: &str| {
        format!(
            "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at, branch_id) VALUES ('a', 'b', 'rel', {TS}, \
             {FOREVER}, 1.0, '{{}}', {TS}, '{branch}')"
        )
    };

    attempt(
        &conn,
        "links: 'main' at T1",
        &link("main", "2026-01-01T00:00:00.000000Z"),
    )
    .await;
    attempt(
        &conn,
        "links: 'b' at T2 (distinct recorded_at)",
        &link("b", "2026-01-01T00:00:01.000000Z"),
    )
    .await;
    attempt(
        &conn,
        "links: 'b' at T1 (same recorded_at)",
        &link("b", "2026-01-01T00:00:00.000000Z"),
    )
    .await;
    attempt(&conn, "links_current: 'main'", &current("main")).await;
    attempt(&conn, "links_current: 'b'", &current("b")).await;

    // ---- 6-9. the guard mechanics the resolution depends on ----
    //
    // Sections 1-5 establish what the schema refuses. These four establish that
    // the answer is buildable: that the column can be added to a table carrying
    // FTS sync triggers, that a `BEFORE INSERT` guard sees an upsert *before*
    // `ON CONFLICT` resolves it -- the only path a cross-lineage concept write
    // can arrive by -- that `branch_id` can be made immutable, and that a
    // lineage is expressible as a subquery rather than as a bound list of
    // unknown length.
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    conn.execute(&v11_schema::concepts_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_current_v11(), ())
        .await
        .unwrap();
    conn.execute(&v11_schema::transaction_log_v11(), ())
        .await
        .unwrap();
    conn.execute(ddl::CREATE_CONCEPTS_FTS, ()).await.unwrap();
    for t in v11_schema::triggers_v11() {
        conn.execute(t, ()).await.unwrap();
    }

    println!("\n6. ADD COLUMN on a table carrying FTS sync triggers + a delete guard:");
    attempt(
        &conn,
        "ALTER TABLE concepts ADD COLUMN branch_id",
        "ALTER TABLE concepts ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'",
    )
    .await;
    attempt(
        &conn,
        "INSERT a concept (FTS trigger must still fire)",
        &format!(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, recorded_at) \
             VALUES ('c0', 'alpha', 'beta gamma', {TS}, {FOREVER}, {TS})"
        ),
    )
    .await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'gamma'",
            (),
        )
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    println!("  {:<48} {} row(s) match", "FTS index after ADD COLUMN", n);

    println!("\n7. does BEFORE INSERT fire ahead of ON CONFLICT ... DO UPDATE?");
    conn.execute(
        "CREATE TRIGGER trg_probe_cross_lineage
         BEFORE INSERT ON concepts
         WHEN EXISTS (SELECT 1 FROM concepts WHERE id = NEW.id AND branch_id <> NEW.branch_id)
         BEGIN SELECT RAISE(ABORT, 'macrame: cross-lineage concept write'); END;",
        (),
    )
    .await
    .unwrap();

    let upsert = |branch: &str| {
        format!(
            "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, branch_id) \
             VALUES ('c0', 'rewritten', {TS}, {FOREVER}, '2026-01-02T00:00:00.000000Z', \
             '{branch}') \
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, \
             recorded_at = excluded.recorded_at"
        )
    };
    attempt(&conn, "upsert on 'main' (same lineage)", &upsert("main")).await;
    attempt(
        &conn,
        "upsert from branch 'b' (cross lineage)",
        &upsert("b"),
    )
    .await;
    attempt(
        &conn,
        "new id from branch 'b'",
        &format!(
            "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, branch_id) \
             VALUES ('c1', 'mine', {TS}, {FOREVER}, {TS}, 'b')"
        ),
    )
    .await;

    println!("\n8. branch_id immutability (BEFORE UPDATE):");
    conn.execute(
        "CREATE TRIGGER trg_probe_branch_immutable
         BEFORE UPDATE ON concepts
         WHEN NEW.branch_id <> OLD.branch_id
         BEGIN SELECT RAISE(ABORT, 'macrame: branch_id is immutable'); END;",
        (),
    )
    .await
    .unwrap();
    attempt(
        &conn,
        "UPDATE concepts SET branch_id = 'b'",
        "UPDATE concepts SET branch_id = 'b', recorded_at = '2026-01-03T00:00:00.000000Z' \
         WHERE id = 'c0'",
    )
    .await;

    println!("\n9. the recursive ancestry CTE, as a predicate subquery:");
    conn.execute(
        "CREATE TABLE branches (
             branch_id TEXT PRIMARY KEY,
             parent_id TEXT REFERENCES branches(branch_id),
             forked_at TEXT
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO branches VALUES ('main', NULL, NULL), ('b', 'main', '2026-01-01T00:00:00.000000Z'), \
         ('c', 'b', '2026-01-02T00:00:00.000000Z'), ('sib', 'main', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();
    let lineage = "SELECT COUNT(*) FROM concepts AS c WHERE c.branch_id IN (
             WITH RECURSIVE ancestry(id) AS (
                 SELECT ?1
                 UNION ALL
                 SELECT b.parent_id FROM branches b JOIN ancestry a ON b.branch_id = a.id
                 WHERE b.parent_id IS NOT NULL
             ) SELECT id FROM ancestry)";
    for reader in ["main", "b", "c", "sib"] {
        let mut rows = conn.query(lineage, libsql::params![reader]).await.unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        println!("  reader {reader:<6} sees {n} concept(s)");
    }
    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {lineage}"),
            libsql::params!["c"],
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let d: String = row.get(3).unwrap();
        println!("    plan: {d}");
    }

    // ---- 10-14. the cold file, which is a second schema this rung must move ----
    //
    // D-026 folds `main.transaction_log` and `cold.transaction_log` through one
    // identical window query, and the archive writer creates the cold tables
    // with `CREATE TABLE IF NOT EXISTS`. Neither of those upgrades a cold file
    // written under v11, so the questions are: does the setup step silently
    // leave the old shape, how does the write fail, and can the archive session
    // upgrade the file in place inside the transaction it already holds --
    // including whether a rollback takes the DDL with it.
    {
        let dir = std::env::temp_dir().join("macrame_cold_probe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cold_path = dir.join("cold.db");

        // A v11-shaped cold file: no branch_id anywhere.
        {
            let db = libsql::Builder::new_local(&cold_path)
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
            "CREATE TABLE transaction_log (seq_id INTEGER PRIMARY KEY, entity_id TEXT NOT NULL, \
             payload TEXT NOT NULL, recorded_at TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();
            conn.execute(
                "INSERT INTO transaction_log VALUES (1, 'c0', '{}', '2026-01-01T00:00:00.000000Z')",
                (),
            )
            .await
            .unwrap();
        }

        let db = libsql::Builder::new_local(dir.join("hot.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            &format!(
                "ATTACH DATABASE '{}' AS cold",
                cold_path.display().to_string().replace('\\', "/")
            ),
            (),
        )
        .await
        .unwrap();

        println!("\n10. does CREATE TABLE IF NOT EXISTS upgrade the old shape?");
        attempt(
            &conn,
            "CREATE TABLE IF NOT EXISTS cold.transaction_log (+branch_id)",
            "CREATE TABLE IF NOT EXISTS cold.transaction_log (seq_id INTEGER PRIMARY KEY, \
         entity_id TEXT NOT NULL, payload TEXT NOT NULL, recorded_at TEXT NOT NULL, \
         branch_id TEXT NOT NULL DEFAULT 'main')",
        )
        .await;
        println!(
            "  columns now: {:?}",
            cols(&conn, "cold", "transaction_log").await
        );

        println!("\n11. writing the new column list into the old shape:");
        attempt(
            &conn,
            "INSERT ... (seq_id, entity_id, payload, recorded_at, branch_id)",
            "INSERT OR IGNORE INTO cold.transaction_log \
         (seq_id, entity_id, payload, recorded_at, branch_id) \
         VALUES (2, 'c1', '{}', '2026-01-02T00:00:00.000000Z', 'b')",
        )
        .await;

        println!("\n12. ALTER on the attached file, inside a transaction:");
        conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        let altered = attempt(
            &conn,
            "ALTER TABLE cold.transaction_log ADD COLUMN branch_id",
            "ALTER TABLE cold.transaction_log ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'",
        )
        .await;
        if altered {
            attempt(
                &conn,
                "INSERT a branch row in the same transaction",
                "INSERT OR IGNORE INTO cold.transaction_log \
             (seq_id, entity_id, payload, recorded_at, branch_id) \
             VALUES (2, 'c1', '{}', '2026-01-02T00:00:00.000000Z', 'b')",
            )
            .await;
        }
        println!(
            "  columns inside txn: {:?}",
            cols(&conn, "cold", "transaction_log").await
        );

        println!("\n13. does ROLLBACK undo DDL on the attached file?");
        conn.execute("ROLLBACK", ()).await.unwrap();
        println!(
            "  columns after rollback: {:?}",
            cols(&conn, "cold", "transaction_log").await
        );
        let mut rows = conn
            .query("SELECT COUNT(*) FROM cold.transaction_log", ())
            .await
            .unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        println!("  rows after rollback: {n}");

        println!("\n14. commit path, and the fold across two shapes:");
        conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        conn.execute(
            "ALTER TABLE cold.transaction_log ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO cold.transaction_log \
         (seq_id, entity_id, payload, recorded_at, branch_id) \
         VALUES (2, 'c1', '{}', '2026-01-02T00:00:00.000000Z', 'b')",
            (),
        )
        .await
        .unwrap();
        conn.execute("COMMIT", ()).await.unwrap();
        println!(
            "  columns after commit: {:?}",
            cols(&conn, "cold", "transaction_log").await
        );

        conn.execute(
            "CREATE TABLE transaction_log (seq_id INTEGER PRIMARY KEY, entity_id TEXT NOT NULL, \
         payload TEXT NOT NULL, recorded_at TEXT NOT NULL, \
         branch_id TEXT NOT NULL DEFAULT 'main')",
            (),
        )
        .await
        .unwrap();
        conn.execute(
        "INSERT INTO transaction_log VALUES (3, 'c2', '{}', '2026-01-03T00:00:00.000000Z', 'main')",
        (),
    )
    .await
    .unwrap();
        let mut rows = conn
            .query(
                "SELECT seq_id, entity_id, branch_id FROM (
                 SELECT seq_id, entity_id, branch_id FROM main.transaction_log
                 UNION ALL
                 SELECT seq_id, entity_id, branch_id FROM cold.transaction_log
             ) ORDER BY seq_id",
                (),
            )
            .await
            .unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            println!(
                "  seq {} entity {} branch {}",
                r.get::<i64>(0).unwrap(),
                r.get::<String>(1).unwrap(),
                r.get::<String>(2).unwrap()
            );
        }
    }
    println!();

    // ---- 15. can `branch_id` carry a declared foreign key into `branches`? ----
    //
    // The whole shape of the guard work turns on this. If the column can be
    // added *with* a `REFERENCES` clause, the engine enforces lineage
    // referential integrity and the `branches` write guard is belt-and-braces.
    // If it cannot, the guard is the only defence and the design has to say so
    // out loud.
    //
    // SQLite documents the constraint on `ADD COLUMN` as: with foreign keys
    // enabled, a column with a REFERENCES clause must default to NULL. That
    // collides head-on with `NOT NULL DEFAULT 'main'`, which is what makes the
    // rung O(1). Asked rather than assumed, and asked on libSQL.
    println!("\n15. ADD COLUMN with a REFERENCES clause");
    {
        let conn = fresh().await;
        conn.execute(
            "CREATE TABLE branches (branch_id TEXT NOT NULL PRIMARY KEY, \
             parent_id TEXT, forked_at TEXT, created_at TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            &format!("INSERT INTO branches VALUES ('main', NULL, NULL, {TS})"),
            (),
        )
        .await
        .unwrap();
        conn.execute(&v11_schema::concepts_v11(), ()).await.unwrap();

        attempt(
            &conn,
            "NOT NULL DEFAULT 'main' REFERENCES",
            "ALTER TABLE concepts ADD COLUMN branch_id TEXT NOT NULL \
             DEFAULT 'main' REFERENCES branches(branch_id)",
        )
        .await;

        // The documented-legal shape, for contrast: nullable, defaulting NULL.
        // It is legal and it is useless — the column the design needs is the
        // one that cannot be null.
        attempt(
            &conn,
            "nullable REFERENCES (the legal shape)",
            "ALTER TABLE concepts ADD COLUMN branch_nullable TEXT \
             REFERENCES branches(branch_id)",
        )
        .await;

        // And on a table built from scratch, where the clause sits in the
        // CREATE rather than an ALTER. This is the fresh-database path, and if
        // it succeeds while the ALTER fails the two paths diverge in
        // constraints — the shape D-035 names.
        conn.execute(
            "CREATE TABLE fresh_shape (id TEXT PRIMARY KEY, \
             branch_id TEXT NOT NULL DEFAULT 'main' REFERENCES branches(branch_id))",
            (),
        )
        .await
        .unwrap();
        attempt(
            &conn,
            "fresh table, unknown branch",
            "INSERT INTO fresh_shape (id, branch_id) VALUES ('x', 'ghost')",
        )
        .await;
        attempt(
            &conn,
            "fresh table, known branch",
            "INSERT INTO fresh_shape (id, branch_id) VALUES ('y', 'main')",
        )
        .await;

        // Does the engine report a *foreign key* failure, distinguishable by
        // extended result code from the CHECK failures that share primary
        // code 19? C-1 turns on this being true.
        match conn
            .execute(
                "INSERT INTO fresh_shape (id, branch_id) VALUES ('z', 'ghost')",
                (),
            )
            .await
        {
            Err(libsql::Error::SqliteFailure(code, msg)) => {
                println!("  extended code {code} ({msg})   [787 = FOREIGNKEY]");
            }
            other => println!("  unexpected: {other:?}"),
        }

        // Accepted at DDL time is not the question. SQLite forbids this shape
        // because pre-existing rows cannot be validated against the new
        // parent; libSQL took the statement anyway, so the only thing that
        // settles it is whether the constraint *fires*.
        println!("  -- enforcement on the ALTERed table --");
        attempt(
            &conn,
            "altered concepts, unknown branch",
            &format!(
                "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, branch_id) \
                 VALUES ('k1', 't', {TS}, {FOREVER}, {TS}, 'ghost')"
            ),
        )
        .await;
        attempt(
            &conn,
            "altered concepts, known branch",
            &format!(
                "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, branch_id) \
                 VALUES ('k2', 't', {TS}, {FOREVER}, {TS}, 'main')"
            ),
        )
        .await;
        attempt(
            &conn,
            "altered concepts, default branch",
            &format!(
                "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at) \
                 VALUES ('k3', 't', {TS}, {FOREVER}, {TS})"
            ),
        )
        .await;

        // The orphan hole the write guard was invented for: does the engine
        // refuse to rename a branch that rows still point at?
        attempt(
            &conn,
            "rename a referenced branch",
            "UPDATE branches SET branch_id = 'trunk' WHERE branch_id = 'main'",
        )
        .await;
        attempt(
            &conn,
            "delete a referenced branch",
            "DELETE FROM branches WHERE branch_id = 'main'",
        )
        .await;

        // And a pre-existing row, written before the ALTER, carrying the
        // default: does an integrity check see it as satisfied?
        let mut rows = conn.query("PRAGMA foreign_key_check", ()).await.unwrap();
        let mut violations = 0;
        while rows.next().await.unwrap().is_some() {
            violations += 1;
        }
        println!("  foreign_key_check violations: {violations}");

        // The rung refused what this section accepted, so the difference is
        // the thing worth naming — and it is not the one either of us guessed.
        // libSQL applies SQLite's rule *dynamically*: the ALTER is refused only
        // when the table already holds rows **and** foreign keys are on. An
        // empty table takes it with keys on, which is why the section above
        // reads ACCEPTED and every fresh database is unaffected. The
        // transaction is not an axis at all.
        println!("  -- what makes the difference --");
        for (label, fk_on, in_txn, rows) in [
            ("fk ON,  no txn,  empty", true, false, false),
            ("fk ON,  in txn,  empty", true, true, false),
            ("fk OFF, in txn,  empty", false, true, false),
            ("fk ON,  no txn,  rows ", true, false, true),
            ("fk OFF, no txn,  rows ", false, false, true),
            ("fk OFF, in txn,  rows ", false, true, true),
        ] {
            let c = libsql::Builder::new_local(":memory:")
                .build()
                .await
                .unwrap()
                .connect()
                .unwrap();
            c.execute(
                if fk_on {
                    "PRAGMA foreign_keys = ON"
                } else {
                    "PRAGMA foreign_keys = OFF"
                },
                (),
            )
            .await
            .unwrap();
            c.execute(
                "CREATE TABLE branches (branch_id TEXT NOT NULL PRIMARY KEY, \
                 created_at TEXT NOT NULL)",
                (),
            )
            .await
            .unwrap();
            c.execute(&format!("INSERT INTO branches VALUES ('main', {TS})"), ())
                .await
                .unwrap();
            // A table of its own, not `v11_schema::concepts_v11()`: the
            // live const already carries `branch_id`, and the question is
            // what happens when the column arrives by ALTER.
            c.execute(
                "CREATE TABLE concepts (id TEXT NOT NULL UNIQUE, title TEXT NOT NULL)",
                (),
            )
            .await
            .unwrap();
            if rows {
                c.execute("INSERT INTO concepts (id, title) VALUES ('r0', 't')", ())
                    .await
                    .unwrap();
            }
            if in_txn {
                c.execute("BEGIN IMMEDIATE", ()).await.unwrap();
            }
            attempt(
                &c,
                label,
                "ALTER TABLE concepts ADD COLUMN branch_id TEXT NOT NULL \
                 DEFAULT 'main' REFERENCES branches(branch_id)",
            )
            .await;
            if in_txn {
                let _ = c.execute("COMMIT", ()).await;
            }
        }

        // Which leaves the question the rung actually turns on. If enforcement
        // has to be suspended to install the constraint, is what lands a
        // constraint or a decoration? Measured rather than assumed, because
        // "suspend the check to add the check" is exactly the shape that
        // deserves the suspicion.
        println!("  -- is the key real after an ALTER taken with keys off? --");
        {
            let c = libsql::Builder::new_local(":memory:")
                .build()
                .await
                .unwrap()
                .connect()
                .unwrap();
            c.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
            c.execute(
                "CREATE TABLE branches (branch_id TEXT NOT NULL PRIMARY KEY, \
                 created_at TEXT NOT NULL)",
                (),
            )
            .await
            .unwrap();
            c.execute(&format!("INSERT INTO branches VALUES ('main', {TS})"), ())
                .await
                .unwrap();
            c.execute(&v11_schema::concepts_v11(), ()).await.unwrap();
            c.execute(
                &format!(
                    "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at) \
                     VALUES ('r0', 't', {TS}, {FOREVER}, {TS})"
                ),
                (),
            )
            .await
            .unwrap();
            attempt(
                &c,
                "alter with keys suspended",
                "ALTER TABLE concepts ADD COLUMN branch_id TEXT NOT NULL \
                 DEFAULT 'main' REFERENCES branches(branch_id)",
            )
            .await;

            // What `apply_step` runs inside the transaction before committing.
            let mut rows = c.query("PRAGMA foreign_key_check", ()).await.unwrap();
            let mut violations = 0;
            while rows.next().await.unwrap().is_some() {
                violations += 1;
            }
            println!("  {:<44} {violations}", "foreign_key_check violations");

            c.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
            attempt(
                &c,
                "then: insert an unknown branch",
                &format!(
                    "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, \
                     branch_id) VALUES ('k9', 't', {TS}, {FOREVER}, {TS}, 'ghost')"
                ),
            )
            .await;
            attempt(
                &c,
                "then: delete a referenced branch",
                "DELETE FROM branches WHERE branch_id = 'main'",
            )
            .await;

            let mut r = c
                .query("PRAGMA foreign_key_list(concepts)", ())
                .await
                .unwrap();
            let mut keys = 0;
            while r.next().await.unwrap().is_some() {
                keys += 1;
            }
            println!("  {:<44} {keys}", "foreign_key_list(concepts) reports");

            // And the converse: the suspension must not be able to launder a
            // violation past the commit. Plant one and ask the check.
            c.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
            c.execute(
                &format!(
                    "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at, \
                     branch_id) VALUES ('bad', 't', {TS}, {FOREVER}, {TS}, 'ghost')"
                ),
                (),
            )
            .await
            .unwrap();
            let mut rows = c.query("PRAGMA foreign_key_check", ()).await.unwrap();
            let mut violations = 0;
            while rows.next().await.unwrap().is_some() {
                violations += 1;
            }
            println!(
                "  {:<44} {violations}",
                "planted one orphan; the check reports"
            );
        }
    }
    println!();
}
