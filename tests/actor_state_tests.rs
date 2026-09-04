//! What the write actor remembers between turns, and the ways remembering it
//! could be wrong (W14.3, [D-248], review C-6, C-24, A-3).
//!
//! Until 0.15.6 the actor held a connection and nothing else, so every fact a
//! turn established died with it. [`ActorState`] keeps three of them — the
//! lineages, the overlap guard, and `INSERT_LINK` — and the argument that this
//! is safe is one sentence: **the actor is the only writer** (D-014). A cache
//! whose only writer is holding it cannot go stale behind its own back.
//!
//! One sentence is exactly the kind of argument that is true when it is written
//! and false two releases later, so it is pinned here rather than asserted. The
//! failure modes a cache adds are not the ones the existing suites look for:
//!
//! * **A stale answer.** The lineages are read once and reused, so a lineage
//!   forked or forgotten on one turn must be visible to the next. The existing
//!   branch suites write on a fork immediately after making it and would catch
//!   the first half; nothing covered the second, because before this release
//!   there was nothing that could get it wrong.
//! * **A held cursor.** The guard is now a statement that outlives its turn,
//!   and the overlap arm returns from inside the row loop. A statement left
//!   mid-scan is what makes SQLite refuse to *end a transaction* — so the
//!   command that fails is not the one that left it, and the symptom appears on
//!   the archive path, two turns and one diagnosis away from the cause.
//! * **A stale plan.** The statements are held across sessions that `ATTACH`
//!   and `DETACH` a second database.
//!
//! [`ActorState`]: macrame
//! [D-248]: ../docs/architecture/s13-decision-register.md#d-248

#[path = "common/harness.rs"]
mod harness;

use std::time::Duration;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::graph::EdgeAssertion;
use macrame::{BranchId, ConceptUpsert, Database};

const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const HOUR: Duration = Duration::from_secs(3_600);

fn id(name: &str) -> BranchId {
    BranchId::new(name).expect("valid branch name")
}

fn edge(source: &str, target: &str) -> EdgeAssertion {
    EdgeAssertion::new(source, target, "LINKS")
        .valid_from(EPOCH)
        .valid_to(OPEN)
}

async fn seeded(h: &TestHarness) -> Database {
    let db = h.db_with_fake_clock().await;
    db.write_concepts(
        ["a", "b", "c"]
            .iter()
            .map(|n| ConceptUpsert::new(*n, "n").valid_from(EPOCH))
            .collect(),
    )
    .await
    .unwrap();
    db
}

/// A refused overlap must not leave the guard mid-scan.
///
/// The assertion at the end is the point and it is not about edges at all: the
/// archive opens a transaction on the actor's connection and commits it, and a
/// statement with rows still to yield is what makes SQLite refuse the commit.
/// The refusal in the middle is the setup — [`check_prepared`] returns from
/// inside its row loop there, which is the one exit that used to leave the
/// statement live.
///
/// It passes when the guard is compiled per call, too. That is the reason to
/// have it: this file exists because holding the statement is new, and a test
/// that only fails on the new code would have to be written after the bug.
#[tokio::test]
async fn an_archive_after_a_refused_overlap_still_commits() {
    let h = TestHarness::new();
    let db = seeded(&h).await;

    // Two **closed** intervals: the guard's overlap arm is the one that
    // catches those, and `defer_to_single_open` hands the open sentinel to
    // `trg_links_single_open` instead — which would refuse this from the
    // trigger, one layer below the statement under test.
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LINKS")
            .valid_from(EPOCH)
            .valid_to("1970-01-01T01:00:00.000000Z"),
    )
    .await
    .unwrap();

    // Refused from inside the guard's row loop: same key, overlapping the
    // interval above, and a `valid_from` the guard's `<> ?4` does not exclude.
    let overlapping = EdgeAssertion::new("a", "b", "LINKS")
        .valid_from("1970-01-01T00:30:00.000000Z")
        .valid_to("1970-01-01T02:00:00.000000Z");
    assert!(
        matches!(
            db.assert_edge(overlapping).await,
            Err(DbError::OverlappingInterval { .. })
        ),
        "the fixture must actually reach the guard's overlap arm"
    );

    h.advance(HOUR);
    db.archive("1970-01-01T00:15:00.000000Z")
        .await
        .expect("a live statement on the connection is what refuses a commit");

    // And the ledger is still writable afterwards, which is the other half of
    // "the statement was returned to its initial state" rather than finalized.
    db.assert_edge(edge("a", "c")).await.unwrap();
}

