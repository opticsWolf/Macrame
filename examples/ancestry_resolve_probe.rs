//! W16.1, step 0: can the ancestry CTE become a bound `VALUES` table, and what
//! does that cost?
//!
//! [A-2] argues the recursive ancestry CTE should be resolved once in Rust and
//! bound as a list. The argument is portability first — Turso has no `WITH
//! RECURSIVE`, so this is the *only* form that runs there — and D-219 already
//! measured the CTE as a constant on libSQL, so the honest expectation here is
//! **parity, not a win**. That expectation is worth checking before the shape
//! is built, because a form that is 20% slower on the engine we ship on today
//! would be a different decision from one that is free.
//!
//! Four questions, in the order that can stop the work:
//!
//! 1. **Feasibility.** Does libSQL accept `WITH lineage(a,b,c) AS (VALUES …)`
//!    with *bound* parameters, including a bound `NULL` cutoff? If it does not,
//!    the alternative is interpolating branch ids into SQL text, and the whole
//!    approach has to be re-argued against that.
//! 2. **The CTE in isolation**, against the `VALUES` form, at ancestry depths
//!    1, 4 and 16.
//! 3. **The CTE where it is actually used** — joined the way `churned_cte` and
//!    `links_cut_cte` join it, on a real forked database.
//! 4. **The read-side round trip.** Today every read runs `lineage_shape`'s
//!    three-aggregate `SELECT`. Resolving in Rust needs the *rows*, not the
//!    aggregates. If loading the whole table costs the same, the ancestry
//!    arrives free and no cache is needed on the read side; if it costs more,
//!    A-2's cached `Vec<Branch>` becomes load-bearing rather than an
//!    optimisation.
//!
//! Question 4 is the one that decides the shape of the release, which is why it
//! is measured here rather than assumed from "`branches` is tiny".
//!
//! Run with:  cargo run --release --example ancestry_resolve_probe
//!
//! [A-2]: ../docs/Macrame%20Codebase%20Review%20v0.15.0.md

use std::time::{Duration, Instant};

use macrame::prelude::*;
use macrame::{Branch, BranchId};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// Best-of, because the question is what the work costs and not what the
/// machine was doing at the time.
///
/// Async, and not a `block_on` inside a sync closure: this binary *is* a
/// runtime, and driving a second one from a worker thread panics rather than
/// measuring.
async fn best_of<F, Fut>(rounds: usize, mut f: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        f().await;
        best = best.min(t.elapsed());
    }
    best
}

/// One statement, stepped to its first row. The unwraps are the point: a
/// discarded `Result` in a timing loop measures the error path.
async fn one_row(conn: &libsql::Connection, sql: &str, params: Vec<libsql::Value>) {
    let mut r = conn.query(sql, params).await.expect("query");
    r.next().await.expect("step").expect("one row");
}

/// One resolved ancestor: exactly the three columns `ancestry_cte` produces.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ancestor {
    branch_id: String,
    dist: i64,
    cutoff: Option<String>,
}

/// The candidate: the recursive CTE, done in Rust.
///
/// Reproduces `ancestry_cte` term for term — the reader itself at `dist` 0 with
/// no cutoff, then one row per ancestor walking `parent_id` up, carrying a
/// **running minimum** of `forked_at`. The running minimum is the part that is
/// easy to get wrong and is what `branch_lifecycle_tests` pins: a fork cut from
/// a branch that was itself cut later must not widen the window its parent had.
fn resolve(branches: &[Branch], start: &str) -> Vec<Ancestor> {
    let find = |name: &str| branches.iter().find(|b| b.id.as_str() == name);
    let mut out = Vec::new();
    let mut cur = start.to_string();
    let mut cutoff: Option<String> = None;
    // Bounded by the number of lineages: `branches` is a tree and each step
    // moves to a strict parent, so this cannot cycle on data the schema
    // permits. The bound is a belt on the braces, not the termination
    // argument -- and `dist` is the loop's own index, which is what the
    // shipped `graph::lineage::resolve` also does.
    for dist in 0..=(branches.len() as i64) {
        out.push(Ancestor {
            branch_id: cur.clone(),
            dist,
            cutoff: cutoff.clone(),
        });
        let Some(node) = find(&cur) else { break };
        let (Some(parent), Some(forked)) = (node.parent.as_ref(), node.forked_at.as_ref()) else {
            break;
        };
        cutoff = Some(match cutoff {
            Some(c) if c.as_str() <= forked.as_str() => c,
            _ => forked.clone(),
        });
        cur = parent.as_str().to_string();
    }
    out
}

