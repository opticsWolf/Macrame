//! The op vocabulary, the runner and the four properties, shared by the
//! generated binary and the replaying one (0.15.26, [D-268], review A-5).
//!
//! Two test binaries drive exactly this code and that is the point. The
//! generated one lives behind `property-tests` and is quarantined
//! ([D-236](../../docs/architecture/s13-decision-register.md#d-236)): it runs,
//! reports, and blocks nothing. A failure discovered there is worth nothing
//! until it is *replayed by something that blocks*, so
//! `lineage_replay_tests.rs` reads the committed case files, runs them through
//! this same runner, and is an ordinary member of the default suite.
//!
//! Putting the vocabulary here rather than in either binary is what makes that
//! honest. If the replay binary carried its own copy of `step`, a promoted case
//! would be replayed against a runner that could drift from the one that found
//! it, and the two would disagree silently — the failure mode
//! [D-030](../../docs/architecture/s13-decision-register.md#d-030) names,
//! arriving in the test tree instead of the SQL.
//!
//! # What the generator is for
//!
//! [D-229](../../docs/architecture/s13-decision-register.md#d-229) was found by
//! hand. Both of its symptoms need a branch writing at an **ancestor's exact
//! interval key**, and every generated history in this repository before this
//! one had a single lineage, so no generator could have reached it. That is the
//! whole content of A-5's third bullet, and the bias in `op_strategy` is the
//! part that answers it: a uniform draw over a realistic key space produces a
//! collision almost never, so the key pool is four wide and weighted hard
//! toward the two the trunk seeds.

#![allow(dead_code)] // each binary uses a different half

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use macrame::graph::EdgeAssertion;
use macrame::integrity::audit_current;
use macrame::{BranchId, ConceptUpsert, Database, ReadPlan};

pub const NODES: [&str; 4] = ["a", "b", "c", "d"];

pub const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
pub const T1: &str = "1970-01-02T00:00:00.000000Z";
pub const T2: &str = "1970-01-03T00:00:00.000000Z";
/// Where every property reads. See [`reach`].
pub const T3: &str = "1970-01-04T00:00:00.000000Z";
pub const OPEN: &str = "9999-12-31T23:59:59.999999Z";
/// An archive cutoff after every `recorded_at` any case can produce, so
/// `archive` takes everything its predicates will let it take. The predicates
/// are the thing under test; a timid cutoff would test the cutoff instead.
pub const LATE: &str = "2999-01-01T00:00:00.000000Z";

/// `main`, and two lineages a history may fork into existence.
///
/// Two rather than one because [D-229](../../docs/architecture/s13-decision-register.md#d-229)'s
/// closed-interval arm stands down when *another* lineage holds the same key,
/// and with a single branch that clause can never be exercised from both sides.
/// Three rather than four because every extra name widens the space a fork has
/// to hit before anything interesting can happen on it.
pub const BRANCHES: [&str; 3] = ["main", "x", "y"];

/// The interval keys, and the weights are the design.
///
/// A link's `entity_id` is `source|target|type|valid_from`, so these four
/// tuples *are* the key space this generator explores. The first two are what
/// [`seed`] writes on the trunk: a branch drawing one of them writes at an
/// ancestor's exact key, which is the precondition for both of D-229's
/// symptoms. The other two exist so that not every history is that shape —
/// a generator that only ever produces its own motivating bug has stopped
/// testing the ordinary path, which is what the coverage test at the foot of
/// `lineage_replay_tests.rs` fails on.
pub const KEYS: [(usize, usize, usize, &str); 4] = [
    (0, 1, 0, EPOCH), // a → b, the trunk's
    (1, 2, 0, EPOCH), // b → c, the trunk's
    (2, 3, 0, EPOCH), // c → d, nobody's yet
    (0, 2, 1, T1),    // a ⊂ c, a later valid_from
];

pub const TYPES: [&str; 2] = ["LEADSTO", "PARTOF"];

/// One operation a caller can reach through the public API.
///
/// Only public methods, which is `doctrine_property_tests.rs`'s rule and is
/// kept here for its reason: a doctrine that holds only when nobody uses the
/// API is not a doctrine. Indices rather than names so the whole history is
/// `Copy` and shrinks the way proptest expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `closed` writes `valid_to = T2` instead of the open sentinel — the
    /// shadow shape, when the branch is not `main` and the key is the trunk's.
    Assert {
        key: usize,
        branch: usize,
        closed: bool,
    },
    /// Closes the interval at `T2`. On a branch, at a key the branch inherited,
    /// this is the only cross-lineage retirement Doctrine III permits and is
    /// D-229's second symptom.
    Retire { key: usize, branch: usize },
    Fork { parent: usize, child: usize },
    /// Whole-ledger, at [`LATE`].
    Archive,
    ArchiveBranch { branch: usize },
}