/// A lineage forgotten on one turn must be unknown on the next.
///
/// `archive_branch` deletes the row from `branches`, and the cached copy is
/// what the next write's existence check reads. Refused **by name** is the
/// whole point: the alternative is a write that reaches the overlap guard and
/// is judged against an ancestry that no longer exists.
#[tokio::test]
async fn a_write_naming_a_forgotten_lineage_is_refused_by_name() {
    let h = TestHarness::new();
    let db = seeded(&h).await;

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(edge("a", "b").on_branch(id("alt")))
        .await
        .unwrap();

    db.archive_branch(id("alt")).await.unwrap();

    match db.assert_edge(edge("a", "c").on_branch(id("alt"))).await {
        Err(DbError::UnknownBranch(name)) => assert_eq!(name, "alt"),
        other => panic!("expected UnknownBranch(\"alt\"), got {other:?}"),
    }
}

/// A lineage forked on one turn must be writable on the next.
///
/// The branch suites cover this incidentally; it is stated here because it is
/// the *other* direction of the same cache, and a `forget_lineages` deleted
/// from `Fork` alone would leave those suites passing on whichever of them
/// happens to open a fresh database per test.
#[tokio::test]
async fn a_write_on_a_lineage_forked_this_turn_is_accepted() {
    let h = TestHarness::new();
    let db = seeded(&h).await;

    // Before the fork, so the cache is populated with the pre-fork `branches`.
    db.assert_edge(edge("a", "b")).await.unwrap();

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(edge("a", "c").on_branch(id("alt")))
        .await
        .expect("the lineage exists, and the actor wrote it itself one turn ago");
}

/// The guard is keyed on the shape, so a fork must re-key it.
///
/// A guard cached without its shape does not error — that is the trouble with
/// it. [`check_prepared`] binds by the shape it was *stored* with, so a `Trunk`
/// guard held across a fork keeps asking the trunk's question: `links_current`
/// with no lineage predicate at all. It then sees a row the trunk cannot see
/// and refuses a write there is nothing wrong with, which is review C-7 and
/// [D-244] arriving through the cache instead of through the SQL.
///
/// So the assertion is on the **verdict**, not on the call succeeding. The
/// branch's belief about `a → c` and the trunk's are separate rows by
/// construction (`links_current` is keyed on `branch_id` since v12), and the
/// trunk asserting over the same interval is the ordinary case.
///
/// [D-244]: ../docs/architecture/s13-decision-register.md#d-244
#[tokio::test]
async fn the_guard_is_recompiled_when_the_fork_changes_its_shape() {
    let h = TestHarness::new();
    let db = seeded(&h).await;

    // Compiles and caches the four-parameter trunk guard, on a database where
    // the trunk's question and the resolved question have the same answer.
    db.assert_edge(edge("a", "b")).await.unwrap();

    db.fork(id("alt"), BranchId::main()).await.unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LINKS")
            .valid_from(EPOCH)
            .valid_to("1970-01-01T01:00:00.000000Z")
            .on_branch(id("alt")),
    )
    .await
    .unwrap();

    // The trunk, over an interval overlapping the branch's — and the trunk
    // cannot see the branch, so there is no overlap to find.
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LINKS")
            .valid_from("1970-01-01T00:30:00.000000Z")
            .valid_to("1970-01-01T02:00:00.000000Z"),
    )
    .await
    .expect("the shape moved under the cached guard, which must be recompiled");
}