/// The `VALUES` form, with every value at a placeholder.
///
/// Bound rather than interpolated: a branch id is caller-supplied text and the
/// crate has exactly one arbitrary-SQL surface already ([D-258]), which is not
/// a second one worth opening for a CTE. The consequence is that the statement
/// *text* varies with ancestry **depth** but not with any name, so the shape
/// cardinality is the number of distinct fork depths rather than the number of
/// branches.
fn ancestry_values(rows: &[Ancestor], first_slot: usize) -> String {
    let tuples: Vec<String> = (0..rows.len())
        .map(|i| {
            let b = first_slot + i * 3;
            format!("(?{}, ?{}, ?{})", b, b + 1, b + 2)
        })
        .collect();
    format!(
        "lineage(branch_id, dist, cutoff) AS (VALUES {})",
        tuples.join(", ")
    )
}

/// The `VALUES` form with `dist` as a literal.
///
/// `dist` is the row's own index in a list this crate built — it is never
/// caller text, so binding it buys nothing and costs a parameter per ancestor.
fn ancestry_values_lit_dist(rows: &[Ancestor], first_slot: usize) -> String {
    let tuples: Vec<String> = (0..rows.len())
        .map(|i| {
            let b = first_slot + i * 2;
            format!("(?{}, {}, ?{})", b, i, b + 1)
        })
        .collect();
    format!(
        "lineage(branch_id, dist, cutoff) AS (VALUES {})",
        tuples.join(", ")
    )
}

fn params_lit_dist(rows: &[Ancestor]) -> Vec<libsql::Value> {
    let mut v = Vec::with_capacity(rows.len() * 2);
    for r in rows {
        v.push(libsql::Value::Text(r.branch_id.clone()));
        v.push(match &r.cutoff {
            Some(c) => libsql::Value::Text(c.clone()),
            None => libsql::Value::Null,
        });
    }
    v
}

/// The CTE this replaces, verbatim from `graph::lineage::ancestry_cte`.
fn ancestry_recursive(slot: usize) -> String {
    format!(
        r#"lineage(branch_id, dist, cutoff) AS (
    SELECT ?{slot}, 0, NULL
    UNION ALL
    SELECT b.parent_id, g.dist + 1,
           CASE WHEN g.cutoff IS NULL OR b.forked_at < g.cutoff
                THEN b.forked_at ELSE g.cutoff END
    FROM branches b JOIN lineage g ON b.branch_id = g.branch_id
    WHERE b.parent_id IS NOT NULL
)"#
    )
}

fn params_for(rows: &[Ancestor]) -> Vec<libsql::Value> {
    let mut v = Vec::with_capacity(rows.len() * 3);
    for r in rows {
        v.push(libsql::Value::Text(r.branch_id.clone()));
        v.push(libsql::Value::Integer(r.dist));
        v.push(match &r.cutoff {
            Some(c) => libsql::Value::Text(c.clone()),
            None => libsql::Value::Null,
        });
    }
    v
}

