//! One lineage's view of a fold (0.15.17, W16.1, [D-259], review C-10).
//!
//! `reconstruct` answers a whole-ledger question and `MaterializedState::edges`
//! carries every lineage's belief with nothing to finish it with. C-10's
//! complaint was not that the answer was wrong — it is the right answer to the
//! question asked — but that **the rule for narrowing it lived only as SQL**, so
//! a caller holding a state had no way to get from it to *what did this branch
//! believe*, short of reimplementing `visible_cte` by hand.
//!
//! `resolve_beliefs` and `reconstruct_on` are the two halves that close it, and
//! this file is the argument that they agree with the SQL they restate.
//!
//! # The oracle, and why it is `ReadPlan` and not the traversal
//!
//! [`Database::edges`] with a plan naming a lineage **and** a recorded instant
//! is the same question in SQL: it lowers through `graph::plan`, so it is the
//! resolution the whole crate reads through, not a second copy written for this
//! test. It is also the only reader that takes both instants without an anchor
//! node — a traversal would answer for a neighbourhood and leave every edge
//! outside it untested, which on a differential test is a way to pass without
//! having compared much.
//!
//! The two answers are shaped differently on purpose and the test bridges that
//! rather than hiding it: `reconstruct_on` returns every belief the lineage
//! holds, at every valid interval, and the plan returns the ones live at one
//! valid instant. So the comparison filters the fold's answer by the interval
//! predicate the plan's SQL applies, `valid_from <= v AND v < valid_to`, and if
//! that filter were wrong the *first* test below would fail on the trunk, where
//! there is nothing to resolve and the two must agree exactly.
//!
//! [D-259]: ../docs/architecture/s13-decision-register.md#d-259

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::branch::Ancestor;
use macrame::error::DbError;
use macrame::graph::EdgeAssertion;
use macrame::temporal::{resolve_beliefs, EdgeBelief};
use macrame::util::Clock;
use macrame::{BranchId, ConceptUpsert, Database, ReadPlan};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
/// Where `exp` closes the edge it inherited from the trunk.
const SHADOWED: &str = "1970-01-01T00:30:00.000000Z";
const STEP: Duration = Duration::from_secs(3_600);

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