pub fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

fn key_parts(k: usize) -> (&'static str, &'static str, &'static str, &'static str) {
    let (s, t, ty, vf) = KEYS[k % KEYS.len()];
    (NODES[s], NODES[t], TYPES[ty], vf)
}

/// The trunk: four concepts and the two edges whose keys the bias points at.
pub async fn seed(db: &Database) {
    db.write_concepts(
        NODES
            .iter()
            .map(|n| ConceptUpsert::new(*n, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();
    for k in [0usize, 1] {
        let (s, t, ty, vf) = key_parts(k);
        db.assert_edge(
            EdgeAssertion::new(s, t, ty)
                .valid_from(vf)
                .valid_to(OPEN),
        )
        .await
        .unwrap();
    }
}

/// Run one op. **Rejections are ignored, deliberately.**
///
/// The generator is free to fork a name that exists, retire an edge nobody
/// asserted, or archive `main`. Those are the schema and the API refusing, and
/// the refusal is the system working; the properties are about the state that
/// results either way. A generator that only proposed legal histories would be
/// asserting that the runner's idea of legality matches the crate's, which is
/// not the claim under test.
pub async fn step(db: &Database, op: Op, tree: &mut Lineages) {
    match op {
        Op::Assert { key, branch, closed } => {
            let (s, t, ty, vf) = key_parts(key);
            let mut e = EdgeAssertion::new(s, t, ty)
                .valid_from(vf)
                .valid_to(if closed { T2 } else { OPEN });
            let name = BRANCHES[branch % BRANCHES.len()];
            if name != "main" {
                e = e.on_branch(id(name));
            }
            let _ = db.assert_edge(e).await;
        }
        Op::Retire { key, branch } => {
            let (s, t, ty, vf) = key_parts(key);
            let name = BRANCHES[branch % BRANCHES.len()];
            let _ = if name == "main" {
                db.retire_edge(s, t, ty, vf, T2).await
            } else {
                db.retire_edge_on(s, t, ty, vf, T2, id(name)).await
            };
        }
        Op::Fork { parent, child } => {
            let p = BRANCHES[parent % BRANCHES.len()];
            let c = BRANCHES[child % BRANCHES.len()];
            // `fork(new, parent)`, in that order: the name being created
            // comes first. Reversed, every call is `BranchExists("main")` and
            // the whole generator quietly explores an unforked ledger — which
            // is exactly the blindness this file exists to end, so it is worth
            // the comment.
            if db.fork(id(c), id(p)).await.is_ok() {
                tree.record(p, c);
            }
        }
        Op::Archive => {
            let _ = db.archive(LATE).await;
        }
        Op::ArchiveBranch { branch } => {
            let name = BRANCHES[branch % BRANCHES.len()];
            if db.archive_branch(id(name)).await.is_ok() {
                tree.forget(name);
            }
        }
    }
}

/// Who forked from whom, as the runner saw it.
///
/// Kept in Rust rather than asked of `branches()` because after
/// `archive_branch` the row is gone and the question the property needs to ask
/// — *was this branch downstream of the one that just left?* — has no one left
/// to answer it.
#[derive(Debug, Default, Clone)]
pub struct Lineages {
    parent: BTreeMap<String, String>,
    live: Vec<String>,
}

impl Lineages {
    pub fn new() -> Self {
        Self {
            parent: BTreeMap::new(),
            live: vec!["main".to_string()],
        }
    }

    fn record(&mut self, parent: &str, child: &str) {
        self.parent.insert(child.to_string(), parent.to_string());
        if !self.live.iter().any(|b| b == child) {
            self.live.push(child.to_string());
        }
    }

    fn forget(&mut self, name: &str) {
        self.live.retain(|b| b != name);
    }

    pub fn live(&self) -> &[String] {
        &self.live
    }

    /// `name`, and everything that forked from it, transitively.
    pub fn subtree(&self, name: &str) -> Vec<String> {
        let mut out = vec![name.to_string()];
        let mut grew = true;
        while grew {
            grew = false;
            for (child, parent) in &self.parent {
                if out.iter().any(|b| b == parent) && !out.iter().any(|b| b == child) {
                    out.push(child.clone());
                    grew = true;
                }
            }
        }
        out
    }
}

/// What one lineage **currently believes**, at [`T3`], as sorted key strings.
///
/// # The instant is load-bearing
///
/// An archive is entitled to take a closed interval that lies entirely behind
/// the cutoff, so a reading taken *inside* such an interval legitimately
/// changes across an archive and would make the property below fail on correct
/// behaviour. Every interval this vocabulary can close ends at `T2`, so `T3` is
/// after all of them: what survives there is what the ledger still believes,
/// which is the thing an archive must never alter.
///
/// # And so is the *absence* of `recorded_at`, which cost an afternoon
///
/// The first version of this asked with an explicit transaction instant, on the
/// reasoning that naming both axes is the more precise question. It is the more
/// precise question and it is the wrong instrument here, because a plan
/// carrying `recorded_at` lowers to the **log fold** rather than to
/// `links_current` — and the fold is derived from `transaction_log`, which
/// [D-229](../../docs/architecture/s13-decision-register.md#d-229)'s links
/// symptom does not touch. Measured against the reverted clause: with
/// `recorded_at` the trunk still reports the edge whose `links` row the archive
/// had just deleted; without it, the trunk correctly stops reaching it. **The
/// property passed against the defect it was written for**, and the difference
/// between the two readings is one builder call.
///
/// That is D-229's own last paragraph arriving from a new direction: the only
/// instrument that shows a ledger missing rows is one that reads the ledger.
/// Every other surface is derived from something the deletion left alone, and
/// derived answers agree with each other whatever the ledger has lost.
pub async fn reach(db: &Database, branch: &str) -> Vec<String> {
    let mut v: Vec<String> = db
        .edges(ReadPlan::new().on(id(branch)).valid_at(T3))
        .await
        .map(|es| {
            es.into_iter()
                .map(|e| format!("{}|{}", e.entity_id(), e.branch_id))
                .collect()
        })
        // A lineage the reader refuses to answer for contributes nothing; the
        // before/after comparison below then compares two empty answers, which
        // is vacuous rather than wrong. `run_history` only asks about branches
        // it watched being created, so this arm is not the ordinary path.
        .unwrap_or_default();
    v.sort();
    v
}

/// Every live lineage's belief, keyed by name.
pub async fn reach_all(db: &Database, tree: &Lineages) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for b in tree.live() {
        out.insert(b.clone(), reach(db, b).await);
    }
    out
}

/// The fold and the lowering, asked the same question.
///
/// `reconstruct_on` resolves the ancestry in Rust; `Database::edges` lowers it
/// through `graph::plan` into SQL. They are two implementations of one rule
/// ([D-259](../../docs/architecture/s13-decision-register.md#d-259)), and a
/// generated history is the only thing that asks them the same question often
/// enough for a disagreement to surface.
pub async fn fold_agrees(db: &Database, branch: &str, recorded: &str) -> Result<(), String> {
    let folded = match db.reconstruct_on(recorded, branch).await {
        Ok(s) => s,
        // A lineage the fold refuses to answer for is not a disagreement; the
        // lowering is asked the same thing below and would refuse too.
        Err(_) => return Ok(()),
    };
    let mut got: Vec<String> = folded
        .edges
        .into_iter()
        .filter(|e| e.valid_from.as_str() <= T3 && T3 < e.valid_to.as_str())
        .map(|e| format!("{}|{}", e.entity_id(), e.branch_id))
        .collect();
    got.sort();
    let mut want: Vec<String> = match db
        .edges(
            ReadPlan::new()
                .on(id(branch))
                .recorded_at(recorded)
                .valid_at(T3),
        )
        .await
    {
        Ok(es) => es
            .into_iter()
            .map(|e| format!("{}|{}", e.entity_id(), e.branch_id))
            .collect(),
        Err(_) => return Ok(()),
    };
    want.sort();
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "`{branch}` at recorded {recorded}: the fold says {got:?}, the lowering says {want:?}"
        ))
    }
}

