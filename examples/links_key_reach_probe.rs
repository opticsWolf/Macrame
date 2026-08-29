//! W12.15 — is the `links` primary key gap reachable through the crate, and
//! what does reaching it cost?
//!
//! §15.4 carries one unshipped item: **`links` is not keyed by lineage** while
//! `links_current` is. Two lineages asserting one edge key at one `recorded_at`
//! collide on `PRIMARY KEY (source_id, target_id, edge_type, valid_from,
//! recorded_at)` with a bare `UNIQUE constraint failed: links.` The plan calls
//! it "unreachable through the crate until branch-scoped writes exist, which is
//! 0.14.5", and leaves it to a later rung "to widen or to decline in writing".
//!
//! Deciding either way needs an answer this probe measures rather than argues:
//!
//! 1. **Is it reachable?** The clock contract says successive calls return
//!    strictly increasing values, so a caller making two sequential assertions
//!    cannot collide. But the batch paths take **one stamp for the whole
//!    batch** (D-014) — and `reject_overlaps_within` groups by
//!    `(source, target, edge_type, branch)`, so a pair differing only in
//!    lineage is not an overlap and is passed straight through to the insert.
//!    **Measured at v14, before the rung:** both batch surfaces collide, and
//!    the caller is handed `engine: SQLite failure: UNIQUE constraint failed:
//!    links.source_id, links.target_id, links.edge_type, links.valid_from,
//!    links.recorded_at` — raw engine text about a key, for a write that named
//!    two lineages and one edge. Against a v15 build the same section reports
//!    `Ok(2)` twice, which is what makes it a regression probe as well as a
//!    reproduction.
//! 2. **What does the caller see?** Nothing typed. The collision reaches
//!    `classify` as an ordinary `UNIQUE` failure, which is a second reason to
//!    widen rather than to refuse: a refusal would have needed a name.
//! 3. **What does the rebuild cost?** Widening the key rebuilds the ledger's
//!    largest table, which is what makes this its own release rather than a
//!    line in an index rung. Section 4 prices the rung at several table sizes.
//!
//! 4. **Where does `branch_id` go?** Appending it keeps the autoindex's
//!    leading columns and so keeps every plan that seeks on them; leading with
//!    it re-plans the archive sweep. Section 5 reads the plans rather than
//!    reasoning about them, because §15.3 made exactly this call by reasoning
//!    once and D-231 had to unmake it.
//!
//! Sections 1–3 run against the shipped v14 schema. Section 4 rebuilds a copy
//! of `links` in the shape the widened key would have, so the number is the
//! rung's own cost rather than a guess at it.

use std::time::Instant;

use macrame::branch::BranchId;
use macrame::graph::EdgeAssertion;
use macrame::schema::ddl;
use macrame::{ConceptUpsert, Database};

const TS: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// Sizes for the rebuild timing. The largest is what §9 calls a working
/// ledger; the rung has to be tolerable there, not only on a toy.
const SIZES: &[usize] = &[1_000, 10_000, 50_000];

fn bid(s: &str) -> BranchId {
    BranchId::new(s).unwrap()
}

/// A database with two concepts and one fork off the trunk.
async fn seeded(dir: &tempfile::TempDir, name: &str) -> Database {
    let db = Database::open_with_cadence(&dir.path().join(name), None)
        .await
        .unwrap();
    db.write_concepts(vec![
        ConceptUpsert::new("a", "n").valid_from(TS),
        ConceptUpsert::new("b", "n").valid_from(TS),
    ])
    .await
    .unwrap();
    db.fork(bid("b1"), bid(ddl::MAIN_BRANCH)).await.unwrap();
    db
}

