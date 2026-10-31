//! 0.21.0 (P4, measurement 1): the bounded valid fold at production size.
//!
//! 0.20.0 measured this once and kept no output, so the item is a measurement,
//! not a re-statement. The ledger is the release bench's own shape:
//! `benches/budgets.rs`'s `graph_analytics` fixture — 1,001 concepts with body
//! text written in bulk, 1,000 `Shape::StarOfStars` edges over them — built
//! here with the same seeding calls in the same chunk sizes.
//!
//! Two arms, plus a control every arm reports beside:
//!
//! * **hot**, `HotLogReach::Covers` — `reconstruct_as_of(AsOf::bitemporal(…))`
//!   on the untouched ledger, valid instant inside the hot log's reach;
//! * **archive-attached**, `HotLogReach::NeedsArchive` — after the same ledger
//!   is archived at a cutoff past everything, so the hot log no longer reaches
//!   the read instant and the fold attaches the sibling `_archive.db` for the
//!   cold delta. The bounded valid fold reads hot and cold in one fold; this is
//!   the arm whose cost the hot-only reading understated.
//! * control — `SELECT 1`, the same normaliser `bench_control_tests` carries.
//!
//!   cargo run --release --example bounded_fold_scale_probe
//!
//! Median of [`SAMPLES`] calls per arm after a warm-up; no cadence, no
//! snapshot writes, one database for the whole run (the crash classes this
//! station shows on many opens in one process are the ones R15 documents).

use std::time::{Duration, Instant};

#[path = "../tests/common/fixtures.rs"]
mod fixtures;

use macrame::prelude::*;
use macrame::temporal::AsOf;

/// A recorded instant every row sits before: the bound for the hot arm.
const LATER: &str = "2200-01-01T00:00:00.000000Z";
/// The valid instant the fold answers, inside the ledger's own intervals.
const VALID: &str = "2026-02-01T00:00:00.000000Z";
/// Past everything the fixture writes, so archiving it at this cutoff empties
/// the hot log of every row it can move.
const FUTURE: &str = "9999-01-01T00:00:00.000000Z";
/// Before the fixture's first stamp: a bound predating every hot row's stamp,
/// which is what puts `hot_log_reach` into its NeedsArchive arm.
const EARLY: &str = "2025-01-01T00:00:00.000000Z";

const SAMPLES: usize = 21;
const EDGES: usize = 1_000;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

async fn count_rows(db: &Database) -> i64 {
    let mut rows = db
        .read_conn()
        .query("SELECT COUNT(*) FROM transaction_log", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn control(db: &Database) -> Duration {
    let t = Instant::now();
    let mut rows = db.read_conn().query("SELECT 1", ()).await.unwrap();
    let _ = rows.next().await.unwrap();
    t.elapsed()
}

async fn fold(db: &Database, valid: Option<&str>, recorded: Option<&str>) -> Duration {
    let t = Instant::now();
    let _ = match (valid, recorded) {
        (Some(v), Some(r)) => db.reconstruct_as_of(AsOf::bitemporal(v, r)).await.unwrap(),
        (Some(v), None) => db.reconstruct_as_of(AsOf::valid_at(v)).await.unwrap(),
        _ => unreachable!("the probe fixes valid time in every arm"),
    };
    t.elapsed()
}

async fn timed(name: &str, db: &Database, valid: Option<&str>, recorded: Option<&str>) {
    // Warm-up: three calls, uncounted.
    for _ in 0..3 {
        fold(db, valid, recorded).await;
        control(db).await;
    }
    let mut folds = Vec::with_capacity(SAMPLES);
    let mut controls = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        folds.push(fold(db, valid, recorded).await);
        controls.push(control(db).await);
    }
    let med = median(folds);
    let ctl = median(controls);
    println!(
        "{:<26} {:>9.3} ms  (call)   control {:>7.3} ms   ratio {:>6.1}x",
        name,
        med.as_secs_f64() * 1000.0,
        ctl.as_secs_f64() * 1000.0,
        med.as_secs_f64() / ctl.as_secs_f64(),
    );
}

/// The bench's own seeding calls, at the bench's chunk sizes.
#[allow(dead_code)]
mod seeders {
    use macrame::prelude::*;

    pub const TS: &str = "2026-01-01T00:00:00.000000Z";
    pub const OPEN: &str = "9999-12-31T23:59:59.999999Z";

    pub fn concept(i: usize) -> ConceptUpsert {
        ConceptUpsert::new(format!("c{i:07}"), format!("Concept {i}"))
            .content(format!("body text for concept number {i}"))
            .valid_from(TS)
    }

    #[allow(dead_code)]
    pub async fn seed_concepts(db: &Database, n: usize) {
        for chunk in (0..n).collect::<Vec<_>>().chunks(2_000) {
            db.write_concepts(chunk.iter().map(|i| concept(*i)).collect())
                .await
                .unwrap();
        }
    }

    pub async fn seed_edges(db: &Database, edges: usize) {
        for chunk in super::fixtures::Shape::StarOfStars
            .edges(edges + 1)
            .chunks(2_000)
        {
            db.bulk_import(chunk.to_vec()).await.unwrap();
        }
    }
}

#[tokio::main]
async fn main() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.db");
    let db = Database::open_with_cadence(&path, None).await.unwrap();

    let t = Instant::now();
    seeders::seed_concepts(&db, EDGES + 1).await;
    seeders::seed_edges(&db, EDGES).await;
    println!(
        "bounded_fold_scale_probe — the release bench's graph fixture ({} concepts, \
         {} StarOfStars edges), built in {:.1} s. {} calls per arm, median.\n",
        EDGES + 1,
        EDGES,
        t.elapsed().as_secs_f64(),
        SAMPLES
    );

    // The hot arm: the fold everything is reachable in.
    timed("hot, bounded at now", &db, Some(VALID), Some(LATER)).await;

    // The archive arm. `archive()` moves a concept row only once a NEWER row
    // exists on the same valid start (D-298: LOG_ARCHIVABLE), so a first pass
    // that moved nothing is the fixture saying so, not an archive failure.
    // Each concept is healed in one bulk call -- a same-start re-assertion of
    // the row the rule wants on top anyway -- which makes every original
    // archivable; the winnings move to the sibling `_archive.db`. The read
    // bound then sits EARLIER than every remaining hot stamp, so
    // `hot_log_reach` answers NeedsArchive and the fold runs attached: the
    // bounded valid fold reading hot and cold in one pass is the measured arm.
    let tooth: i64 = count_rows(&db).await;
    let healed = {
        let rows: Vec<ConceptUpsert> = (0..=EDGES)
            .map(|i| seeders::concept(i).valid_to(seeders::OPEN))
            .collect();
        db.write_concepts(rows).await.unwrap()
    };
    let report = db.archive(FUTURE).await.unwrap();
    let after: i64 = count_rows(&db).await;
    println!(
        "  healed {healed} concepts in place; archive() moved {} log rows (of {tooth}) hot\u{2192}cold; hot log now {after} rows",
        report.log_entries_archived,
    );
    assert!(
        report.log_entries_archived > 0,
        "the fixture built nothing archivable; the arm below is not attached"
    );
    timed(
        "archive-attached, preheal cell",
        &db,
        Some(VALID),
        Some(EARLY),
    )
    .await;

    db.close().await.unwrap();
}
