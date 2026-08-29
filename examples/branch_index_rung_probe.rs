//! W12.14 — the index rung §15.4 owes, measured against the reader that now
//! exists rather than the one that existed when it was priced.
//!
//! [D-219] measured three shapes of `idx_lc_traversal_cover` at 0.14.3 and
//! found that folding `branch_id` in *after* the range columns beats both
//! today's index and the one [§15.3] proposed. That result is the whole
//! justification for the rung, and it was measured against `RESOLVED_UNION` —
//! a hand-written `branch_id IN (ancestry)` traversal. **The same probe run
//! then proved that query is not a resolution** (D-219 §3), and 0.14.4 shipped
//! something else; 0.14.6 changed it again. So the number that justifies the
//! rung describes a query the crate has never executed.
//!
//! This probe re-asks it against `TraversalBuilder::build_sql()` — the exact
//! text the shipped reader runs, obtained from the builder rather than
//! transcribed, because a transcription is how the first measurement stopped
//! matching the code.
//!
//! Sections:
//!
//! 1. **What the shipped read touches.** The Trunk shape and the Resolved
//!    shape, explained, with the `links_current` column set each one names.
//! 2. **Five cover shapes × two shapes of the read**, on the fixture D-219 §3
//!    used, so the numbers are comparable to the ones in the register.
//! 3. **The same, with post-fork churn**, because the fold arm is the half
//!    that reads `links_current` twice and D-223 measured it as the fixed cost.
//! 4. **The write side.** Whatever the read wants, every assertion pays for it.
//!
//! [D-219]: ../docs/architecture/s13-decision-register.md#d-219
//! [§15.3]: ../docs/Macrame%20Road%20to%201.0.md

use std::time::Instant;

use macrame::graph::TraversalBuilder;
use macrame::schema::ddl;

const TS: &str = "2026-01-01T00:00:00.000000Z";
/// After the fork point, so a row stamped here is churn the reader cannot see
/// directly and has to fold out of the log.
const TS_LATE: &str = "2026-06-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const MAIN: &str = ddl::MAIN_BRANCH;

const WIDTH: usize = 10;
const DEPTH: usize = 3;
const REPEATS: usize = 25;

/// The shapes under test, newest column list last.
///
/// Carried here rather than read from `ddl`, for the reason
/// `branch_traversal_probe` gives: this probe *varies* the index, so the shape
/// it varies away from has to be fixed text in this file.
const SHAPES: &[(&str, &str)] = &[
    (
        "today (v12)",
        "(source_id, valid_from, valid_to, weight, edge_type, target_id)",
    ),
    (
        "branch_id first (§15.3)",
        "(branch_id, source_id, valid_from, valid_to, weight, edge_type, target_id)",
    ),
    (
        "folded after range (D-219's winner)",
        "(source_id, valid_from, valid_to, branch_id, weight, edge_type, target_id)",
    ),
    (
        "folded + recorded_at",
        "(source_id, valid_from, valid_to, branch_id, recorded_at, weight, edge_type, target_id)",
    ),
    (
        "designed for the shipped read",
        "(branch_id, recorded_at, source_id, target_id, edge_type, valid_from, valid_to, weight)",
    ),
];

const LC_OPEN: &str = "CREATE INDEX IF NOT EXISTS idx_lc_open_interval \
     ON links_current (source_id, target_id, edge_type, valid_to, valid_from);";

/// The two-index configuration: today's cover index untouched, plus one built
/// for the predicate `churned` and `links_cut` actually seek on.
///
/// D-219 §4 says in as many words that there is no fourth index — *"the same
/// index gains a column"*. That sentence is about a reader that resolved by
/// `branch_id IN (ancestry)`, where one index could serve both shapes because
/// both walked `links_current` by `source_id`. The shipped reader does not: its
/// walk joins a CTE, and its only base scans lead on `branch_id`. The two
/// shapes have stopped sharing an access path, so whether they can share an
/// index is a question again rather than one D-219 closed.
const LC_LINEAGE_CUT: &str = "CREATE INDEX idx_lc_lineage_cut ON links_current \
     (branch_id, recorded_at, source_id, target_id, edge_type, valid_from, valid_to, weight);";

// ───────────────────────────────────────────────────────────────────────────
// Fixture
// ───────────────────────────────────────────────────────────────────────────

