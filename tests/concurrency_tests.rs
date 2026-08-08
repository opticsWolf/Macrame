//! §5.1 — the Write Actor's concurrency contract.
//!
//! Every test here opens **one** `Database` and drives it through the public
//! surface. That is the R15 constraint (`.cargo/config.toml`,
//! `RUST_TEST_THREADS = "1"`): libSQL faults with STATUS_ACCESS_VIOLATION when
//! several local databases are open concurrently in one process, so a test that
//! wanted a database per case would belong behind `property-tests` with the rest
//! of the quarantine. None of these need one.

#[path = "common/harness.rs"]
mod harness;

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use harness::TestHarness;
use macrame::prelude::*;

const T1: &str = "2026-01-01T00:00:00.000000Z";
const T2: &str = "2026-02-01T00:00:00.000000Z";

async fn count(db: &Database, sql: &str) -> i64 {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

async fn scalar(db: &Database, sql: &str) -> Option<String> {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .and_then(|row| row.get(0).ok())
}

/// Create the concepts every edge below hangs off (`links` has a foreign key
/// into `concepts` and `PRAGMA foreign_keys` is ON).
async fn seed_nodes(db: &Database, ids: impl IntoIterator<Item = String>) {
    let concepts: Vec<ConceptUpsert> = ids
        .into_iter()
        .map(|id| {
            let title = format!("Node {id}");
            ConceptUpsert::new(id, title).valid_from(T1)
        })
        .collect();
    db.write_concepts(concepts).await.unwrap();
}

fn edge(source: &str, target: &str, edge_type: &str, valid_from: &str) -> EdgeAssertion {
    EdgeAssertion::new(source, target, edge_type).valid_from(valid_from)
}

/// Poll each future exactly once and return, **without yielding to the runtime**.
///
/// This is what makes the starvation test below a statement about the actor's
/// `biased` select rather than about how fast the machine happens to be. These
/// tests run on the `#[tokio::test]` default single-threaded runtime, and a
/// future that resolves `Ready` does not hand control back to the executor — so
/// between entering this function and leaving it, the actor task cannot run. A
/// command whose only pending point is `rx.await` has therefore *sent* by the
/// time we return, and the queue state we set up here is the queue state the
/// actor wakes up to.
async fn poll_once_each<T>(futures: &mut [Pin<Box<dyn Future<Output = T> + '_>>]) {
    std::future::poll_fn(|cx| {
        for f in futures.iter_mut() {
            let _ = f.as_mut().poll(cx);
        }
        Poll::Ready(())
    })
    .await
}

/// §5.1.5 — the whole reason there are two channels.
///
/// A backlog of background chunks must not delay UI-driven work. The test
/// queues 60 low-priority chunks (the channel holds 64) and *then* 8
/// high-priority asserts, with no opportunity for the actor to run in between,
/// and requires every one of the 8 to be stamped before every one of the 60.
///
/// The ordering evidence is `recorded_at`: the actor stamps from a `SystemClock`
/// whose contract is a strictly increasing value per call, so the column is a
/// faithful record of the order in which the actor serviced commands.
///
/// The margin matters. Merge the two channels into one FIFO and the probes land
/// dead last. Drop `biased` from the `select!` and tokio picks a ready branch at
/// random, which gets all 8 probes through first with probability ~2^-8 — this
/// fails ~99.6% of the time rather than flapping.
#[tokio::test]
async fn high_priority_writes_are_serviced_before_a_low_priority_backlog() {
    const BACKLOG: usize = 60;
    const PROBES: usize = 8;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let nodes = std::iter::once("SRC".to_string())
        .chain((0..BACKLOG).map(|i| format!("B{i}")))
        .chain((0..PROBES).map(|i| format!("P{i}")));
    seed_nodes(&db, nodes).await;

    // Queue the background work. Each `bulk_import` here is one chunk, so this
    // is 60 commands sitting in the low-priority channel.
    let mut backlog: Vec<Pin<Box<dyn Future<Output = Result<usize>>>>> = (0..BACKLOG)
        .map(|i| {
            let target = format!("B{i}");
            Box::pin(db.bulk_import(vec![edge("SRC", &target, "BACKLOG", T1)]))
                as Pin<Box<dyn Future<Output = _>>>
        })
        .collect();
    poll_once_each(&mut backlog).await;

    // ...and now the UI-driven work, which arrives strictly later.
    let mut probes: Vec<Pin<Box<dyn Future<Output = Result<()>>>>> = (0..PROBES)
        .map(|i| {
            let target = format!("P{i}");
            Box::pin(db.assert_edge(edge("SRC", &target, "PROBE", T1)))
                as Pin<Box<dyn Future<Output = _>>>
        })
        .collect();
    poll_once_each(&mut probes).await;

    // Only here does the actor get to run.
    for probe in probes {
        probe.await.unwrap();
    }
    for chunk in backlog {
        chunk.await.unwrap();
    }

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM links WHERE edge_type = 'PROBE'").await,
        PROBES as i64
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM links WHERE edge_type = 'BACKLOG'"
        )
        .await,
        BACKLOG as i64
    );

    let last_probe = scalar(
        &db,
        "SELECT MAX(recorded_at) FROM links WHERE edge_type = 'PROBE'",
    )
    .await
    .unwrap();
    let first_backlog = scalar(
        &db,
        "SELECT MIN(recorded_at) FROM links WHERE edge_type = 'BACKLOG'",
    )
    .await
    .unwrap();

    assert!(
        last_probe < first_backlog,
        "a high-priority write queued behind {BACKLOG} background chunks was serviced after \
         one of them: last probe {last_probe} >= first backlog chunk {first_backlog}"
    );
}

