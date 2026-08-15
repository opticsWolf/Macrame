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
//! # The `storm` arm, and what it found (W1.1)
//!
//! `control` answers *"does this harness provoke anything?"* and it answers it
//! by varying everything at once: 48 concurrent opens, a first query on each,
//! and a saturated soak database beside them. That is the right shape for its
//! own question and the wrong shape for the next one, which is **which of those
//! is load-bearing**.
//!
//! `storm` runs the open storm alone — no soak database, no readers, no actor —
//! and puts each candidate variable behind its own flag.
//!
//! **Read the column as a boolean, never as a rate.** D-124 retracted a claim
//! built on eight runs per arm, and measured the noise band at ~30 points at
//! n = 20. These are n = 6–8. The only distinction they can carry is
//! *eliminated* versus *not eliminated*, and the one row that clears that bar
//! does so on a 100× volume difference rather than on its fault count.
//! Measured on the reference machine, debug build, `--opens 48 --secs 2`:
//!
//! | configuration | faults | eliminated? |
//! |---|---|---|
//! | `--first-use build` | 0/6 | **yes** — and at ~880,000 opens per run, so this is volume, not luck |
//! | `--first-use connect` | 6/6 | no — the fault needs `connect()` |
//! | `--first-use query` (default) | 6/6 | no |
//! | `--serial-opens` | 6/6 | no — serialising `build()` |
//! | `--serial-connect` | 5/6 | no — serialising `connect()` |
//! | `--hold` | 6/6 | no — serialising teardown |
//! | `--sequential` | 4/6 | no — removing overlap |
//! | `--sequential --current-thread` | 2/8 | no — removing overlap *and* thread migration |
//!
//! # The conclusion, which is not the one the mitigation was built on
//!
//! **R15 is not a concurrency bug, in any of the three senses available.** Not
//! simultaneity: `--sequential` has no overlap anywhere. Not thread migration:
//! `--sequential --current-thread` is one task pinned to one thread, which is
//! the mechanism this risk row has listed as *unlocated* since 0.8.0. Not
//! teardown: `--hold` drops every handle one at a time in the parent. All three
//! still fault.
//!
//! What survives every arm is **cumulative `connect()`**. `build()` alone is
//! clean through ~880,000 opens per run; add `connect()` and the process dies
//! within a few thousand, however they are spread over threads and time.
//!
//! Two consequences, both load-bearing:
//!
//! * **`RUST_TEST_THREADS = "1"` cannot work by the mechanism recorded for it.**
//!   It serialises, and every arm here that serialises something still faults.
//!   That it lowered the observed rate is not in doubt; *why* is, and the
//!   likeliest reading is that it lowers connections-per-run rather than
//!   removing a race. A volume threshold also explains what the rate history
//!   could not: five published figures between 45% and 93% that were never
//!   competing estimates of one constant (D-147).
//! * **The production claim gets *stronger*, not weaker.** "One process, one
//!   file, a bounded set of connections opened once and never churned" is
//!   precisely the shape that never accumulates `connect()` calls, and it is
//!   consistent with the 500-sequential-opens-clean figure in the binding notes
//!   — 500 is simply well under the threshold. What is exposed is the test
//!   suite, which opens thousands of databases per run.
//!
//! # W1.2a — there is no threshold, and the near-miss is the lesson
//!
//! `--progress-file` makes the child record its running `connect()` total once
//! per round, so a faulting run reports where it died. The fault kills the
//! process, so the count has to be durable *before* the fault rather than
//! reported after it.
//!
//! Pooled over n = 20 at `--opens 16 --secs 15 --sequential --current-thread`,
//! release, in two sessions:
//!
//! ```text
//! faults (17):  2144  2272  2272  6544 10176 10928 17536 17536 17536
//!              17536 17536 17536 17536 17536 19520 33856 40416
//! clean  (3):  42416 44864 46384   <- all three hit the 15 s deadline
//! ```
//!
//! **Not a fixed cumulative count.** Fault positions span an order of magnitude
//! and the three clean runs stopped only because time ran out. That is the shape
//! of a **per-`connect()` probability** — very roughly 1 in 20,000–25,000 on this
//! machine — and not of a leak with a ceiling. So there is no magic number to
//! send upstream, and the reproducer is the deliverable instead.
//!
//! **Now read the 17,536 column, because it nearly went into a bug report.** In
//! the first session 8 of 11 faults landed on that exact value, which is not
//! what noise looks like and reads as a hard threshold. Re-running the
//! *identical* configuration produced it once in eight, against a spread of
//! 2,272–40,416. **The cluster did not replicate.** It is left in the table above
//! rather than dropped, because an instrument that manufactures a convincing
//! exact constant is worth knowing about — this is [D-124]'s noise-band finding
//! arriving in a new coordinate, and the only thing that caught it was running
//! the same configuration twice before believing it.
//!
//! [D-124]: ../docs/architecture/s13-decision-register.md#d-124
//!
//! # W1.3 — distinct databases, not just `connect()` calls
//!
//! Every arm above opened a fresh file per iteration, so *cumulative
//! `connect()`* and *cumulative distinct databases* moved together and neither
//! could be separated from the other. `--same-file` holds one path and reopens
//! it. Same config otherwise, `--opens 16 --secs 15 --sequential
//! --current-thread`, release, n = 8:
//!
//! | | faults | where the run got to |
//! |---|---|---|
//! | distinct file per open | 6/8 | faults from 2,272; clean runs 42,416–44,864 |
//! | **`--same-file`** | **1/8** | one fault at 30,448; 7 clean runs **40,352–43,360** |
//!
//! Read the *counts*, not the rates — they are the robust half. At ~40,000
//! reconnections to one database, seven runs in eight are fine; against distinct
//! databases the process is usually dead well before 20,000, sometimes by 2,272.
//!
//! **So the dominant variable is distinct databases, and `connect()` is the call
//! that pays for them.** Reopening one file is not immune (1/8), so this refines
//! the volume finding rather than replacing it.
//!
//! It also sharpens who is exposed, in the direction that matters here: this
//! crate's production shape — one file, a bounded set of connections opened once
//! — is the *safest* point on both axes at once. The test suite, which creates a
//! fresh temporary database per case, is the worst on both.
//!
//! # Running it
//!
//! ```text
//! cargo run --release --example r15_soak -- --arm claim   --secs 20 --runs 10
//! cargo run --release --example r15_soak -- --arm control --secs 20 --runs 10
//!
//! # W1.1 — walk the variables. Change ONE thing between any two runs.
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --first-use build
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --first-use connect
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --serial-opens
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --serial-connect
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --hold
//! cargo run --release --example r15_soak -- --arm storm --runs 6 --sequential
//!
//! # The arm that rules out concurrency outright
//! cargo run --release --example r15_soak -- --arm storm --runs 8 \
//!     --sequential --current-thread
//!
//! # W1.2a — where the fault lands. Run it TWICE before believing a cluster.
//! cargo run --release --example r15_soak -- --arm storm --runs 12 --secs 15 \
//!     --opens 16 --sequential --current-thread
//! ```
//!
//! Every comparison here is against the default `storm` run at the **same**
//! `--opens`. Two flags at once measures nothing — with the one exception above,
//! where `--sequential --current-thread` is deliberately both halves of a single
//! claim: no overlap *and* no migration.

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
    /// Concurrent opens per round in the `storm` arm. `control` keeps
    /// [`CONTROL_OPENS`], because its threshold argument is calibrated on it.
    opens: usize,
    /// Serialise the `build()` call through a mutex — W1.2's hypothesis as
    /// written, and measured to make no difference (6/6 faults either way).
    serial_opens: bool,
    /// Serialise `connect()` instead. W1.2's hypothesis *corrected* by W1.1:
    /// `--first-use build` never faults and `--first-use connect` always does,
    /// so `connect` looked like where a serialising mitigation would have to go.
    /// Measured at 5/6 — no better than nothing, which is what motivated
    /// [`Args::hold`].
    serial_connect: bool,
    /// Keep every handle alive until the round ends, then drop them one at a
    /// time in the parent.
    ///
    /// The variable neither serialising flag controls. Each opener drops its
    /// `Connection` and `Database` at the end of its own task, so **teardown is
    /// concurrent no matter which call is serialised** — which is the one
    /// explanation consistent with `--serial-connect` still faulting at 5/6
    /// while `--first-use build` never faults at ~880,000 opens per run.
    hold: bool,
    /// No concurrency at all: one task, open→connect→query→drop in a loop.
    ///
    /// The last variable W1.1 names — *open count regardless of timing*. Every
    /// other flag serialises one call while leaving the rest concurrent; this
    /// removes concurrency entirely at the same per-round count, so a clean run
    /// says the fault needs simultaneity and a faulting run says it needs only
    /// volume. Nothing else here distinguishes those two.
    sequential: bool,
    /// Build a current-thread runtime instead of a multi-thread one.
    ///
    /// **`--sequential` alone does not rule out concurrency, and this is why.**
    /// A single task on a multi-thread runtime still migrates across worker
    /// threads at every `.await`, so connections can be created on one OS thread
    /// and dropped on another with no overlap anywhere. That is a different
    /// mechanism from simultaneity and the risk row has named it as unlocated
    /// since 0.8.0. `--sequential --current-thread` is the arm that separates
    /// them: one task, one thread, no migration, no overlap.
    current_thread: bool,
    /// Reopen one file instead of a fresh file per open (W1.3).
    ///
    /// Distinguishes *cumulative `connect()`* from *cumulative distinct
    /// databases*, which every arm so far has varied together. It also decides
    /// how simple the upstream reproducer can be, and it bears directly on this
    /// crate's own exposure claim: a caller who reopens one database in a loop
    /// is in a different regime from a test suite that makes thousands.
    same_file: bool,
    /// Where the child records its running `connect()` total (W1.2a).
    ///
    /// The fault kills the process, so the child's own summary line never
    /// prints on the runs that matter — the count has to be *durable before*
    /// the fault, not reported after it. Rewritten once per round, which is
    /// cheap next to a round of opens and gives resolution of `--opens`.
    progress_file: Option<String>,
    /// How far each opener goes: `build`, `connect`, or `query`.
    ///
    /// Three steps, because the first measurement showed they are not one thing:
    /// `build` alone runs ~880,000 times in two seconds without faulting, which
    /// is fast enough to suspect it of being lazy rather than safe. Splitting
    /// `connect` out is what distinguishes "opening is fine" from "`build` does
    /// nothing until you connect".
    first_use: String,
}