async fn fresh() -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    for stmt in [
        ddl::CREATE_BRANCHES_TABLE,
        ddl::CREATE_CONCEPTS_TABLE,
        ddl::CREATE_LINKS_TABLE,
        ddl::CREATE_LINKS_CURRENT_TABLE,
        ddl::CREATE_TRANSACTION_LOG_TABLE,
    ] {
        conn.execute(stmt, ()).await.unwrap();
    }
    conn.execute(ddl::SEED_MAIN_BRANCH, libsql::params![TS])
        .await
        .unwrap();
    conn.execute(LC_OPEN, ()).await.unwrap();
    conn
}

async fn build_chain(conn: &libsql::Connection, n: usize) -> Vec<String> {
    let mut path = vec![MAIN.to_string()];
    for i in 0..n {
        let id = format!("b{i}");
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES (?1, ?2, ?3, ?3)",
            libsql::params![id.as_str(), path[path.len() - 1].as_str(), TS],
        )
        .await
        .unwrap();
        path.push(id);
    }
    path
}

/// A tree of fan-out `WIDTH` and height `DEPTH`, written straight into the
/// projection.
///
/// `spread` deals the edges round-robin across the ancestry path; `None` puts
/// every edge on the trunk. `churn` is the fraction of rows stamped *after* the
/// fork point, which is what sends a key down [`links_cut`]'s fold arm — and
/// those rows get a matching pre-cutoff `transaction_log` entry, because a
/// churned key with nothing to fold is a key that simply disappears and would
/// make the fold arm look free.
async fn build_graph(conn: &libsql::Connection, spread: Option<&[String]>, churn: usize) -> usize {
    let tx = conn.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('n0', 'root', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    let mut frontier = vec!["n0".to_string()];
    let mut next_id = 1usize;
    let mut edges = 0usize;
    let mut churned = 0usize;
    for _ in 0..DEPTH {
        let mut next = Vec::with_capacity(frontier.len() * WIDTH);
        for parent in &frontier {
            for _ in 0..WIDTH {
                let id = format!("n{next_id}");
                next_id += 1;
                tx.execute(
                    "INSERT INTO concepts (id, title, valid_from, recorded_at) \
                     VALUES (?1, 'x', ?2, ?2)",
                    libsql::params![id.as_str(), TS],
                )
                .await
                .unwrap();
                let branch = match spread {
                    Some(chain) if !chain.is_empty() => chain[edges % chain.len()].as_str(),
                    _ => MAIN,
                };
                let late = churned < churn;
                let stamp = if late { TS_LATE } else { TS };
                tx.execute(
                    "INSERT INTO links_current \
                         (source_id, target_id, edge_type, valid_from, valid_to, \
                          weight, properties, recorded_at, branch_id) \
                     VALUES (?1, ?2, 'LINKS', ?3, ?4, 1.0, '{}', ?5, ?6)",
                    libsql::params![parent.as_str(), id.as_str(), TS, FOREVER, stamp, branch],
                )
                .await
                .unwrap();
                if late {
                    churned += 1;
                    // The pre-fork belief the reader is entitled to, in the one
                    // place it survives once the projection has moved on.
                    let entity = format!("{parent}|{id}|LINKS|{TS}");
                    let payload = format!(
                        r#"{{"source_id":"{parent}","target_id":"{id}","edge_type":"LINKS",
                             "valid_from":"{TS}","valid_to":"{FOREVER}","weight":1.0}}"#
                    );
                    tx.execute(
                        "INSERT INTO transaction_log \
                             (table_name, entity_id, operation, payload, recorded_at, branch_id) \
                         VALUES ('links', ?1, 'INSERT', ?2, ?3, ?4)",
                        libsql::params![entity.as_str(), payload.as_str(), TS, branch],
                    )
                    .await
                    .unwrap();
                }
                edges += 1;
                next.push(id);
            }
        }
        frontier = next;
    }
    tx.commit().await.unwrap();
    edges
}

// ───────────────────────────────────────────────────────────────────────────
// Measurement
// ───────────────────────────────────────────────────────────────────────────

fn params(branch: Option<&str>) -> Vec<libsql::Value> {
    let mut p: Vec<libsql::Value> = vec![
        "n0".into(),
        (DEPTH as i64).into(),
        TS.into(),
        libsql::Value::Real(0.0),
    ];
    if let Some(b) = branch {
        p.push(b.into());
    }
    p
}

