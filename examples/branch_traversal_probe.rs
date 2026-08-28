//! W12.3, step 0: what does **resolve at read** cost a traversal, and does the
//! index the plan proposes actually get used?
//!
//! [§15.3] names three ways to make `links_current` lineage-aware and says to
//! take them in order — *"(1) first, measured, with (2) as the escape hatch"* —
//! and then states the deliverable in one sentence: *"depth-3 traversal on a
//! chain of 1, 10 and 100 branches, against the same fixture unbranched."* That
//! is what this file produces. Nothing here is shipped code; the traversal SQL
//! is written out longhand rather than taken from `graph::builder`, because the
//! builder cannot express a lineage predicate and the whole question is what
//! happens when it can.
//!
//! # The three things it measures, and why the third was not in the plan
//!
//! 1. **The chain cost.** A branch reads the rows on the path from itself to
//!    the root, so the predicate is `branch_id IN (ancestry)` and the ancestry
//!    is itself a recursive CTE. §15.3 predicts *"a factor of chain depth"*.
//! 2. **The index.** §15.3 says `idx_lc_traversal_cover` *"gains `branch_id` as
//!    its lead column"*. That is a prediction about the planner, and the
//!    planner is the thing this crate has been wrong about before — D-042's
//!    column order was measured, not reasoned, precisely because putting
//!    `edge_type` second silently cost the covering plan for the *unfiltered*
//!    traversal. The same trap is open here.
//! 3. **Union is not resolution, and the plan does not say so.** §15.2 charges
//!    the overlay with needing a monotonic *"nearest version on the path"* rule
//!    and treats that as an overlay-only cost. It is not: since v12
//!    `links_current` is keyed `(source, target, type, valid_from, branch_id)`,
//!    so a branch correcting an inherited edge writes its **own** row beside the
//!    ancestor's, and `branch_id IN (ancestry)` returns both. A traversal that
//!    unions its ancestry sees an edge it was told was corrected, at the old
//!    weight, alongside the new one. §4 measures what the fix costs.
//!
//! Run with:  cargo run --release --example branch_traversal_probe
//!
//! [§15.3]: ../docs/Macrame%20Road%20to%201.0.md

use std::time::Instant;

use macrame::schema::ddl;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
const MAIN: &str = ddl::MAIN_BRANCH;

/// v12's two `links_current` indices, pinned.
///
/// The crate declares these `pub(crate)`, and a probe should carry its own copy
/// even if it could reach them: §3 below *varies* the cover index, so the shape
/// it is varying away from has to be a fixed thing in this file rather than
/// whatever `ddl` says on the day it is run.
const LC_COVER_V12: &str = "CREATE INDEX IF NOT EXISTS idx_lc_traversal_cover \
     ON links_current (source_id, valid_from, valid_to, weight, edge_type, target_id);";
const LC_OPEN_V12: &str = "CREATE INDEX IF NOT EXISTS idx_lc_open_interval \
     ON links_current (source_id, target_id, edge_type, valid_to, valid_from);";

/// Fan-out per layer. Three layers gives 1 + 10 + 100 + 1000 = 1,111 nodes and
/// 1,110 edges — big enough that a plan change shows up in the timing and small
/// enough that a hundred branch chains still build in seconds.
const WIDTH: usize = 10;
const DEPTH: usize = 3;

/// Best-of, because this is a timing probe on a machine doing other things and
/// the minimum is the least noisy statistic available (the same choice the
/// walk-CTE measurement in `graph::builder` records).
///
/// A discarded warm-up pass precedes them. Without it the first query against a
/// fresh database pays for the page cache, and since §2 runs the plain query
/// first, the plain column absorbed a cost the lineage column never paid — the
/// first draft reported the lineage-filtered traversal at **0.75x** the plain
/// one at chain depth 1, which is not a thing that can be true.
const REPEATS: usize = 25;

