//! Generated branch histories, and the four things that must stay true across
//! one (0.15.26, [D-268], review A-5).
//!
//! Every property this codebase checked by generation before this file ran on a
//! graph with **one lineage**. `doctrine_property_tests` draws five ops, none of
//! which forks; `integrity_property_tests` has an archive-shaped strategy and no
//! branch. So the whole branch wave — D-214 through D-232, a schema version and
//! two archive predicates — was generated-tested only in the shape where every
//! row belongs to `main`.
//!
//! [D-229](../docs/architecture/s13-decision-register.md#d-229) is what that
//! cost. Both of its symptoms need a branch writing at an ancestor's exact
//! interval key: the archive predicates asked for *"a later assertion for the
//! same interval key"* without asking whose, so one `archive` made the trunk
//! stop reaching a node it believed in. It was found by hand, while preparing
//! something else. A-5's claim is that a generator would have found it, and the
//! bias in [`history_strategy`] is that claim made concrete — the key pool is
//! four wide and weighted five-to-one toward the two the trunk seeds, because a
//! uniform draw over any realistic key space reaches the collision never.
//!
//! **The claim is verified rather than asserted.** Reverting D-229's
//! `newer.branch_id = links.branch_id` clause in a scratch tree makes the first
//! property below fail, with a named case, well inside the budget. The
//! measurement is in D-268; a generator that has never caught its own motivating
//! defect is decoration.
//!
//! # This binary is quarantined, and the promotion is why that is survivable
//!
//! Like its two siblings it sits behind the `property-tests` feature and CI runs
//! it as a step that reports and blocks nothing
//! ([D-236](../docs/architecture/s13-decision-register.md#d-236)). A discovery
//! that only ever lands somewhere the exit code is discarded is not a gate. So a
//! failure here writes the offending history to `tests/lineage_cases/` as text,
//! and `lineage_replay_tests.rs` — an ordinary, unconditional member of the
//! suite — replays every file in that directory through the same runner. Commit
//! the file and the case blocks releases from then on, with nobody copying
//! anything by hand.

#[path = "common/harness.rs"]
mod harness;
#[path = "common/lineage_ops.rs"]
mod lineage;

use std::future::Future;

use harness::TestHarness;
use lineage::{history_strategy, run_history, Op};
use macrame::util::Clock;
use proptest::prelude::*;
use std::time::Duration;

/// One op is one hour, which only has to be larger than the clock's resolution.
const STEP: Duration = Duration::from_secs(3_600);

/// One runtime for the whole binary — the same R15 mitigation, for the same
/// reason, as `doctrine_property_tests.rs`. See the long note there; it is not
/// restated here, because a rate written down twice is a rate that will
/// disagree with itself.
static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn block_on<F: Future>(f: F) -> F::Output {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    })
    .block_on(f)
}

/// Write the failing history where the blocking binary will find it.
///
/// Called on every failing run, so under shrinking it is called repeatedly and
/// the **last** write is the minimised case — which is the one proptest reports
/// and the one worth keeping. The name is fixed rather than unique on purpose:
/// a directory that accumulates one file per shrink step is a directory nobody
/// commits.
///
/// A write failure is swallowed. The property has already failed and proptest's
/// own report is the primary signal; turning a read-only checkout into a
/// different error message would hide it.
fn promote(history: &[Op], why: &str) {
    let dir = lineage::cases_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = format!(
        "# Written by lineage_property_tests.rs, which is quarantined.\n\
         #\n\
         # Commit this file to make the case block releases: lineage_replay_tests.rs\n\
         # runs every *.case in this directory through the same runner and is not\n\
         # feature-gated. Rename it to say what it is, and add a line saying why.\n\
         #\n\
         # {why}\n\n{}",
        lineage::render(history)
    );
    let _ = std::fs::write(dir.join("pending.case"), body);
}

proptest! {
    // Sixteen. See `history_strategy` for the trade: ops per open, not opens.
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    /// **The four properties, over one generated history.**
    ///
    /// They are one test rather than four because each one is checked after
    /// every op and three of the four are cheap reads of the state the fourth
    /// already has open. Splitting them would multiply the databases by four
    /// for no extra reach, and databases are the thing R15 counts.
    ///
    /// What is checked, after every operation:
    ///
    /// 1. **An archive never changes what any lineage believes.** The reading
    ///    is taken before and after each `archive`, at an instant later than
    ///    every interval this vocabulary can close — see `reach`, where the
    ///    choice of instant is the difference between a property and a false
    ///    alarm. This is the one D-229 fails, in both of its shapes.
    /// 2. **`audit_current` reports zero.** Included knowing it is blind to
    ///    D-229 by construction; it is here for the ops nothing else drives at
    ///    this volume.
    /// 3. **The fold and the lowering agree** for every live lineage:
    ///    `reconstruct_on` resolves ancestry in Rust, `Database::edges` lowers
    ///    it into SQL, and D-259 shipped both.
    /// 4. **`archive_branch` leaves every other lineage alone** — the same
    ///    before/after reading, excluding the archived branch and everything
    ///    that forked from it, for which losing rows is the operation working.
    #[test]
    fn a_generated_branch_history_keeps_its_four_invariants(history in history_strategy()) {
        let outcome = block_on(async {
            let h = TestHarness::new();
            let db = h.db_with_fake_clock().await;
            let advance = || h.advance(STEP);
            let now = || h.clock.now();
            let r = run_history(&db, &history, &advance, &now).await;
            // Deliberately, not `Drop`: `Database` has none, so dropping one
            // detaches the write actor while it still owns the sole write
            // connection. A binary that opens thousands should close them.
            let _ = db.close().await;
            r
        });
        if let Err(why) = &outcome {
            promote(&history, why);
        }
        prop_assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    }
}