/// Doctrine VI, after every op: `links_current` is the image of `links`.
///
/// Cheap, and included knowing it is **silent across D-229** — the archive
/// deletes from `links` and re-derives `links_current` from what survives, so
/// the projection agrees with a ledger that has lost rows. It is here because
/// the ops this generator adds are ones nothing else drives at this volume,
/// not because it can see the defect the file is named for.
pub async fn audit_is_silent(db: &Database) -> Result<(), String> {
    match audit_current(db.read_conn()).await {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("audit_current reported {n} rows of drift")),
        Err(e) => Err(format!("audit_current failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// The case-file format
// ---------------------------------------------------------------------------

/// Where a promoted case lives. One directory, read whole by the replay binary.
pub fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("lineage_cases")
}

/// One op per line, names rather than indices.
///
/// The format is text and not proptest's seed file on purpose. A
/// `.proptest-regressions` line is a hash: it reproduces the failure and tells
/// the next reader nothing about what was once wrong here, and it reproduces it
/// only for as long as the strategy and the proptest version agree with the day
/// it was written. A history that says what it does survives both.
pub fn render(history: &[Op]) -> String {
    let mut s = String::new();
    for op in history {
        match *op {
            Op::Assert { key, branch, closed } => {
                let _ = writeln!(
                    s,
                    "assert key={} branch={} closed={}",
                    key % KEYS.len(),
                    BRANCHES[branch % BRANCHES.len()],
                    u8::from(closed)
                );
            }
            Op::Retire { key, branch } => {
                let _ = writeln!(
                    s,
                    "retire key={} branch={}",
                    key % KEYS.len(),
                    BRANCHES[branch % BRANCHES.len()]
                );
            }
            Op::Fork { parent, child } => {
                let _ = writeln!(
                    s,
                    "fork parent={} child={}",
                    BRANCHES[parent % BRANCHES.len()],
                    BRANCHES[child % BRANCHES.len()]
                );
            }
            Op::Archive => {
                let _ = writeln!(s, "archive");
            }
            Op::ArchiveBranch { branch } => {
                let _ = writeln!(s, "archive_branch branch={}", BRANCHES[branch % BRANCHES.len()]);
            }
        }
    }
    s
}

fn branch_index(name: &str) -> Result<usize, String> {
    BRANCHES
        .iter()
        .position(|b| *b == name)
        .ok_or_else(|| format!("unknown branch `{name}`"))
}

fn field<'a>(tok: &'a str, want: &str) -> Result<&'a str, String> {
    tok.strip_prefix(want)
        .and_then(|r| r.strip_prefix('='))
        .ok_or_else(|| format!("expected `{want}=…`, found `{tok}`"))
}

