//! D-271's *other* open finding: `archive_branch` costs what the **trunk**
//! costs, not what the lineage being archived costs — so does an index leading
//! with `branch_id` earn its write cost?
//!
//! 0.15.28 measured `archive_branch_small_lineage` at two scales and found
//! **10.7 ms at a 2,000-edge trunk against 23.4 ms at 8,000**, on a lineage of
//! twenty rows either way, where the arm's own comment expected a figure flat
//! in the trunk. The diagnosis it recorded is a schema one: every
//! lineage-scoped statement in `archive_branch_session` filters on
//! `branch_id = ?`, and **no index on any of these tables leads with that
//! column** — `links`' primary key carries it last (D-232) and
//! `idx_txlog_fold_partition` carries it third — so each statement scans a
//! trunk-sized table and a twenty-row lineage pays the trunk's price.
//!
//! That entry stopped there, deliberately: *"whether an index leading with
//! `branch_id` earns its write cost is a decision with its own measurement and
//! is not taken here."* This probe is that measurement.
//!
//! # Why it is not obvious either way
//!
//! `branch_id` is the **lowest-cardinality column in the ledger**. Most rows
//! say `main`, and an index whose keys are almost all one value is the classic
//! shape a planner declines to use once `ANALYZE` has told it so — `sqlite_stat1`
//! reports rows-per-key, not rows-per-*this*-key, and there is no histogram.
//! So there are two ways this can fail: the index can cost writes and never be
//! chosen, or it can be chosen and be slower than the scan it replaced. Both
//! are D-089's failure mode, and both are only visible by running `ANALYZE`,
//! which the crate does (`PRAGMA optimize`, D-149). Every arm here is therefore
//! reported **before and after `ANALYZE`**.
//!
//! # What it reports
//!
//! 1. **The fixture** — trunk rows per table, and the lineage's own rows, so
//!    the ratio between what is archived and what is scanned is on the page.
//! 2. **The seven lineage-scoped statements**, planned. Six are
//!    `archive_branch_session`'s; the seventh is `refuse_unarchivable_branch`'s
//!    concept check, which D-271 did not count and which scans `concepts`.
//! 3. **The eighth scan nobody can see** — `branch_id` on all four ledger
//!    tables is `REFERENCES branches(branch_id)` and `PRAGMA foreign_keys` is
//!    `ON`, so `DELETE FROM branches` makes SQLite look for children in each of
//!    them. That check has no `EXPLAIN QUERY PLAN` output; it is measured.
//! 4. **`archive_branch` end to end**, through the public call, per index set
//!    and per trunk size — the figure the bench arm reports.
//! 5. **The write side**, which is what the index has to earn: seeding the
//!    trunk, one bulk batch, single-edge assertions, and the file on disk.
//!
//! # The index sets
//!
//! - **none** — today's schema.
//! - **links** — `idx_links_branch ON links (branch_id)`. The one table whose
//!   scan is paid four times over (copy, key collection, delete, FK check).
//! - **links+log** — plus `idx_txlog_branch ON transaction_log (branch_id)`.
//! - **four** — plus `concepts (branch_id)` and `links_current (branch_id)`,
//!   which is every table that carries the foreign key.
//! - **covering** — `links (branch_id, source_id, target_id, edge_type,
//!   valid_from)` in place of the bare one, which covers the key collection
//!   and the delete's search but not the copy to cold storage.
//!
//! # The reproduced-query hazard
//!
//! The seven statements below are **copies** of `pub(crate)` text in
//! `temporal::archive` and `integrity::rebuild`, the same hazard
//! `txlog_fold_index_probe` and `branch_write_guard_probe` carry. They are used
//! for the plans only. Every timing in section 4 goes through
//! `Database::archive_branch`, which runs the real text.
//!
//! Run it:
//!
//! ```text
//! cargo run --release --example branch_archive_index_probe
//! cargo run --release --example branch_archive_index_probe -- --trunk 8000
//! cargo run --release --example branch_archive_index_probe -- --lineage 200 --repeats 5
//! cargo run --release --example branch_archive_index_probe -- --skip-plans
//! ```

