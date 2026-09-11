//! Plan §8.1 / F1 scoping: what fraction of the random-pair bulk path is the
//! `links_current` mirror trigger, and is the chunked rebuild the honest cost
//! of skipping it?
//!
//! ```text
//! cargo run --release --example bulk_skip_probe -- --n 16000 --sessions 3
//! ```
//!
//! Arms, fresh file each:
//!
//! 1. **shipped** — the ordinary `bulk_import`.
//! 2. **skip_mirror** — `trg_links_current_sync` dropped (the single-open
//!    guard and the log mirror stay: the ledger is still the ledger), the same
//!    bulk, then `rebuild_current_chunked` (the one-pass re-derivation), then
//!    the trigger recreated from its own named `const`. Between the drop and
//!    the rebuild, `links_current` is stale — the window F1's opt-in exists
//!    to name.
//!
//! The probe verifies the skip arm's projection agrees with the ledger
//! (`audit_current` at zero), which is the whole correctness claim F1 would
//! rest on.

use macrame::prelude::*;
use std::time::Instant;

const TS: &str = "2025-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

fn parse_args() -> (usize, usize) {
    let mut n = 16000;
    let mut sessions = 3;
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--n" => { n = argv[i + 1].parse().unwrap(); i += 2; }
            "--sessions" => { sessions = argv[i + 1].parse().unwrap(); i += 2; }
            other => panic!("unknown arg {other}"),
        }
    }
    (n, sessions)
}

fn edges(n: usize, concepts: usize) -> Vec<EdgeAssertion> {
    let mut s: u64 = 20250910;
    let mut rng = move || {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        (s >> 11) as usize % concepts
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let a = rng();
        let b = rng();
        if a != b && seen.insert((a, b)) {
            out.push(
                EdgeAssertion::new(format!("c{a:06}"), format!("c{b:06}"), "RELATES")
                    .valid_from(TS)
                    .valid_to(OPEN),
            );
        }
    }
    out
}

async fn seed(db: &Database, n: usize) {
    let concepts: Vec<_> = (0..n).map(|i| ConceptUpsert::new(format!("c{i:06}"), "T").valid_from(TS)).collect();
    db.write_concepts(concepts).await.unwrap();
}

async fn arm_skip(db: &Database, n: usize, skip: bool) -> (f64, f64) {
    let rows = edges(n, n / 2);
    // Drop (and later restore) exactly one trigger: the links_current mirror.
    // The log mirror and the single-open guard stay — the ledger keeps its
    // record and its rule; only the per-row projection is deferred.
    if skip {
        let conn = db.raw().connect().unwrap();
        conn.execute("DROP TRIGGER trg_links_current_sync", ())
            .await
            .unwrap();
        drop(conn);
    }

    let holds: std::sync::Arc<std::sync::Mutex<f64>> = std::sync::Arc::default();
    let sink = holds.clone();
    let t0 = Instant::now();
    let control = BulkControl::new().on_progress(move |p| {
        *sink.lock().unwrap() += p.held.as_secs_f64();
    });
    let written = db.bulk_import_with(rows, control).await.unwrap();
    assert_eq!(written, n);
    let bulk = t0.elapsed().as_secs_f64();

    let (report, rebuild) = if skip {
        let t1 = Instant::now();
        let report = db.rebuild_current_chunked().await.unwrap();
        let conn = db.raw().connect().unwrap();
        conn.execute(macrame::schema::ddl::CREATE_LINKS_CURRENT_SYNC, ())
            .await
            .unwrap();
        drop(conn);
        (Some(report.rows_rebuilt), t1.elapsed().as_secs_f64())
    } else {
        (None, 0.0)
    };

    // The correctness claim F1 rests on: the projection agrees with what the
    // shipped path maintains. `audit_current` is the crate's own symmetric-
    // difference audit, Err(CurrentDrift) when anything disagrees.
    let audit = macrame::integrity::audit_current(db.read_conn()).await.unwrap();
    assert_eq!(audit, 0, "the skipped mirror left the projection drifting");

    if skip {
        println!("           rebuilt {report:?}");
    }
    let holds = std::sync::Arc::try_unwrap(holds)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    (bulk, rebuild + holds * 0.0)
}

#[tokio::main]
async fn main() {
    let (n, sessions) = parse_args();
    let dir = std::env::temp_dir().join(format!("macrame_bulk_skip_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut arms: Vec<[Vec<f64>; 2]> = vec![[Vec::new(), Vec::new()], [Vec::new(), Vec::new()]];
    for session in 0..sessions {
        for (arm, drop) in [(0usize, false), (1usize, true)] {
            let path = dir.join(format!("skip_{arm}.db"));
            let _ = std::fs::remove_file(&path);
            let db = Database::open(&path).await.unwrap();
            seed(&db, n / 2).await;
            let (bulk, rebuild) = arm_skip(&db, n, drop).await;
            arms[arm][0].push(bulk);
            arms[arm][1].push(rebuild);
            db.close().await.unwrap();
            println!(
                "session {session}: {} bulk={bulk:.2}s rebuild/hold={rebuild:.3}s",
                if drop { "skip_mirror" } else { "shipped     " }
            );
        }
    }

    for (name, arm) in [("shipped", &arms[0]), ("skip_mirror", &arms[1])] {
        let mut b = arm[0].clone();
        b.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let bm = b[b.len() / 2];
        let mut r = arm[1].clone();
        r.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let rm = r[r.len() / 2];
        println!("{name:12} bulk={bm:.2}s  rebuild={rm:.3}s");
    }
    std::fs::remove_dir_all(&dir).ok();
}