/// Read a `(branch_id, dist, cutoff)` relation back, ordered, for comparison.
async fn read_ancestry(
    conn: &libsql::Connection,
    sql: &str,
    params: Vec<libsql::Value>,
) -> Vec<Ancestor> {
    let mut rows = conn.query(sql, params).await.expect("ancestry query");
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.expect("row") {
        out.push(Ancestor {
            branch_id: r.get::<String>(0).expect("branch_id"),
            dist: r.get::<i64>(1).expect("dist"),
            cutoff: r.get::<Option<String>>(2).expect("cutoff"),
        });
    }
    out.sort_by_key(|a| a.dist);
    out
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ancestry.db");
    let db = Database::open(&path).await.expect("open");

    // A trunk with a graph on it, so the joins in §3 have rows to touch.
    let mut concepts = Vec::new();
    for i in 0..400 {
        concepts.push(ConceptUpsert::new(format!("c{i}"), "N").valid_from(T0));
    }
    db.write_concepts(concepts).await.expect("concepts");
    let mut edges = Vec::new();
    for i in 0..399 {
        edges.push(
            EdgeAssertion::new(format!("c{i}"), format!("c{}", i + 1), "LINKS")
                .valid_from(T0)
                .valid_to(FOREVER),
        );
    }
    db.bulk_import(edges).await.expect("edges");

    // A chain 16 deep. Each fork's `forked_at` is later than the last, which is
    // the shape where the running minimum does *not* bite -- §4 builds the one
    // where it does.
    let mut chain = vec!["main".to_string()];
    for i in 1..=16 {
        let name = format!("b{i}");
        let parent = chain.last().expect("parent").clone();
        db.fork(
            BranchId::new(&name).expect("name"),
            BranchId::new(&parent).expect("parent"),
        )
        .await
        .expect("fork");
        chain.push(name);
    }

    let branches = db.branches().await.expect("branches");
    let conn = db.diagnostic_conn().await.expect("diagnostic");

    println!("=== 1. does libSQL accept a bound VALUES CTE? ===\n");
    for depth in [1_usize, 4, 16] {
        let start = &chain[depth];
        let rust = resolve(&branches, start);
        let sql = format!(
            "WITH {} SELECT branch_id, dist, cutoff FROM lineage",
            ancestry_values(&rust, 1)
        );
        let got = read_ancestry(&conn, &sql, params_for(&rust)).await;
        println!(
            "  depth {depth:>2}: {} rows bound, read back {} -- {}",
            rust.len(),
            got.len(),
            if got == rust { "identical" } else { "DIFFERS" }
        );
    }

    println!("\n=== 2. the Rust walk against the CTE it replaces ===\n");
    let mut disagreements = 0;
    for start in &chain {
        let cte = format!(
            "WITH RECURSIVE {} SELECT branch_id, dist, cutoff FROM lineage",
            ancestry_recursive(1)
        );
        let from_sql = read_ancestry(&conn, &cte, vec![libsql::Value::Text(start.clone())]).await;
        let from_rust = resolve(&branches, start);
        let same = from_sql == from_rust;
        if !same {
            disagreements += 1;
            println!("  {start:>4}: DIFFERS\n    sql  {from_sql:?}\n    rust {from_rust:?}");
        }
    }
    println!(
        "  {} lineages compared, {} disagreements",
        chain.len(),
        disagreements
    );

    println!("\n=== 3. the ancestry alone: recursive against bound ===\n");
    for depth in [1_usize, 4, 16] {
        let start = chain[depth].clone();
        let rust = resolve(&branches, &start);

        let cte = format!(
            "WITH RECURSIVE {} SELECT COUNT(*) FROM lineage",
            ancestry_recursive(1)
        );
        let rec = best_of(200, || {
            one_row(&conn, &cte, vec![libsql::Value::Text(start.clone())])
        })
        .await;

        let vals = format!(
            "WITH {} SELECT COUNT(*) FROM lineage",
            ancestry_values(&rust, 1)
        );
        let bound = best_of(200, || one_row(&conn, &vals, params_for(&rust))).await;

        println!(
            "  depth {depth:>2} ({} rows):  recursive {:>7.2} µs   bound {:>7.2} µs   {:+.1}%",
            rust.len(),
            rec.as_secs_f64() * 1e6,
            bound.as_secs_f64() * 1e6,
            (bound.as_secs_f64() / rec.as_secs_f64() - 1.0) * 100.0,
        );
    }

    println!("\n=== 4. joined the way the readers join it ===\n");
    // `churned_cte`'s shape: the ancestry joined against `links_current` to
    // find the ancestors' post-cutoff beliefs. This is where the CTE's cost
    // either disappears into the join or does not.
    for depth in [1_usize, 2, 4, 6, 8, 12, 16] {
        let start = chain[depth].clone();
        let rust = resolve(&branches, &start);
        let body = "SELECT COUNT(*) FROM links_current lc JOIN lineage g \
                    ON lc.branch_id = g.branch_id \
                    WHERE g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff";

        let cte = format!("WITH RECURSIVE {} {body}", ancestry_recursive(1));
        let rec = best_of(120, || {
            one_row(&conn, &cte, vec![libsql::Value::Text(start.clone())])
        })
        .await;

        let vals = format!("WITH {} {body}", ancestry_values(&rust, 1));
        let bound = best_of(120, || one_row(&conn, &vals, params_for(&rust))).await;

        println!(
            "  depth {depth:>2}:  recursive {:>7.2} µs   bound {:>7.2} µs   {:+.1}%",
            rec.as_secs_f64() * 1e6,
            bound.as_secs_f64() * 1e6,
            (bound.as_secs_f64() / rec.as_secs_f64() - 1.0) * 100.0,
        );
    }

    println!("\n=== 7. is the slope the parameters? dist bound against dist literal ===\n");
    for depth in [1_usize, 4, 8, 16] {
        let start = chain[depth].clone();
        let rust = resolve(&branches, &start);
        let body = "SELECT COUNT(*) FROM links_current lc JOIN lineage g \
                    ON lc.branch_id = g.branch_id \
                    WHERE g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff";

        let three = format!("WITH {} {body}", ancestry_values(&rust, 1));
        let p3 = best_of(120, || one_row(&conn, &three, params_for(&rust))).await;

        let two = format!("WITH {} {body}", ancestry_values_lit_dist(&rust, 1));
        let p2 = best_of(120, || one_row(&conn, &two, params_lit_dist(&rust))).await;

        println!(
            "  depth {depth:>2}:  3/row {:>7.2} \u{00b5}s ({:>2} params)   2/row {:>7.2} \u{00b5}s ({:>2} params)   {:+.1}%",
            p3.as_secs_f64() * 1e6,
            rust.len() * 3,
            p2.as_secs_f64() * 1e6,
            rust.len() * 2,
            (p2.as_secs_f64() / p3.as_secs_f64() - 1.0) * 100.0,
        );
    }

    println!("\n=== 5. the read-side round trip: aggregates against rows ===\n");
    // What `lineage_shape` asks today.
    let aggregates = "SELECT (SELECT COUNT(*) FROM branches), \
                             (SELECT COUNT(*) FROM branches WHERE branch_id = ?1), \
                             (SELECT COUNT(*) FROM branches \
                               WHERE branch_id = ?1 AND parent_id IS NULL)";
    let agg = best_of(300, || {
        one_row(&conn, aggregates, vec![libsql::Value::Text("b16".into())])
    })
    .await;

    // What resolving in Rust needs instead: the rows themselves.
    let rows_sql = "SELECT branch_id, parent_id, forked_at FROM branches";
    let rows = best_of(300, || async {
        let mut r = conn.query(rows_sql, ()).await.expect("rows");
        let mut n = 0;
        while r.next().await.expect("row").is_some() {
            n += 1;
        }
        assert_eq!(n, 17, "17 lineages");
    })
    .await;

    println!(
        "  three aggregates : {:>7.2} µs\n  17 rows loaded   : {:>7.2} µs   {:+.1}%",
        agg.as_secs_f64() * 1e6,
        rows.as_secs_f64() * 1e6,
        (rows.as_secs_f64() / agg.as_secs_f64() - 1.0) * 100.0,
    );

    println!("\n=== 6. the running minimum, on the shape that needs it ===\n");
    // A fork cut from a branch that was itself cut *later* than its own parent:
    // the grandparent's window must stay the narrower of the two, not widen to
    // the child's later fork point. This is the clamp `ancestry_cte` carries and
    // the one a Rust walk would silently drop.
    let db2dir = tempfile::tempdir().expect("tempdir2");
    let db2 = Database::open(db2dir.path().join("clamp.db"))
        .await
        .expect("open2");
    db2.write_concepts(vec![ConceptUpsert::new("x", "N").valid_from(T0)])
        .await
        .expect("w");
    db2.fork(
        BranchId::new("early").expect("n"),
        BranchId::new("main").expect("p"),
    )
    .await
    .expect("fork early");
    // `late` forks from `early` after `early` forked from `main`, so walking up
    // from `late` sees fork points in increasing order and the running minimum
    // must keep the *earliest*.
    db2.fork(
        BranchId::new("late").expect("n"),
        BranchId::new("early").expect("p"),
    )
    .await
    .expect("fork late");

    let b2 = db2.branches().await.expect("branches2");
    let conn2 = db2.diagnostic_conn().await.expect("diag2");
    let cte = format!(
        "WITH RECURSIVE {} SELECT branch_id, dist, cutoff FROM lineage",
        ancestry_recursive(1)
    );
    let from_sql = read_ancestry(&conn2, &cte, vec![libsql::Value::Text("late".into())]).await;
    let from_rust = resolve(&b2, "late");
    println!("  sql  {from_sql:?}");
    println!("  rust {from_rust:?}");
    println!(
        "  {}",
        if from_sql == from_rust {
            "identical -- the clamp is reproduced"
        } else {
            "DIFFERS -- the clamp is not reproduced"
        }
    );

    db.close().await.expect("close");
    db2.close().await.expect("close2");
}