use std::time::Instant;

use macrame::branch::BranchId;
use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database};

const GENESIS: &str = "2020-01-01T00:00:00.000000Z";

/// A lineage name is used once: `archive_branch` moves the `branches` row to
/// the cold file, and a second fork under the same name would collide there.
static ROUND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `source_id, target_id, edge_type, valid_from, branch_id` — copied from
/// `integrity::rebuild::PROJECTION_KEY`.
const PROJECTION_KEY: &str = "source_id, target_id, edge_type, valid_from, branch_id";

/// The lineage-scoped statements, as (label, sql). The two `INSERT ... SELECT`
/// copies are reduced to their `SELECT` halves: the insert goes to an attached
/// cold database this probe does not open, and the scan being asked about is on
/// the reading side either way.
fn statements(extra: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "copy links -> cold",
            format!(
                "SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                 valid_to, weight, properties, branch_id \
                 FROM links WHERE branch_id = ?1{extra}"
            ),
        ),
        (
            "collect archived keys",
            format!(
                "SELECT DISTINCT {PROJECTION_KEY} FROM links WHERE branch_id = ?1{extra}"
            ),
        ),
        (
            "delete links",
            format!("DELETE FROM links WHERE branch_id = ?1{extra}"),
        ),
        (
            "copy concepts -> cold",
            "SELECT rowid_pk, id, title, content, embedding_model, valid_from, \
             valid_to, recorded_at, retired, branch_id FROM concepts WHERE branch_id = ?1"
                .to_string(),
        ),
        (
            "copy txlog -> cold",
            format!(
                "SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, \
                 branch_id FROM transaction_log WHERE branch_id = ?1{extra}"
            ),
        ),
        (
            "delete txlog",
            format!("DELETE FROM transaction_log WHERE branch_id = ?1{extra}"),
        ),
        (
            "refuse: concept minted here, named elsewhere",
            "SELECT c.id FROM concepts c WHERE c.branch_id = ?1 AND EXISTS ( \
             SELECT 1 FROM links l WHERE l.branch_id <> ?1 \
             AND (l.source_id = c.id OR l.target_id = c.id)) LIMIT 1"
                .to_string(),
        ),
    ]
}

/// The index sets under test, in the order they are reported.
/// (label, DDL, the predicate the reading statements must carry for that DDL
/// to be usable). Only the partial arm needs one: SQLite uses a partial index
/// only where the query's `WHERE` **implies** the index's, and `branch_id = ?1`
/// against a bound parameter implies nothing at all.
fn index_sets() -> Vec<(&'static str, Vec<&'static str>, &'static str)> {
    vec![
        ("none", vec![], ""),
        // Against the shipped partial pair: whatever this buys is something only
        // a *full* index can serve, which is the foreign-key child search
        // `DELETE FROM branches` performs and no query text can reach.
        (
            "plus-full",
            vec![
                "CREATE INDEX idx_links_branch_full ON links (branch_id)",
                "CREATE INDEX idx_txlog_branch_full ON transaction_log (branch_id)",
            ],
            "",
        ),
        (
            "plus-fk4",
            vec![
                "CREATE INDEX idx_links_branch_full ON links (branch_id)",
                "CREATE INDEX idx_txlog_branch_full ON transaction_log (branch_id)",
                "CREATE INDEX idx_concepts_branch ON concepts (branch_id)",
                "CREATE INDEX idx_lc_branch ON links_current (branch_id)",
            ],
            "",
        ),
        (
            "links",
            vec!["CREATE INDEX idx_links_branch ON links (branch_id)"],
            "",
        ),
        (
            "log",
            vec!["CREATE INDEX idx_txlog_branch ON transaction_log (branch_id)"],
            "",
        ),
        (
            "links+log",
            vec![
                "CREATE INDEX idx_links_branch ON links (branch_id)",
                "CREATE INDEX idx_txlog_branch ON transaction_log (branch_id)",
            ],
            "",
        ),
        (
            "four",
            vec![
                "CREATE INDEX idx_links_branch ON links (branch_id)",
                "CREATE INDEX idx_txlog_branch ON transaction_log (branch_id)",
                "CREATE INDEX idx_concepts_branch ON concepts (branch_id)",
                "CREATE INDEX idx_lc_branch ON links_current (branch_id)",
            ],
            "",
        ),
        (
            "covering",
            vec![
                "CREATE INDEX idx_links_branch ON links \
                 (branch_id, source_id, target_id, edge_type, valid_from)",
                "CREATE INDEX idx_txlog_branch ON transaction_log (branch_id)",
                "CREATE INDEX idx_concepts_branch ON concepts (branch_id)",
                "CREATE INDEX idx_lc_branch ON links_current (branch_id)",
            ],
            "",
        ),
        // The trunk is never archivable -- `refuse_unarchivable_branch` refuses
        // it first thing -- so the rows that dominate both tables, and that
        // every ordinary write adds to, need not be in this index at all. The
        // price is that the statements have to restate that invariant, or
        // SQLite cannot prove the index applies.
        (
            "partial",
            vec![
                "CREATE INDEX idx_links_branch ON links (branch_id) \
                 WHERE branch_id <> 'main'",
                "CREATE INDEX idx_txlog_branch ON transaction_log (branch_id) \
                 WHERE branch_id <> 'main'",
            ],
            " AND branch_id <> 'main'",
        ),
    ]
}

