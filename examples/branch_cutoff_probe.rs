//! W12.6, step 0: does a branch stop reading its parent at the fork point?
//!
//! It did not. 0.14.4 shipped a reader that resolves *which lineage holds an
//! edge* — nearest ancestor wins, [D-220] — and never looks at
//! `branches.forked_at` at all. [§15.3] says the opposite in as many words:
//!
//! > A read on branch B resolves B's ancestry: rows written on B, plus rows
//! > written on each ancestor A **before the fork point on the path down from
//! > A**. Copy-on-write.
//!
//! and `schema::ddl`'s own comment on the column says it is *"in the
//! `recorded_at` domain — the transaction-time instant the lineage diverged,
//! which is what §15.3's visibility cutoffs are computed over."* Nothing
//! computed them. §1 is that measurement.
//!
//! # The dependency map, because the repair is not where the symptom is
//!
//! §1 shows a branch absorbing a trunk write made after it forked. The obvious
//! reading is that a `WHERE recorded_at <= cutoff` is missing from the read.
//! **§2 is the load-bearing section and it says that reading is wrong**, so the
//! order here is: symptom, then the constraint that rules out the obvious fix,
//! then the forms compared, then the cost.
//!
//! 1. **`trg_links_current_sync`** is `ON CONFLICT (…) DO UPDATE SET … ,
//!    recorded_at = excluded.recorded_at`. So `links_current` holds exactly one
//!    belief per `(edge key, lineage)` and it is always the newest one.
//! 2. Therefore, once an ancestor **churns** an edge after a fork, the version
//!    the branch inherited is **not in the projection at all**. `links_current`
//!    answers *current as of now*; it structurally cannot answer *current as of
//!    t*.
//! 3. Therefore a `recorded_at <= cutoff` predicate over `links_current` does
//!    not restore the inherited edge — it **removes** it. §3 measures that as a
//!    lost subtree, which is a quieter wrong answer than the one it replaces.
//! 4. Therefore the pre-fork version has exactly one home: `transaction_log`.
//!    The repair is a hybrid — the projection for keys the ancestors have not
//!    touched, a bounded fold for the keys they have — and its cost is a
//!    function of **post-fork churn**, not of history size. §4 prices it.
//!
//! Every "just add the filter" instinct fails at step 3, and it fails on a
//! different churn kind than the one the symptom is usually noticed on, which
//! is why §3 varies the kind rather than only the read.
//!
//! Run with:  cargo run --release --example branch_cutoff_probe
//!
//! [D-220]: ../docs/architecture/s13-decision-register.md
//! [§15.3]: ../docs/Macrame%20Road%20to%201.0.md

use std::time::{Duration, Instant};

use macrame::graph::TraversalBuilder;

/// When the trunk's own graph is written.
const TS: &str = "2026-01-01T00:00:00.000000Z";
/// When the branch forks. Everything the trunk writes after this is post-fork.
const FORK: &str = "2026-02-01T00:00:00.000000Z";
/// When the trunk churns.
const CHURN: &str = "2026-05-01T00:00:00.000000Z";
/// When every read below happens.
const NOW: &str = "2026-06-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";
/// The lineage every branched read below is on.
const BRANCH: &str = "b";

/// Fan-out per layer and layers, for §4's timing fixture.
///
/// 10⁴ gives 11,111 nodes and 11,110 edges, which is the smallest tree that
/// lets the churn axis run to 10,000 post-fork writes without churning every
/// edge in it — the point of the axis is the *fraction*, and a fixture where
/// the top of the range is 100% cannot show the curve bending.
const WIDTH: usize = 10;
const DEPTH: usize = 4;

/// Best-of, after a discarded warm-up. See `branch_traversal_probe`'s note: a
/// fresh database pays for the page cache on the first query, and whichever
/// form runs first absorbs it.
const REPEATS: usize = 15;

// ───────────────────────────────────────────────────────────────────────────
// The forms
// ───────────────────────────────────────────────────────────────────────────