/// The same guarantee at its worst shape for a biased select: **one** probe
/// against a saturated low-priority queue, rather than the eight above.
///
/// `bulk_import` awaits each chunk before sending the next, so a single caller
/// can only ever have one command in flight. The backlog here is therefore built
/// from concurrent callers — which is also the realistic shape of the problem:
/// background importers and the UI are different tasks.
///
/// **This test asserted something the design does not promise, and failed for
/// that reason rather than from a defect.** It used to `.await` the probe
/// directly and then require `COUNT(BACKLOG) == 0`. Two things were wrong with
/// that, and the second is the interesting one:
///
/// * `.await` yields. The probe's command had not reached the channel yet, so
///   the actor woke with *only* low-priority work queued and drained all forty
///   chunks before the probe ever arrived — measured 40/40, which is why the
///   failure could not be explained away as one chunk already in flight. The
///   probe has to be *enqueued* before the actor runs, which is precisely what
///   [`poll_once_each`] exists for and what the test above already does.
/// * A count taken after the probe resolves is a wall-clock race in any case:
///   the actor keeps draining the backlog while the assertion's own `SELECT`
///   awaits. §8 is explicit that this invariant is "stated as an ordering
///   property over committed `seq_id`s, not a wall-clock timing measurement, so
///   it is deterministic" — and a count is a timing measurement wearing an
///   ordering's clothes.
///
/// So the claim is restated as ordering, which is both what the architecture
/// specifies and what is actually true: the probe, enqueued while forty chunks
/// sit unserviced, is stamped before every one of them. Preempting work already
/// accepted is not something two-tier channels can do — a queued command cannot
/// be retracted — and §5.1.5's guarantee is about what the actor picks up next,
/// not about cancelling what it already holds.
#[tokio::test]
async fn a_lone_high_priority_write_is_still_serviced_before_a_saturated_backlog() {
    const BACKLOG: usize = 40;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let nodes = std::iter::once("SRC".to_string())
        .chain((0..BACKLOG).map(|i| format!("B{i}")))
        .chain(std::iter::once("PROBE".to_string()));
    seed_nodes(&db, nodes).await;

    let mut backlog: Vec<Pin<Box<dyn Future<Output = Result<usize>>>>> = (0..BACKLOG)
        .map(|i| {
            let target = format!("B{i}");
            Box::pin(db.bulk_import(vec![edge("SRC", &target, "BACKLOG", T1)]))
                as Pin<Box<dyn Future<Output = _>>>
        })
        .collect();
    poll_once_each(&mut backlog).await;

    // Enqueued, not awaited: the actor must not get to run between the backlog
    // being queued and this command landing in the high-priority channel.
    let mut probe: Vec<Pin<Box<dyn Future<Output = Result<()>>>>> =
        vec![Box::pin(db.assert_edge(edge("SRC", "PROBE", "PROBE", T1)))
            as Pin<Box<dyn Future<Output = _>>>];
    poll_once_each(&mut probe).await;

    // The timeout is the deadlock guard, not the assertion: the probe must not
    // need the backlog's callers to be polled before it can finish.
    for p in probe {
        tokio::time::timeout(Duration::from_secs(5), p)
            .await
            .expect("high-priority write never completed behind the backlog")
            .unwrap();
    }

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM links WHERE edge_type = 'PROBE'").await,
        1
    );

    for chunk in backlog {
        chunk.await.unwrap();
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM links WHERE edge_type = 'BACKLOG'"
        )
        .await,
        BACKLOG as i64,
        "the backlog must still be serviced, only later"
    );

    // The claim itself, as an ordering over the actor's own stamps: the single
    // probe was serviced before every one of the forty chunks that were already
    // queued when it arrived.
    let probe_at = scalar(
        &db,
        "SELECT MAX(recorded_at) FROM links WHERE edge_type = 'PROBE'",
    )
    .await
    .unwrap();
    let first_backlog = scalar(
        &db,
        "SELECT MIN(recorded_at) FROM links WHERE edge_type = 'BACKLOG'",
    )
    .await
    .unwrap();
    assert!(
        probe_at < first_backlog,
        "a lone high-priority write queued behind {BACKLOG} background chunks was serviced \
         after one of them: probe {probe_at} >= first backlog chunk {first_backlog}"
    );
}