fn edge(i: usize) -> EdgeAssertion {
    EdgeAssertion::new("hub", format!("t{i}"), "LINKS")
        .valid_from(format!("2026-01-01T00:00:00.{:06}Z", i % 1_000_000))
        .valid_to(format!("2026-01-01T00:00:00.{:06}Z", (i % 1_000_000) + 1))
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .map(|r| r.get::<i64>(0).unwrap())
        .unwrap_or(0)
}

async fn plan(conn: &libsql::Connection, sql: &str) -> String {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ["alt"])
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(3).unwrap());
    }
    out.join(" | ")
}

/// Bytes on disk, main file plus WAL, which is where an index shows up.
fn file_bytes(path: &std::path::Path) -> u64 {
    let mut n = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal = path.with_extension("db-wal");
    n += std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0);
    n
}

struct Arm {
    label: &'static str,
    seed_ms: f64,
    batch_ms: f64,
    single_ms: f64,
    bytes: u64,
    archive_before: Vec<f64>,
    archive_drift: Vec<f64>,
    archive_after: Vec<f64>,
    plans_before: Vec<String>,
    plans_after: Vec<String>,
}

/// One index set, from an empty file: create the indexes, seed the trunk with
/// them in place so the write cost is paid honestly, then archive a fresh
/// lineage `repeats` times before and after `ANALYZE`.
// Ten of them, and grouping them into a `Params` struct would only move the
// same ten flags one line further away from the loop that fills them.
#[allow(clippy::too_many_arguments)]
async fn run_arm(
    dir: &std::path::Path,
    label: &'static str,
    indexes: &[&str],
    trunk: usize,
    lineage: usize,
    repeats: usize,
    batch: usize,
    holdouts: usize,
    extra: &str,
    skip_plans: bool,
) -> Result<Arm, Box<dyn std::error::Error>> {
    let path = dir.join(format!("{label}.db"));
    let _ = std::fs::remove_file(&path);
    let db = Database::open(&path).await?;

    // Before the seed, so every insert below pays for them — which is the cost
    // side of the question and cannot be measured by adding them afterwards.
    {
        let conn = db.raw().connect()?;
        for ddl in indexes {
            conn.execute(ddl, ()).await?;
        }
    }

    db.upsert_concept(ConceptUpsert::new("hub", "hub").valid_from(GENESIS))
        .await?;
    for i in 0..(trunk + 6 * batch + 64) {
        db.upsert_concept(ConceptUpsert::new(format!("t{i}"), "t").valid_from(GENESIS))
            .await?;
    }

    let start = Instant::now();
    db.bulk_import((0..trunk).map(edge).collect()).await?;
    let seed_ms = start.elapsed().as_secs_f64() * 1e3;

    // Five batches rather than one, best of them reported: this is the number
    // the index has to earn, so it is not left to a single sample.
    let mut batch_ms = f64::INFINITY;
    for b in 0..5 {
        let lo = trunk + b * batch;
        let start = Instant::now();
        db.bulk_import((lo..lo + batch).map(edge).collect()).await?;
        batch_ms = batch_ms.min(start.elapsed().as_secs_f64() * 1e3);
    }

    let singles = 64;
    let base = trunk + 5 * batch;
    let start = Instant::now();
    for i in 0..singles {
        db.assert_edge(edge(base + i)).await?;
    }
    let single_ms = start.elapsed().as_secs_f64() * 1e3 / singles as f64;

    // Lineages that stay. Without them the only surviving `branch_id` is
    // `main`, `sqlite_stat1` records one distinct key, and the planner's refusal
    // below would be an artefact of a fixture with no branches in it. Four live
    // forks is what a working ledger looks like.
    for h in 0..holdouts {
        let branch = BranchId::new(format!("hold{h}"))?;
        db.fork(branch.clone(), BranchId::main()).await?;
        db.bulk_import(
            (0..lineage)
                .map(|i| edge(i).on_branch(branch.clone()))
                .collect(),
        )
        .await?;
    }

    let bytes = file_bytes(&path);

    {
        let conn = db.read_conn();
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND                  name LIKE 'idx_%branch%' ORDER BY name",
                (),
            )
            .await?;
        let mut names = Vec::new();
        while let Some(r) = rows.next().await? {
            names.push(r.get::<String>(0)?);
        }
        println!(
            "   rows: links {}  links_current {}  concepts {}  txlog {}   indexes present: {:?}",
            scalar(conn, "SELECT count(*) FROM links").await,
            scalar(conn, "SELECT count(*) FROM links_current").await,
            scalar(conn, "SELECT count(*) FROM concepts").await,
            scalar(conn, "SELECT count(*) FROM transaction_log").await,
            names,
        );
    }

    let plans_before = if skip_plans {
        Vec::new()
    } else {
        let conn = db.read_conn();
        let mut v = Vec::new();
        for (_, sql) in statements(extra) {
            v.push(plan(conn, &sql).await);
        }
        v
    };

    let archive_before = archive_runs(&db, lineage, repeats).await?;
    // Drift control: the same rounds again with nothing changed between them,
    // so a difference in the third phase is `ANALYZE` and not the file ageing.
    let archive_drift = archive_runs(&db, lineage, repeats).await?;

    db.raw().connect()?.execute("ANALYZE", ()).await?;
    {
        let conn = db.read_conn();
        let mut rows = conn
            .query(
                "SELECT idx, stat FROM sqlite_stat1 WHERE idx LIKE '%branch%' ORDER BY idx",
                (),
            )
            .await?;
        let mut v = Vec::new();
        while let Some(r) = rows.next().await? {
            v.push(format!("{} [{}]", r.get::<String>(0)?, r.get::<String>(1)?));
        }
        println!("   sqlite_stat1: {}", v.join("  "));
    }

    let plans_after = if skip_plans {
        Vec::new()
    } else {
        let conn = db.read_conn();
        let mut v = Vec::new();
        for (_, sql) in statements(extra) {
            v.push(plan(conn, &sql).await);
        }
        v
    };

    let archive_after = archive_runs(&db, lineage, repeats).await?;

    Ok(Arm {
        label,
        seed_ms,
        batch_ms,
        single_ms,
        bytes,
        archive_before,
        archive_drift,
        archive_after,
        plans_before,
        plans_after,
    })
}

