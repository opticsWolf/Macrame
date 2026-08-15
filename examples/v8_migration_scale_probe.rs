//! What does the v7 → v8 rung cost on a database with real data in it? (0.8.0)
//!
//! **Every test of the rung runs on four concepts.** `migration_tests.rs` proves
//! it is *correct* — the rowid values carry across by value, the suspension is
//! necessary, a pre-existing orphan is refused — and says nothing about what it
//! costs, because at four rows nothing costs anything. That is a gap [D-088]
//! would not tolerate in a query path and should not tolerate here: this rung
//! rewrites a ledger table on somebody's data, holding the write lock, and the
//! operator running it has been told nothing about how long their database will
//! be unavailable or how much free disk they need first.
//!
//! Three numbers, and the third is the one nobody has looked at:
//!
//! 1. **Wall time**, which is the length of the outage.
//! 2. **Peak disk**, sampled during the run rather than inferred. The rung
//!    copies `concepts` into a new table before dropping the old one, so both
//!    exist at once, and the FTS index is dropped and rebuilt on top of that.
//! 3. **`PRAGMA foreign_key_check`**, which [D-117] put *inside* the
//!    transaction. It is a whole-database scan, it is new in this rung, and it
//!    runs while the lock is held. If it dominates, the cost of the rung is not
//!    where anyone would look for it.
//!
//! The fixture is the same pinned v7 schema `migration_tests.rs` checks against
//! (`tests/common/v7_schema.rs`) — one pin, two readers, so a measurement can
//! never be taken against a shape the correctness tests do not also use.
//!
//! **Why the FTS index is populated.** A real v7 database has one, the rung
//! rebuilds it, and seeding through the v7 triggers is what makes that rebuild
//! cost what it will cost in the field. Leaving it empty would have produced a
//! reassuring number about a database nobody has.
//!
//! Run with:  cargo run --release --example v8_migration_scale_probe

#[path = "../tests/common/v7_schema.rs"]
mod v7_schema;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use macrame::schema::migrations;
use v7_schema::{v7_schema, TS};

/// Concepts per arm. Links are `LINK_FACTOR` times this.
const SCALES: &[usize] = &[1_000, 10_000, 50_000, 200_000];

/// Out-degree. Three is on the low side for a knowledge graph and is chosen for
/// that reason: it keeps `links` from dominating the file, so the `concepts`
/// rebuild — the thing being measured — is not hidden behind a bigger table.
const LINK_FACTOR: usize = 3;

/// Rows per multi-value INSERT while seeding. Seeding is not the measurement;
/// this only keeps the fixture build from taking longer than the thing it is a
/// fixture for.
const BATCH: usize = 500;

/// Total bytes of the database and its sidecars.
fn db_bytes(path: &Path) -> u64 {
    let mut total = 0;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let p = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap().to_string_lossy()
        ));
        if let Ok(m) = std::fs::metadata(&p) {
            total += m.len();
        }
    }
    total
}

/// Poll the file size on another thread for the duration of the migration.
///
/// Sampled, not computed. The obvious estimate — "about twice `concepts`" —
/// assumes the copy is the peak, and the FTS rebuild and the WAL are both
/// capable of beating it. An estimate that happens to be right is
/// indistinguishable from one that is wrong until somebody measures.
struct PeakSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakSampler {
    fn start(path: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(db_bytes(path)));
        let (s, pk, p) = (stop.clone(), peak.clone(), path.to_path_buf());
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let now = db_bytes(&p);
                pk.fetch_max(now, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
            }
            pk.fetch_max(db_bytes(&p), Ordering::Relaxed);
        });
        Self {
            stop,
            peak,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
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

/// Seed a v7 database with `concepts` concepts and `concepts * LINK_FACTOR`
/// links, plus a transaction_log row per write.
///
/// The log rows are there for the `foreign_key_check` measurement: that pragma
/// scans the whole database, so a fixture with an empty ledger would understate
/// it on every real database, all of which have one.
async fn seed(conn: &libsql::Connection, n_concepts: usize) {
    v7_schema(conn).await;

    // `query`, not `execute`: both of these pragmas return a row, and `execute`
    // refuses anything that does (`ExecuteReturnedRows`).
    conn.query("PRAGMA journal_mode = WAL", ()).await.unwrap();
    conn.execute("BEGIN", ()).await.unwrap();

    for chunk_start in (0..n_concepts).step_by(BATCH) {
        let end = (chunk_start + BATCH).min(n_concepts);
        let values: Vec<String> = (chunk_start..end)
            .map(|i| {
                format!(
                    "('c{i}', 'Concept {i}', \
                      'body text for concept {i} with enough words to make the \
                       full text index do real work', '{TS}', '{TS}')"
                )
            })
            .collect();
        conn.execute(
            &format!(
                "INSERT INTO concepts (id, title, content, valid_from, recorded_at) \
                 VALUES {}",
                values.join(",")
            ),
            (),
        )
        .await
        .unwrap();
    }

    let n_links = n_concepts * LINK_FACTOR;
    for chunk_start in (0..n_links).step_by(BATCH) {
        let end = (chunk_start + BATCH).min(n_links);
        let values: Vec<String> = (chunk_start..end)
            .map(|i| {
                // Deterministic, and deliberately not `i` to `i+1`: a chain
                // gives every concept in-degree 1, and the rung's cost is in
                // the inbound foreign keys the suspension exists for.
                let src = i % n_concepts;
                let tgt = (i * 7 + 13) % n_concepts;
                // The edge type carries the BAND (`i / n_concepts`), not
                // `i % 4`. With `i % 4` every `i` collided with `i +
                // n_concepts` — same src, same tgt because the stride is a
                // multiple of the modulus, and the same type whenever
                // n_concepts is a multiple of 4, which all the scales are. The
                // uniqueness constraint then dropped two links in three and an
                // `INSERT OR IGNORE` swallowed it, so the probe reported a
                // 3x-links fixture while measuring a 1x one.
                format!(
                    "('c{src}', 'c{tgt}', 'KNOWS_{}', '{TS}', \
                      '9999-12-31T23:59:59.999999Z', 1.0, '{{}}', '{TS}')",
                    i / n_concepts
                )
            })
            .collect();
        // A plain INSERT, so a collision is a panic rather than a quietly
        // smaller fixture. `OR IGNORE` is what let the defect above ship.
        conn.execute(
            &format!(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
                 valid_to, weight, properties, recorded_at) VALUES {}",
                values.join(",")
            ),
            (),
        )
        .await
        .unwrap();
    }

    conn.execute("COMMIT", ()).await.unwrap();

    let got = scalar(conn, "SELECT COUNT(*) FROM links").await;
    assert_eq!(
        got as usize, n_links,
        "fixture is not the size it claims: wanted {n_links} links, got {got}"
    );
    conn.execute("PRAGMA user_version = 7", ()).await.unwrap();
    conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .unwrap();
}