/// **0.14.4's branched read**, restated because the crate no longer emits it.
///
/// Nearest-ancestor resolution with no cutoff anywhere: the ancestry carries
/// `dist` and nothing else, and every ancestor row is a candidate however
/// recently it was written. This is the only form in this file that is a
/// restatement rather than the shipped query — everything else comes out of
/// `TraversalBuilder::build_sql`, so a probe that drifts from the crate drifts
/// only in the column that is *supposed* to be historical.
const NO_CUTOFF: &str = r#"
WITH RECURSIVE lineage(branch_id, dist) AS (
    SELECT ?5, 0
    UNION ALL
    SELECT b.parent_id, g.dist + 1
    FROM branches b JOIN lineage g ON b.branch_id = g.branch_id
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
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN visible l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
)
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id
"#;

/// **The instinct §2 rules out**: the cutoff as a predicate on the projection.
///
/// Identical to `NO_CUTOFF` but for the ancestry carrying `cutoff` and the
/// `visible` reduction dropping any ancestor row recorded after it. It is the
/// change a reader of §15.3 would make, it is one line, and §3 measures what it
/// does to a retired edge.
const NAIVE_FILTER: &str = r#"
WITH RECURSIVE lineage(branch_id, dist, cutoff) AS (
    SELECT ?5, 0, NULL
    UNION ALL
    SELECT b.parent_id, g.dist + 1,
           CASE WHEN g.cutoff IS NULL OR b.forked_at < g.cutoff
                THEN b.forked_at ELSE g.cutoff END
    FROM branches b JOIN lineage g ON b.branch_id = g.branch_id
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
        WHERE g.cutoff IS NULL OR l.recorded_at <= g.cutoff
    ) WHERE rn = 1
),
walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN visible l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
)
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id
"#;

/// The shipped branched read, taken from the builder rather than copied.
fn shipped(depth: usize) -> String {
    TraversalBuilder::new("n0")
        .max_depth(depth)
        .on_branch(BRANCH)
        .build_sql()
}

/// The shipped *unbranched* read — `LineageShape::Trunk`, no resolution at all.
fn plain(depth: usize) -> String {
    TraversalBuilder::new("n0").max_depth(depth).build_sql()
}

fn branched_params(depth: usize) -> Vec<libsql::Value> {
    vec![
        "n0".into(),
        (depth as i64).into(),
        NOW.into(),
        0.0f64.into(),
        BRANCH.into(),
    ]
}

fn plain_params(depth: usize) -> Vec<libsql::Value> {
    vec![
        "n0".into(),
        (depth as i64).into(),
        NOW.into(),
        0.0f64.into(),
    ]
}

// ───────────────────────────────────────────────────────────────────────────
// Fixture
// ───────────────────────────────────────────────────────────────────────────

async fn fresh() -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    macrame::schema::run_migrations(&conn).await.unwrap();
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES (?1, 'main', ?2, ?2)",
        libsql::params![BRANCH, FORK],
    )
    .await
    .unwrap();
    conn
}

async fn concept(conn: &libsql::Connection, id: &str, at: &str) {
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'x', ?2, ?2)",
        libsql::params![id, at],
    )
    .await
    .unwrap();
}

/// One assertion, written to `links` so the sync and log triggers both fire.
///
/// That is the whole point of this probe: §2's finding is a property of the
/// sync trigger, and a fixture that wrote `links_current` directly — which
/// `branch_traversal_probe` does, correctly, because it measures plans — would
/// have no `transaction_log` to fold and no `DO UPDATE` to observe.
async fn assert_edge(
    conn: &libsql::Connection,
    source: &str,
    target: &str,
    branch: &str,
    valid_to: &str,
    weight: f64,
    at: &str,
) {
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at, branch_id) \
         VALUES (?1, ?2, 'LINKS', ?3, ?4, ?5, '{}', ?6, ?7)",
        libsql::params![source, target, TS, valid_to, weight, at, branch],
    )
    .await
    .unwrap();
}

/// The chain `a → b → c → d` on the trunk, all recorded before the fork.
async fn small(conn: &libsql::Connection) {
    for id in ["n0", "n1", "n2", "n3"] {
        concept(conn, id, TS).await;
    }
    for (s, t) in [("n0", "n1"), ("n1", "n2"), ("n2", "n3")] {
        assert_edge(conn, s, t, "main", FOREVER, 1.0, TS).await;
    }
}

