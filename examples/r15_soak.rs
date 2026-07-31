//! T5.2: defend — or refute — the claim R15's mitigation rests on.
//!
//! R15 is an intermittent `STATUS_ACCESS_VIOLATION` (`0xC0000005`) on libSQL
//! 0.9.30. The mitigation shipped is `RUST_TEST_THREADS = "1"`, and the claim
//! carrying it is *"production exposure is nil by construction — an application
//! opens one `Database` and holds it for its lifetime."* Nothing tests that.
//!
//! # Why this is an example with a subprocess runner, and not a `#[test]`
//!
//! **The fault kills the process.** It is not a panic and not a SQLite error, so
//! there is nothing to catch, nothing to assert on, and a test that provoked it
//! would take the whole binary down — reported, as the plan already documents,
//! as *fewer passing tests and no failures*. Measuring a process-level fault
//! requires running the work in a child and reading its exit code, which is what
//! `--runs` does: the parent re-executes this same binary with `--child` and
//! tallies how many died.
//!
//! # The two arms, and why the second one is not optional
//!
//! * `claim` — one long-lived `Database`, heavy concurrent read load across many
//!   Tokio tasks, and a write actor kept saturated. This is the shape the claim
//!   describes. A clean result here is evidence *for* the claim.
//! * `control` — the same, **plus** a second task opening 32+ databases
//!   concurrently in the same process, which is the thing already measured to
//!   fault (2/12 at 32, 5/12 at 128).
//!
//! Without the control arm a clean `claim` run cannot be distinguished from a
//! harness that never provoked anything, and the whole exercise would be
//! unfalsifiable — which is the defect §9's budget table had before D-055.
//!
//! # What the claim should be sharpened to
//!
//! *"An application opens one `Database`"* is not what this crate does:
//! `open_inner` opens **three** connections, four with the cadence. What this
//! can actually test — and therefore what production exposure should be stated
//! as — is **one process, one file, a bounded set of connections opened once and
//! never churned**. The `claim` arm is built to be exactly that and nothing more.
//!
//! # Running it
//!
//! ```text
//! cargo run --release --example r15_soak -- --arm claim   --secs 20 --runs 10
//! cargo run --release --example r15_soak -- --arm control --secs 20 --runs 10
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Concurrent opens in the control arm. The measured threshold sits between 16
/// and 32 on the reference machine; 48 is above it with margin, so a clean
/// control run is a real negative rather than a near miss.
const CONTROL_OPENS: usize = 48;

/// Concurrent readers in both arms.
const READERS: usize = 16;

struct Args {
    arm: String,
    secs: u64,
    runs: usize,
    child: bool,
}

fn parse() -> Args {
    let mut a = Args {
        arm: "claim".into(),
        secs: 20,
        runs: 1,
        child: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--arm" => {
                a.arm = argv[i + 1].clone();
                i += 2;
            }
            "--secs" => {
                a.secs = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--runs" => {
                a.runs = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--child" => {
                a.child = true;
                i += 1;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn main() {
    let args = parse();
    if args.child || args.runs <= 1 {
        run_arm(&args.arm, args.secs);
    } else {
        supervise(&args);
    }
}

/// Re-execute this binary `runs` times and tally how many died.
///
/// Exit codes: `0` clean, `5` is how Windows surfaces `0xC0000005` through
/// `ExitStatus::code()`, and anything else is an ordinary failure that should be
/// read as a bug rather than as R15.
fn supervise(args: &Args) {
    let exe = std::env::current_exe().expect("current_exe");
    println!(
        "R15 soak: arm={} secs={} runs={}\n{}\n",
        args.arm,
        args.secs,
        args.runs,
        "-".repeat(60)
    );

    let mut faults = 0usize;
    let mut other = 0usize;
    for run in 1..=args.runs {
        let t = Instant::now();
        let status = std::process::Command::new(&exe)
            .args([
                "--child",
                "--arm",
                &args.arm,
                "--secs",
                &args.secs.to_string(),
            ])
            .status()
            .expect("spawn child");
        let code = status.code();
        let verdict = match code {
            Some(0) => "clean",
            // 0xC0000005 truncated by the shell's exit-code convention.
            Some(5) | Some(-1073741819) => {
                faults += 1;
                "R15 FAULT (access violation)"
            }
            _ => {
                other += 1;
                "other failure"
            }
        };
        println!(
            "  run {run:>3}/{:<3}  {:>6.1}s  exit {:>12}  {verdict}",
            args.runs,
            t.elapsed().as_secs_f64(),
            code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        );
    }

    println!("\n{}", "-".repeat(60));
    println!(
        "arm={}  faults {}/{}  other failures {}",
        args.arm, faults, args.runs, other
    );
    match (args.arm.as_str(), faults) {
        ("claim", 0) => println!(
            "\nThe claim survives this load: one process, one file, a bounded set \n\
             of connections opened once and never churned, {} readers and a \n\
             saturated actor. Read it together with the control arm — a clean \n\
             claim run means nothing on its own.",
            READERS
        ),
        ("claim", n) => println!(
            "\nR15 REACHES THE CLAIMED-SAFE SHAPE: {n} faults with no concurrent \n\
             opens beyond `open()`. The mitigation's justification does not hold \n\
             and the plan changes."
        ),
        ("control", 0) => println!(
            "\nThe control did NOT fault, so this run provoked nothing and a clean \n\
             claim arm is not evidence. Raise --runs or --secs before concluding \n\
             anything from either arm."
        ),
        ("control", n) => println!(
            "\nThe control faulted {n} times, so the harness does provoke R15 and \n\
             a clean claim arm is a real negative."
        ),
        _ => {}
    }
    std::process::exit(if other > 0 { 1 } else { 0 });
}

fn run_arm(arm: &str, secs: u64) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        match arm {
            "claim" => soak(secs, false).await,
            "control" => soak(secs, true).await,
            other => panic!("unknown arm {other}"),
        }
    });
}