struct Row {
    concepts: usize,
    links: i64,
    log: i64,
    before: u64,
    peak: u64,
    after: u64,
    settled: u64,
    migrate: Duration,
    fk_check: Duration,
}

async fn arm(dir: &Path, n_concepts: usize) -> Row {
    let path = dir.join(format!("v7_{n_concepts}.db"));
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();

    seed(&conn, n_concepts).await;
    let links = scalar(&conn, "SELECT COUNT(*) FROM links").await;
    let log = scalar(&conn, "SELECT COUNT(*) FROM transaction_log").await;
    let concepts_before = scalar(&conn, "SELECT COUNT(*) FROM concepts").await;
    drop(conn);
    drop(db);

    let before = db_bytes(&path);

    // The measurement. A fresh handle, because that is what an upgrade is: a
    // process starting against a database written by an older build.
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();

    let sampler = PeakSampler::start(&path);
    let t0 = Instant::now();
    let outcome = migrations::run(&conn).await.unwrap();
    let migrate = t0.elapsed();
    let peak = sampler.finish();

    // The rung must actually have run, or this is a timing of nothing. The
    // arithmetic below would otherwise report a very fast migration.
    assert_eq!(
        scalar(&conn, "PRAGMA user_version").await,
        8,
        "fixture did not climb to v8; the measurement is meaningless"
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM concepts").await,
        concepts_before,
        "the rung lost or duplicated concept rows"
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT COUNT(*) FROM concepts WHERE rowid_pk IS NULL"
        )
        .await,
        0,
        "a concept came out of the rung with no rowid_pk"
    );
    assert_eq!(
        (outcome.from, outcome.to),
        (7, 8),
        "the fixture did not enter at v7, so this timed the wrong rung"
    );

    // The third number, isolated. Same table sizes, same indices, on the far
    // side — a fair proxy for what the pragma costs inside the transaction,
    // and the only way to see it separately at all, since inside the rung it is
    // inseparable from the rebuild.
    let t1 = Instant::now();
    let mut rows = conn.query("PRAGMA foreign_key_check", ()).await.unwrap();
    let mut violations = 0;
    while rows.next().await.unwrap().is_some() {
        violations += 1;
    }
    let fk_check = t1.elapsed();
    assert_eq!(violations, 0, "the migrated database has FK violations");

    let after = db_bytes(&path);

    // `after` still carries an un-checkpointed WAL, so on its own it answers
    // "how much disk did this need" and not "how much disk does it keep". The
    // operator needs both: the first sizes the free space they must have before
    // starting, the second is what they get back afterwards.
    drop(rows);
    conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .unwrap();
    let settled = db_bytes(&path);
    drop(conn);
    drop(db);

    Row {
        concepts: n_concepts,
        links,
        log,
        before,
        peak,
        after,
        settled,
        migrate,
        fk_check,
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().unwrap();
    println!("v7 -> v8 migration cost, {} arms\n", SCALES.len());

    let mut rows = Vec::new();
    for &n in SCALES {
        eprintln!("seeding and migrating {n} concepts...");
        rows.push(arm(dir.path(), n).await);
    }

    println!(
        "| concepts | links | log rows | before | peak | settled | migrate | fk_check | us/concept |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!(
            "| {:>7} | {:>7} | {:>7} | {:>6.1} MiB | {:>6.1} MiB | {:>6.1} MiB | {:>7.2} s | {:>6.3} s | {:>6.1} |",
            r.concepts,
            r.links,
            r.log,
            mib(r.before),
            mib(r.peak),
            mib(r.settled),
            r.migrate.as_secs_f64(),
            r.fk_check.as_secs_f64(),
            r.migrate.as_micros() as f64 / r.concepts as f64,
        );
    }

    println!("\npeak as a multiple of the starting file, and fk_check as a share of the rung:");
    for r in &rows {
        println!(
            "  {:>7} concepts:  peak {:.2}x before, settled {:.2}x before, \
             fk_check {:.0}% of migrate  (end state before checkpoint {:.1} MiB)",
            r.concepts,
            r.peak as f64 / r.before.max(1) as f64,
            r.settled as f64 / r.before.max(1) as f64,
            100.0 * r.fk_check.as_secs_f64() / r.migrate.as_secs_f64().max(1e-9),
            mib(r.after),
        );
    }
}