/// D-011 / D-014: `bulk_import` is atomic **per chunk**, not overall.
///
/// This is the tradeoff the `chunk_rows` doc comment states, and it is a real
/// consequence a caller has to plan for, not an implementation detail: a failure
/// partway leaves the earlier chunks committed.
///
/// # This test asserts the property, not where the boundary falls (0.12.0, W4)
///
/// Through 0.11.0 the counts here were exact — `chunk_rows::EDGES` rows survive
/// — because the chunk size was a constant and the boundary was therefore a
/// fact about the code. W3 made the size a function of measured hold, so the
/// boundary is now machine- and load-dependent, and a test that pinned it would
/// be pinning the speed of the machine that ran it.
///
/// What §5.1.6 actually promises survives that change intact, and is what is
/// checked below: **a prefix commits**, contiguously and not the whole batch;
/// and **each chunk is one transaction under one stamp**, so the committed rows
/// partition into contiguous same-stamp runs. This test is the executable form
/// of that section, which is why it is kept rather than relaxed into a smoke
/// test.
///
/// The third property — that the failing chunk rolls back *whole* — is not here
/// and cannot be, for the reason `a_violation_in_a_single_chunk_batch_commits_nothing`
/// below states: it is only visible when a good row shares the failing chunk,
/// and whether one does is now a fact about the machine. Asserting it here is
/// the mistake this test made in its first version and paid for at 26 runs in
/// 120.
#[tokio::test]
async fn bulk_import_is_atomic_per_chunk_not_overall() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    // Several chunks' worth at any size the controller can choose, so the
    // violation is reached with a prefix already committed.
    let n = 400usize;
    let nodes = std::iter::once("SRC".to_string()).chain((0..=n).map(|i| format!("T{i}")));
    seed_nodes(&db, nodes).await;

    let mut edges: Vec<EdgeAssertion> = (0..n)
        .map(|i| edge("SRC", &format!("T{i}"), "KNOWS", T1))
        .collect();
    // A second open interval on an edge already asserted earlier in this batch,
    // which trips `trg_links_single_open` in whichever chunk it lands in.
    edges.push(edge("SRC", &format!("T{n}"), "KNOWS", T1));
    edges.push(edge("SRC", "T0", "KNOWS", T2));

    let edge_count = edges.len();
    let err = db.bulk_import(edges).await.unwrap_err();
    assert!(
        matches!(err, DbError::SingleOpenViolation { .. }),
        "got {err:?}"
    );

    let committed = count(&db, "SELECT COUNT(*) FROM links").await;
    assert!(
        committed > 0,
        "nothing committed -- `bulk_import` is not all-or-nothing, so a failure \
         at row {n} must leave the chunks before it in place"
    );
    // Strictly fewer than the batch, not fewer than `n`.
    //
    // `committed < n` was the first version of this line and it was wrong for
    // the reason this test's own doc comment gives two paragraphs up: it
    // assumes at least one *good* row shares the failing chunk, which is only
    // true if the violation does not land first in its chunk. Where the
    // boundaries fall is now a fact about the machine, so that happens — 26
    // times in 120 runs while the R15 study loaded this box, and never once
    // under the fixed chunk size this test was written against.
    //
    // Asserting the violating chunk rolls back *whole* needs a batch whose
    // boundaries are known, which is what the single-chunk test below is for.
    assert!(
        committed < edge_count as i64,
        "everything committed ({committed} of {edge_count} rows), so nothing \
         rolled back and the violation was not caught"
    );

    // The prefix is contiguous: targets T0..T{committed-1} are present and
    // nothing past them is. Written as one query so a gap anywhere shows up.
    let in_prefix = count(
        &db,
        &format!(
            "SELECT COUNT(*) FROM links WHERE CAST(SUBSTR(target_id, 2) AS INTEGER) < {committed}"
        ),
    )
    .await;
    assert_eq!(
        in_prefix, committed,
        "the committed rows are not the contiguous prefix of the batch"
    );

    // §5.1.6, as an ordering: every stamp covers one contiguous run, so no two
    // chunks share a stamp and no chunk carries two. Equivalent and cheaper to
    // check — the number of stamps equals the number of runs of equal stamps
    // when the rows are read in batch order.
    let stamps = count(&db, "SELECT COUNT(DISTINCT recorded_at) FROM links").await;
    let runs = count(
        &db,
        "SELECT COUNT(*) FROM ( \
             SELECT recorded_at, LAG(recorded_at) OVER ( \
                 ORDER BY CAST(SUBSTR(target_id, 2) AS INTEGER)) AS prev \
             FROM links \
         ) WHERE prev IS NULL OR prev <> recorded_at",
    )
    .await;
    assert!(stamps >= 1, "the committed prefix carries no stamp at all");
    assert_eq!(
        stamps, runs,
        "a stamp spans a discontiguous set of rows: {stamps} distinct stamps \
         across {runs} runs, so a chunk is not one transaction under one stamp"
    );

    // The materialized view is not left inconsistent by the partial import.
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}