// ───────────────────────────────────────────────────────────────────────────
// The two traversals, written out because the builder cannot express one
// ───────────────────────────────────────────────────────────────────────────

/// Today's traversal: `links_current` with no lineage predicate at all.
///
/// Copied in shape from `GraphQuery::walk_cte`, minus the edge-type filter and
/// the transaction-time fold, which are orthogonal to the question.
const UNBRANCHED: &str = r#"
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
)
SELECT DISTINCT node_id FROM walk
"#;

/// Resolve at read, the naive form: the ancestry as a set, joined by `IN`.
///
/// This is what §15.3 describes. §4 shows why it is not sufficient on its own.
const RESOLVED_UNION: &str = r#"
WITH RECURSIVE lineage(branch_id) AS (
    SELECT ?4
    UNION ALL
    SELECT b.parent_id FROM branches b JOIN lineage l ON b.branch_id = l.branch_id
    WHERE b.parent_id IS NOT NULL
),
walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.branch_id IN (SELECT branch_id FROM lineage)
)
SELECT DISTINCT node_id FROM walk
"#;

/// Resolve at read, correctly: the **nearest** ancestor holding each edge wins.
///
/// `lineage` carries its own distance from the reading branch, and the edge set
/// is reduced to one row per `(source, target, type, valid_from)` before the
/// walk ever sees it. That reduction is the cost §15.2 charged to the overlay
/// and which turns out to be owed here too.
const RESOLVED_NEAREST: &str = r#"
WITH RECURSIVE lineage(branch_id, dist) AS (
    SELECT ?4, 0
    UNION ALL
    SELECT b.parent_id, l.dist + 1 FROM branches b JOIN lineage l ON b.branch_id = l.branch_id
    WHERE b.parent_id IS NOT NULL
),
visible(source_id, target_id, edge_type, valid_from, valid_to, weight) AS (
    SELECT source_id, target_id, edge_type, valid_from, valid_to, weight FROM (
        SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.weight,
               ROW_NUMBER() OVER (
                   PARTITION BY l.source_id, l.target_id, l.edge_type, l.valid_from
                   ORDER BY g.dist
               ) AS rn
        FROM links_current l
        JOIN lineage g ON g.branch_id = l.branch_id
    ) WHERE rn = 1
),
walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT v.target_id, w.depth + 1
    FROM walk w
    JOIN visible v ON v.source_id = w.node_id
    WHERE w.depth < ?2
      AND v.valid_from <= ?3 AND ?3 < v.valid_to
)
SELECT DISTINCT node_id FROM walk
"#;

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
    ] {
        conn.execute(stmt, ()).await.unwrap();
    }
    // Takes `created_at` as a parameter, and must run before any row is
    // stamped: every `branch_id` default names `main` and the key is real.
    conn.execute(ddl::SEED_MAIN_BRANCH, libsql::params![TS])
        .await
        .unwrap();
    conn
}

/// A tree of fan-out `WIDTH` and height `DEPTH`.
///
/// Written straight into `concepts` and `links_current` rather than through the
/// public API: the write path is not what is being measured, and going through
/// it would put the log triggers and the single-open probe in the timing.
///
/// `spread` is the lineage each edge lands on. `None` puts every edge on the
/// trunk — the copy-on-write case, where a branch has asserted nothing and
/// inherits everything. `Some(chain)` deals the edges round-robin across the
/// chain, which is the other end of the range: every row on the path is a row
/// the predicate has to admit, and the ancestry set stops being a formality.
async fn build_graph(conn: &libsql::Connection, spread: Option<&[String]>) -> usize {
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
                tx.execute(
                    "INSERT INTO links_current \
                         (source_id, target_id, edge_type, valid_from, valid_to, \
                          weight, properties, recorded_at, branch_id) \
                     VALUES (?1, ?2, 'LINKS', ?3, ?4, 1.0, '{}', ?3, ?5)",
                    libsql::params![parent.as_str(), id.as_str(), TS, FOREVER, branch],
                )
                .await
                .unwrap();
                edges += 1;
                next.push(id);
            }
        }
        frontier = next;
    }
    tx.commit().await.unwrap();
    edges
}

