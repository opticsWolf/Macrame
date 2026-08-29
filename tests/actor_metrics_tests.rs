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
///
/// # It reads the source line by line, since 0.12.15
///
/// The first version split on the literal `"\n///\n"`, which is a string a
/// Windows checkout does not contain: `core.autocrlf=true` is the default there
/// and every line ends `\r\n`, so the terminator never matched, `table` became
/// the entire rest of the file, and every kind was "named" — the test failed on
/// `assert_edge`, which appears in `connection.rs` a hundred times and in no
/// table. **It reported the two lists as disagreeing when they agreed**, on a
/// fresh clone, for a reason having nothing to do with either list. A test that
/// reads source has to tolerate both line endings, and taking lines is how,
/// because `lines()` already handles it.
#[test]
fn the_budget_exemptions_and_their_documented_table_agree() {
    let source = include_str!("../src/connection.rs");
    let table: String = source
        .lines()
        .skip_while(|l| !l.contains("| Path | Bound | Why it cannot be chunked |"))
        // The table runs to the first doc line that is not a table row — `///`
        // alone, or prose. `|` is the delimiter, so a row always has one.
        .take_while(|l| l.trim_start().starts_with("///") && l.contains('|'))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        table.lines().count() > 3,
        "CHUNK_BUDGET's exemption table has moved, been renamed, or lost its \
         rows — {} line(s) matched",
        table.lines().count()
    );

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

/// **A forced low-priority turn would be unbounded by contract, and that is why
/// there is no fairness floor** (0.13.26, W10.4, D-199).
///
/// W10.4 measured the starvation D-153 found and then measured the fix's price.
/// The obvious floor — "after N starved turns, take one low-priority command"
/// — cannot choose *which* command: the low queue is an mpsc channel and its
/// head is not inspectable. So the floor's cost is whatever is at that head,
/// and this test pins the fact that makes that unbounded: **a low-priority path
/// exists whose kind is budget-exempt by contract**.
///
/// `archive` is that path. It is reached only through `Database::archive`, it is
/// low-priority, and `CHUNK_BUDGET`'s table exempts it because copy-then-delete
/// must be atomic (D-012) — measured at 3.3 s unwindowed on an 8,000-key
/// backlog (D-080). Admitting one of those into an interactive write's wait
/// trades the guarantee the tier split exists to provide for the starvation of
/// work that is declared not latency-sensitive.
///
/// If this ever goes red — if every low-priority kind gains a bound — then a
/// floor becomes boundable and W10.4's decision is due for review. That is the
/// only thing that changes it, and it is why this is a test rather than a
/// paragraph.
#[tokio::test]
async fn a_forced_low_turn_would_be_unbounded_by_contract() {
    let harness = TestHarness::new();
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();

    // Reached through the low tier, and counted, so this is not an assertion
    // about a table but about a path the actor really takes.
    db.archive("2027-01-01T00:00:00.000000Z").await.unwrap();
    assert_eq!(
        turns_for(&db.metrics(), CommandKind::Archive),
        1,
        "`archive()` no longer takes an `Archive` turn, so this test is no \
         longer about the low tier"
    );

    assert!(
        CommandKind::Archive.exempt_from_budget(),
        "every low-priority kind now states a bound, which is the one thing \
         that would make a fairness floor boundable. W10.4 declined the floor \
         because a forced low turn admits an arbitrary low command and one of \
         them has no latency bound by contract (D-199). Re-read that decision \
         rather than deleting this assertion."
    );

    db.close().await.unwrap();
}