/// One long-lived `Database` under load, optionally alongside the open storm.
async fn soak(secs: u64, with_open_storm: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    // Cadence on: the cadence connection is the fourth one `open()` takes, and
    // the claim is about the set of connections a real application holds. Turning
    // it off would test a shape no application runs.
    let db = Arc::new(
        Database::open(dir.path().join("soak.db"))
            .await
            .unwrap(),
    );

    // A little topology, so reads have something to walk.
    db.write_concepts((0..500).map(|i| {
        ConceptUpsert::new(format!("c{i:05}"), format!("C{i}"))
            .content("body")
            .valid_from(TS)
    })
    .collect())
    .await
    .unwrap();
    db.bulk_import(
        (1..500)
            .map(|i| {
                EdgeAssertion::new("c00000", format!("c{i:05}"), "LINKS")
                    .valid_from(TS)
                    .valid_to(OPEN)
            })
            .collect(),
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(secs);
    let reads = Arc::new(AtomicU64::new(0));
    let writes = Arc::new(AtomicU64::new(0));
    let opens = Arc::new(AtomicU64::new(0));

    let mut tasks = Vec::new();

    // Heavy concurrent read load on the shared reader.
    for r in 0..READERS {
        let db = Arc::clone(&db);
        let reads = Arc::clone(&reads);
        tasks.push(tokio::spawn(async move {
            while Instant::now() < deadline {
                let _ = db.load_subgraph("c00000", 2, TS, 64 << 20).await;
                let _ = macrame::temporal::reconstruct(db.read_conn(), TS, None, None).await;
                reads.fetch_add(2, Ordering::Relaxed);
                if r == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }));
    }

    // The actor kept saturated, so the write connection is never idle.
    {
        let db = Arc::clone(&db);
        let writes = Arc::clone(&writes);
        tasks.push(tokio::spawn(async move {
            let mut i = 0u64;
            while Instant::now() < deadline {
                let _ = db
                    .write_concepts(vec![ConceptUpsert::new(
                        format!("w{i:09}"),
                        format!("W{i}"),
                    )
                    .content("body")
                    .valid_from(TS)])
                    .await;
                writes.fetch_add(1, Ordering::Relaxed);
                i += 1;
            }
        }));
    }

    // The control arm: the thing already measured to fault, running beside the
    // shape the claim describes. Separate files, so this is concurrent *open*
    // and not contention on the soak database.
    if with_open_storm {
        let storm_dir = dir.path().to_path_buf();
        let opens = Arc::clone(&opens);
        tasks.push(tokio::spawn(async move {
            let mut round = 0u64;
            while Instant::now() < deadline {
                let mut batch = Vec::new();
                for i in 0..CONTROL_OPENS {
                    let p = storm_dir.join(format!("storm-{round}-{i}.db"));
                    batch.push(tokio::spawn(async move {
                        let d = libsql::Builder::new_local(&p).build().await.unwrap();
                        let c = d.connect().unwrap();
                        let _ = c.query("SELECT 1", ()).await;
                    }));
                }
                for h in batch {
                    let _ = h.await;
                }
                opens.fetch_add(CONTROL_OPENS as u64, Ordering::Relaxed);
                round += 1;
            }
        }));
    }

    for t in tasks {
        let _ = t.await;
    }

    eprintln!(
        "  [child] reads={} writes={} concurrent_opens={}",
        reads.load(Ordering::Relaxed),
        writes.load(Ordering::Relaxed),
        opens.load(Ordering::Relaxed)
    );

    Arc::into_inner(db).unwrap().close().await.unwrap();
}