/// Parse a case file. Blank lines and `#` comments are skipped, so a promoted
/// case can carry the sentence explaining why it exists.
pub fn parse(text: &str) -> Result<Vec<Op>, String> {
    let mut out = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let t: Vec<&str> = line.split_whitespace().collect();
        let at = |e: String| format!("line {}: {e}", n + 1);
        let op = match t[0] {
            "assert" if t.len() == 4 => Op::Assert {
                key: field(t[1], "key").map_err(at)?.parse().map_err(|_| at("bad key".into()))?,
                branch: branch_index(field(t[2], "branch").map_err(at)?).map_err(at)?,
                closed: field(t[3], "closed").map_err(at)? != "0",
            },
            "retire" if t.len() == 3 => Op::Retire {
                key: field(t[1], "key").map_err(at)?.parse().map_err(|_| at("bad key".into()))?,
                branch: branch_index(field(t[2], "branch").map_err(at)?).map_err(at)?,
            },
            "fork" if t.len() == 3 => Op::Fork {
                parent: branch_index(field(t[1], "parent").map_err(at)?).map_err(at)?,
                child: branch_index(field(t[2], "child").map_err(at)?).map_err(at)?,
            },
            "archive" if t.len() == 1 => Op::Archive,
            "archive_branch" if t.len() == 2 => Op::ArchiveBranch {
                branch: branch_index(field(t[1], "branch").map_err(at)?).map_err(at)?,
            },
            other => return Err(at(format!("unknown op `{other}`"))),
        };
        out.push(op);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The runner both binaries call
// ---------------------------------------------------------------------------

/// Run a whole history, checking every property after every op.
///
/// `advance` moves the harness clock and `now` reads it. They are closures
/// rather than a `TestHarness` parameter so this module does not depend on the
/// harness module — two `#[path]` includes that also include each other is a
/// tangle, and neither binary needs it.
///
/// **The clock moves after every op** because `recorded_at` is what a fork's
/// cutoff and both archive predicates compare on. With a still clock every row
/// in a case shares one transaction instant, "the newer assertion at this key"
/// has no answer, and a history that looks like it forks after a write does
/// not actually do so. The generator would then be exploring a space where its
/// motivating defect cannot occur.
pub async fn run_history(
    db: &Database,
    history: &[Op],
    advance: &dyn Fn(),
    now: &dyn Fn() -> String,
) -> Result<(), String> {
    let mut tree = Lineages::new();
    seed(db).await;
    advance();

    for (i, op) in history.iter().enumerate() {
        let where_ = |e: String| format!("after op {i} ({op:?}): {e}");

        // Only the two archiving ops carry a before/after claim, and taking the
        // reading unconditionally would double the reads for every case.
        let before = match op {
            Op::Archive | Op::ArchiveBranch { .. } => Some(reach_all(db, &tree).await),
            _ => None,
        };
        let doomed = match op {
            Op::ArchiveBranch { branch } => tree.subtree(BRANCHES[branch % BRANCHES.len()]),
            _ => Vec::new(),
        };

        step(db, *op, &mut tree).await;
        advance();
        let stamp = now();

        audit_is_silent(db).await.map_err(where_)?;

        for b in tree.live() {
            fold_agrees(db, b, &stamp).await.map_err(where_)?;
        }

        if let Some(before) = before {
            let after = reach_all(db, &tree).await;
            for (name, was) in &before {
                if doomed.iter().any(|d| d == name) {
                    // `archive_branch` is *supposed* to take this lineage's
                    // rows, and a descendant reads through the ancestor it just
                    // lost. Both are the operation working.
                    continue;
                }
                let Some(is) = after.get(name) else { continue };
                if was != is {
                    return Err(where_(format!(
                        "`{name}` believed {was:?} and now believes {is:?} — \
                         an archive is a move, not a retirement"
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The strategy, and the bias that is the point of it
// ---------------------------------------------------------------------------

use proptest::prelude::*;

/// Draw a key, weighted **hard** toward the two the trunk seeds.
///
/// This is the difference between a generator that would have found
/// [D-229](../../docs/architecture/s13-decision-register.md#d-229) and one that
/// merely looks like it would. Both symptoms need a branch writing at an
/// ancestor's exact interval key; uniform over four keys that is one write in
/// four, and uniform over any *realistic* key space it is never. 5:5:1:1 puts
/// five sixths of the writes on a key some other lineage may already hold,
/// while leaving a sixth that nobody shares so the ordinary path is still
/// exercised. The coverage test bounds this on both sides rather than trusting
/// the ratio to stay true when the op set changes.
fn key_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![5 => Just(0usize), 5 => Just(1usize), 1 => Just(2usize), 1 => Just(3usize)]
}

/// Which lineage writes. `main` is a third of the draws, because a branch
/// shadowing a key the trunk never wrote to is not the interesting shape.
fn branch_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![3 => Just(0usize), 4 => Just(1usize), 2 => Just(2usize)]
}

pub fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (key_strategy(), branch_strategy(), any::<bool>())
            .prop_map(|(key, branch, closed)| Op::Assert { key, branch, closed }),
        3 => (key_strategy(), branch_strategy()).prop_map(|(key, branch)| Op::Retire { key, branch }),
        // Parent skewed to `main`: a chain deeper than one rung is worth
        // reaching, but a history that never forks off the trunk never gets a
        // second lineage onto the seeded keys at all.
        3 => (prop_oneof![4 => Just(0usize), 1 => Just(1usize), 1 => Just(2usize)], 1..BRANCHES.len())
            .prop_map(|(parent, child)| Op::Fork { parent, child }),
        2 => Just(Op::Archive),
        1 => (0..BRANCHES.len()).prop_map(|branch| Op::ArchiveBranch { branch }),
    ]
}

/// **Few cases, long histories** — the R15 trade, made deliberately.
///
/// Each case opens a database, and cumulative `connect()` is what
/// [R15](../../docs/architecture/s13-decision-register.md#d-147) counts
/// ([D-148](../../docs/architecture/s13-decision-register.md#d-148)), so the
/// lever that buys coverage without buying crashes is ops per open rather than
/// opens. A fork has to happen before a branch write can collide with anything,
/// and an archive after both, so a six-op history barely reaches the shape this
/// file is for; twenty reaches it repeatedly.
pub fn history_strategy() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op_strategy(), 6..20)
}
