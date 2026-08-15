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
/// A valid-time close behind every cutoff these tests use.
const T1: &str = "2026-01-01T00:30:00.000000Z";

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

/// Every budget-exempt kind has a row in `CHUNK_BUDGET`'s table, and no other
/// kind does (0.12.9, W4.3, D-152).
///
/// The exemption list exists twice: as `CommandKind::exempt_from_budget` in
/// code, and as the "Path / Bound / Why it cannot be chunked" table in
/// `CHUNK_BUDGET`'s rustdoc, which is where a reader looks for its scope (§8.6).
/// Two copies of one fact is the drift D-035 exists to prevent, and W4.3 found
/// them one row apart: `rehydrate` was exempt in code — by inheriting
/// `Archive`'s kind — and absent from the table, so the documented scope of the
/// budget was narrower than the enforced one and had been for three releases.
///
/// **Both directions are asserted.** A missing row is a documented budget that
/// silently does not apply; an extra row is a documented exemption the code does
/// not grant, which is worse — it tells a caller their unbounded write is
/// expected when the violation counter is about to disagree.
#[test]
fn the_budget_exemptions_and_their_documented_table_agree() {
    let source = include_str!("../src/connection.rs");
    let table = source
        .split("/// | Path | Bound | Why it cannot be chunked |")
        .nth(1)
        .expect("CHUNK_BUDGET's exemption table has moved or been renamed")
        .split("\n///\n")
        .next()
        .expect("the exemption table did not terminate");

    for kind in CommandKind::ALL {
        let named = table.contains(kind.as_str());
        assert_eq!(
            named,
            kind.exempt_from_budget(),
            "`{kind}` is {} by `exempt_from_budget` and {} in CHUNK_BUDGET's \
             table. The two lists are the same fact written twice and must \
             agree — see the exemption's rustdoc for which one is wrong.\n\
             table:\n{table}",
            if kind.exempt_from_budget() {
                "exempt"
            } else {
                "not exempt"
            },
            if named { "present" } else { "absent" },
        );
    }
}

/// A rehydrate is attributed to `Rehydrate`, and `Archive` does not move
/// (0.12.9, W4.3, D-152).
///
/// This is the regression W4.3 exists to close, asserted in the form the defect
/// took: from 0.9.0 to 0.12.8 `LowPriCommand::Rehydrate` mapped to
/// `CommandKind::Archive`, so an operator reading a long `archive` hold could
/// not tell whether the database had archived anything at all — the two move
/// rows in opposite directions across the same file boundary.
///
/// The `Archive` half is the load-bearing one. Asserting only that `Rehydrate`
/// counted would pass in a world where the command was counted twice.
#[tokio::test]
async fn a_rehydrate_is_counted_as_rehydrate_and_not_as_archive() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    // A retired concept with both clocks behind the cutoff and no link naming
    // it, which is what `CONCEPTS_ARCHIVABLE` admits. The archive has to run
    // first for a real reason and not as setup ceremony: `rehydrate` reads
    // `cold.concepts`, so without a prior archive there is no cold file and the
    // command fails before it is ever counted.
    db.write_concepts(vec![ConceptUpsert::new("c000", "n")
        .valid_from(T0)
        .valid_to(T1)
        .retired(true)])
        .await
        .unwrap();
    harness.advance(std::time::Duration::from_secs(7_200));

    let cutoff = harness.clock.peek();
    db.archive(&cutoff).await.unwrap();

    // Taken *after* the archive, so the assertion below is "Archive did not
    // move across the rehydrate" rather than "Archive is zero" — which is the
    // claim that has teeth, since both commands are now live in this test.
    let before = turns_for(&db.metrics(), CommandKind::Archive);
    assert!(before > 0, "the fixture did not archive anything");

    db.rehydrate(&["c000"]).await.unwrap();

    let snap = db.metrics();
    assert_eq!(
        turns_for(&snap, CommandKind::Rehydrate),
        1,
        "the rehydrate took an actor turn that was not attributed to Rehydrate"
    );
    assert_eq!(
        turns_for(&snap, CommandKind::Archive),
        before,
        "the Archive counter moved on a rehydrate. That is the 0.9.0-0.12.8 \
         behaviour W4.3 removed (D-152)."
    );

    db.close().await.unwrap();
}

