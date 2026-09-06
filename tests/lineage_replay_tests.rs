//! The blocking half of the lineage generator (0.15.26, [D-268], review A-5).
//!
//! `lineage_property_tests.rs` discovers; this replays. It is **not** behind
//! `property-tests`, which is the entire reason it exists: the generated binary
//! is quarantined ([D-236](../docs/architecture/s13-decision-register.md#d-236))
//! and its exit code is discarded, so a bug it found would go on being found,
//! reported, and ignored.
//!
//! # Why not `.proptest-regressions`
//!
//! Proptest already persists every failure it has seen and replays it before
//! generating a novel case, which looks like the automation this needs. It is
//! not, for two reasons and the first is fatal: the file belongs to the property
//! binary, so the replay happens inside the step whose result nothing reads. The
//! second is that a `cc <hash>` line reproduces a failure only while the
//! strategy and the proptest version agree with the day it was written, and
//! tells the next reader nothing about what was once wrong here. A history in
//! text survives a strategy change as a *readable case*, and this file will say
//! so loudly when one stops parsing.
//!
//! # What it costs
//!
//! One database per case file, opened deterministically, in the suite whose
//! CRASH retry already handles [R15](../docs/architecture/s13-decision-register.md#d-147).
//! That is the budget: case files are for defects that actually occurred, and a
//! directory that grows without an entry explaining each addition is a directory
//! that will be deleted wholesale by someone in a hurry.

#[path = "common/harness.rs"]
mod harness;
#[path = "common/lineage_ops.rs"]
mod lineage;

use std::time::Duration;

use harness::TestHarness;
use lineage::{cases_dir, history_strategy, parse, run_history, Op, BRANCHES, KEYS};
use macrame::util::Clock;
use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;

const STEP: Duration = Duration::from_secs(3_600);

/// Every `*.case` file, sorted, so a failure names the same file on every
/// machine.
fn case_files() -> Vec<std::path::PathBuf> {
    let dir = cases_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "case"))
        .collect();
    out.sort();
    out
}

/// **The gate.** Every promoted history, through the same runner that found it.
#[tokio::test]
async fn every_promoted_case_still_holds() {
    let files = case_files();
    assert!(
        !files.is_empty(),
        "no case files in {} — the directory is the gate, and an empty one \
         silently gates nothing",
        cases_dir().display()
    );

    for path in files {
        let text = std::fs::read_to_string(&path).expect("case file is readable");
        let history: Vec<Op> = parse(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            !history.is_empty(),
            "{}: parsed to no operations",
            path.display()
        );

        let h = TestHarness::new();
        let db = h.db_with_fake_clock().await;
        let advance = || h.advance(STEP);
        let now = || h.clock.now();
        let outcome = run_history(&db, &history, &advance, &now).await;
        let _ = db.close().await;

        if let Err(why) = outcome {
            panic!("{}: {why}", path.display());
        }
    }
}

/// **The generator is checked for reach, and it fails on too much as well as
/// too little.**
///
/// A bias is a probability, and probabilities drift when the op set changes. A
/// generator that has quietly stopped producing the shape it was written for
/// is worse than no generator at all: everything passes, and the passing is
/// what gets pointed at. So this draws histories with no database in sight and
/// counts the shapes.
///
/// The upper bound is not symmetry for its own sake. If nearly every history is
/// a branch shadowing the trunk at a shared key, the ordinary single-lineage
/// path — the one almost every real caller is on — has stopped being generated,
/// and the suite would be spending its whole budget on the exotic case while
/// reporting broad coverage.
///
/// No database, so this is deterministic, fast, and safely outside the
/// quarantine: it is a test of the strategy, and the strategy needs no engine.
#[test]
fn the_strategy_still_reaches_the_shapes_it_was_written_for() {
    const DRAWS: usize = 2_000;

    let mut runner = TestRunner::deterministic();
    let strategy = history_strategy();

    let mut shadow = 0; // a non-main lineage writing at a key the trunk seeds
    let mut inherited_retire = 0; // a non-main lineage retiring such a key
    let mut archive_with_fork = 0; // an archive after a fork
    let mut branch_archive_after_write = 0; // archive_branch on a branch that wrote
    let mut single_lineage = 0; // no fork at all: the ordinary path

    for _ in 0..DRAWS {
        let history = strategy.new_tree(&mut runner).unwrap().current();

        let mut forked = false;
        let mut written: Vec<usize> = Vec::new();
        let (mut s, mut r, mut a, mut ba) = (false, false, false, false);

        for op in &history {
            match *op {
                Op::Fork { child, .. } => {
                    // A fork the runner would refuse (child == main) is not a
                    // fork, and counting it here would let this test pass on a
                    // strategy that never produces a real one.
                    if BRANCHES[child % BRANCHES.len()] != "main" {
                        forked = true;
                    }
                }
                Op::Assert { key, branch, .. } => {
                    if branch % BRANCHES.len() != 0 {
                        written.push(branch % BRANCHES.len());
                        if key % KEYS.len() <= 1 {
                            s = true;
                        }
                    }
                }
                Op::Retire { key, branch } => {
                    if branch % BRANCHES.len() != 0 {
                        written.push(branch % BRANCHES.len());
                        if key % KEYS.len() <= 1 {
                            r = true;
                        }
                    }
                }
                Op::Archive => {
                    if forked {
                        a = true;
                    }
                }
                Op::ArchiveBranch { branch } => {
                    let b = branch % BRANCHES.len();
                    if b != 0 && written.contains(&b) {
                        ba = true;
                    }
                }
            }
        }

        shadow += usize::from(s);
        inherited_retire += usize::from(r);
        archive_with_fork += usize::from(a);
        branch_archive_after_write += usize::from(ba);
        single_lineage += usize::from(!forked);
    }

    // Each shape is drawn from a chain of events, so the floors are low on
    // purpose: what is being falsified is "this never happens any more", not a
    // particular rate. The names are what a failure prints, so they say what
    // stopped being reachable.
    let bounds: [(&str, usize, usize, usize); 5] = [
        ("a branch writing at a key the trunk seeds", shadow, 2, 98),
        ("a branch retiring an inherited key", inherited_retire, 2, 90),
        ("an archive with a live fork in the history", archive_with_fork, 2, 95),
        (
            "archive_branch on a branch that has written",
            branch_archive_after_write,
            1,
            80,
        ),
        ("a history that never forks", single_lineage, 1, 90),
    ];

    for (name, n, floor, ceiling) in bounds {
        let pct = n * 100 / DRAWS;
        assert!(
            pct >= floor,
            "{name}: {n}/{DRAWS} ({pct}%) — below the {floor}% floor. The \
             generator has stopped reaching this shape; fix the bias, not this \
             number."
        );
        assert!(
            pct <= ceiling,
            "{name}: {n}/{DRAWS} ({pct}%) — above the {ceiling}% ceiling. \
             Everything is now this shape, so whatever it was contrasted \
             against is no longer being generated."
        );
    }
}
