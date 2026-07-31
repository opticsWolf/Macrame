//! T2.1, step 0: what does `CHECK (weight >= 0.0)` actually refuse?
//!
//! Two claims to settle before writing a migration rung that cannot be undone
//! without another one.
//!
//! **The plan's claim: the CHECK accepts `+∞`.** If true, the loader guard has
//! to stay whatever else happens, because `+∞` is exactly as unsound for
//! Dijkstra as a negative weight is — every relaxation succeeds and the "shortest"
//! path is whichever was reached first. Probed here rather than assumed, and on
//! libSQL, for the reason D-078 made expensive: a probe on a different engine is
//! a probe on a different engine.
//!
//! **The claim the plan does not make: does rebuilding `links` work at all?**
//! SQLite has no `ADD CONSTRAINT`, so the rung is a full table rebuild — and
//! `links` is the one table in this schema carrying four triggers, a foreign key
//! into `concepts`, and a composite primary key. T1.2 established that a rename
//! reparses the whole schema; `links`'s triggers reference `links_current`, and
//! `links_current`'s do not reference `links`, so the direction matters and the
//! order that worked there may not be the order that works here.
//!
//! Run with:  cargo run --release --example weight_check_probe

use macrame::schema::ddl;

async fn try_weight(conn: &libsql::Connection, label: &str, sql: &str) {
    match conn.execute(sql, ()).await {
        Ok(_) => println!("  {label:<28} ACCEPTED"),
        Err(e) => {
            let msg = e.to_string();
            let short = msg.split(':').next_back().unwrap_or(&msg).trim().to_string();
            println!("  {label:<28} refused ({short})");
        }
    }
}