async fn run(conn: &libsql::Connection, sql: &str, branch: Option<&str>) -> (usize, f64) {
    let mut best = f64::MAX;
    let mut count = 0usize;
    for i in 0..=REPEATS {
        let t = Instant::now();
        let mut rows = conn.query(sql, params(branch)).await.unwrap();
        let mut n = 0usize;
        while rows.next().await.unwrap().is_some() {
            n += 1;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if i > 0 {
            best = best.min(ms);
        }
        count = n;
    }
    (count, best)
}

async fn plan_of(conn: &libsql::Connection, sql: &str, branch: Option<&str>) -> Vec<String> {
    let sql = sql.trim().trim_end_matches(';');
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), params(branch))
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(3).unwrap());
    }
    out
}

/// The plan lines for the `links_current` scans, which are the only ones this
/// probe's index can change.
///
/// Filtered by **alias**, not by table name: `EXPLAIN QUERY PLAN` prints
/// whatever the query called the table, and every access to `links_current` in
/// both shapes is aliased — `l` in the walk, `lc` in the two cut CTEs. The
/// first draft of this filter looked for `links_current` and silently returned
/// nothing, which is D-223's note about aliases and plan assertions arriving
/// from the other direction: there, an alias broke a guard that named the
/// table; here, a name broke a filter that should have said the alias.
fn lc_lines(plan: &[String]) -> Vec<String> {
    plan.iter()
        .filter(|line| {
            line.contains(" lc ")
                || line.ends_with(" lc")
                || line.contains("idx_lc_")
                || ((line.starts_with("SEARCH l ") || line.starts_with("SCAN l "))
                    && !line.contains("lineage"))
        })
        .cloned()
        .collect()
}

async fn set_cover(conn: &libsql::Connection, columns: &str) {
    conn.execute("DROP INDEX IF EXISTS idx_lc_traversal_cover", ())
        .await
        .unwrap();
    conn.execute(
        &format!("CREATE INDEX idx_lc_traversal_cover ON links_current {columns};"),
        (),
    )
    .await
    .unwrap();
}

fn trunk_sql() -> String {
    TraversalBuilder::new("n0").max_depth(DEPTH).build_sql()
}

fn resolved_sql(branch: &str) -> String {
    TraversalBuilder::new("n0")
        .max_depth(DEPTH)
        .on_branch(branch)
        .build_sql()
}