async fn seed(db: &Database, names: &[&str]) {
    db.write_concepts(
        names
            .iter()
            .map(|n| ConceptUpsert::new(*n, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();
}

async fn edge(db: &Database, s: &str, t: &str, branch: Option<&str>) {
    let mut e = EdgeAssertion::new(s, t, "LEADSTO")
        .valid_from(EPOCH)
        .valid_to(OPEN);
    if let Some(b) = branch {
        e = e.on_branch(id(b));
    }
    db.assert_edge(e).await.unwrap();
}

/// A key, without the lineage: what the two answers are compared on.
fn keys(mut beliefs: Vec<EdgeBelief>) -> Vec<String> {
    beliefs.sort();
    beliefs
        .into_iter()
        .map(|e| format!("{}|{}", e.entity_id(), e.branch_id))
        .collect()
}

/// The fold's answer, narrowed to the valid instant the plan reads at.
fn live_at(beliefs: Vec<EdgeBelief>, valid: &str) -> Vec<EdgeBelief> {
    beliefs
        .into_iter()
        .filter(|e| e.valid_from.as_str() <= valid && valid < e.valid_to.as_str())
        .collect()
}

/// **The differential this half of the release exists to pass.**
///
/// Every lineage, at every instant the fixture has a write at, against the
/// lowering every other reader in the crate goes through.
async fn agrees_with_the_plan(db: &Database, branch: &str, recorded: &str, at: &str) {
    let folded = db.reconstruct_on(recorded, branch).await.unwrap();
    let planned = db
        .edges(
            ReadPlan::new()
                .on(id(branch))
                .recorded_at(recorded)
                .valid_at(at),
        )
        .await
        .unwrap();
    let (got, want) = (keys(live_at(folded.edges, at)), keys(planned));
    assert_eq!(got, want, "`{branch}` at recorded {recorded}, valid {at}");
    // Two empty answers agree, and would agree for any implementation. Every
    // instant this file sweeps has at least one edge visible from every
    // lineage: `a→b` is asserted before the fork, and `exp`'s shadow of it is
    // live at `EPOCH` even after the divergence. So an empty comparison here is
    // a broken fixture, not a passing test.
    assert!(
        !got.is_empty(),
        "vacuous: `{branch}` sees nothing at {recorded}"
    );
}

/// The trunk, a fork, and writes on both sides of the fork point.
///
/// Laid out so every arm of the resolution is exercised by the sweep below:
/// an edge the branch **inherits** (`a→b`, pre-fork), one the trunk added
/// **after** the fork and the branch must not see (`c→d`), one the branch added
/// on its own (`b→c`), and one the branch **shadows** at a key the trunk also
/// holds (`a→b`, re-asserted on `exp` with a closed interval).
///
/// The shadow is here rather than in the one test about it because without it
/// no key in this fixture is held by two lineages — and a resolution that
/// picked the *farthest* lineage instead of the nearest passed the whole sweep.
/// That was not hypothetical; it is what mutating the comparison did before
/// this edge was added.
async fn forked_and_diverged(h: &TestHarness) -> (Database, Vec<String>) {
    let db = h.db_with_fake_clock().await;
    let mut marks = Vec::new();

    seed(&db, &["a", "b", "c", "d"]).await;
    edge(&db, "a", "b", None).await;
    h.advance(STEP);
    marks.push(h.clock.now()); // seeded, not yet forked

    db.fork(id("exp"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    marks.push(h.clock.now()); // forked, nothing diverged

    // Both sides move after the fork, which is the only configuration in which
    // the cutoff and the distance rule can be told apart from doing nothing.
    edge(&db, "c", "d", None).await;
    edge(&db, "b", "c", Some("exp")).await;
    // `exp` shadows the inherited key with a closed interval: the one
    // cross-lineage retirement that does not close the ancestor's row.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(EPOCH)
            .valid_to(SHADOWED)
            .on_branch(id("exp")),
    )
    .await
    .unwrap();
    h.advance(STEP);
    marks.push(h.clock.now()); // diverged
    marks.push("2999-01-01T00:00:00.000000Z".to_string());

    (db, marks)
}

// ---------------------------------------------------------------------------
// the fold against the lowering
// ---------------------------------------------------------------------------

/// An unforked database: `reconstruct_on` is `reconstruct`, and both are the
/// plan's answer. This is the arm that would catch a wrong interval filter in
/// the bridge above, so it runs first.
#[tokio::test]
async fn on_the_trunk_of_an_unforked_ledger_nothing_changes() {
    let h = TestHarness::new();
    let db = h.db_with_fake_clock().await;
    seed(&db, &["a", "b", "c"]).await;
    edge(&db, "a", "b", None).await;
    edge(&db, "b", "c", None).await;
    h.advance(STEP);
    let now = h.clock.now();

    let whole = db.reconstruct(&now).await.unwrap();
    let one = db.reconstruct_on(&now, "main").await.unwrap();
    assert_eq!(whole.edges, one.edges, "one lineage, so nothing to narrow");
    assert_eq!(whole.concepts.len(), one.concepts.len());

    agrees_with_the_plan(&db, "main", &now, EPOCH).await;
    db.close().await.unwrap();
}

/// Every lineage of the diverged fixture, at every instant it was written at.
#[tokio::test]
async fn the_resolved_fold_agrees_with_the_lowering_at_every_instant() {
    let h = TestHarness::starting_at(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));
    let (db, marks) = forked_and_diverged(&h).await;

    // Four instants: before the fork, after it with nothing diverged, after the
    // divergence, and far in the future. The cutoff is a no-op at the first two
    // and bites at the last two, so a `reconstruct_on` that ignored it would
    // pass half this sweep — which is why the sweep is the whole of it.
    assert_eq!(marks.len(), 4);
    for branch in ["main", "exp"] {
        for recorded in &marks {
            agrees_with_the_plan(&db, branch, recorded, EPOCH).await;
        }
    }
    db.close().await.unwrap();
}

/// The cutoff, stated directly, so a failure says which rule broke.
///
/// The sweep above would catch this and report it as "the two answers differ",
/// which is true and one step away from what went wrong.
#[tokio::test]
async fn a_branch_does_not_see_what_the_trunk_wrote_after_the_fork() {
    let h = TestHarness::starting_at(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));
    let (db, _) = forked_and_diverged(&h).await;
    let now = h.clock.now();

    let whole = db.reconstruct(&now).await.unwrap();
    let on_exp = db.reconstruct_on(&now, "exp").await.unwrap();

    let has = |s: &[EdgeBelief], src: &str| s.iter().any(|e| e.source_id == src);
    assert!(has(&whole.edges, "c"), "the ledger holds the trunk's `c→d`");
    assert!(
        !has(&on_exp.edges, "c"),
        "`exp` forked before it and must not inherit it: {:?}",
        on_exp.edges
    );
    assert!(has(&on_exp.edges, "a"), "`a→b` is pre-fork and inherited");
    assert!(has(&on_exp.edges, "b"), "`b→c` is the branch's own");
    db.close().await.unwrap();
}

/// The branch's own row wins at a key the trunk also holds, and the trunk's
/// row is untouched — Doctrine III's shadowing, seen from the fold.
#[tokio::test]
async fn the_nearest_lineage_wins_at_a_shared_key() {
    let h = TestHarness::starting_at(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));
    let (db, marks) = forked_and_diverged(&h).await;
    let now = marks[2].clone();

    let on_exp = db.reconstruct_on(&now, "exp").await.unwrap();
    let shadowed: Vec<_> = on_exp
        .edges
        .iter()
        .filter(|e| e.source_id == "a" && e.target_id == "b")
        .collect();
    assert_eq!(shadowed.len(), 1, "one belief per key: {shadowed:?}");
    assert_eq!(shadowed[0].branch_id, "exp", "the nearer lineage");
    assert_eq!(shadowed[0].valid_to, SHADOWED);

    // And the trunk still believes what it believed.
    let on_main = db.reconstruct_on(&now, "main").await.unwrap();
    let trunk: Vec<_> = on_main
        .edges
        .iter()
        .filter(|e| e.source_id == "a" && e.target_id == "b")
        .collect();
    assert_eq!(trunk.len(), 1);
    assert_eq!(trunk[0].valid_to, OPEN, "the ancestor's row is not closed");

    agrees_with_the_plan(&db, "exp", &now, EPOCH).await;
    db.close().await.unwrap();
}

/// An unregistered lineage is refused by name, not answered for the trunk.
#[tokio::test]
async fn an_unknown_lineage_is_refused_by_name() {
    let h = TestHarness::new();
    let (db, _) = forked_and_diverged(&h).await;
    let now = h.clock.now();

    match db.reconstruct_on(&now, "ghost").await {
        Err(DbError::UnknownBranch(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownBranch, got {other:?}"),
    }
    match db.ancestry("ghost").await {
        Err(DbError::UnknownBranch(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownBranch, got {other:?}"),
    }
    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// the pure half, on its own
// ---------------------------------------------------------------------------

/// The ancestry a caller gets is the one the readers resolve against.
#[tokio::test]
async fn the_published_ancestry_is_the_one_the_reader_uses() {
    let h = TestHarness::starting_at(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));
    let (db, _) = forked_and_diverged(&h).await;

    let anc = db.ancestry("exp").await.unwrap();
    assert_eq!(anc.len(), 2);
    assert_eq!(anc[0].branch_id, "exp");
    assert_eq!(anc[0].dist, 0);
    assert_eq!(anc[0].cutoff, None, "the reader has no cutoff");
    assert_eq!(anc[1].branch_id, "main");
    assert_eq!(anc[1].dist, 1);
    assert!(anc[1].cutoff.is_some(), "`main` is cut at the fork point");

    // The trunk of a forked ledger is a root: itself, and nothing above it.
    let anc = db.ancestry("main").await.unwrap();
    assert_eq!(anc.len(), 1);
    assert_eq!(anc[0].branch_id, "main");
    db.close().await.unwrap();
}

fn belief(src: &str, branch: &str) -> EdgeBelief {
    EdgeBelief::new(src, "t", "LEADSTO", EPOCH, OPEN).on_branch(branch)
}

fn ancestor(name: &str, dist: i64) -> Ancestor {
    Ancestor::new(name, dist)
}

/// Nearest wins, a lineage outside the ancestry is dropped, and the answer does
/// not depend on the order the beliefs arrived in.
///
/// Written against the function rather than against a database because that is
/// the point of it being pure: these are the three properties a caller relies on
/// and none of them needs a connection to state.
#[test]
fn the_pure_resolution_picks_by_distance_and_ignores_strangers() {
    let anc = [ancestor("exp", 0), ancestor("main", 1)];

    // One key, held by both lineages, plus a sibling nobody asked about.
    let held = vec![
        belief("a", "main"),
        belief("a", "exp"),
        belief("a", "sibling"),
    ];
    let out = resolve_beliefs(&held, &anc);
    assert_eq!(out.len(), 1, "one row per key: {out:?}");
    assert_eq!(out[0].branch_id, "exp", "dist 0 beats dist 1");

    // The same input, reversed. A `HashMap` iterated for the winner rather than
    // compared would pass one of these two and not the other.
    let reversed: Vec<_> = held.iter().rev().cloned().collect();
    assert_eq!(resolve_beliefs(&reversed, &anc), out);

    // A key only the ancestor holds is inherited; a key only a stranger holds
    // is not there at all.
    let out = resolve_beliefs(&[belief("b", "main"), belief("c", "sibling")], &anc);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].source_id, "b");

    // An empty ancestry sees nothing, which is what "no lineage is visible"
    // means and is not the same as "resolve nothing and pass everything".
    assert!(resolve_beliefs(&held, &[]).is_empty());
}

// ---------------------------------------------------------------------------
// the cold arm
// ---------------------------------------------------------------------------

/// The bounded fold reaches the archive, and reads the same answer through it.
///
/// `reconstruct_on` spells its own two folds — the ancestry has to be joined in
/// before the window picks a winner, which no existing constant does — and the
/// cold one is a `UNION ALL` of two files under one cutoff predicate. That is
/// the half a hot-only suite never compiles a row through, and the failure it
/// would hide is not a crash: it is a lineage quietly losing the rows that
/// moved.
///
/// # Reaching the arm is the hard part of this test
///
/// With an archive file beside the log, reach is `NeedsArchive` only when the
/// **newest surviving hot stamp is after the instant asked for** — so a read at
/// "now" takes the hot arm however much was archived, and the first version of
/// this test passed while never compiling the cold SQL at all. It is asserted
/// here rather than reasoned about: the same read is issued before and after
/// the archive, at an instant early enough to force the crossing, and the two
/// answers must agree.
#[tokio::test]
async fn a_lineages_view_survives_the_rows_moving_to_cold_storage() {
    let h = TestHarness::starting_at(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000));
    let (db, marks) = forked_and_diverged(&h).await;

    // The seeded instant, before the fork: early enough that the divergence
    // writes are all *after* it, which is what makes the hot log stop covering
    // once an archive exists.
    let early = marks[0].clone();
    let before = keys(db.reconstruct_on(&early, "exp").await.unwrap().edges);
    assert!(!before.is_empty(), "the fixture has something to lose");

    h.advance(STEP);
    let report = db.archive(&h.clock.now()).await.unwrap();
    assert!(
        db.reconstruct(&early).await.is_ok(),
        "the whole-ledger read crosses the boundary too, and is the control"
    );

    let after = keys(db.reconstruct_on(&early, "exp").await.unwrap().edges);
    assert_eq!(
        before, after,
        "an archive is a move, not a retirement (report: {report:?})"
    );

    // And the cutoff still holds on the far side: `exp` forked after `early`,
    // so at `early` it sees the trunk's seed and nothing of the divergence.
    let view = db.reconstruct_on(&early, "exp").await.unwrap();
    assert!(
        !view.edges.iter().any(|e| e.source_id == "c"),
        "the trunk's post-fork edge is still not inherited: {:?}",
        view.edges
    );
    db.close().await.unwrap();
}