/// A chain of `n` branches hanging off `main`, root-first, `main` included.
///
/// Returns the whole path rather than its leaf, because §2b deals rows across
/// it and the leaf alone cannot say what the path was.
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

// ───────────────────────────────────────────────────────────────────────────
// Measurement
// ───────────────────────────────────────────────────────────────────────────

async fn run(conn: &libsql::Connection, sql: &str, branch: Option<&str>) -> (usize, f64) {
    let mut best = f64::MAX;
    let mut count = 0usize;
    for i in 0..=REPEATS {
        let t = Instant::now();
        let mut rows = match branch {
            Some(b) => conn
                .query(sql, libsql::params!["n0", DEPTH as i64, TS, b])
                .await
                .unwrap(),
            None => conn
                .query(sql, libsql::params!["n0", DEPTH as i64, TS])
                .await
                .unwrap(),
        };
        let mut n = 0usize;
        while rows.next().await.unwrap().is_some() {
            n += 1;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        // Pass 0 is the warm-up and is thrown away — see `REPEATS`.
        if i > 0 {
            best = best.min(ms);
        }
        count = n;
    }
    (count, best)
}

async fn plan_of(conn: &libsql::Connection, sql: &str, branch: Option<&str>) -> Vec<String> {
    let eqp = format!("EXPLAIN QUERY PLAN {sql}");
    let mut rows = match branch {
        Some(b) => conn
            .query(&eqp, libsql::params!["n0", DEPTH as i64, TS, b])
            .await
            .unwrap(),
        None => conn
            .query(&eqp, libsql::params!["n0", DEPTH as i64, TS])
            .await
            .unwrap(),
    };
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(3).unwrap());
    }
    out
}

fn show_plan(label: &str, lines: &[String]) {
    println!("  {label}");
    for l in lines {
        println!("      {l}");
    }
}