async fn ids(conn: &libsql::Connection, sql: &str, params: Vec<libsql::Value>) -> Vec<String> {
    let mut rows = conn.query(sql, params).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get(0).unwrap());
    }
    out
}

async fn scalar(
    conn: &libsql::Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> i64 {
    conn.query(sql, params)
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

async fn best_of(conn: &libsql::Connection, sql: &str, params: Vec<libsql::Value>) -> Duration {
    let _ = ids(conn, sql, params.clone()).await;
    let mut best = Duration::MAX;
    for _ in 0..REPEATS {
        let t = Instant::now();
        let _ = ids(conn, sql, params.clone()).await;
        best = best.min(t.elapsed());
    }
    best
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

// ───────────────────────────────────────────────────────────────────────────
// §1 — the finding
// ───────────────────────────────────────────────────────────────────────────

async fn section_1() {
    println!("\n§1  Does a branch stop reading its parent at the fork point?");
    println!("    b forks from main on {FORK}. The trunk then records a new");
    println!("    edge on {CHURN}, which is after it.\n");

    let conn = fresh().await;
    small(&conn).await;
    concept(&conn, "n4", CHURN).await;
    assert_edge(&conn, "n3", "n4", "main", FOREVER, 1.0, CHURN).await;

    let p = branched_params(5);
    println!(
        "    trunk                        {:?}",
        ids(&conn, &plain(5), plain_params(5)).await
    );
    println!(
        "    b, 0.14.4 (no cutoff)        {:?}",
        ids(&conn, NO_CUTOFF, p.clone()).await
    );
    println!(
        "    b, 0.14.6 (shipped)          {:?}",
        ids(&conn, &shipped(5), p.clone()).await
    );
    let leaked = ids(&conn, NO_CUTOFF, p).await.iter().any(|n| n == "n4");
    println!("\n    0.14.4 absorbed the post-fork trunk write: {leaked}");
}

// ───────────────────────────────────────────────────────────────────────────
// §2 — why it is not a `WHERE` clause  (the load-bearing section)
// ───────────────────────────────────────────────────────────────────────────

async fn section_2() {
    println!("\n§2  Where the pre-fork version lives after the trunk churns it.");
    println!("    The trunk retires n1 → n2 on {CHURN}, after b forked.\n");

    let conn = fresh().await;
    small(&conn).await;
    assert_edge(&conn, "n1", "n2", "main", CHURN, 1.0, CHURN).await;

    const KEY: &str = "source_id = 'n1' AND target_id = 'n2' AND branch_id = 'main'";
    let rows = scalar(
        &conn,
        &format!("SELECT COUNT(*) FROM links_current WHERE {KEY}"),
        (),
    )
    .await;
    let survives = scalar(
        &conn,
        &format!("SELECT COUNT(*) FROM links_current WHERE {KEY} AND recorded_at <= ?1"),
        libsql::params![FORK],
    )
    .await;
    let open = scalar(
        &conn,
        &format!("SELECT COUNT(*) FROM links_current WHERE {KEY} AND valid_to = ?1"),
        libsql::params![FOREVER],
    )
    .await;
    let ledger = scalar(
        &conn,
        &format!("SELECT COUNT(*) FROM links WHERE {KEY}"),
        (),
    )
    .await;
    let logged = scalar(
        &conn,
        "SELECT COUNT(*) FROM transaction_log \
         WHERE table_name = 'links' AND entity_id LIKE 'n1|n2|%' \
           AND branch_id = 'main' AND recorded_at <= ?1",
        libsql::params![FORK],
    )
    .await;

    println!("    links_current rows for the key on main             {rows}");
    println!("    ... of those, recorded at or before the fork       {survives}");
    println!("    ... of those, still open                           {open}");
    println!("    links (append-only) rows for the key               {ledger}");
    println!("    transaction_log entries at or before the fork      {logged}");
    println!();
    println!("    The projection keeps one belief per key per lineage and the");
    println!("    sync trigger's DO UPDATE carries recorded_at forward, so a");
    println!("    `recorded_at <= cutoff` filter over it returns {survives} rows for an");
    println!("    edge the branch is entitled to see. The pre-fork belief is in");
    println!(
        "    the log ({logged} entr{}), and nowhere else.",
        if logged == 1 { "y" } else { "ies" }
    );
}

// ───────────────────────────────────────────────────────────────────────────
// §3 — the three forms against the four churn kinds
// ───────────────────────────────────────────────────────────────────────────

/// What the trunk does to its own graph after `b` has forked.
#[derive(Clone, Copy)]
enum Churn {
    /// A brand-new edge, extending the chain.
    NewEdge,
    /// The inherited `n1 → n2`, at a tenth of its weight.
    Reweight,
    /// The inherited `n1 → n2`, closed.
    Retire,
    /// A key first asserted after the fork, then corrected — still after it.
    TwicePostFork,
}

impl Churn {
    fn label(self) -> &'static str {
        match self {
            Churn::NewEdge => "new edge",
            Churn::Reweight => "reweight",
            Churn::Retire => "retire",
            Churn::TwicePostFork => "twice, post-fork",
        }
    }

    /// What `b` must reach. The trunk's chain is `n0 → n1 → n2 → n3`, and `b`
    /// inherits all of it because every trunk write above happened before the
    /// fork; the churn is what must not reach it.
    fn expected(self) -> &'static [&'static str] {
        &["n0", "n1", "n2", "n3"]
    }

    /// The weight floor the read is taken under.
    ///
    /// `Reweight` needs one and the others do not, which is a finding in its
    /// own right: with an unfiltered read the trunk's correction changes no
    /// node's reachability, so **every** form scores `ok` and the row says
    /// nothing. A reweight is only observable through a predicate that the two
    /// weights fall on opposite sides of. That is the shape of the kind: it is
    /// the churn least likely to be noticed, because the wrong answer is a
    /// plausible number rather than a missing node.
    fn floor(self) -> f64 {
        match self {
            Churn::Reweight => 0.5,
            _ => 0.0,
        }
    }

    async fn apply(self, conn: &libsql::Connection) {
        match self {
            Churn::NewEdge => {
                concept(conn, "n4", CHURN).await;
                assert_edge(conn, "n3", "n4", "main", FOREVER, 1.0, CHURN).await;
            }
            Churn::Reweight => assert_edge(conn, "n1", "n2", "main", FOREVER, 0.1, CHURN).await,
            Churn::Retire => assert_edge(conn, "n1", "n2", "main", CHURN, 1.0, CHURN).await,
            Churn::TwicePostFork => {
                concept(conn, "n4", CHURN).await;
                assert_edge(conn, "n0", "n4", "main", FOREVER, 1.0, CHURN).await;
                assert_edge(
                    conn,
                    "n0",
                    "n4",
                    "main",
                    FOREVER,
                    0.2,
                    "2026-05-02T00:00:00.000000Z",
                )
                .await;
            }
        }
    }
}

async fn section_3() {
    println!("\n§3  Three forms, four churn kinds. `ok` is what b must reach.\n");
    println!(
        "    {:<18} {:<6} {:<10} {:<10} {:<10}",
        "churn on main", "floor", "0.14.4", "naive", "shipped"
    );

    for kind in [
        Churn::NewEdge,
        Churn::Reweight,
        Churn::Retire,
        Churn::TwicePostFork,
    ] {
        let conn = fresh().await;
        small(&conn).await;
        kind.apply(&conn).await;

        let want: Vec<String> = kind.expected().iter().map(|s| s.to_string()).collect();
        let mut params = branched_params(5);
        params[3] = kind.floor().into();
        let mut cells = Vec::new();
        for sql in [NO_CUTOFF, NAIVE_FILTER, &shipped(5)] {
            let got = ids(&conn, sql, params.clone()).await;
            cells.push(if got == want {
                "ok".to_string()
            } else {
                format!("{} nodes", got.len())
            });
        }
        println!(
            "    {:<18} {:<6} {:<10} {:<10} {:<10}",
            kind.label(),
            kind.floor(),
            cells[0],
            cells[1],
            cells[2]
        );
    }

    println!();
    println!("    Neither historical form is right on all four, and they are");
    println!("    wrong on complementary kinds: 0.14.4 admits post-fork writes,");
    println!("    the naive filter removes edges it should have restored.");
    println!("    `retire` is the row to look at — both forms lose n2 *and* n3,");
    println!("    so the wrong answer is a missing subtree rather than a spare");
    println!("    node, and nothing in the result says a subtree went missing.");
}

// ───────────────────────────────────────────────────────────────────────────
// §4 — what the repair costs, as a function of post-fork churn
// ───────────────────────────────────────────────────────────────────────────

/// A tree of fan-out `WIDTH` and height `DEPTH`, all on the trunk before the
/// fork. Returns every edge as `(source, target)`, in insertion order.
async fn build_tree(conn: &libsql::Connection) -> Vec<(String, String)> {
    let tx = conn.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('n0', 'r', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    let mut frontier = vec!["n0".to_string()];
    let mut next_id = 1usize;
    let mut edges = Vec::new();
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
                tx.execute(
                    "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
                     valid_to, weight, properties, recorded_at, branch_id) \
                     VALUES (?1, ?2, 'LINKS', ?3, ?4, 1.0, '{}', ?3, 'main')",
                    libsql::params![parent.as_str(), id.as_str(), TS, FOREVER],
                )
                .await
                .unwrap();
                edges.push((parent.clone(), id.clone()));
                next.push(id);
            }
        }
        frontier = next;
    }
    tx.commit().await.unwrap();
    edges
}