fn edge(branch: Option<&str>) -> EdgeAssertion {
    let e = EdgeAssertion::new("a", "b", "LINKS")
        .valid_from(TS)
        .valid_to(FOREVER);
    match branch {
        Some(b) => e.on_branch(bid(b)),
        None => e,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// 1. Reachability
// ───────────────────────────────────────────────────────────────────────────

/// Two lineages, one edge key, **two** calls — so two stamps.
async fn sequential(dir: &tempfile::TempDir) {
    let db = seeded(dir, "seq.db").await;

    let first = db.assert_edge(edge(None)).await;
    let second = db.assert_edge(edge(Some("b1"))).await;

    println!("  trunk assertion      : {}", outcome(&first));
    println!("  branch assertion     : {}", outcome(&second));
    println!(
        "  -> the clock's strictly-increasing contract separates the two \
         stamps, so the key does not collide."
    );
}

/// Two lineages, one edge key, **one** batch — so one stamp (D-014).
async fn one_batch(dir: &tempfile::TempDir) {
    let db = seeded(dir, "batch.db").await;

    let res = db
        .write_bulk_atomic(vec![edge(None), edge(Some("b1"))])
        .await;

    match &res {
        Ok(n) => println!("  write_bulk_atomic    : Ok({n}) — both lineages' rows landed (v15)"),
        Err(e) => println!("  write_bulk_atomic    : COLLIDES — the caller sees: {e}"),
    }
}

/// The chunked path takes a stamp per chunk, so a pair inside one chunk shares
/// it exactly as the atomic path does.
async fn one_chunk(dir: &tempfile::TempDir) {
    let db = seeded(dir, "chunk.db").await;

    let res = db.bulk_import(vec![edge(None), edge(Some("b1"))]).await;
    match &res {
        Ok(n) => println!("  bulk_import          : Ok({n}) — both landed (v15)"),
        Err(e) => println!("  bulk_import          : COLLIDES — {e}"),
    }
}

/// Retirement writes a row too, and on a branch it is a *shadow* retirement:
/// the branch's own row at the ancestor's key. Its stamp comes from the same
/// clock, one per actor turn — so this arm is here to show which paths can and
/// cannot share one.
async fn shadow_retire(dir: &tempfile::TempDir) {
    // Forked *after* the trunk edge, or the fork cutoff puts it out of the
    // branch's sight and there is nothing to shadow-retire.
    let db = Database::open_with_cadence(&dir.path().join("retire.db"), None)
        .await
        .unwrap();
    db.write_concepts(vec![
        ConceptUpsert::new("a", "n").valid_from(TS),
        ConceptUpsert::new("b", "n").valid_from(TS),
    ])
    .await
    .unwrap();
    db.assert_edge(edge(None)).await.unwrap();
    db.fork(bid("b1"), bid(ddl::MAIN_BRANCH)).await.unwrap();

    // Two turns, two stamps: the branch retires what it inherited, then the
    // trunk retires its own. Both write to `links` at the same edge key.
    let a = db
        .retire_edge_on("a", "b", "LINKS", TS, TS, bid("b1"))
        .await;
    println!("  branch retirement    : {}", outcome(&a));
}

fn outcome<T: std::fmt::Debug>(r: &Result<T, macrame::DbError>) -> String {
    match r {
        Ok(v) => format!("Ok({v:?})"),
        Err(e) => format!("{e}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// 4. What the rung would cost
// ───────────────────────────────────────────────────────────────────────────

/// Rebuild `links` under the widened key, on `n` rows, and report the wall time.
///
/// The recipe is the one D-083 fixed and D-119 needed: a `links_new` created
/// from **pinned text** rather than from `ddl`, an `INSERT … SELECT`, a drop
/// and a rename, with foreign keys suspended because `links` is referenced.
/// Run here against a copy so the probe measures the statement sequence at
/// realistic table sizes without needing the rung to exist.
async fn rebuild_cost(n: usize) -> (f64, f64) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open_with_cadence(&dir.path().join("r.db"), None)
        .await
        .unwrap();

    let ids: Vec<String> = (0..n).map(|i| format!("n{i:07}")).collect();
    for chunk in ids.chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id, "n").valid_from(TS))
                .collect(),
        )
        .await
        .unwrap();
    }

    let edges: Vec<EdgeAssertion> = (0..n)
        .map(|i| {
            EdgeAssertion::new(&ids[i], &ids[(i + 1) % n], "LINKS")
                .valid_from(TS)
                .valid_to(FOREVER)
        })
        .collect();
    db.bulk_import(edges).await.unwrap();

    // `read_conn()` is read-only, and this section writes. A second local
    // connection to the same file is what the rung itself would run on.
    let path = dir.path().join("r.db");
    drop(db);
    let raw = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    let rows: u64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();

    // The rung, statement for statement, on a scratch copy of the table so the
    // live one is left alone and the probe can be re-run.
    let t = Instant::now();
    conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
    conn.execute(LINKS_NEW_PINNED, ()).await.unwrap();
    conn.execute(
        "INSERT INTO links_probe_new \
         (source_id, target_id, edge_type, valid_from, recorded_at, valid_to, \
          weight, properties, branch_id) \
         SELECT source_id, target_id, edge_type, valid_from, recorded_at, valid_to, \
          weight, properties, branch_id FROM links",
        (),
    )
    .await
    .unwrap();
    let copied = t.elapsed().as_secs_f64() * 1e3;
    conn.execute("DROP TABLE links_probe_new", ())
        .await
        .unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();

    (rows as f64, copied)
}