/// The failing chunk rolls back **whole**, stated where the boundary is known
/// (0.12.0, W4).
///
/// The test above cannot say this without naming a size: if the violating row
/// happened to land first in its chunk, no good row would be lost with it. A
/// batch small enough to be one chunk at any size the controller can choose
/// removes the question — the first chunk is always the path's ceiling, so two
/// rows are always one transaction, and a violation must therefore commit
/// nothing at all.
#[tokio::test]
async fn a_violation_in_a_single_chunk_batch_commits_nothing() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    seed_nodes(&db, ["SRC", "T0", "T1"].map(String::from)).await;

    let edges = vec![
        edge("SRC", "T1", "KNOWS", T1),
        edge("SRC", "T0", "KNOWS", T1),
        edge("SRC", "T0", "KNOWS", T2),
    ];
    let err = db.bulk_import(edges).await.unwrap_err();
    assert!(
        matches!(err, DbError::SingleOpenViolation { .. }),
        "got {err:?}"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM links").await,
        0,
        "the good rows that shared a chunk with the failure must have rolled \
         back with it"
    );
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}

/// The same per-chunk boundary on the concepts path.
///
/// The failure here is `trg_concepts_monotonic_ra`: every row in a chunk shares
/// one stamp, so a chunk that upserts the same id twice makes the second write
/// an UPDATE whose `recorded_at` does not advance.
#[tokio::test]
async fn write_concepts_commits_earlier_chunks_when_a_later_one_fails() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let n = chunk_rows::CONCEPTS;
    let mut concepts: Vec<ConceptUpsert> = (0..n)
        .map(|i| ConceptUpsert::new(format!("C{i}"), format!("Concept {i}")).valid_from(T1))
        .collect();
    concepts.push(ConceptUpsert::new("KEEP_ME", "Good row in a doomed chunk").valid_from(T1));
    concepts.push(ConceptUpsert::new("DUP", "First").valid_from(T1));
    concepts.push(ConceptUpsert::new("DUP", "Second").valid_from(T1));

    let err = db.write_concepts(concepts).await.unwrap_err();
    assert!(
        matches!(err, DbError::RecordedAtRegression { .. }),
        "got {err:?}"
    );

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM concepts").await,
        n as i64,
        "chunk one is committed and stays committed"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM concepts WHERE id IN ('KEEP_ME', 'DUP')"
        )
        .await,
        0,
        "the failing chunk must roll back whole, including the rows before the failure"
    );
}

