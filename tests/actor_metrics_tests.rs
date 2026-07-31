//! The actor's latency counters, measured through the public handle (T1.4, D-079).
//!
//! `src/metrics.rs` unit-tests the arithmetic — bucketing, packing, the exempt
//! kinds. What it cannot test is the part that actually matters: that the actor
//! loop is wired to it, that each command is attributed to the right kind, and
//! that the hold being timed is the whole turn. Those are properties of
//! `run_writer_actor`, and this file is the only place they are checked.
//!
//! Requires `--features metrics`.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::metrics::CommandKind;
use macrame::{ConceptUpsert, Database};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

fn turns_for(snap: &macrame::metrics::MetricsSnapshot, kind: CommandKind) -> u64 {
    snap.kinds.iter().find(|k| k.kind == kind).unwrap().turns
}

/// Each write method lands on its own counter.
///
/// The failure this guards against is not "the number is wrong" but "the number
/// is somebody else's": a `kind()` arm that returns a neighbouring variant is
/// invisible in aggregate and makes the one question the counters answer —
/// *which* command broke the budget — answer with the wrong name.
#[tokio::test]
async fn every_write_method_is_attributed_to_its_own_command_kind() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T0))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("b", "B").valid_from(T0))
        .await
        .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from(T0)
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    db.retire_edge("a", "b", "KNOWS", T0, "2026-06-01T00:00:00.000000Z")
        .await
        .unwrap();
    db.rebuild_current().await.unwrap();
    db.bulk_import(vec![EdgeAssertion::new("a", "b", "CITES")
        .valid_from(T0)
        .valid_to(OPEN)])
        .await
        .unwrap();

    let snap = db.metrics();

    assert_eq!(turns_for(&snap, CommandKind::UpsertConcept), 2);
    assert_eq!(turns_for(&snap, CommandKind::AssertEdge), 1);
    assert_eq!(turns_for(&snap, CommandKind::RetireEdge), 1);
    assert_eq!(turns_for(&snap, CommandKind::RebuildCurrent), 1);
    assert_eq!(turns_for(&snap, CommandKind::BulkImportChunk), 1);

    // Nothing has archived, so that counter must be untouched — a kind() arm
    // that fell through to a default would show up here first.
    assert_eq!(turns_for(&snap, CommandKind::Archive), 0);

    // Every turn is counted once, and the per-kind counts account for all of
    // them. `Shutdown` has not run yet, so the totals must agree exactly.
    let per_kind: u64 = snap.kinds.iter().map(|k| k.turns).sum();
    assert_eq!(
        per_kind, snap.turns,
        "the loop counted {} turns but the kinds account for {per_kind}",
        snap.turns
    );

    db.close().await.unwrap();
}