/// **`analyze()` and `optimize()` are attributed to different kinds, and the
/// flag is not inverted** (0.13.24, W10.5, D-197).
///
/// The split's own regression, in the two shapes it can take: both arms mapping
/// to one kind (the split never reached `LowPriCommand::kind`) or the
/// `incremental` flag read the wrong way round (the counts are right and the
/// names are swapped). Neither is visible in a total, so each call is checked
/// against **both** counters at the moment it runs.
///
/// This is not a re-run of `every_write_method_is_attributed_to_its_own_command_kind`.
/// That one reads a snapshot after the fact, and a swapped pair survives it.
#[tokio::test]
async fn analyze_and_optimize_are_counted_apart() {
    let harness = TestHarness::new();
    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();

    db.upsert_concept(ConceptUpsert::new("a", "A").valid_from(T0))
        .await
        .unwrap();

    db.analyze().await.unwrap();
    let snap = db.metrics();
    assert_eq!(
        (
            turns_for(&snap, CommandKind::Analyze),
            turns_for(&snap, CommandKind::Optimize)
        ),
        (1, 0),
        "`analyze()` is not attributed to `Analyze` alone. Either the two          kinds are still one, or `LowPriCommand::kind` reads `incremental`          backwards (W10.5, D-197)"
    );

    db.optimize().await.unwrap();
    let snap = db.metrics();
    assert_eq!(
        (
            turns_for(&snap, CommandKind::Analyze),
            turns_for(&snap, CommandKind::Optimize)
        ),
        (1, 1),
        "`optimize()` did not land on `Optimize`, or it moved `Analyze` as          well. The point of the split is that `close()`'s automatic call is          distinguishable from an explicit one (W10.5, D-197)"
    );

    // And `close()` runs one more, which is the automatic path the split
    // exists to make visible. Asserted through a fresh handle, since the
    // metrics of a closed one are gone with it.
    db.close().await.unwrap();

    let db = Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap();
    assert_eq!(
        turns_for(&db.metrics(), CommandKind::Optimize),
        0,
        "a freshly opened handle has already run an optimize; `open()` is not          supposed to touch statistics (D-149)"
    );
    db.close().await.unwrap();
}

/// **Neither `Analyze` nor `Optimize` is budget-exempt, and since 0.13.24 those
/// are two decisions** (W10.5, D-197; was D-168).
///
/// They were one kind until 0.13.23, and D-168 declined to decide the exemption
/// *because* they were: a judgement about the explicit call would have landed on
/// `close()`'s automatic one without being made about it. W10.5 split them. Both
/// answers came back the same; the reasons did not, and this test carries both
/// so the next person to reach for the exemption meets the right one.
///
/// - **`Analyze` cannot state a `Bound`.** One indivisible statement whose cost
///   tracks the data — 19.1 ms at 40,000 edges against 3 ms
///   (`examples/analyze_hold.rs`). Every call is a violation, permanently, and
///   the exemption table's `Bound` column is what it cannot fill in.
/// - **`Optimize`'s violations are the informative ones.** 90–220 µs when it
///   declines, which is almost always; 10.7 ms on a never-analysed database and
///   460 ms once the table has grown past SQLite's staleness ratio
///   (`examples/optimize_hold.rs`). Exempting it would delete the signal that
///   says which calls did work.
/// - **Lowering `analysis_limit`** is the third wrong fix and is not checkable
///   here; it buys the number by sampling too little to separate the two
///   `source_id`-leading indices, which is the whole purpose of having
///   statistics (D-149). Named so the reader of this test meets it.
#[test]
fn analyze_is_not_budget_exempt_and_that_is_deliberate() {
    assert!(
        !CommandKind::Analyze.exempt_from_budget(),
        "`Analyze` was added to the budget exemptions. The exemption table has \
         a `Bound` column and this kind cannot fill it in: the honest entry is \
         \"the size of the table, damped 3–4x\", which is the absence of a \
         bound rather than one. Every call being a violation is the intended \
         reading, not the defect (D-166, D-197)."
    );

    assert!(
        !CommandKind::Optimize.exempt_from_budget(),
        "`Optimize` was added to the budget exemptions. It is under budget \
         whenever it declines to re-analyse, which is nearly always — so its \
         violations are not noise, they are the calls that actually did work \
         (10.7 ms cold, 460 ms once the table had grown 25x). Exempting it \
         deletes the one signal that distinguishes the two (D-197)."
    );

    // The exemption list is a judgement per kind, so the sibling that shares
    // the argument is pinned beside it rather than left to inference. Since
    // 0.14.16 the shadow rebuild is two kinds and they land on opposite sides
    // of the criterion, which makes this pair the clearest statement of it in
    // the suite (D-233).
    assert!(
        !CommandKind::ShadowRebuild.exempt_from_budget(),
        "`ShadowRebuild` was exempted. It is the *fill* half — Begin and the \
         Fill chunks — and those are meant to fit the budget, so their \
         overage is workload-dependent and a violation discriminates. \
         Exempting it deletes the only signal that a fill chunk regressed \
         (D-082's goal, D-233's mechanism)."
    );

    assert!(
        CommandKind::ShadowSwap.exempt_from_budget(),
        "`ShadowSwap` lost its exemption. It exceeds by construction — three \
         index builds under the write lock, 46.8 ms against a 3 ms budget \
         (D-082), with no healthy state in which it fits — so counting it \
         puts a permanent `N(rebuilds)` in `budget_violations()` on every \
         database that has ever repaired its projection. That is exactly the \
         failure `Rehydrate`'s exemption exists to prevent (W4.3, D-233)."
    );
}