// ───────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("W12.3 probe — resolve at read, measured\n");
    println!(
        "fixture: tree of fan-out {WIDTH}, height {DEPTH}; traversal depth {DEPTH}; \
         best of {REPEATS}\n"
    );

    // ---- 1. The baseline, and the shape of the answer ----
    let conn = fresh().await;
    let edges = build_graph(&conn, None).await;
    for idx in [LC_COVER_V12, LC_OPEN_V12] {
        conn.execute(idx, ()).await.unwrap();
    }
    let (base_n, base_ms) = run(&conn, UNBRANCHED, None).await;
    println!("--- 1. unbranched baseline ---");
    println!("  {edges} edges, {base_n} nodes reached, {base_ms:.3} ms");
    show_plan("", &plan_of(&conn, UNBRANCHED, None).await);
    println!();

    // ---- 2. The chain cost, measured against the same database ----
    //
    // Both queries run on one connection, so the ratio is a property of the SQL
    // rather than of two b-trees built at different times. The first draft of
    // this probe compared across databases and reported the lineage-filtered
    // traversal as *faster* than the unfiltered one, which is not a finding.
    //
    // 2a is the copy-on-write case: every edge on the trunk, the branch holding
    // nothing. That is the common case and the cheap end of the range.
    // 2b deals the same edges across the whole path, so every ancestry entry is
    // one the predicate has to admit.
    for (label, spread) in [
        ("2a. branch holds nothing", false),
        ("2b. rows spread over the path", true),
    ] {
        println!("--- {label} ---");
        println!(
            "  {:>7}  {:>7}  {:>10}  {:>10}  {:>8}",
            "chain", "nodes", "plain ms", "lineage ms", "ratio"
        );
        for chain in [1usize, 10, 100] {
            let conn = fresh().await;
            let path = build_chain(&conn, chain).await;
            build_graph(&conn, if spread { Some(&path) } else { None }).await;
            for idx in [LC_COVER_V12, LC_OPEN_V12] {
                conn.execute(idx, ()).await.unwrap();
            }
            let leaf = path.last().unwrap().clone();
            let (_, plain_ms) = run(&conn, UNBRANCHED, None).await;
            let (n, ms) = run(&conn, RESOLVED_UNION, Some(&leaf)).await;
            println!(
                "  {chain:>7}  {n:>7}  {plain_ms:>10.3}  {ms:>10.3}  {:>7.2}x",
                ms / plain_ms
            );
        }
        println!();
    }

    // ---- 3. The index §15.3 proposes ----
    //
    // The claim under test is that `idx_lc_traversal_cover` should gain
    // `branch_id` as its **lead** column. A lead column is a seek column, and
    // the predicate here is `IN (subquery)` rather than an equality — so the
    // question is whether the planner will drive the index from it at all, or
    // whether leading on `branch_id` merely moves `source_id` out of first
    // position and loses the seek the recursive step depends on.
    //
    // Measured on the spread fixture, because on the copy-on-write one the
    // predicate admits every row and an index that helps it cannot show that it
    // does.
    println!("--- 3. the index, and whether the planner takes it ---");
    let conn = fresh().await;
    let path = build_chain(&conn, 10).await;
    build_graph(&conn, Some(&path)).await;
    conn.execute(LC_OPEN_V12, ()).await.unwrap();
    let leaf = path.last().unwrap().clone();

    for (label, cover) in [
        ("source_id first (today)", LC_COVER_V12),
        (
            "branch_id first (§15.3)",
            "CREATE INDEX idx_lc_traversal_cover ON links_current \
             (branch_id, source_id, valid_from, valid_to, weight, edge_type, target_id);",
        ),
        (
            "source_id first, branch_id folded in",
            "CREATE INDEX idx_lc_traversal_cover ON links_current \
             (source_id, valid_from, valid_to, branch_id, weight, edge_type, target_id);",
        ),
    ] {
        conn.execute("DROP INDEX IF EXISTS idx_lc_traversal_cover", ())
            .await
            .unwrap();
        conn.execute(cover, ()).await.unwrap();
        let (_, ms) = run(&conn, RESOLVED_UNION, Some(&leaf)).await;
        let (_, plain_ms) = run(&conn, UNBRANCHED, None).await;
        println!("  {label:<38} {ms:>8.3} ms lineage / {plain_ms:>7.3} ms plain");
        show_plan("", &plan_of(&conn, RESOLVED_UNION, Some(&leaf)).await);
    }
    println!();

    // ---- 4. Union is not resolution ----
    //
    // The correction below is exactly what a branch is for: the trunk says the
    // first edge out of the root has weight 1.0, the branch says 0.25. Under
    // v12's per-lineage key that is a second row, not an overwrite.
    println!("--- 4. what the union form actually returns ---");
    let conn = fresh().await;
    let path = build_chain(&conn, 10).await;
    build_graph(&conn, None).await;
    for idx in [LC_COVER_V12, LC_OPEN_V12] {
        conn.execute(idx, ()).await.unwrap();
    }
    let leaf = path.last().unwrap().clone();
    conn.execute(
        "INSERT INTO links_current \
             (source_id, target_id, edge_type, valid_from, valid_to, weight, \
              properties, recorded_at, branch_id) \
         VALUES ('n0', 'n1', 'LINKS', ?1, ?2, 0.25, '{}', ?1, ?3)",
        libsql::params![TS, FOREVER, leaf.as_str()],
    )
    .await
    .unwrap();

    let mut rows = conn
        .query(
            "WITH RECURSIVE lineage(branch_id) AS (
                 SELECT ?1 UNION ALL
                 SELECT b.parent_id FROM branches b JOIN lineage l ON b.branch_id = l.branch_id
                 WHERE b.parent_id IS NOT NULL)
             SELECT branch_id, weight FROM links_current
             WHERE source_id = 'n0' AND target_id = 'n1'
               AND branch_id IN (SELECT branch_id FROM lineage)
             ORDER BY weight",
            libsql::params![leaf.as_str()],
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        seen.push(format!(
            "{}@{:.2}",
            r.get::<String>(0).unwrap(),
            r.get::<f64>(1).unwrap()
        ));
    }
    println!(
        "  one corrected edge, read from the branch: {} row(s) — {}",
        seen.len(),
        seen.join(", ")
    );
    println!(
        "  {}",
        if seen.len() > 1 {
            "the ancestor's superseded belief is still in the answer"
        } else {
            "resolution happened without being asked for"
        }
    );

    let (_, plain_ms) = run(&conn, UNBRANCHED, None).await;
    let (union_n, union_ms) = run(&conn, RESOLVED_UNION, Some(&leaf)).await;
    let (near_n, near_ms) = run(&conn, RESOLVED_NEAREST, Some(&leaf)).await;
    println!("  plain   : {plain_ms:.3} ms");
    println!(
        "  union   : {union_n} nodes, {union_ms:.3} ms  ({:.2}x plain)",
        union_ms / plain_ms
    );
    println!(
        "  nearest : {near_n} nodes, {near_ms:.3} ms  ({:.2}x plain, {:.2}x the union form)",
        near_ms / plain_ms,
        near_ms / union_ms
    );
    show_plan(
        "nearest-ancestor plan:",
        &plan_of(&conn, RESOLVED_NEAREST, Some(&leaf)).await,
    );
    println!();

    // ---- 4b. Retirement on a branch, which is the case that decides it ----
    //
    // §4 is a *correction*: the branch disagrees about a weight, both rows name
    // the same nodes, and the reachable set is unchanged either way. Retirement
    // is the case where the reachable set is the whole point — a branch saying
    // "on my lineage this edge is no longer believed" — and the mechanism
    // available to it is **shadowing**: since `links_current` is keyed per
    // lineage, the branch writes its own row at the ancestor's key with a closed
    // interval, and the ancestor's row is left untouched. That is the only form
    // of retirement Doctrine III permits across lineages, because closing the
    // ancestor's own row is the parent corruption branching exists to prevent.
    //
    // Whether shadowing *works* is a property of the read, not of the write, and
    // it is the question 0.14.4 turns on. Two traversals, one edge, one node
    // that is only reachable through it.
    println!("--- 4b. retiring an inherited edge, by shadowing ---");
    let conn = fresh().await;
    let path = build_chain(&conn, 10).await;
    build_graph(&conn, None).await;
    for idx in [LC_COVER_V12, LC_OPEN_V12] {
        conn.execute(idx, ()).await.unwrap();
    }
    let leaf = path.last().unwrap().clone();

    // `n1` is one of the root's ten children and the sole route to its own
    // hundred descendants, so retiring `n0 -> n1` should cost the branch 111
    // nodes — itself, its ten children and their hundred.
    conn.execute(
        "INSERT INTO links_current \
             (source_id, target_id, edge_type, valid_from, valid_to, weight, \
              properties, recorded_at, branch_id) \
         VALUES ('n0', 'n1', 'LINKS', ?1, ?2, 1.0, '{}', ?1, ?3)",
        libsql::params![TS, TS, leaf.as_str()],
    )
    .await
    .unwrap();

    let (union_n, _) = run(&conn, RESOLVED_UNION, Some(&leaf)).await;
    let (near_n, _) = run(&conn, RESOLVED_NEAREST, Some(&leaf)).await;
    let (plain_n, _) = run(&conn, UNBRANCHED, None).await;
    println!("  trunk    : {plain_n} nodes");
    println!(
        "  union    : {union_n} nodes — {}",
        if union_n == plain_n {
            "the retirement had no effect at all"
        } else {
            "the retirement was seen"
        }
    );
    println!(
        "  nearest  : {near_n} nodes — {}",
        if near_n < plain_n {
            "the shadow closed the edge and the subtree went with it"
        } else {
            "the shadow did not take"
        }
    );
    println!();
    // ---- 6. What resolution costs a database that never forked ----
    //
    // Every database this crate has written so far has exactly one lineage, and
    // that stays the common case after `fork()` ships. If the resolved form is
    // still 3x here, then emitting it unconditionally makes every existing
    // caller pay for a feature none of them use, and the read path needs to pick
    // a shape the way `cold_lineage` picks one at the archive boundary.
    //
    // The `branches` register holds one row; the ancestry is `{main}`; the
    // `ROW_NUMBER()` partition has exactly one row per group and every `rn` is 1.
    // The question is whether the planner can see that, and it cannot be
    // reasoned about — a window function is opaque to it.
    println!("--- 6. single lineage: what the resolution costs when it cannot help ---");
    let conn = fresh().await;
    build_graph(&conn, None).await;
    for idx in [LC_COVER_V12, LC_OPEN_V12] {
        conn.execute(idx, ()).await.unwrap();
    }
    let (plain_n, plain_ms) = run(&conn, UNBRANCHED, None).await;
    let (union_n, union_ms) = run(&conn, RESOLVED_UNION, Some(MAIN)).await;
    let (near_n, near_ms) = run(&conn, RESOLVED_NEAREST, Some(MAIN)).await;
    println!("  plain   : {plain_n} nodes, {plain_ms:.3} ms");
    println!(
        "  union   : {union_n} nodes, {union_ms:.3} ms  ({:.2}x plain)",
        union_ms / plain_ms
    );
    println!(
        "  nearest : {near_n} nodes, {near_ms:.3} ms  ({:.2}x plain)",
        near_ms / plain_ms
    );
    println!(
        "  {}",
        if near_ms / plain_ms > 1.5 {
            "one shape for both cases would charge every existing caller for a \
             feature none of them use"
        } else {
            "one shape is affordable and the read path does not need to branch"
        }
    );
    println!();
    // ---- 5. The write side, which is what an index costs ----
    //
    // §15.3 calls the index change *"a fourth index write per assertion on a
    // table that already takes four"*. There is no fourth index: the same index
    // gains a column. What that costs is the difference between these rows.
    println!("--- 5. insert cost per index shape ---");
    for (label, cover) in [
        ("no cover index", ""),
        ("source_id first (today)", LC_COVER_V12),
        (
            "branch_id first (§15.3)",
            "CREATE INDEX idx_lc_traversal_cover ON links_current \
             (branch_id, source_id, valid_from, valid_to, weight, edge_type, target_id);",
        ),
        (
            "source_id first, branch_id folded in",
            "CREATE INDEX idx_lc_traversal_cover ON links_current \
             (source_id, valid_from, valid_to, branch_id, weight, edge_type, target_id);",
        ),
    ] {
        // Best of five on a fresh database each time, for the reason §3 and §4
        // are best-of: a single shot here came back 14.0, 14.2 and 29.6 ms for
        // the same row across three runs, which is a measurement of the machine.
        let mut best = f64::MAX;
        for _ in 0..5 {
            let conn = fresh().await;
            build_graph(&conn, None).await;
            if !cover.is_empty() {
                conn.execute(cover, ()).await.unwrap();
            }
            let t = Instant::now();
            let tx = conn.transaction().await.unwrap();
            for i in 0..2000 {
                tx.execute(
                    "INSERT INTO links_current \
                         (source_id, target_id, edge_type, valid_from, valid_to, weight, \
                          properties, recorded_at, branch_id) \
                     VALUES ('n0', ?1, 'X', ?2, ?3, 1.0, '{}', ?2, ?4)",
                    libsql::params![format!("n{}", i + 1), TS, FOREVER, MAIN],
                )
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("  {label:<38} {best:>8.1} ms / 2000 rows");
    }
}