// ───────────────────────────────────────────────────────────────────────────
// 5. Where `branch_id` goes in the key
// ───────────────────────────────────────────────────────────────────────────

/// The three candidate keys. The autoindex is the only index over four of this
/// table's columns, so its column order *is* the access path for everything
/// below.
const KEYS: &[(&str, &str)] = &[
    (
        "v14 (no lineage)",
        "(source_id, target_id, edge_type, valid_from, recorded_at)",
    ),
    (
        "appended (v15)",
        "(source_id, target_id, edge_type, valid_from, recorded_at, branch_id)",
    ),
    (
        "branch-leading",
        "(branch_id, source_id, target_id, edge_type, valid_from, recorded_at)",
    ),
];

/// The reads that seek `links` on its key, taken from the code that runs them.
const READS: &[(&str, &str)] = &[
    (
        "archive sweep (LINKS_ARCHIVABLE's outer scan)",
        "SELECT source_id FROM links WHERE recorded_at < ?1 \
         AND (valid_to <> '9999-12-31T23:59:59.999999Z' AND valid_to <= ?1)",
    ),
    (
        "the supersession probe (its inner EXISTS)",
        "SELECT 1 FROM links newer WHERE newer.source_id = ?1 \
         AND newer.target_id = ?2 AND newer.edge_type = ?3 \
         AND newer.valid_from = ?4 AND newer.recorded_at > ?5",
    ),
    (
        "abandonment (archive_branch)",
        "SELECT source_id FROM links WHERE branch_id = ?1",
    ),
];

/// The plan for `sql` on a `links` built with `key`, as one line.
async fn plan_under(key: &str, sql: &str) -> String {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        &format!(
            "CREATE TABLE links (
                source_id TEXT NOT NULL, target_id TEXT NOT NULL,
                edge_type TEXT NOT NULL, valid_from TEXT NOT NULL,
                recorded_at TEXT NOT NULL, valid_to TEXT NOT NULL,
                weight REAL NOT NULL, properties TEXT NOT NULL,
                branch_id TEXT NOT NULL DEFAULT 'main',
                PRIMARY KEY {key}
            )"
        ),
        (),
    )
    .await
    .unwrap();
    // The two indices v11 added, or the archive sweep's plan is a statement
    // about a schema this crate has not shipped since 0.12.
    conn.execute(
        "CREATE INDEX idx_links_recorded_at ON links (recorded_at)",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE INDEX idx_links_target ON links (target_id)", ())
        .await
        .unwrap();

    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(3).unwrap());
    }
    out.join(" | ")
}

async fn key_placement() {
    for (label, sql) in READS {
        println!("  {label}");
        for (name, key) in KEYS {
            println!("    {name:<18} {}", plan_under(key, sql).await);
        }
        println!();
    }
}

/// The widened shape, written out. Not `ddl::CREATE_LINKS_TABLE` with a
/// substitution: D-083's rule is that a rebuild pins its own text, and a probe
/// that reads the shape from `ddl` would measure whatever `ddl` becomes.
const LINKS_NEW_PINNED: &str = "\
CREATE TABLE links_probe_new (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    branch_id   TEXT NOT NULL DEFAULT 'main',
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at, branch_id)
);";

#[tokio::main]
async fn main() {
    println!("== 1. two lineages, one edge key, two write shapes ==\n");

    let dir = tempfile::TempDir::new().unwrap();
    sequential(&dir).await;
    println!();
    one_batch(&dir).await;
    println!();
    one_chunk(&dir).await;
    println!();
    shadow_retire(&dir).await;

    println!("\n== 5. where `branch_id` goes in the key ==\n");
    key_placement().await;

    println!("== 4. what the rebuild would cost ==\n");
    println!("  {:>8}  {:>10}  {:>12}", "edges", "links rows", "copy ms");
    for &n in SIZES {
        let (rows, ms) = rebuild_cost(n).await;
        println!("  {n:>8}  {rows:>10.0}  {ms:>12.1}");
    }
}