/// The swap turn is attributed to `ShadowSwap`, and the fill half does not
/// absorb it (0.14.16, W12.16, D-233).
///
/// Driven step by step rather than through `rebuild_current_chunked`, because
/// the property is *per step* and the loop gives no seam — the same reason
/// `an_archive_during_the_build_abandons_the_rebuild` drives it this way. Each
/// step is checked against both counters, so an arm that returned the
/// neighbouring variant fails here whichever direction it leans.
///
/// **The `and not` half is the load-bearing one.** Asserting only that
/// `ShadowSwap` counted would pass in a world where the swap was counted twice,
/// and asserting only after the fact would pass in a world where `Begin` were
/// the thing being misattributed.
#[tokio::test]
async fn a_swap_is_counted_as_shadow_swap_and_not_as_shadow_rebuild() {
    use macrame::integrity::{ShadowOutcome, ShadowStep};

    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

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

    let fills = |snap: &_| turns_for(snap, CommandKind::ShadowRebuild);
    let swaps = |snap: &_| turns_for(snap, CommandKind::ShadowSwap);

    let ShadowOutcome::Started { build_start, epoch } =
        db.shadow_step(ShadowStep::Begin).await.unwrap()
    else {
        panic!("Begin returned the wrong outcome")
    };
    let snap = db.metrics();
    assert_eq!(fills(&snap), 1, "Begin is a fill-half turn");
    assert_eq!(swaps(&snap), 0, "Begin was counted as a swap");

    db.shadow_step(ShadowStep::Fill { after: None })
        .await
        .unwrap();
    let snap = db.metrics();
    assert_eq!(
        fills(&snap),
        2,
        "the Fill chunk did not land on ShadowRebuild"
    );
    assert_eq!(swaps(&snap), 0, "a Fill chunk was counted as a swap");

    db.shadow_step(ShadowStep::Swap { build_start, epoch })
        .await
        .unwrap();
    let snap = db.metrics();
    assert_eq!(
        swaps(&snap),
        1,
        "the swap took an actor turn that was not attributed to ShadowSwap"
    );
    assert_eq!(
        fills(&snap),
        2,
        "the ShadowRebuild counter moved on a swap. That is the 0.6.0-0.14.15 \
         behaviour D-233 removed: one kind over two hold distributions, whose \
         `over_budget` then read `N(rebuilds) + regressions` and could not be \
         decomposed."
    );

    db.close().await.unwrap();
}