// ───────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("W12.14 probe — the owed index rung against the shipped reader\n");
    println!(
        "fixture: tree of fan-out {WIDTH}, height {DEPTH}; traversal depth {DEPTH}; \
         best of {REPEATS}\n"
    );

    // ---- 1. What the shipped read touches ----
    println!("--- 1. the two shapes of the shipped read ---");
    let conn = fresh().await;
    let path = build_chain(&conn, 10).await;
    build_graph(&conn, Some(&path), 0).await;
    set_cover(&conn, SHAPES[0].1).await;
    let leaf = path.last().unwrap().clone();

    for (label, sql, branch) in [
        ("Trunk", trunk_sql(), None),
        ("Resolved", resolved_sql(&leaf), Some(leaf.as_str())),
    ] {
        println!("  {label}:");
        for line in plan_of(&conn, &sql, branch).await {
            println!("      {line}");
        }
    }
    println!();
    println!("  The Resolved walk joins `visible`, a CTE — it does not touch");
    println!("  links_current at all. The only base scans are `churned` and");
    println!("  `links_cut`, which between them name source_id, target_id,");
    println!("  edge_type, valid_from, valid_to, weight, branch_id AND");
    println!("  recorded_at: eight of the table's nine columns.");
    println!();

    // ---- 2. The five shapes, zero churn ----
    for (label, spread, churn) in [
        ("2a. rows spread over the path, no churn", true, 0usize),
        ("2b. branch holds nothing, no churn", false, 0),
    ] {
        println!("--- {label} ---");
        println!(
            "  {:<38} {:>10} {:>12} {:>8}",
            "cover index", "trunk ms", "resolved ms", "ratio"
        );
        let conn = fresh().await;
        let path = build_chain(&conn, 10).await;
        build_graph(&conn, if spread { Some(&path) } else { None }, churn).await;
        let leaf = path.last().unwrap().clone();
        let (t_sql, r_sql) = (trunk_sql(), resolved_sql(&leaf));

        for (name, columns) in SHAPES {
            set_cover(&conn, columns).await;
            let (_, t_ms) = run(&conn, &t_sql, None).await;
            let (n, r_ms) = run(&conn, &r_sql, Some(&leaf)).await;
            println!(
                "  {name:<38} {t_ms:>10.3} {r_ms:>12.3} {:>7.2}x  ({n} nodes)",
                r_ms / t_ms
            );
            // The trunk plan is the one a pinned test guards: D-042's covering
            // walk. A shape that buys the branched read by taking that away is
            // not a trade this crate can make quietly.
            for line in lc_lines(&plan_of(&conn, &t_sql, None).await) {
                println!("      trunk    {line}");
            }
            for line in lc_lines(&plan_of(&conn, &r_sql, Some(&leaf)).await) {
                println!("      resolved {line}");
            }
        }
        println!();
    }

    // ---- 3. With post-fork churn ----
    println!("--- 3. the fold arm carrying rows (10% post-fork churn) ---");
    println!(
        "  {:<38} {:>10} {:>12} {:>8}",
        "cover index", "trunk ms", "resolved ms", "ratio"
    );
    let conn = fresh().await;
    let path = build_chain(&conn, 10).await;
    let edges = build_graph(&conn, Some(&path), 111).await;
    let leaf = path.last().unwrap().clone();
    let (t_sql, r_sql) = (trunk_sql(), resolved_sql(&leaf));
    for (name, columns) in SHAPES {
        set_cover(&conn, columns).await;
        let (_, t_ms) = run(&conn, &t_sql, None).await;
        let (n, r_ms) = run(&conn, &r_sql, Some(&leaf)).await;
        println!(
            "  {name:<38} {t_ms:>10.3} {r_ms:>12.3} {:>7.2}x  ({n} nodes)",
            r_ms / t_ms
        );
    }
    println!("  ({edges} edges, 111 of them stamped after the fork)\n");

    // ---- 5. Two indices instead of one ----
    //
    // The trade every single-index shape forces: leading on `branch_id` is what
    // the branched read wants, and is exactly what D-042 says the trunk walk
    // must not have. A second index does not have to choose.
    println!("--- 5. today's cover index kept, and a second one added ---");
    for (label, churn) in [("no churn", 0usize), ("10% post-fork churn", 111)] {
        let conn = fresh().await;
        let path = build_chain(&conn, 10).await;
        build_graph(&conn, Some(&path), churn).await;
        let leaf = path.last().unwrap().clone();
        let (t_sql, r_sql) = (trunk_sql(), resolved_sql(&leaf));

        set_cover(&conn, SHAPES[0].1).await;
        let (_, t_one) = run(&conn, &t_sql, None).await;
        let (_, r_one) = run(&conn, &r_sql, Some(&leaf)).await;

        conn.execute(LC_LINEAGE_CUT, ()).await.unwrap();
        let (_, t_two) = run(&conn, &t_sql, None).await;
        let (_, r_two) = run(&conn, &r_sql, Some(&leaf)).await;

        println!(
            "  {label:<22} trunk {t_one:>7.3} -> {t_two:>7.3} ms    \
resolved {r_one:>8.3} -> {r_two:>8.3} ms"
        );
        for line in lc_lines(&plan_of(&conn, &t_sql, None).await) {
            println!("      trunk    {line}");
        }
        for line in lc_lines(&plan_of(&conn, &r_sql, Some(&leaf)).await) {
            println!("      resolved {line}");
        }
    }
    println!();

    // ---- 4. The write side ----
    //
    // D-219 §4 measured this and found the three shapes indistinguishable. Two
    // of the shapes here are wider than any it tried, so the question is open
    // again rather than settled by that entry.
    println!("--- 4. what every assertion pays for the index ---");
    println!("  {:<38} {:>12}", "cover index", "2,000 rows");
    for (name, columns) in std::iter::once(&("no cover index", ""))
        .chain(SHAPES.iter())
        .chain(std::iter::once(&("today + idx_lc_lineage_cut", "+")))
    {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let conn = fresh().await;
            if *columns == "+" {
                set_cover(&conn, SHAPES[0].1).await;
                conn.execute(LC_LINEAGE_CUT, ()).await.unwrap();
            } else if !columns.is_empty() {
                set_cover(&conn, columns).await;
            }
            conn.execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) \
                 VALUES ('n0', 'r', ?1, ?1)",
                libsql::params![TS],
            )
            .await
            .unwrap();
            let t = Instant::now();
            let tx = conn.transaction().await.unwrap();
            for i in 0..2000 {
                tx.execute(
                    "INSERT INTO links_current \
                         (source_id, target_id, edge_type, valid_from, valid_to, \
                          weight, properties, recorded_at, branch_id) \
                     VALUES ('n0', ?1, 'LINKS', ?2, ?3, 1.0, '{}', ?2, 'main')",
                    libsql::params![format!("t{i}"), TS, FOREVER],
                )
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("  {name:<38} {best:>9.1} ms");
    }
}