/// §5.1.2 — the read connection is `PRAGMA query_only = ON`, and that has to be
/// a property of the connection rather than a convention callers follow.
///
/// `read_conn()` is public and hands out a raw `libsql::Connection`, so nothing
/// but the pragma stands between a caller and a write that bypasses the actor
/// entirely — no stamp, no single-open guard ordering, no serialization against
/// the write connection.
#[tokio::test]
async fn the_read_connection_refuses_writes() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    seed_nodes(&db, ["A".to_string(), "B".to_string()]).await;
    db.assert_edge(edge("A", "B", "KNOWS", T1)).await.unwrap();

    let conn = db.read_conn();

    let insert = conn
        .execute(
            "INSERT INTO concepts (id, title, content, valid_from, valid_to, recorded_at, retired) \
             VALUES ('SNEAK', 'Bypassed the actor', '', ?1, '9999-12-31T23:59:59.999999Z', ?1, 0)",
            libsql::params![T1],
        )
        .await;
    assert!(insert.is_err(), "the read connection accepted an INSERT");

    let update = conn
        .execute("UPDATE concepts SET title = 'Rewritten' WHERE id = 'A'", ())
        .await;
    assert!(update.is_err(), "the read connection accepted an UPDATE");

    let delete = conn.execute("DELETE FROM links", ()).await;
    assert!(delete.is_err(), "the read connection accepted a DELETE");

    let ddl = conn.execute("CREATE TABLE sneak (x TEXT)", ()).await;
    assert!(ddl.is_err(), "the read connection accepted DDL");

    // Nothing landed, and reading still works -- `query_only` refuses writes, it
    // does not disable the connection.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'SNEAK'").await,
        0
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM links").await, 1);
    assert_eq!(
        scalar(&db, "SELECT title FROM concepts WHERE id = 'A'")
            .await
            .unwrap(),
        "Node A"
    );
}

/// Readers hold a WAL snapshot, not a lock the writer waits on.
///
/// The interesting case is not "a query runs while a write runs" but an *open
/// row stream*: `query()` leaves a statement mid-iteration holding a read
/// transaction. If that blocked the write connection, the actor would stall
/// until the reader drained — and §5.1.8 warns that awaiting a write is a
/// channel wait that `busy_timeout` does not bound, so the caller would hang
/// rather than get `SQLITE_BUSY`. Hence the timeout: a stall is the defect.
#[tokio::test]
async fn open_readers_do_not_block_the_writer() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let nodes = std::iter::once("SRC".to_string()).chain((0..32).map(|i| format!("N{i}")));
    seed_nodes(&db, nodes).await;
    db.bulk_import(
        (0..32)
            .map(|i| edge("SRC", &format!("N{i}"), "KNOWS", T1))
            .collect(),
    )
    .await
    .unwrap();

    // Four readers, each parked mid-result-set.
    let mut streams = Vec::new();
    for _ in 0..4 {
        let mut rows = db
            .read_conn()
            .query(
                "SELECT source_id, target_id FROM links ORDER BY target_id",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().expect("seeded rows expected");
        streams.push(rows);
    }

    let limit = Duration::from_secs(5);

    // A single-statement write...
    tokio::time::timeout(limit, db.assert_edge(edge("SRC", "N0", "LIKES", T1)))
        .await
        .expect("assert_edge blocked behind open readers")
        .unwrap();

    // ...and one that takes the write lock for a whole transaction (BEGIN
    // IMMEDIATE), which is where a reader-held lock would actually bite.
    let written = tokio::time::timeout(
        limit,
        db.write_bulk_atomic(
            (0..32)
                .map(|i| edge("SRC", &format!("N{i}"), "CITES", T1))
                .collect(),
        ),
    )
    .await
    .expect("write_bulk_atomic blocked behind open readers")
    .unwrap();
    assert_eq!(written, 32);

    // The readers survive the writes and still drain their own snapshot.
    for mut rows in streams {
        let mut seen = 1;
        while tokio::time::timeout(limit, rows.next())
            .await
            .expect("an open reader stalled after the writer committed")
            .unwrap()
            .is_some()
        {
            seen += 1;
        }
        assert!(
            seen >= 32,
            "reader saw {seen} rows, expected its 32-row snapshot at least"
        );
    }

    // A fresh read sees everything the writer committed.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM links").await, 32 + 1 + 32);
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
}