/// A swap that exceeds the budget is not a violation, on a fixture that makes it
/// exceed (0.14.16, W12.16, D-233).
///
/// **This is the quiet half, and it is what makes the exemption self-policing.**
/// It fails in both directions: narrow the exemption to re-count the swap and
/// the count goes to 1; widen it to swallow the fill half and the canary in
/// `metrics.rs` goes red instead. Un-exempting is one function arm and one table
/// row, and between them these two tests make that flip loud.
///
/// # The fixture is load-bearing, and the first draft of this test was not
///
/// Asserting `violations().is_empty()` after a rebuild is the obvious form and
/// it is worthless. On a three-edge fixture the swap finishes inside 3 ms, so
/// the assertion passes whether the kind is exempt or not — verified by
/// mutation: removing `ShadowSwap` from `exempt_from_budget` left that draft
/// green. And the assertion cannot simply be moved to a larger fixture, because
/// **fill chunks legitimately exceed the budget once the graph is real**:
/// measured in a debug build at 200 keys × 4 generations, the longest fill is
/// 3.14 ms — one violation — while the swap is 6.1 ms with none. That is the
/// counter working exactly as intended, and it means "a rebuild leaves the
/// violation list empty" is a property of small fixtures rather than of
/// rebuilds.
///
/// So the fixture is sized to put the swap over the budget, that is asserted
/// first, and only the swap's own count is claimed. A fixture that stops
/// exceeding fails loudly and says to grow it, rather than passing on a
/// technicality.
#[tokio::test]
async fn a_swap_over_budget_is_not_a_violation() {
    const KEYS: usize = 400;
    const GENERATIONS: usize = 4;

    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    db.write_concepts(
        (0..KEYS)
            .map(|i| ConceptUpsert::new(format!("n{i}"), "n").valid_from(T0))
            .collect(),
    )
    .await
    .unwrap();
    // Several generations at one edge key, so the projection has something to
    // rank and the swap has three indexes' worth of rows to build over.
    for generation in 0..GENERATIONS {
        db.bulk_import(
            (0..KEYS)
                .map(|i| {
                    EdgeAssertion::new(format!("n{i}"), format!("n{}", (i + 1) % KEYS), "KNOWS")
                        .valid_from(T0)
                        .valid_to(OPEN)
                        .weight(generation as f64 + 1.0)
                })
                .collect(),
        )
        .await
        .unwrap();
    }

    db.rebuild_current_chunked().await.unwrap();

    let snap = db.metrics();
    let swap = snap
        .kinds
        .iter()
        .find(|k| k.kind == CommandKind::ShadowSwap)
        .unwrap();

    assert_eq!(swap.turns, 1, "a rebuild is exactly one swap turn");
    assert!(
        swap.longest > macrame::CHUNK_BUDGET,
        "the swap took {:?}, which is inside the {:?} budget, so this test \
         asserts nothing about the exemption. Grow KEYS or GENERATIONS until \
         it exceeds — the number to beat is D-082's 46.8 ms at 10,000 keys, \
         and 400 x 4 was ~6 ms in a debug build when this was written.",
        swap.longest,
        macrame::CHUNK_BUDGET
    );
    assert_eq!(
        swap.over_budget,
        0,
        "the swap exceeded the budget by {:?} and was counted as a violation. \
         It exceeds on every rebuild — three index builds under the write lock \
         — so counting it puts a permanent entry in `budget_violations()` on \
         any database that has ever repaired its projection, which is what \
         D-233 removed and what `Rehydrate`'s exemption exists to prevent.",
        swap.longest.saturating_sub(macrame::CHUNK_BUDGET)
    );
    assert!(
        !snap
            .budget_violations()
            .iter()
            .any(|k| k.kind == CommandKind::ShadowSwap),
        "`budget_violations()` named the swap"
    );

    // The list is deliberately *not* asserted empty. At this fixture size the
    // fill chunks may exceed the budget too, and when they do that is the
    // counter reporting a real hold rather than a defect in this test — the
    // fill half is not exempt precisely so it can say so.
    db.close().await.unwrap();
}