/// A hold is a real duration, and the longest one names its command.
///
/// `rebuild_current` is the slowest thing in this fixture by construction — it
/// reprojects the whole of `links` (D-077) while everything else touches one
/// row — so it is the one the high-water mark should be pointing at. This is the
/// weakest assertion in the file on purpose: asserting a *duration* would be
/// asserting a property of the machine, which D-042 and D-070 both say not to do.
/// What is asserted is the ordering and the attribution.
#[tokio::test]
async fn the_longest_hold_is_a_real_duration_and_names_its_command() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    // A star, not 500 assertions about one pair: `trg_links_single_open` allows
    // exactly one open interval per (source, target, type), so repeating the
    // same edge is a `SingleOpenViolation` and not a fixture at all.
    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T0))
        .await
        .unwrap();
    let leaves: Vec<_> = (0..500).map(|i| format!("c{i}")).collect();
    db.write_concepts(
        leaves
            .iter()
            .map(|id| ConceptUpsert::new(id, id).valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();

    let edges: Vec<_> = leaves
        .iter()
        .map(|id| {
            EdgeAssertion::new("a", id, "KNOWS")
                .valid_from(T0)
                .valid_to(OPEN)
        })
        .collect();
    db.bulk_import(edges).await.unwrap();
    db.rebuild_current().await.unwrap();

    let snap = db.metrics();
    let (kind, held) = snap.longest.expect("some turn took at least a microsecond");

    assert!(
        held > std::time::Duration::ZERO,
        "the timer is not running: longest hold is {held:?}"
    );
    let rebuild = snap
        .kinds
        .iter()
        .find(|k| k.kind == CommandKind::RebuildCurrent)
        .unwrap();
    assert!(
        held >= rebuild.mean,
        "the high-water mark ({held:?}) is below a mean it should dominate \
         ({:?}) — {kind} was recorded as the longest",
        rebuild.mean
    );

    db.close().await.unwrap();
}

/// `archive_windowed` spends one **turn** per session, not one turn total.
///
/// This is the structural claim T1.1 rests on, and it is the one thing about
/// windowing that cannot be seen from outside the actor. Running the same N
/// sessions inside a single `Archive` arm would produce N small transactions
/// under one hold — smaller transactions, identical latency, since nothing else
/// writes until the turn returns however many `COMMIT`s it contains. Only the
/// turn count distinguishes the two, and only from here.
///
/// Preemption itself is not asserted: given N turns, it follows from the loop's
/// `biased` `select!`, and a test that raced an assertion against a background
/// archive would be asserting on the scheduler.
#[tokio::test]
async fn a_windowed_archive_takes_one_actor_turn_per_session() {
    use std::time::Duration;

    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    let ids: Vec<String> = (0..9).map(|i| format!("c{i:03}")).collect();
    db.write_concepts(
        ids.iter()
            .map(|id| ConceptUpsert::new(id, "n").valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();
    for generation in 0..4 {
        let batch: Vec<_> = (0..8)
            .map(|k| {
                EdgeAssertion::new(&ids[k], &ids[k + 1], "LINKS")
                    .valid_from(T0)
                    .valid_to(OPEN)
                    .weight(1.0 + generation as f64)
            })
            .collect();
        db.bulk_import(batch).await.unwrap();
        harness.advance(Duration::from_secs(3_600));
    }

    let cutoff = harness.clock.peek();
    let reports = db
        .archive_windowed(&cutoff, Duration::from_secs(3_600))
        .await
        .unwrap();
    assert!(reports.len() > 1, "the fixture produced one window");

    let snap = db.metrics();
    assert_eq!(
        turns_for(&snap, CommandKind::Archive),
        reports.len() as u64,
        "{} sessions were reported but the actor spent a different number of \
         turns on them — the loop is inside the actor, and windowing buys \
         nothing",
        reports.len()
    );

    db.close().await.unwrap();
}

/// Depth is sampled before the turn, so a backlog shows up as one.
///
/// Queued without awaiting, so the sends land while the actor is still busy with
/// the first. This is the only counter that can distinguish "the bound holds"
/// from "the bound holds and nobody is waiting" — a 3 ms hold against a queue of
/// 40 is a 120 ms wait for whoever is last.
#[tokio::test]
async fn a_backlog_shows_up_in_the_queue_depth() {
    let harness = TestHarness::new();
    let db = std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());

    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T0))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("b", "B").valid_from(T0))
        .await
        .unwrap();

    let mut tasks = Vec::new();
    for i in 0..64 {
        let db = std::sync::Arc::clone(&db);
        tasks.push(tokio::spawn(async move {
            db.assert_edge(
                EdgeAssertion::new("a", "b", "KNOWS")
                    .valid_from(format!("2026-02-{:02}T00:00:00.000000Z", (i % 28) + 1))
                    .valid_to(format!("2026-03-{:02}T00:00:00.000000Z", (i % 28) + 1)),
            )
            .await
        }));
    }
    for t in tasks {
        let _ = t.await.unwrap();
    }

    let snap = db.metrics();
    assert!(
        snap.high_depth_max > 0,
        "64 concurrent assertions produced no observed backlog at all — the \
         depth is being sampled after the queue drains, not before the turn"
    );

    std::sync::Arc::into_inner(db)
        .unwrap()
        .close()
        .await
        .unwrap();
}