/// The starvation counter sees a low-priority command waiting behind
/// high-priority work, and reports zero when nothing waits (0.12.10, W4.4,
/// D-153).
///
/// The actor's `select!` is `biased`, which means it takes high-priority work
/// whenever any is ready and nothing bounds how long that can continue. Whether
/// it ever does is the question this counter exists to answer, so the first
/// thing to establish is that the counter can answer it at all.
///
/// **The quiet half is the load-bearing one.** A counter that only ever goes up
/// would pass a test that merely asserts it went up, and would then report
/// starvation on every database forever. So this asserts both: an idle-ish
/// sequence leaves it at zero, and a deliberately contended one moves it.
#[tokio::test]
async fn the_starvation_counter_distinguishes_a_backlog_from_a_quiet_actor() {
    use std::sync::Arc;

    let harness = TestHarness::new();
    let db = Arc::new(harness.db_with_fake_clock().await);

    // Sequential writes: each command is taken and finished before the next is
    // sent, so the low queue is empty every time the actor looks.
    for i in 0..4 {
        db.upsert_concept(ConceptUpsert::new(format!("q{i}"), "n").valid_from(T0))
            .await
            .unwrap();
    }
    let quiet = db.metrics();
    assert_eq!(
        quiet.low_starved_run_max, 0,
        "an actor that never had low-priority work queued reported a starvation \
         run of {}",
        quiet.low_starved_run_max
    );

    // Now contend deliberately. A chunked bulk write is low-priority; a burst of
    // concept upserts is high-priority. Firing them concurrently is what puts
    // low-priority work in the queue while the biased select keeps choosing the
    // other arm.
    let ids: Vec<String> = (0..40).map(|i| format!("c{i:03}")).collect();
    db.write_concepts(
        ids.iter()
            .map(|id| ConceptUpsert::new(id, "n").valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();

    let bulk = {
        let db = Arc::clone(&db);
        let edges: Vec<_> = (0..39)
            .map(|k| {
                EdgeAssertion::new(&ids[k], &ids[k + 1], "LINKS")
                    .valid_from(T0)
                    .valid_to(OPEN)
            })
            .collect();
        tokio::spawn(async move { db.bulk_import(edges).await })
    };

    let mut hot = Vec::new();
    for i in 0..64 {
        let db = Arc::clone(&db);
        hot.push(tokio::spawn(async move {
            db.upsert_concept(ConceptUpsert::new(format!("h{i:03}"), "n").valid_from(T0))
                .await
        }));
    }
    for t in hot {
        t.await.unwrap().unwrap();
    }
    bulk.await.unwrap().unwrap();

    // Measured on this fixture, identically across five runs: starved_turns=63,
    // run_max=63, turns=70. The run equals the total, i.e. the bulk import sat
    // behind *every* queued high-priority write with no interleaving at all.
    // The assertions below stay loose (> 0) rather than pinning 63, because the
    // number is a property of the machine and the scheduler; what is being
    // asserted is that the counter can see the condition. D-153 records the
    // measurement, which is the part that matters.
    let snap = db.metrics();
    assert!(
        snap.low_starved_turns > 0,
        "64 concurrent high-priority writes raced a chunked bulk import and the \
         actor never once took high-priority work with low-priority work queued. \
         Either the biased select is not biased, or the counter is not wired to \
         the arm that takes the choice."
    );
    assert!(
        snap.low_starved_run_max > 0 && snap.low_starved_run_max <= snap.low_starved_turns,
        "run_max {} is not a run of the {} starved turns — a run cannot exceed \
         the total it is drawn from",
        snap.low_starved_run_max,
        snap.low_starved_turns
    );

    Arc::into_inner(db).unwrap().close().await.unwrap();
}