#[tokio::main]
async fn main() {
    // ---- 1. what the CHECK admits ----
    println!("\n1. CHECK (weight >= 0.0), values offered directly:");
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE w (id INTEGER PRIMARY KEY, weight REAL NOT NULL CHECK (weight >= 0.0))",
        (),
    )
    .await
    .unwrap();

    for (label, expr) in [
        ("1.0", "1.0"),
        ("0.0", "0.0"),
        ("-0.0", "-0.0"),
        ("-1.5", "-1.5"),
        ("+inf (9e999)", "9e999"),
        ("-inf (-9e999)", "-9e999"),
        ("NaN (0.0/0.0)", "0.0/0.0"),
        ("integer 3", "3"),
        ("text '5'", "'5'"),
        ("text 'abc'", "'abc'"),
    ] {
        try_weight(
            &conn,
            label,
            &format!("INSERT INTO w (weight) VALUES ({expr})"),
        )
        .await;
    }

    // ---- 2. does the table rebuild work on `links`? ----
    println!("\n2. rebuilding `links` with the CHECK, in one transaction:");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("m.db");
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    macrame::schema::migrations::run(&conn).await.unwrap();

    for id in ["c0", "c1"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) \
             VALUES (?1, 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
            libsql::params![id],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) VALUES ('c0','c1','A', \
         '2026-01-01T00:00:00.000000Z','9999-12-31T23:59:59.999999Z',2.5,'{}', \
         '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    let new_ddl = ddl::CREATE_LINKS_TABLE
        .replacen("links", "links_rebuild", 1)
        .replace(
            "weight      REAL NOT NULL DEFAULT 1.0,",
            "weight      REAL NOT NULL DEFAULT 1.0 CHECK (weight >= 0.0),",
        );

    let mut ok = true;
    let step = |label: &'static str| label;
    for (label, sql) in [
        ("BEGIN IMMEDIATE", "BEGIN IMMEDIATE".to_string()),
        ("create links_rebuild", new_ddl.clone()),
        (
            "copy rows",
            "INSERT INTO links_rebuild (source_id, target_id, edge_type, valid_from, \
             recorded_at, valid_to, weight, properties) SELECT source_id, target_id, \
             edge_type, valid_from, recorded_at, valid_to, weight, properties FROM links"
                .to_string(),
        ),
        ("drop links (takes its triggers)", "DROP TABLE links".to_string()),
        (
            "rename",
            "ALTER TABLE links_rebuild RENAME TO links".to_string(),
        ),
    ] {
        match conn.execute(&sql, ()).await {
            Ok(_) => println!("  {:<38} OK", step(label)),
            Err(e) => {
                println!("  {:<38} ERR: {e}", step(label));
                ok = false;
            }
        }
    }
    if ok {
        for trigger in ddl::CREATE_TRIGGERS {
            if trigger.contains("ON links\n") || trigger.contains("ON links\r\n") {
                if let Err(e) = conn.execute(trigger, ()).await {
                    println!("  recreate trigger                       ERR: {e}");
                    ok = false;
                }
            }
        }
        println!("  recreate links triggers                OK={ok}");
    }
    let commit = conn.execute("COMMIT", ()).await;
    println!("  COMMIT                                 {commit:?}");

    // ---- 3. did it survive, and does the CHECK bite? ----
    println!("\n3. after the rebuild:");
    let n: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    println!("  links holds {n} row(s)");

    let mut triggers = Vec::new();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='links' \
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        triggers.push(r.get::<String>(0).unwrap());
    }
    println!("  triggers on links: {triggers:?}");

    try_weight(
        &conn,
        "negative weight now",
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) VALUES ('c0','c1','NEG', \
         '2026-01-01T00:00:00.000000Z','2027-01-01T00:00:00.000000Z',-1.0,'{}', \
         '2026-02-01T00:00:00.000000Z')",
    )
    .await;

    // The FK must have survived the rename too — SQLite's modern rename fixes up
    // references, but this table is the *child*, so it is its own FK clause that
    // has to have come through the DDL substitution intact.
    try_weight(
        &conn,
        "edge to a missing concept",
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) VALUES ('c0','nope','FK', \
         '2026-01-01T00:00:00.000000Z','2027-01-01T00:00:00.000000Z',1.0,'{}', \
         '2026-03-01T00:00:00.000000Z')",
    )
    .await;

    // ---- 4. the hole the CHECK does not close ----
    //
    // `REAL` is an affinity, not a type: SQLite converts what it can and stores
    // the rest as-is. `'abc'` cannot become a number, so it stays TEXT — and in
    // SQLite's type ordering every TEXT value sorts above every numeric one, so
    // `'abc' >= 0.0` is *true* and the CHECK passes it. What the read side then
    // does with it is the question that decides how wide this constraint should
    // be.
    println!("\n4. a text weight, which the CHECK admits:");
    try_weight(
        &conn,
        "weight = 'abc'",
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at) VALUES ('c0','c1','TXT', \
         '2026-01-01T00:00:00.000000Z','9999-12-31T23:59:59.999999Z','abc','{}', \
         '2026-04-01T00:00:00.000000Z')",
    )
    .await;

    let mut rows = conn
        .query(
            "SELECT typeof(weight), weight FROM links WHERE edge_type = 'TXT'",
            (),
        )
        .await
        .unwrap();
    if let Some(r) = rows.next().await.unwrap() {
        println!("  stored typeof: {:?}", r.get::<String>(0).unwrap());
        println!("  as a Value:    {:?}", r.get_value(1));
        // `r.get::<f64>(1)` is NOT called here. It does not return `Err` — it
        // reaches `unreachable!("invalid value type")` inside libsql 0.9.30 and
        // **panics**. Which is the finding: a text weight is not a wrong answer
        // on the read path, it is a panic in a dependency, and the panic happens
        // wherever the row is read rather than where the bad value was written.
        println!("  read as f64:   panics (libsql rows.rs: invalid value type)");
    }

    // Would `typeof(weight) = 'real'` close it, and does it disturb the values
    // that must keep working?
    println!("\n5. CHECK (weight >= 0.0 AND typeof(weight) = 'real'):");
    let db3 = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn3 = db3.connect().unwrap();
    conn3
        .execute(
            "CREATE TABLE w2 (id INTEGER PRIMARY KEY, weight REAL NOT NULL \
             CHECK (weight >= 0.0 AND typeof(weight) = 'real'))",
            (),
        )
        .await
        .unwrap();
    for (label, expr) in [
        ("1.0", "1.0"),
        ("0.0", "0.0"),
        ("integer 3", "3"),
        ("text '5'", "'5'"),
        ("text 'abc'", "'abc'"),
        ("+inf (9e999)", "9e999"),
        ("-1.5", "-1.5"),
    ] {
        try_weight(
            &conn3,
            label,
            &format!("INSERT INTO w2 (weight) VALUES ({expr})"),
        )
        .await;
    }
}