/// Fork, write `lineage` edges on the fork, archive it, `repeats` times.
///
/// A fresh lineage each round because the operation destroys the one it is
/// given — `BatchSize::PerIteration` in the bench arm, for the same reason. The
/// fork and the writes are outside the timer.
async fn archive_runs(
    db: &Database,
    lineage: usize,
    repeats: usize,
) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for _ in 0..repeats {
        let n = ROUND.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let branch = BranchId::new(format!("alt{n}"))?;
        db.fork(branch.clone(), BranchId::main()).await?;
        db.bulk_import(
            (0..lineage)
                .map(|i| edge(i).on_branch(branch.clone()))
                .collect(),
        )
        .await?;
        let start = Instant::now();
        let report = db.archive_branch(branch).await?;
        out.push(start.elapsed().as_secs_f64() * 1e3);
        assert!(
            report.links_archived > 0,
            "the fixture abandoned an empty lineage"
        );
    }
    Ok(out)
}

fn best(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::INFINITY, f64::min)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let trunk = flag("--trunk", 2_000);
    let lineage = flag("--lineage", 20);
    let repeats = flag("--repeats", 5);
    let batch = flag("--batch", 200);
    let holdouts = flag("--holdouts", 4);
    let skip_plans = args.iter().any(|a| a == "--skip-plans");
    let only: Option<String> = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .cloned();

    println!(
        "branch archive index, trunk {trunk} edges, lineage {lineage} rows, \
         {repeats} archives per arm\n"
    );

    let dir = tempfile::tempdir()?;
    let mut arms = Vec::new();

    for (label, indexes, extra) in index_sets() {
        if let Some(ref want) = only {
            if want != label {
                continue;
            }
        }
        println!("-- {label} --");
        let arm = run_arm(
            dir.path(),
            label,
            &indexes,
            trunk,
            lineage,
            repeats,
            batch,
            holdouts,
            extra,
            skip_plans,
        )
        .await?;
        println!(
            "   seed {:.0} ms   batch {:.1} ms   single {:.2} ms   file {:.1} MB",
            arm.seed_ms,
            arm.batch_ms,
            arm.single_ms,
            arm.bytes as f64 / 1e6
        );
        println!(
            "   archive best-of-{repeats}: before {:.2} ms   again {:.2} ms                after ANALYZE {:.2} ms",
            best(&arm.archive_before),
            best(&arm.archive_drift),
            best(&arm.archive_after)
        );
        arms.push(arm);
    }

    // ---- the fixture, from the last arm's file, which every arm shares ----
    println!();

    if !skip_plans {
        for (i, (label, _)) in statements("").into_iter().enumerate() {
            println!("\n{label}");
            for arm in &arms {
                println!(
                    "  {:>10}  pre-ANALYZE  {}",
                    arm.label,
                    arm.plans_before.get(i).map(String::as_str).unwrap_or("-")
                );
                println!(
                    "  {:>10}  post-ANALYZE {}",
                    "",
                    arm.plans_after.get(i).map(String::as_str).unwrap_or("-")
                );
            }
        }
    }

    println!("\n\nsummary (ms, best of {repeats}; bytes on disk after the seed)");
    println!(
        "  {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "arm", "seed", "batch", "single", "file MB", "archive", "again", "+ANALYZE"
    );
    for arm in &arms {
        println!(
            "  {:>10} {:>10.0} {:>10.1} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>10.2}",
            arm.label,
            arm.seed_ms,
            arm.batch_ms,
            arm.single_ms,
            arm.bytes as f64 / 1e6,
            best(&arm.archive_before),
            best(&arm.archive_drift),
            best(&arm.archive_after),
        );
    }
    println!(
        "\n  every archive run, in order:\n{}",
        arms.iter()
            .map(|a| format!(
                "  {:>10}  before {:?}\n  {:>10}  after  {:?}",
                a.label,
                a.archive_before
                    .iter()
                    .map(|v| format!("{v:.1}"))
                    .collect::<Vec<_>>(),
                "",
                a.archive_after
                    .iter()
                    .map(|v| format!("{v:.1}"))
                    .collect::<Vec<_>>(),
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    Ok(())
}