fn parse() -> Args {
    let mut a = Args {
        arm: "claim".into(),
        secs: 20,
        runs: 1,
        child: false,
        opens: CONTROL_OPENS,
        serial_opens: false,
        serial_connect: false,
        hold: false,
        sequential: false,
        current_thread: false,
        same_file: false,
        progress_file: None,
        first_use: "query".into(),
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
            "--opens" => {
                a.opens = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--serial-opens" => {
                a.serial_opens = true;
                i += 1;
            }
            "--serial-connect" => {
                a.serial_connect = true;
                i += 1;
            }
            "--hold" => {
                a.hold = true;
                i += 1;
            }
            "--sequential" => {
                a.sequential = true;
                i += 1;
            }
            "--current-thread" => {
                a.current_thread = true;
                i += 1;
            }
            "--first-use" => {
                a.first_use = argv[i + 1].clone();
                assert!(
                    matches!(a.first_use.as_str(), "build" | "connect" | "query"),
                    "--first-use must be build, connect or query"
                );
                i += 2;
            }
            // Kept as sugar for the original two-way split.
            "--no-first-query" => {
                a.first_use = "build".into();
                i += 1;
            }
            "--same-file" => {
                a.same_file = true;
                i += 1;
            }
            "--progress-file" => {
                a.progress_file = Some(argv[i + 1].clone());
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

/// The file this opener touches.
///
/// Under `--same-file` every opener in every round uses one path, which turns
/// "cumulative connect()" and "cumulative distinct databases" from one variable
/// into two.
fn storm_path(dir: &std::path::Path, round: u64, i: usize, same: bool) -> std::path::PathBuf {
    if same {
        dir.join("storm-single.db")
    } else {
        dir.join(format!("storm-{round}-{i}.db"))
    }
}

/// One round of concurrent opens, with the three variables W1.1 separates.
///
/// Returns how many opens completed. Errors are swallowed deliberately: R15 is a
/// process fault, so anything that comes back as a `Result` is by definition not
/// the thing being measured, and failing the round on it would turn an unrelated
/// I/O error into a false R15 signal.
async fn open_round(dir: &std::path::Path, round: u64, args: &Args) -> u64 {
    // `tokio::sync::Mutex` rather than `std`: it is held across `.build()`,
    // which is an await point.
    let gate = Arc::new(tokio::sync::Mutex::new(()));

    // The no-concurrency arm short-circuits the whole spawn/join machinery
    // rather than driving it with a width of one: a single task that still goes
    // through `tokio::spawn` is not the same process state as a task that never
    // spawned, and the difference is exactly what is under test here.
    if args.sequential {
        let mut held = Vec::new();
        for i in 0..args.opens {
            let p = storm_path(dir, round, i, args.same_file);
            let Ok(d) = libsql::Builder::new_local(&p).build().await else {
                continue;
            };
            if args.first_use == "build" {
                if args.hold {
                    held.push((d, None));
                }
                continue;
            }
            let Ok(c) = d.connect() else { continue };
            if args.first_use == "query" {
                let _ = c.query("SELECT 1", ()).await;
            }
            if args.hold {
                held.push((d, Some(c)));
            }
        }
        drop(held);
        return args.opens as u64;
    }

    let mut batch = Vec::new();
    for i in 0..args.opens {
        let p = storm_path(dir, round, i, args.same_file);
        let gate = Arc::clone(&gate);
        let serial = args.serial_opens;
        let serial_connect = args.serial_connect;
        let first_use = args.first_use.clone();
        let hold = args.hold;
        batch.push(tokio::spawn(async move {
            // Each guard covers exactly one call and nothing after it. Widening
            // either to cover the rest would test a much stronger claim than the
            // one being made, and a clean run under a wide lock would not say
            // which call needed it.
            let d = {
                let _g = if serial { Some(gate.lock().await) } else { None };
                libsql::Builder::new_local(&p).build().await
            };
            let Ok(d) = d else { return None };
            if first_use == "build" {
                return hold.then_some((d, None));
            }
            let c = {
                let _g = if serial_connect {
                    Some(gate.lock().await)
                } else {
                    None
                };
                d.connect()
            };
            let Ok(c) = c else { return None };
            if first_use == "query" {
                let _ = c.query("SELECT 1", ()).await;
            }
            // Under `--hold` the handles travel back to the parent and are
            // dropped there, one at a time, after every task has joined. Without
            // it they drop here, concurrently with 47 other teardowns.
            hold.then_some((d, Some(c)))
        }));
    }
    let mut held = Vec::new();
    for h in batch {
        if let Ok(Some(handles)) = h.await {
            held.push(handles);
        }
    }
    // Sequential, and explicitly so: `Vec::drop` would also be sequential, but
    // writing it out is the difference between the property being tested and the
    // property being incidental to a container's implementation.
    for (d, c) in held.drain(..) {
        drop(c);
        drop(d);
    }
    args.opens as u64
}

fn main() {
    let args = parse();
    if args.child || args.runs <= 1 {
        run_arm(&args);
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
    let shape = if args.arm == "storm" {
        format!(
            " opens={} serial_build={} serial_connect={} hold={} seq={} current_thread={} first_use={}",
            args.opens,
            args.serial_opens,
            args.serial_connect,
            args.hold,
            args.sequential,
            args.current_thread,
            args.first_use
        )
    } else {
        String::new()
    };
    println!(
        "R15 soak: arm={} secs={} runs={}{shape}\n{}\n",
        args.arm,
        args.secs,
        args.runs,
        "-".repeat(60)
    );

    let mut faults = 0usize;
    let mut other = 0usize;
    // W1.2a: cumulative `connect()` counts. A constant total across different
    // `--opens` says leak or handle-table exhaustion; a spread says a
    // per-call probability. Nothing else here distinguishes those.
    let mut fault_counts: Vec<u64> = Vec::new();
    let mut clean_counts: Vec<u64> = Vec::new();
    let progress = std::env::temp_dir().join(format!("r15-progress-{}", std::process::id()));
    for run in 1..=args.runs {
        let t = Instant::now();
        let _ = std::fs::remove_file(&progress);
        let mut child = std::process::Command::new(&exe);
        child.args(["--progress-file", &progress.to_string_lossy()]);
        child.args([
            "--child",
            "--arm",
            &args.arm,
            "--secs",
            &args.secs.to_string(),
            "--opens",
            &args.opens.to_string(),
        ]);
        if args.serial_opens {
            child.arg("--serial-opens");
        }
        if args.serial_connect {
            child.arg("--serial-connect");
        }
        if args.hold {
            child.arg("--hold");
        }
        if args.sequential {
            child.arg("--sequential");
        }
        if args.current_thread {
            child.arg("--current-thread");
        }
        if args.same_file {
            child.arg("--same-file");
        }
        child.args(["--first-use", &args.first_use]);
        let status = child.status().expect("spawn child");
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
        // Read before the next run wipes it. A faulting child leaves the last
        // round boundary it got past, so this is a floor on where the fault
        // landed, accurate to within `--opens`.
        let reached: Option<u64> = std::fs::read_to_string(&progress)
            .ok()
            .and_then(|t| t.trim().parse().ok());
        match (code, reached) {
            (Some(5) | Some(-1073741819), Some(n)) => fault_counts.push(n),
            (Some(0), Some(n)) => clean_counts.push(n),
            _ => {}
        }
        println!(
            "  run {run:>3}/{:<3}  {:>6.1}s  exit {:>12}  connects {:>8}  {verdict}",
            args.runs,
            t.elapsed().as_secs_f64(),
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            reached.map(|n| n.to_string()).unwrap_or_else(|| "-".into())
        );
    }
    let _ = std::fs::remove_file(&progress);

    println!("\n{}", "-".repeat(60));
    println!(
        "arm={}  faults {}/{}  other failures {}",
        args.arm, faults, args.runs, other
    );
    let summarise = |label: &str, v: &[u64]| {
        if v.is_empty() {
            return;
        }
        let (lo, hi) = (v.iter().min().unwrap(), v.iter().max().unwrap());
        let mean = v.iter().sum::<u64>() / v.len() as u64;
        println!(
            "  {label:<21} n={:<3} min={lo:<8} mean={mean:<8} max={hi}",
            v.len()
        );
    };
    summarise("connects at fault", &fault_counts);
    summarise("connects when clean", &clean_counts);
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
        ("storm", 0) if args.serial_opens => println!(
            "\nSerialised opens: 0/{} faults at opens={}. This is the result W1.2 \n\
             is looking for, and it is only evidence when read against a NON-serial \n\
             run at the same --opens that DID fault. Run that comparison before \n\
             putting a mutex anywhere near `open()`.",
            args.runs, args.opens
        ),
        ("storm", n) if args.serial_opens => println!(
            "\nSerialised opens still faulted {n}/{} at opens={}. The open is not \n\
             the whole story and a process-wide open mutex is not the mitigation. \n\
             Vary --no-first-query next: the fault may be in first use, not open.",
            args.runs, args.opens
        ),
        ("storm", 0) => println!(
            "\nStorm alone did not fault at opens={} in {} runs. If `control` faults \n\
             at the same count, the opens are not sufficient on their own and the \n\
             concurrent load is part of the trigger — which would make a mutex on \n\
             `open()` the wrong fix regardless of what --serial-opens shows.",
            args.opens, args.runs
        ),
        ("storm", n) => println!(
            "\nStorm alone faulted {n}/{} at opens={} with first_use={} seq={}. \n\
             The measured picture (see the module docs) is that this is driven by \n\
             cumulative connect() volume and NOT by concurrency: --sequential \n\
             faults too. If you are looking for a mitigation, a lock is not it. \n\
             The open question is whether the threshold is a fixed total or a rate.",
            args.runs, args.opens, args.first_use, args.sequential
        ),
        _ => {}
    }
    std::process::exit(if other > 0 { 1 } else { 0 });
}

fn run_arm(args: &Args) {
    let rt = if args.current_thread {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    rt.block_on(async move {
        match args.arm.as_str() {
            "claim" => soak(args.secs, false).await,
            "control" => soak(args.secs, true).await,
            "storm" => storm(args).await,
            other => panic!("unknown arm {other}"),
        }
    });
}

/// The open storm with nothing else running (W1.1).
///
/// No soak database, no readers, no actor. Everything `control` holds constant
/// is simply absent, so a fault here is attributable to the opens and a clean
/// run at the same `--opens` that `control` faults at is itself a finding: it
/// would mean the storm needs the concurrent *load* to provoke anything, and the
/// variable is not the open count at all.
///
/// Runs rounds until `--secs` elapses, so it is comparable to the other arms on
/// duration rather than on round count.
async fn storm(args: &Args) {
    let dir = tempfile::TempDir::new().unwrap();
    let deadline = Instant::now() + Duration::from_secs(args.secs);
    let mut round = 0u64;
    let mut opened = 0u64;
    while Instant::now() < deadline {
        opened += open_round(dir.path(), round, args).await;
        round += 1;
        // Durable before the next round, because the fault takes the process
        // and with it anything still sitting in a buffer. Truncate-and-write
        // rather than append: the parent wants the last value, not a log.
        if let Some(path) = &args.progress_file {
            let _ = std::fs::write(path, opened.to_string());
        }
    }
    eprintln!(
        "  [child] rounds={round} opens={opened} per_round={} serial_build={} serial_connect={} first_use={}",
        args.opens, args.serial_opens, args.serial_connect, args.first_use
    );
}

/// One long-lived `Database` under load, optionally alongside the open storm.
async fn soak(secs: u64, with_open_storm: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    // Cadence on: the cadence connection is the fourth one `open()` takes, and
    // the claim is about the set of connections a real application holds. Turning
    // it off would test a shape no application runs.
    let db = Arc::new(Database::open(dir.path().join("soak.db")).await.unwrap());

    // A little topology, so reads have something to walk.
    db.write_concepts(
        (0..500)
            .map(|i| {
                ConceptUpsert::new(format!("c{i:05}"), format!("C{i}"))
                    .content("body")
                    .valid_from(TS)
            })
            .collect(),
    )
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
            // Built here rather than taken from the command line, and that is
            // deliberate: `control`'s job is to be a *fixed* probe whose
            // threshold argument (see `CONTROL_OPENS`) stays true across runs.
            // Letting `--opens` or `--serial-opens` reach it would make a clean
            // control arm mean something different from run to run, and the
            // claim arm's verdict reads "the control provoked something" as a
            // constant. Vary the storm in the `storm` arm, which exists for it.
            let fixed = Args {
                arm: "control".into(),
                secs: 0,
                runs: 1,
                child: true,
                opens: CONTROL_OPENS,
                serial_opens: false,
                serial_connect: false,
                hold: false,
                sequential: false,
                current_thread: false,
                same_file: false,
                progress_file: None,
                first_use: "query".into(),
            };
            let mut round = 0u64;
            while Instant::now() < deadline {
                opens.fetch_add(open_round(&storm_dir, round, &fixed).await, Ordering::Relaxed);
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
