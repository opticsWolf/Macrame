//! T1.1: does windowing the archive actually bound the hold, and what does it cost?
//!
//! The plan's claim is that `archive(cutoff)` is one transaction whose size is
//! set by how long since the last one, and that N smaller sessions satisfy
//! D-012 just as well. Both halves of that are about *latency*, and neither says
//! what windowing costs in total work — which is the number that decides whether
//! anyone will use it.
//!
//! Two things are measured, because they can move in opposite directions:
//!
//!   1. **the longest single archive hold** — what windowing exists to shrink;
//!   2. **total wall time for the whole run** — what windowing threatens.
//!
//! (2) is the risk D-077 implies: `rebuild_within` reprojects *all* of `links`,
//! so its cost is set by the surviving table and not by the batch being
//! archived. Naively, N windows means N full reprojections and windowing makes
//! the archive several times slower overall. `archive_session` therefore skips
//! the rebuild when its `DELETE` removed nothing; this measures whether that is
//! enough, and what remains when it is not.
//!
//! The fixture drives a `FakeClock`, so transaction time is a controlled axis
//! rather than whatever the wall clock did during the run. Without that there is
//! no way to ask for "one window per hour" and mean anything by it.
//!
//! Run with:  cargo run --release --features metrics --example archive_window_diag

use std::sync::Arc;
use std::time::{Duration, Instant};

use macrame::graph::EdgeAssertion;
use macrame::util::parse_iso8601_utc;
use macrame::util::FakeClock;
use macrame::{ConceptUpsert, Database};

const EPOCH: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// One hour of transaction time per generation of edges.
const GENERATION: Duration = Duration::from_secs(3_600);

/// Seed `keys` edges re-asserted over `generations` hours of transaction time.
///
/// Every generation after the first supersedes the one before, so all but the
/// last are archivable — which is the situation `archive()` is for. The clock is
/// advanced an hour between generations so the rows land in distinct windows.
async fn seed(db: &Database, clock: &FakeClock, keys: usize, generations: usize) {
    let nodes = keys + 1;
    for chunk in (0..nodes).collect::<Vec<_>>().chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|i| ConceptUpsert::new(format!("c{i:07}"), "n").valid_from(EPOCH))
                .collect(),
        )
        .await
        .unwrap();
    }

    for gen in 0..generations {
        let batch: Vec<_> = (0..keys)
            .map(|k| {
                EdgeAssertion::new(format!("c{k:07}"), format!("c{:07}", k + 1), "LINKS")
                    .valid_from(EPOCH)
                    .valid_to(OPEN)
                    .weight(1.0 + gen as f64)
            })
            .collect();
        for chunk in batch.chunks(2_000) {
            db.bulk_import(chunk.to_vec()).await.unwrap();
        }
        clock.advance(GENERATION);
    }
}

struct Run {
    sessions: usize,
    archived: usize,
    total_ms: f64,
    longest_hold_ms: f64,
}

async fn measure(keys: usize, generations: usize, window: Option<Duration>) -> Run {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("w.db");
    let clock = Arc::new(FakeClock::new(parse_iso8601_utc(EPOCH).unwrap()));
    let db = Database::open_with_clock(&path, None, clock.clone())
        .await
        .unwrap();

    seed(&db, &clock, keys, generations).await;

    // Everything but the final generation is superseded and therefore
    // archivable. The cutoff is the clock's current position.
    let cutoff = clock.peek();

    let before = db.metrics();
    let start = Instant::now();
    let reports = match window {
        Some(w) => db.archive_windowed(&cutoff, w).await.unwrap(),
        None => vec![db.archive(&cutoff).await.unwrap()],
    };
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after = db.metrics();

    // The per-turn hold, from the actor's own counters (T1.4) rather than from
    // this side of the channel — the wall time above includes queueing, and the
    // whole question is what the *actor* held for.
    let longest_hold_ms = archive_longest_ms(&before, &after);

    db.close().await.unwrap();

    Run {
        sessions: reports.len(),
        archived: reports.iter().map(|r| r.links_archived).sum(),
        total_ms,
        longest_hold_ms,
    }
}

/// The longest `Archive` hold, in ms.
///
/// The per-kind high-water mark, not `MetricsSnapshot::longest` — that one is
/// global and here it would be permanently the seed's bulk import. Nothing
/// archives before this point, so the `before` snapshot is only read to assert
/// that.
fn archive_longest_ms(
    before: &macrame::metrics::MetricsSnapshot,
    after: &macrame::metrics::MetricsSnapshot,
) -> f64 {
    let kind = |s: &macrame::metrics::MetricsSnapshot| {
        s.kinds
            .iter()
            .find(|k| k.kind == macrame::metrics::CommandKind::Archive)
            .unwrap()
            .clone()
    };
    assert_eq!(kind(before).turns, 0, "the fixture archived during seeding");
    kind(after).longest.as_secs_f64() * 1000.0
}

#[tokio::main]
async fn main() {
    println!("libSQL 0.9.30. `hold ms` is the longest single archive turn, read");
    println!("from the actor's own per-kind high-water mark (T1.4).\n");
    println!(
        "{:>7} {:>6} {:>16} {:>9} {:>10} {:>11} {:>10}",
        "keys", "gens", "window", "sessions", "archived", "total ms", "hold ms"
    );

    for (keys, generations) in [(2_000usize, 8usize), (8_000, 8)] {
        let whole = measure(keys, generations, None).await;
        println!(
            "{:>7} {:>6} {:>16} {:>9} {:>10} {:>11.1} {:>10.1}",
            keys,
            generations,
            "-- one session",
            whole.sessions,
            whole.archived,
            whole.total_ms,
            whole.longest_hold_ms
        );

        for hours in [4u64, 2, 1] {
            let w = Duration::from_secs(3_600 * hours);
            let run = measure(keys, generations, Some(w)).await;
            println!(
                "{:>7} {:>6} {:>16} {:>9} {:>10} {:>11.1} {:>10.1}",
                keys,
                generations,
                format!("{hours}h"),
                run.sessions,
                run.archived,
                run.total_ms,
                run.longest_hold_ms
            );
            assert_eq!(
                run.archived, whole.archived,
                "windowing changed *what* was archived, which would make every \
                 timing above meaningless"
            );
        }
        println!();
    }
}