/// Reweight the first `n` trunk edges, all after the fork.
async fn churn(conn: &libsql::Connection, edges: &[(String, String)], n: usize) {
    let tx = conn.transaction().await.unwrap();
    for (i, (source, target)) in edges.iter().take(n).enumerate() {
        tx.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
             valid_to, weight, properties, recorded_at, branch_id) \
             VALUES (?1, ?2, 'LINKS', ?3, ?4, 0.9, '{}', ?5, 'main')",
            libsql::params![
                source.as_str(),
                target.as_str(),
                TS,
                FOREVER,
                format!("2026-05-01T00:00:00.{i:06}Z")
            ],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn section_4() {
    println!("\n§4  Branch-read cost against post-fork trunk churn.");
    println!(
        "    {WIDTH}^{DEPTH} tree on the trunk, one long-lived branch, reads at depth {DEPTH}."
    );
    println!("    Best of {REPEATS} after a discarded warm-up.\n");
    println!(
        "    {:>8} {:>7} {:>10} {:>10} {:>10} {:>9} {:>9}",
        "churn", "frac", "plain", "0.14.4", "shipped", "vs plain", "vs .4"
    );

    for n in [0usize, 1_000, 10_000] {
        let conn = fresh().await;
        let edges = build_tree(&conn).await;
        let total = edges.len();
        let n = n.min(total);
        churn(&conn, &edges, n).await;

        let plain_ms = best_of(&conn, &plain(DEPTH), plain_params(DEPTH)).await;
        let old_ms = best_of(&conn, NO_CUTOFF, branched_params(DEPTH)).await;
        let new_ms = best_of(&conn, &shipped(DEPTH), branched_params(DEPTH)).await;

        // Every form must still reach the whole tree: the churn is a reweight,
        // so a form that changed the reachable set has a defect, not a cost.
        let reached = ids(&conn, &shipped(DEPTH), branched_params(DEPTH))
            .await
            .len();
        assert_eq!(
            reached,
            total + 1,
            "the shipped read lost nodes at churn {n}"
        );

        println!(
            "    {:>8} {:>6.0}% {:>9.3}ms {:>9.3}ms {:>9.3}ms {:>8.2}x {:>8.2}x",
            n,
            100.0 * n as f64 / total as f64,
            ms(plain_ms),
            ms(old_ms),
            ms(new_ms),
            ms(new_ms) / ms(plain_ms),
            ms(new_ms) / ms(old_ms),
        );
    }

    println!();
    println!("    `vs .4` is what the cutoff itself costs: the difference between");
    println!("    a resolved read and a resolved-and-bounded one, on the same");
    println!("    ledger. `vs plain` carries D-220's 3.02x resolution cost with");
    println!("    it and is the number a caller who has never forked would pay,");
    println!("    except that such a caller gets LineageShape::Trunk and pays 1x.");
}

// ───────────────────────────────────────────────────────────────────────────
// §5 — the index the fixed cost points at, priced rather than built
// ───────────────────────────────────────────────────────────────────────────

/// The seek `churned` wants and `links_current` does not have.
///
/// `churned` asks *which rows on these lineages were recorded after this
/// instant*. `links_current`'s two indices lead on `source_id`
/// ([D-042](../docs/architecture/s13-decision-register.md)), so there is
/// nothing for either half of that to seek on and the CTE scans the table —
/// which is what §4's cost at **zero** churn is. This is the shape that would
/// remove it.
const LC_LINEAGE_RECORDED: &str = "CREATE INDEX idx_lc_lineage_recorded \
     ON links_current (branch_id, recorded_at);";

async fn section_5() {
    println!("\n§5  What an index on (branch_id, recorded_at) would buy.");
    println!("    Measured, not shipped: an index is a schema rung and a write");
    println!("    cost on every assertion, and this one is priced here so the");
    println!("    v13 rung can take it on evidence or decline it in writing.\n");
    println!(
        "    {:>8} {:>7} {:>11} {:>11} {:>9}",
        "churn", "frac", "no index", "indexed", "saved"
    );

    for n in [0usize, 1_000, 10_000] {
        let conn = fresh().await;
        let edges = build_tree(&conn).await;
        let total = edges.len();
        let n = n.min(total);
        churn(&conn, &edges, n).await;
        conn.execute("ANALYZE", ()).await.unwrap();

        let before = best_of(&conn, &shipped(DEPTH), branched_params(DEPTH)).await;
        conn.execute(LC_LINEAGE_RECORDED, ()).await.unwrap();
        conn.execute("ANALYZE", ()).await.unwrap();
        let after = best_of(&conn, &shipped(DEPTH), branched_params(DEPTH)).await;

        println!(
            "    {:>8} {:>6.0}% {:>10.3}ms {:>10.3}ms {:>8.0}%",
            n,
            100.0 * n as f64 / total as f64,
            ms(before),
            ms(after),
            100.0 * (1.0 - ms(after) / ms(before)),
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// §6 — where the fixed cost is, from the plan rather than from a guess
// ───────────────────────────────────────────────────────────────────────────

async fn plan_of(conn: &libsql::Connection, sql: &str, params: Vec<libsql::Value>) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(3).unwrap());
    }
    out
}

/// §4 says the cutoff costs 1.4x at **zero** churn, where the fold arm returns
/// nothing at all. §5 says an index on the churn probe buys almost none of that
/// back, so the scan is not where it goes. This asks the planner instead.
async fn section_6() {
    println!("\n§6  The plans, for the two branched forms at zero churn.\n");

    let conn = fresh().await;
    let _ = build_tree(&conn).await;
    conn.execute("ANALYZE", ()).await.unwrap();

    for (label, sql) in [
        ("0.14.4", NO_CUTOFF.to_string()),
        ("shipped", shipped(DEPTH)),
    ] {
        println!("    {label}");
        for line in plan_of(&conn, &sql, branched_params(DEPTH)).await {
            println!("      {line}");
        }
        println!();
    }

    println!("    Three differences, and the row count is not one of them:");
    println!("    * `lineage` goes CO-ROUTINE -> MATERIALIZE, because three CTEs");
    println!("      now read it instead of one.");
    println!("    * the churn arm builds an AUTOMATIC COVERING INDEX over");
    println!("      links_current and a TEMP B-TREE for its window, and it does");
    println!("      that whether or not the arm yields a single row.");
    println!("    * `links_cut` is a COMPOUND QUERY, so the walk joins a");
    println!("      materialised relation rather than a table with indices.");
    println!();
    println!("    That is where §4's 1.4x at zero churn goes, and it is why §5's");
    println!("    index buys so little: the cost is the arm's machinery, not the");
    println!("    rows it scans. The lever is not an index — it is not emitting");
    println!("    the arm at all when a probe says nothing on the ancestry is");
    println!("    churned, which is D-220's two-shape choice one level down and");
    println!("    is exact for the same reason: an empty churn set makes the");
    println!("    naive filter and the hybrid return identical rows. Recorded");
    println!("    rather than built — see D-223.");
}

#[tokio::main]
async fn main() {
    println!("branch_cutoff_probe — W12.6, the fork point as a visibility cutoff");
    section_1().await;
    section_2().await;
    section_3().await;
    section_4().await;
    section_5().await;
    section_6().await;
}
