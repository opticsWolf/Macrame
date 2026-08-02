<!--nav-->
← [previous](s4-schema.md) · [index](README.md) · [next](s6-s10-flows-to-dependencies.md) →
<!--/nav-->

## §5 Modules

### 5.1 connection.rs — the handle, the pragmas, and the Write Actor

Amended in 0.4.5; corrected and hardened in 0.5.0; clarified in 0.5.1 and 0.5.2. This section is the substance of the amendment; everything else in the document either feeds it or is fed by it.

#### 5.1.1 Why a Mutex is not enough

The v0.4.0 handle wrapped a single shared connection, and [§2](s0-s3-foundations.md#2-system-context) promised that "write serialization is structural rather than negotiated" because all writes went through one transaction() entry point. That was true — and insufficient. The failure mode is not concurrent writes, which the entry point prevented, but concurrent occupancy: a background task inside transaction() writing 50,000 analytics annotations holds the file-level write lock for the entire duration of the transaction, and a tokio::sync::Mutex around the connection merely converts the file lock into a task queue with no say over its ordering and no bound on its service time. The UI thread blocks behind the whole bulk job. In SQLite, the lock is held at the file-system level for the duration of a transaction; no wrapper above the engine can interrupt it. The only cure is to govern how long any single transaction holds the lock and who gets it next. 0.4.5 therefore replaces the shared entry point with a dedicated owner.

#### 5.1.2 Handle shape and the clock contract
```rust
// shape only — see Appendix A for normative signatures

pub struct Database {
    db: libsql::Database,                       // engine handle; readers can be spawned freely
    read_conn: libsql::Connection,              // WAL reader — never writes, never traverses the actor
    high_pri_tx: mpsc::Sender<HighPriCommand>,  // UI-driven work
    low_pri_tx: mpsc::Sender<LowPriCommand>,    // background work
    clock: Arc<dyn Clock>,
    archive_path: PathBuf,                      // (0.5.2) cold DB, derived by convention at open()
    schema_version: u32,
    writer: Option<JoinHandle<Result<()>>>,
}
```

The write connection has deliberately no field: it is moved into the actor task at open() and no other code path can name it. Reads do not traverse the actor at all — as_of() traversals, reconstruct() folds, audit_current(), vector and hybrid search are all served from read_conn, because under WAL a reader never blocks on the writer, and routing reads through the actor would add latency for nothing. The actor's loop stays tight: it schedules writes and nothing else.
```rust
// shape only — see Appendix A for normative signatures

impl Database {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let db = libsql::Builder::new_local(path).build().await?;
        let write_conn = configure(db.connect()?).await?;
        let read_conn  = configure(db.connect()?).await?;

        // (0.5.0) Structural read-guard: even a logic error in connection
        // routing cannot produce a write through the read path.
        read_conn.execute("PRAGMA query_only = ON", ()).await?;

        migrations::run(&write_conn).await?;

        let (high_pri_tx, high_pri_rx) = mpsc::channel(256);
        let (low_pri_tx,  low_pri_rx)  = mpsc::channel(64);
        // (0.5.2) SystemClock reads MAX(recorded_at) to floor itself — see below.
        let clock: Arc = Arc::new(SystemClock::new(&read_conn).await?);
        let writer = tokio::spawn(run_writer_actor(
            write_conn, Arc::clone(&clock), high_pri_rx, lowpri_rx));

        // (0.5.2) Archive path derived by convention: foo.db -> foo_archive.db
        let archive_path = derive_archive_path(path);

        Ok(Self { db, read_conn, high_pri_tx, low_pri_tx, clock, archive_path,
                  schema_version: migrations::current_version(),
                  writer: Some(writer) })
    }
}

/// Identical pragma set on every connection — writer and readers alike.
async fn configure(conn: libsql::Connection) -> Result<libsql::Connection> {
    conn.execute("PRAGMA journal_mode = WAL", ()).await?;
    conn.execute("PRAGMA synchronous = NORMAL", ()).await?;
    conn.execute("PRAGMA busy_timeout = 5000", ()).await?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    // (0.5.0) Explicit: SQLite defaults recursive_triggers to OFF, which is
    // correct here — trg_links_current_sync fires an upsert into links_current,
    // and we must not re-fire any trigger on links_current.
    conn.execute("PRAGMA recursive_triggers = OFF", ()).await?;
    Ok(conn)
}
```

The pragma set is small and intentional, unchanged from 0.4.0 except for the two 0.5.0 additions noted inline. WAL journaling gives non-blocking readers behind a single writer; synchronous = NORMAL is the durability/speed equilibrium for WAL mode on desktop hardware, where the failure window is a power loss mid-WAL-checkpoint rather than a lost committed transaction; and foreign_keys = ON is non-negotiable in a schema whose referential integrity is part of its meaning. busy_timeout = 5000 is set on every connection — writer and readers — because even with perfect in-process queueing, a read transaction that escalates, or a manual SQLite CLI query against the file, can hold the lock briefly; the timeout converts that into a bounded wait instead of an immediate SQLITE_BUSY. Five seconds is long enough to survive a WAL checkpoint flush and short enough that a genuinely stuck lock surfaces as a typed error rather than a hung UI.

Migrations run through user_version, the engine's own schema-version slot, with each migration an idempotent step reviewed as ordinary DDL. The clock is injected here and threaded everywhere: production uses SystemClock (RFC 3339, microsecond precision, UTC); every test uses FakeClock, which is what makes the entire temporal test suite deterministic — including, as of 0.4.5, the actor, which receives the same Arc the harness holds.

Clock monotonicity guarantee (0.5.1). Both SystemClock and FakeClock implement the Clock trait with a strict monotonicity contract:
```rust
pub trait Clock: Send + Sync {
    /// Returns the current timestamp as ISO-8601 UTC.
    /// CONTRACT: successive calls return strictly increasing values,
    /// even across application restarts and NTP corrections.
    fn now(&self) -> String;
}
```

SystemClock enforces this by maintaining an interior Mutex tracking the last-issued timestamp. On each call to now(), it computes max(wall_clock, last_issued + 1μs) and updates the interior state. On construction (SystemClock::new()), it queries the database for MAX(recorded_at) across concepts and links and floors the interior state to that value, so that a restart after an NTP backward correction cannot issue a timestamp older than the newest row in the database. This guarantees that trg_concepts_monotonic_ra ([§4.3](s4-schema.md#43-the-transaction-log)) never rejects a legitimate update due to clock drift. FakeClock is monotonic by construction: the harness advances it explicitly via advance(Duration), and it never moves backwards.

Timestamp parsing contract (0.5.2, [D-027](s13-decision-register.md#d-027)). Because recorded_at is stored as ISO-8601 text, SystemClock::new() must parse the stored string back into a SystemTime to use as its floor. This is the one place in the clock module where a panic is possible if handled carelessly, so the contract is explicit:
```rust
// util/clock.rs — shape only

impl SystemClock {
    pub async fn new(conn: &libsql::Connection) -> Result<Self> {
        // SQLite MAX() on ISO-8601 'Z' text is lexicographic, which is
        // chronologically correct for UTC strings with identical format.
        let max_ts: Option<String> = conn.query(
            "SELECT MAX(recorded_at) FROM (
                 SELECT MAX(recorded_at) AS recorded_at FROM concepts
                 UNION ALL
                 SELECT MAX(recorded_at) AS recorded_at FROM links
             )", ()
        ).await?.next().await?.and_then(|row| row.get(0).ok());

        let floor = match max_ts {
            Some(ts) => parse_iso8601_utc(&ts).unwrap_or_else(|e| {
                // Corrupt or manually-edited timestamp. Don't panic on
                // startup; fall back to wall clock and warn. The
                // monotonicity trigger (§4.3) prevents the corruption
                // from propagating into new writes.
                tracing::warn!(
                    "SystemClock: failed to parse MAX(recorded_at)={:?}: {}; \
                     falling back to wall clock", ts, e
                );
                SystemTime::now()
            }),
            None => SystemTime::now(),  // empty database
        };

        Ok(Self { floor: Mutex::new(floor) })
    }
}

/// Strict parser for the exact format SystemClock produces:
/// "YYYY-MM-DDTHH:MM:SS.ffffffZ" (microsecond precision, UTC).
/// Also accepts second precision ("YYYY-MM-DDTHH:MM:SSZ") for
/// compatibility with rows written by older crate versions.
/// Rejects offsets, missing 'Z', and malformed components.
fn parse_iso8601_utc(s: &str) -> Result<SystemTime> { /* … */ }
```

The decisions: parse failure never panics — a corrupt recorded_at falls back to SystemTime::now() with a tracing::warn!; the parser is strict (accepts only the crate's own format plus second-precision backward compatibility, rejects offsets and missing Z); and there is no chrono dependency — the parser is ~20 lines of manual string slicing, and adding a datetime crate for one call site is not justified. The lexicographic MAX() is safe precisely because every recorded_at is a UTC Z string in an identical fixed format, which is why [§4.1](s4-schema.md#41-concepts-and-per-model-embeddings) mandates the explicit Z suffix.

#### 5.1.3 The two-tier command channel
```rust
// shape only — see Appendix A for normative signatures

/// Work that preempts background jobs at the next transaction boundary.
pub enum HighPriCommand {
    AssertEdge {
        source: String, target: String, edge_type: String,
        valid_from: String, valid_to: String,
        weight: f64, properties: String,
        responder: oneshot::Sender<Result<()>>,
    },
    RetireEdge { /* … */ responder: oneshot::Sender<Result<()>> },
    UpsertConcept { /* … */ responder: oneshot::Sender<Result<()>> },
    /// The explicit atomic escape hatch (§5.1.6): one transaction, one stamp,
    /// one stall. High-priority because the caller has accepted the cost.
    WriteBulkAtomic { rows: Vec<AnnotationRow>,
                      responder: oneshot::Sender<Result<()>> },
    /// Integrity repair preempts analytics: a detected drift is a
    /// corrective act (§5.8). See §5.8 for sizing and operational guidance.
    RebuildCurrent { responder: oneshot::Sender<Result<RebuildReport>> },
    Shutdown { responder: oneshot::Sender<Result<()>> },
}

/// Work that yields to any high-priority command at every chunk boundary.
pub enum LowPriCommand {
    WriteAnnotationsChunk { chunk: Vec<AnnotationRow>,
                            responder: oneshot::Sender<Result<()>> },
    BulkImportChunk { /* … */ responder: oneshot::Sender<Result<()>> },
    /// Archive is one atomic transaction by nature (§5.7); it is low-priority
    /// because it is scheduled, not user-driven.
    Archive { cutoff: String, responder: oneshot::Sender<Result<ArchiveReport>> },
}
```

> **These two sketches are 0.4.5 vintage and the crate has moved past them (0.5.4).** They are restored as written because the prose around them argues from their shape, but three things in them are no longer true: the payload fields are now typed values (`EdgeAssertion`, `ConceptUpsert`, `Annotation`) rather than loose columns, since a command carrying strings is an arbitrary-SQL channel ([D-034](s13-decision-register.md#d-034)); the wildcard `_ => LoopCtl::Continue` in `execute` below is the exact defect [D-034](s13-decision-register.md#d-034) removed, because it dropped the responder for nine of twelve commands; and the command set has since gained `RegisterModel`, `UpsertEmbeddingChunk` ([D-048](s13-decision-register.md#d-048)) and `WriteAnalyticsChunk` ([D-041](s13-decision-register.md#d-041)). [Appendix A](appendices.md#appendix-a--public-api-normative) is the current surface.

Every command carries its own oneshot responder: the actor answers each request individually, and the caller's await is the join point between the two tasks. Two tiers are enough for this system — user-driven work and background work — and select! extends to a third arm without structural change should a maintenance tier ever appear. The channels are bounded, and the bounds are backpressure: when the low-priority queue is full, an analytics worker blocks on send().await, which is exactly correct — the producer is throttled by the consumer, and the UI is never a party to the negotiation.

#### 5.1.4 The actor loop
```rust
// shape only — see Appendix A for normative signatures

enum LoopCtl { Continue, Break }

async fn run_writer_actor(
    conn: libsql::Connection,
    clock: Arc<dyn Clock>,
    mut high_pri_rx: mpsc::Receiver<HighPriCommand>,
    mut low_pri_rx: mpsc::Receiver<LowPriCommand>,
) -> Result<()> {
    loop {
        let ctl = tokio::select! {
            // biased polls the arms in written order: while the high-priority
            // queue is non-empty, the low-priority arm is never even polled.
            biased;

            Some(cmd) = high_pri_rx.recv() => cmd.execute(&conn, &*clock).await,
            Some(cmd) = low_pri_rx.recv()  => cmd.execute(&conn, &*clock).await,
            else => LoopCtl::Break,                     // both channels closed
        };
        if matches!(ctl, LoopCtl::Break) { break; }
    }
    Ok(())
}
```

Each execute runs exactly one transaction — BEGIN IMMEDIATE … COMMIT — and routes its DbError, if any, to the command's responder rather than to the loop; a failed assertion must not kill the writer. The loop is the only scheduler in the system, and its policy is one line: between any two transactions, re-check the UI queue before touching the background queue. The priority queue decides who goes next; it does not — cannot — interrupt a transaction already in flight, because SQLite's lock is not preemptible. That single fact is the reason [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) exists.
```rust
// shape only — see Appendix A for normative signatures

impl HighPriCommand {
    async fn execute(self, conn: &libsql::Connection, clock: &dyn Clock) -> LoopCtl {
        match self {
            HighPriCommand::AssertEdge { source, target, edge_type,
                                         valid_from, valid_to, weight,
                                         properties, responder } => {
                let stamp = clock.now();
                let result = async {
                    let tx = conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .await?;
                    // Fires trg_links_current_sync and trg_links_log_i
                    // inside the transaction.
                    tx.execute(
                        "INSERT INTO links (source_id, target_id, edge_type,
                                            valid_from, valid_to, weight,
                                            properties, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        libsql::params![source, target, edge_type, valid_from,
                                        valid_to, weight, properties, stamp],
                    ).await?;
                    tx.commit().await?;
                    Ok(())
                }.await;
                let _ = responder.send(result);
                LoopCtl::Continue
            }
            HighPriCommand::Shutdown { responder } => {
                let _ = responder.send(Ok(()));
                LoopCtl::Break
            }
            /* RetireEdge, UpsertConcept, RebuildCurrent,
               WriteBulkAtomic — same shape */
            _ => LoopCtl::Continue,
        }
    }
}
```

transaction() as a public entry point is withdrawn in 0.4.5 ([D-016](s13-decision-register.md#d-016)): a caller-held closure would let arbitrary work hold the write lock for arbitrary time, reintroducing exactly the starvation this amendment removes. The mechanism survives as the actor-internal shape above — every write is still one BEGIN IMMEDIATE … COMMIT, so the v0.4.0 invariant "all writes funnel through a single transaction entry point" is preserved and strengthened: the entry point is now a task, and the task is the only thing in the process that can write.

#### 5.1.5 Cooperative chunking — the golden rule

The priority queue decides who goes next. It does not interrupt a transaction already running. If a background worker were allowed to submit 50,000 annotation writes as one command, the high-priority queue would still be blocked until that transaction committed — the actor would hold the lock on behalf of the background for the entire duration, and the biased poll would be irrelevant.

The Golden Rule: low-priority workers must chunk their data. Chunk sizes of 500 to 1,000 rows are the calibration: large enough that per-transaction overhead (BEGIN, COMMIT, fsync under synchronous = NORMAL) amortizes to noise, small enough that a chunk commits in 2–3 ms even where trigger amplification applies (a links insert fans out to three writes — the row, the links_current upsert, the log entry). The writer yields back to the select! after every chunk, and the biased poll does the rest.

**Re-derived in 0.5.5 from measurement ([D-058](s13-decision-register.md#d-058)). The paragraph above states the rule correctly and calibrates it wrongly, in three separate ways.** It is kept as written because the corrections are the interesting part; what follows supersedes its numbers.

*The rule itself is a bound on duration, not on rows.* A background chunk must commit fast enough that an interactive write queued behind it is not made to wait — the SQLite write lock is not preemptible, so that wait is the full remaining duration of the chunk in flight. The bound is **3 ms** ([§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s figure, kept rather than renegotiated), which puts a worst-case interactive assertion at 3 ms of queueing plus its own ≤ 5 ms, inside a 60 Hz frame. A row count is not the rule; it is an answer to the rule, and it is different for every path.

*Correction 1 — one constant cannot serve four paths.* At 1,000 rows the four bulk paths take **3.5 ms, 24 ms, 89 ms and 143 ms**. Their per-row costs span 60×, from an annotation at ~2.5 µs to an embedding at ~150 µs, because they write to tables carrying respectively no triggers, an FTS5 trigger pair, the two ledger triggers, and a DiskANN index. `CHUNK_ROWS = 1000` therefore satisfied the bound on exactly one path and missed it by up to 48× on the others. It is replaced by four constants, each derived from a measured sweep and verified at its own size: **edges 90, concepts 70, annotations 600, embeddings 30**, measuring 2.39 / 2.35 / 2.36 / 2.06 ms.

*Correction 2 — per-transaction overhead is not noise.* A one-row chunk costs **0.7–0.9 ms** on every path: BEGIN, COMMIT and the fsync are a fixed ~0.8 ms, which is over a quarter of the whole bound before a single row is written. The old paragraph asserts this amortizes away and never measured it; it is in fact the reason the sizes above are what they are, since only ~2.2 ms of the 3 ms is available to rows at all. It is also the floor on chunking granularity: chunks below ~30 rows spend more time in transaction overhead than in work.

*Correction 3 — two of the four paths are superlinear, and the first explanation of that was wrong.* On the edge path marginal cost rises from ~11 µs per row at n=10 to ~103 µs at n=1,000; on the embedding path from ~35 µs to ~151 µs. This was originally read as a property of *chunk size*, with the conclusion that shrinking those chunks was free — 1,000 edges "88.5 ms as one chunk against ~27 ms as eleven". **That is wrong ([D-059](s13-decision-register.md#d-059)):** the sweep measured every chunk size into a *fresh* database, so chunk size and table size were the same variable, and the 27 ms was eleven copies of the first chunk. Measured end to end with both arms finishing at the same table, 1,000 edges cost **85.5 ms as one transaction and 94.7 ms as eleven chunks** — chunking is ~11% slower. Smaller chunks buy latency and cost throughput on *every* path. The bound and the four constants stand, because those were measured directly; what does not stand is the claim that two of them were free.

The mechanism is diagnosed in [D-059](s13-decision-register.md#d-059) and differs between the two paths. Both are superlinear in the size of the **structure being probed** rather than the chunk: for embeddings that is inherent (DiskANN insertion rewires a graph that grows as the chunk fills it, 49 → 224 µs per vector as the corpus goes 0 → 8,000); for edges it is a **defect** — `trg_links_single_open`'s `EXISTS` is served by `idx_lc_traversal_cover` with only `source_id` bound, so every insert scans the source's whole out-degree. That one is not a chunking problem at all: it slows every interactive `assert_edge` on a high-degree node, and it means these chunk sizes meet the 3 ms bound on an empty database and not on a large one.

**The edge defect is fixed in 0.5.6** — `idx_lc_open_interval`, shipped as the `v5 → v6` rung ([§4.2](s4-schema.md#42-links-assertion-history-and-current-belief-materialization)). Re-measured with a 90-row chunk, separating the two variables the way [D-059](s13-decision-register.md#d-059) established is necessary:

| | without the index | with it |
|---|---|---|
| into an empty table | 3.18 ms | **2.69 ms** |
| into a 2,000-edge hub | 28.6 ms | **8.58 ms** |

The index is a win at every table size measured, including the empty one, so it costs the write path nothing to carry. The four constants were re-derived against it and **stand unchanged**: 2.76 / 2.57 / 2.56 / 2.26 ms for edges 90, concepts 70, annotations 600, embeddings 30 — all inside the bound, and uniformly ~9% above [D-058](s13-decision-register.md#d-058)'s figures on paths whose code did not change, which is session drift rather than regression.

#### The bound's scope: three operations are exempt

Recorded here because this is where a reader looks for it. Until 0.5.6 the exemptions lived in three separate rustdoc notes, so the rule read as though it had none:

| Path | Bound | Why it cannot be chunked |
|---|---|---|
| `write_bulk_atomic` | none — the caller sizes it | [D-014](s13-decision-register.md#d-014): the batch is *one act* under one stamp. Splitting it is the thing the method exists not to do |
| `archive()` | measured **26.8 ms** for 2,000 archivable edges | [D-012](s13-decision-register.md#d-012): copy-then-delete must be atomic, or a crash between the phases duplicates or loses rows |
| `rebuild_current()` | ~50 s per 10M edges | [D-023](s13-decision-register.md#d-023): the window between the `DELETE` and the `INSERT` is the whole of current belief |

All three are atomic **by contract**. Capping the batch and adding a third priority tier were both considered and neither taken: capping breaks the guarantee the operation exists to provide, and a third tier changes which caller waits without changing how long the lock is held. The defect was never the exemption — it was stating the bound as though it had none. A caller who needs the latency bound rather than the atomicity has `bulk_import`, which is the same write chunked and explicitly not atomic overall ([D-011](s13-decision-register.md#d-011)).
```rust
// shape only — see Appendix A for normative signatures

impl Database {
    /// Background write-back of analytics results (e.g. 50K Louvain labels).
    ///
    /// Fidelity boundary (§5.1.6): chunked writes are NOT transaction-time
    /// atomic. Each chunk commits under its own recorded_at; a reconstruct()
    /// mid-write observes a prefix of the results. Callers who need atomicity
    /// use writeanalyticsresults_atomic().
    pub async fn write_analytics_results(
        &self,
        results: Vec<AnnotationRow>,
    ) -> Result {
        // 0.5.5: chunk_rows::ANNOTATIONS, derived per path (D-058).
        const CHUNK_SIZE: usize = 500;

        for chunk in results.chunks(CHUNK_SIZE) {
            let (tx, rx) = oneshot::channel();
            self.low_pri_tx.send(LowPriCommand::WriteAnnotationsChunk {
                chunk: chunk.to_vec(),
                responder: tx,
            }).await.maperr(|| DbError::WriterUnavailable)?;
            // Wait for this chunk to commit. The await itself yields control:
            // the actor finishes the chunk, loops, and polls the high-priority
            // queue before our next send can land.
            rx.await.maperr(|| DbError::WriterDroppedResponder)??;
        }
        Ok(())
    }
}
```

Note what the error handling does not do: it does not unwrap(). If the actor task has died, send fails and the oneshot drops; both are mapped to typed variants ([§7](s6-s10-flows-to-dependencies.md#7-errors)), so an actor failure degrades to an error the caller can catch, log, and recover from — rather than a panic that cascades through every in-flight request in the process.

#### 5.1.6 The fidelity boundary of chunked writes

[Doctrine II](s0-s3-foundations.md#doctrine-ii) stamps recorded_at per application transaction; under chunking, each chunk *is* the application transaction. A 50,000-row write-back therefore lands under roughly 100 distinct stamps, and reconstruct(ts) called mid-write observes a prefix — chunk 42 learned, chunk 43 not yet. This is correct bitemporal behavior (each chunk is a distinct "the database learned these facts" event), and the ledger is unaffected (seq_id remains strictly monotonic across chunks — gaps are possible only on rollback, not on successful commit; [Doctrine IV](s0-s3-foundations.md#doctrine-iv) never sees the seams), but it is a fidelity boundary callers must be told about:

write_analytics_results — chunked, low-priority, not transaction-time atomic. The default for analytics write-backs, bulk imports, and any job where partial visibility is harmless.
writeanalyticsresults_atomic — one WriteBulkAtomic command, one transaction, one stamp, and one UI stall for the duration. The explicit escape hatch when the operation must be visible all-at-once or not at all.

The gap between the two is documented in both rustdocs and pinned by test ([§8](s6-s10-flows-to-dependencies.md#8-testing-strategy)), in the same spirit as [Doctrine VIII](s0-s3-foundations.md#doctrine-viii)'s as_of/reconstruct distinction: fidelity is a parameter, never a silent default.

#### 5.1.7 Shutdown and snapshot coordination
```rust
// shape only — see Appendix A for normative signatures

impl Database {
    /// Clean shutdown: stop the actor, then anchor the log with a final snapshot.
    pub async fn close(mut self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self.high_pri_tx
            .send(HighPriCommand::Shutdown { responder: tx }).await;
        let _ = rx.await;   // best-effort: the actor may already be gone
        if let Some(handle) = self.writer.take() { let _ = handle.await; }

        // The log is durable and the actor is quiescent: fold the tail from
        // the read side and write the final snapshot file (§5.5).
        // No write connection needed.
        snapshot::write_final(&self.read_conn).await?;
        Ok(())
    }
}
```

The sequence matters. Shutdown is answered between transactions — the actor is never mid-commit when it accepts it — so every accepted command is durable before the task exits. Dropping the handle without close() also terminates the actor (both channels close, the else arm fires), but skips the final snapshot: that is an unclean shutdown by definition, and recovery is exactly the [§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots) path — fold from the last snapshot, lose nothing, pay the cold-start latency. The writer does not restart automatically after a panic or an unclean exit; every subsequent operation returns WriterUnavailable, and the application reopens the Database. This is a deliberate choice ([D-015](s13-decision-register.md#d-015)): a dead writer indicates a bug that should surface in a crash report, not be silently papered over by a respawn.

#### 5.1.8 Write-queue latency and caller timeouts (0.5.2, D-028)

busy_timeout governs SQLite lock acquisition. It does not govern the write path described in this section, and callers must understand why. When the UI sends a HighPriCommand::AssertEdge while a rebuild_current() (~50 s at 10M edges) or an archive() transaction is in flight, the command sits in the mpsc channel and the caller awaits its oneshot responder in Rust — no SQLite call is executing on the caller's behalf, so busy_timeout never engages. The await blocks for the duration of the in-flight transaction.

This is correct behavior, not a defect: under WAL, readers are unaffected, so the UI can still render, traverse, and search while the write queues. But the latency contract of every write method must state it plainly. The rustdoc for assert_edge (and every HighPriCommand-backed method) therefore carries a # Latency section:
```rust
/// # Latency
///
/// Under normal operation, completes in < 5 ms (§9). If a `rebuild_current()`
/// or an `archive()` transaction is in flight, this call waits for that
/// transaction to commit — up to ~50 s at 10M edges (§5.8 sizing table).
/// `busy_timeout` does **not** bound this wait: it is a channel wait in Rust,
/// not a lock wait in SQLite. Reads are unaffected under WAL.
///
/// To bound the wait, wrap the call:
///
/// ```ignore
/// tokio::time::timeout(Duration::from_secs(2), db.assert_edge(edge)).await
/// ```
///
/// A timeout is **not** a cancellation. The command remains queued and commits
/// when the actor reaches it; the timeout only stops *you* waiting. For a
/// ledger this is the correct semantic — an asserted fact is asserted whether
/// or not anyone is still listening. True abandonment is an application-layer
/// `CancellationToken` checked *before* `send`, not after (D-028).
```

That last paragraph is the substance of [D-028](s13-decision-register.md#d-028) and the reason crate-level write cancellation was declined. A cancellable write command would require the actor to dequeue and discard, which introduces a failure mode the ledger does not want: a command reported as cancelled that in fact ran, or ran partially. The channel is a commitment queue, not a work queue.

#### 5.1.9 The two read paths off the handle (0.6.0, D-091)

`read_conn()` and `diagnostic_conn()` both hand out something that cannot write, and they are not the same kind of thing.

`read_conn()` returns a **shared** `&libsql::Connection` carrying `PRAGMA query_only = ON`. That pragma is per-connection and reversible by its holder in one statement, so it is a *guardrail*: it prevents an accident, not an intent. [§4.7](s4-schema.md#47-what-this-schema-does-not-enforce) invariant 2 cited it as the read-only path, and that citation was doing more work than the mechanism supports.

`diagnostic_conn()` opens the file again with `SQLITE_OPEN_READ_ONLY` and returns a connection **of the caller's own**. That is an OS-level boundary: `PRAGMA query_only = OFF` is accepted and changes nothing, and the next write fails with `readonly`. `tests/diagnostic_conn_tests.rs` asserts the pair in *both* directions, because asserting only that the diagnostic connection refuses a write would pass equally for a second `query_only` connection — what distinguishes them is what happens after the pragma comes off.

The second need it serves is independence. `read_conn()` is shared, so a long reporting query on it competes with every traversal and fold in the process; two diagnostic connections hold their own per-connection state.

Two consequences worth stating rather than discovering. `SQLITE_OPEN_READ_ONLY` drops `SQLITE_OPEN_CREATE` with it, so a path that does not exist is `SQLITE_CANTOPEN` rather than a fresh empty database — reported as [`DbError::DiagnosticConn`](s6-s10-flows-to-dependencies.md#7-errors), which names the file and says why, rather than as `NotFound`, which renders "node {0} not found" and would send a caller looking for a concept. And the stronger boundary is not uniformly stronger: `CREATE TEMP TABLE` **succeeds** on a read-only connection and is refused by `query_only`, because temp tables live in a separate writable temporary database. That is the mechanism [D-050](s13-decision-register.md#d-050) measured when it removed `TwoPhaseTempTable`, so one of that decision's two reasons no longer applies to every connection this crate can offer. Recorded, not acted on — D-050's other reason is untouched.

`Database::raw()` is `#[doc(hidden)]` rather than private ([D-068](s13-decision-register.md#d-068), [D-091](s13-decision-register.md#d-091)): the file is reachable by any SQLite client on the machine, so removing the supported way to reach it would buy the appearance of a guarantee rather than the guarantee.

#### 5.1.10 `ActorShared` — what the actor shares with the handle

The actor owns its state and is stateless per command. Two things nevertheless have to be visible from both sides, and they live in one struct behind an `Arc`:

```rust
struct ActorShared {
    metrics: crate::metrics::ActorMetrics,
    archive_epoch: std::sync::atomic::AtomicU64,
}
```

`metrics` is the hold-time histogram [§5.10](s5-modules.md#510-metricsrs--what-the-actor-holds-the-lock-for) describes: written by the actor, read by `Database::metrics()`.

`archive_epoch` is **not a measurement**, and it is deliberately outside `ActorMetrics` for that reason. It is the interlock that makes the chunked rebuild safe ([§5.8](s5-modules.md#58-integrity--audit-and-rebuild)): every completed archive increments it, and a shadow swap presenting a stale epoch is refused. Putting it in `ActorMetrics` would have made a correctness mechanism vanish when the `metrics` feature is off — which is the default. A counter that guards an invariant and a counter that reports a duration look identical in a struct and are not the same kind of thing.

### 5.2 graph/builder.rs — traversal, valid time, and attribute fidelity

**The three analytics types are closed as of 0.8.0 ([D-114](s13-decision-register.md#d-114)).** `Subgraph`, `NodeData` and `EdgeRef` had public fields through 0.7.0, which made the representation itself public API — the `BTreeMap`, the `String` keys, the two-map adjacency. None of that was ever an intended promise, and [D-087](s13-decision-register.md#d-087)'s interning cannot start while `EdgeRef::node` is a public `String`. Fields are private and the surface is accessors returning borrowed views, so nothing costs an allocation that field access did not. The break is deliberately taken with the representation **unchanged**, so anything depending on the old shape fails against code that still behaves identically — measured at zero behavioural difference across all four fixtures. `add_edge` became public in the same change: five call sites were building adjacency by hand, each maintaining the reverse edge itself, where the type had always had one function that does it.

The builder compiles every traversal into a recursive CTE over `links_current`, with every parameter bound and none interpolated — edge types included, as of 0.5.4.

**Rewritten in 0.6.0 ([D-076](s13-decision-register.md#d-076)), and the previous form is described below because the change is a correction rather than a tuning.** Through 0.5.6 the recursion used `UNION ALL` and carried a `path` column of visited ids, refusing a target already present in it. That restricts the walk to *simple paths* — so `walk` held one row per distinct path to each node rather than one row per node, and the trailing `SELECT DISTINCT` collapsed the duplication only after the work had been done. The row count is multiplicative in branching factor per hop: a **328-edge** graph at depth 6 produced **299,593** walk rows and took **403 ms** on libSQL 0.9.30. It survived five releases because every fixture measuring it was a *tree*, where there is exactly one path to each node and the pathological term is identically 1.

The current form dedupes on entry. `UNION` — not `UNION ALL` — bounds `walk` by `V × (depth+1)`, and termination comes from the depth bound rather than from inspecting a path, so the path column and its `INSTR` check are gone entirely. The same traversal now emits 49 rows in 0.2 ms. The projections keep their `DISTINCT`, because a node still legitimately appears at more than one depth. The two are equivalent: excising the cycles from any walk of length `k ≤ D` gives a simple path of length `≤ k` to the same node, so the reachable sets coincide.

```sql
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION                                             -- dedupes the queue
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to      -- the valid-time window
      AND l.weight >= ?4
      AND l.edge_type IN (?5, ?6, ...)                -- bound, never spliced
)
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
```

**Two corrections to the pre-0.5.4 text (0.5.4).** First, the document described two SQL shapes — an "active mode" carrying `l.valid_to > :now`, and an `as_of(ts)` variant that "rewrites exactly two predicates". There is one shape. The half-open window `l.valid_from <= ?3 AND ?3 < l.valid_to` is parameterised by a timestamp, and "active" is that predicate evaluated at `now`. One shape means one thing to test and one place for the [D-029](s13-decision-register.md#d-029) canonical-form requirement to bite; a rewrite that produces a second predicate string is a second thing that can be wrong.

Second, the claim that "the terminal concept filter likewise" is rewritten is false, and the truth is a limitation worth stating plainly: `c.retired = 0` is evaluated against the **live** concept row at every `ts`. A concept retired today is therefore absent from `as_of(last year)`. This is defensible — `retired` is the application axis, not the domain axis ([§4.1](s4-schema.md#41-concepts-and-per-model-embeddings)), and it answers "should the user see this now?" rather than "did this hold then?" — but it means a valid-time traversal is filtered by a present-tense predicate, and callers reconstructing a historical view should use `reconstruct(ts)` ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)), which folds the log and sees `retired` as it was believed. The distinction is [Doctrine VIII](s0-s3-foundations.md#doctrine-viii)'s, applied one level down.

The projection returns node ids alone, ordered by id. Attributes are a separate step, because the three modes read from two different places:

```rust
pub enum AttributeMode {
    Current,   // live attributes from `concepts`. Fast. WRONG for historical text.
    AtTime,    // attributes as believed at ts, hydrated from transaction_log.
    Omit,      // topology only; the concepts read is skipped entirely.
}
```

`AtTime` hydration leaves the walk untouched and resolves attributes for the result set with one window query — latest log entry per entity with `recorded_at <= :ts` — merged over the live rows ([§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration)). The walk's cost is unchanged; the hydration is one indexed scan over `idx_txlog_entity` bounded by the result-set size, not by the log size.

Separating hydration from the walk is what fixed the second defect [D-039](s13-decision-register.md#d-039) records. The pre-0.5.4 `build_sql` always emitted the `concepts` join, so `attribute_mode` was stored on the builder, exposed by a builder method, and never read: a caller asking for `AtTime` received live attributes with no indication the mode had been ignored. That is the exact failure [Doctrine II](s0-s3-foundations.md#doctrine-ii) exists to prevent, and it arrived as a silent wrong answer rather than as an error.

**Edge types are bind parameters, not literals (0.5.4, [D-039](s13-decision-register.md#d-039)).** An earlier version spliced them into the CTE with `format!("'{t}'")`, which made any caller-supplied string a SQL fragment on the read path. The only validation in the crate, `validate_edge_type`, runs from `EdgeAssertion::normalized` on the *write* path, so a traversal never passed through it — and that function's own doc comment cited the traversal CTE as its justification. Binding removes the question rather than answering it: unlike a table name, an edge type is a value, and values can be parameters. Contrast [D-037](s13-decision-register.md#d-037), where a model name is an identifier, cannot be bound, and validation was therefore the only option.

**The recursive step is index-only (0.5.4, [D-042](s13-decision-register.md#d-042)).** `idx_lc_traversal_cover` carries every column the walk reads, so the hop join never fetches a base-table row: `EXPLAIN QUERY PLAN` reports `SEARCH l USING COVERING INDEX idx_lc_traversal_cover (source_id=? AND valid_from<?)`, and `links_current` itself is not touched until the terminal join to `concepts`. That plan shape is asserted by test rather than assumed, including the seek constraint and not merely the word `COVERING` — the column order is what makes the difference, and an index in the wrong order still exists, still reports as covering, and still walks more of itself than it needs to. [D-042](s13-decision-register.md#d-042) records the ordering argument and the measurement that corrected it.

~~**Cycle-detection performance note (0.5.1).** `INSTR(w.path, CAST(l.target_id AS BLOB))` is O(path length) per hop… If a use case ever requires depth ≥ 20, benchmark this against a visited-set CTE (`json_each` over a JSON array of visited ids) and document the crossover.~~ **Obsolete as of 0.6.0 ([D-076](s13-decision-register.md#d-076)): there is no path column and no cycle check.** The note is struck rather than deleted because it is a good example of the trap it fell into — it costed the cycle check carefully, per hop, and concluded the design was "correct and fast for the target workload". It was correct. The cost it was measuring was not the one that mattered, and depth was not the variable: at depth 6 on a 328-edge *graph* the query took 403 ms, while the `INSTR` it warns about at depth 50 would still have been ~1,300 bytes of memchr. Depth bounded the path length; branching factor multiplied the number of paths, and nothing here was watching that.

### 5.3 graph/vector_filter.rs — strategies and the byte-budget cost model

Vector queries rarely arrive naked: the caller wants "the ten nearest neighbours of this embedding *among concepts reachable in two hops*", or "*among edges of type `CITES` with weight ≥ 0.7*". DiskANN answers the pure-vector question and the relational engine answers the pure-filter question; the composition is where naive implementations go wrong, because the two access paths cannot be nested — the DiskANN index is opaque to SQL predicates, and the relational filter is opaque to the index.

**Two** strategies are available, and the choice is a cost decision, not a style decision. The 0.4.5 text named three — `VectorFirst` / `FilterFirst` / `Staged` — and 0.5.4 renamed them without changing them. 0.5.5 removes the third, for reasons recorded below and in [D-050](s13-decision-register.md#d-050).

- **`PostFilter`** (was `VectorFirst`) — retrieve a generous top-k′ from DiskANN, then post-filter against the relational predicate. Cheap when the filter is loose, because most of the top-k′ survives; **it is where the answer set falls off the end of k′ when the filter is tight**, and that failure is silent by nature. See the escalation rule below, which is what stops it being silent here.
- **`PreFilterCTE`** (was `FilterFirst`) — materialize the candidate id set from the relational predicate, then re-rank candidates by exact vector distance: a brute-force scan over `F32_BLOB` rows with no index. Cheap when the candidate set is small and the filter selective; a full-table distance scan when it is not. Exact by construction — every candidate is scored, so nothing can fall off an end.

The byte-budget model makes the choice arithmetically rather than by rule of thumb ([D-007](s13-decision-register.md#d-007)). For each strategy the planner estimates bytes touched:

| Strategy | Estimated bytes |
|---|---|
| `PostFilter` | `k′ × (vector_bytes + row_bytes)` + the filter pass |
| `PreFilterCTE` | the filtered scan + `\|candidates\| × vector_bytes` of exact distance computation |

`vector_bytes` comes from the model's declared dimension (768 × 4 for `nomic_v1`, read from the schema per [D-037](s13-decision-register.md#d-037) rather than from a registry the crate maintains); selectivity is `|candidates| / corpus`, measured by the counting probe below; and the planner takes the minimum. Estimates are logged at `debug` alongside the chosen strategy **and returned to the caller** as a `CostEstimate`, so tuning is empirical and a test can assert on the plan rather than scrape log output. The subgraph byte budget of [§5.4](s5-modules.md#54-graphsubgraphrs-and-graphalgorithmsrs--native-in-memory-analytics) applies as a hard ceiling on the candidate set regardless of strategy, and exceeding it raises `SubgraphTooLarge` rather than silently degrading.

**The strategy may never change the answer, and this is enforced rather than intended.** `PostFilter`'s failure mode is not a slow query but a short one: a top-ten that returns four rows and reports success. So when a post-filtered pass comes back short *and* its underlying index scan was saturated — it returned every row it asked for, so more existed — the planner cannot conclude the missing matches do not exist, and escalates to `PreFilterCTE`. The acceptance gate is that the two strategies agree on every query, swept across filter tightness and k. Strategy is then a performance decision and nothing else, which is the only form in which a planner is safe to have; a planner that picks between different answers is a fidelity leak of the kind [Doctrine VIII](s0-s3-foundations.md#doctrine-viii) names.

**Implementation status (0.5.5): implemented.** `FilteredVectorSearch` is the public surface — a builder mirroring [`TraversalBuilder`](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), taking a model, a query vector, a k and a traversal. Both strategies have execution bodies, `CostEstimator` reads its `byte_budget`, and the estimate is logged and returned. The strategy is planner-chosen; `FilteredVectorSearch::strategy` forces one, and exists for the agreement test above.

**The three premises of the 0.4.5 design, now all three measured (0.5.5).** They were recorded in 0.5.4 as unestablished. Measuring them is what removed a strategy:

1. **`CREATE TEMP TABLE` on the read connection fails** with `SQLITE_READONLY (8), "attempt to write a readonly database"`. `PRAGMA query_only = ON` ([D-019](s13-decision-register.md#d-019)) covers the TEMP database too. D-019 is the runtime half of the write-serialization guarantee and does not give way, so the staging mechanism is unavailable on the only connection a caller can reach. The reformulation the 0.5.4 note proposed — carry the candidates in the statement as bound parameters — is what `PreFilterCTE` now does, and it needs no write privilege at all, which makes it strictly better than the mechanism it replaces rather than a concession to it.
2. **The allow-list push-down does not exist.** `vector_top_k` refuses a fourth argument at runtime — *"too many arguments on vector_top_k() - max 3"* — and `vectorIndexSearch` in the bundled amalgamation rejects `argc != 3` before inspecting anything. So `TwoPhaseTempTable` had neither of its two mechanisms, and its row in the cost table priced an operation the engine cannot be asked to perform. It is removed ([D-050](s13-decision-register.md#d-050)).
3. **Selectivity has no source, so it is measured rather than estimated.** SQLite maintains no histograms and `sqlite_stat1` carries average rows-per-key, which estimates an equality predicate and not multi-hop reachability. The planner therefore runs a **bounded counting probe**: the traversal, under a cap (`DEFAULT_PROBE_CAP`, 10,000). It costs a fraction of what it prices and the cap bounds that fraction; above the cap the planner knows only "at least the cap", which is already enough to reject `PreFilterCTE`. The probe doubles as the candidate set, so the walk is paid for once rather than twice. `CandidateCount::Exact` and `::AtLeast` keep the distinction in the type, because a capped probe has not measured a count and must not be read as though it had.

### 5.4 graph/subgraph.rs and graph/algorithms.rs — native in-memory analytics

Superseded in 0.5.4 by [D-039](s13-decision-register.md#d-039), which replaced the petgraph bridge this section used to describe. The analytics surface is five algorithms — Dijkstra, A\*, strongly connected components, k-core, and Louvain — implemented in-crate over an adjacency list, with no external graph dependency.

`Database::load_subgraph(start_node, max_hops, now_ts, byte_budget)` walks `links_current` under the same bounded CTE shape as [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), hydrates node attributes, and returns a `Subgraph` holding `BTreeMap` node and adjacency maps. Two boundary conditions are enforced during the load rather than left to the algorithms:

- **The byte budget is measured and enforced, in linear time.** `DbError::SubgraphTooLarge` existed in the error enum from 0.4.0 and was constructed nowhere, so [D-007](s13-decision-register.md#d-007)'s budget was declared and unenforced through four releases. `load_subgraph` now carries a running payload total and refuses the moment it passes the budget — topology as edges arrive, attributes as they hydrate. The total is accumulated rather than recomputed: `estimated_bytes()` is O(V + E), and calling it per row made loading O(E²), so the check that bounds a load was itself the thing that did not scale ([D-047](s13-decision-register.md#d-047)). Both the agreement between the running total and the derivation, and the growth rate, are pinned by test.
- **Negative and NaN edge weights are refused at the boundary** with a typed `NegativeEdgeWeight`. Dijkstra and A\* are correct only for non-negative weights; a negative weight yields a shortest path that is merely a path. `links.weight` is a bare `REAL NOT NULL` with no CHECK, and adding one is a schema change against the [D-036](s13-decision-register.md#d-036) freeze that is not taken unilaterally — so the refusal sits at the load boundary instead. Note where that leaves it: `EdgeAssertion::normalized` does not check weight either, so the *write* path accepts a negative weight and only the read refuses it — the odd one of the three gaps [§4.7](s4-schema.md#47-what-this-schema-does-not-enforce) now gathers, and the only one still genuinely open ([D-074](s13-decision-register.md#d-074)).

Determinism is a property of the data structures, not of a sort applied at the end. `Subgraph` uses `BTreeMap`; the algorithms return `BTreeMap`/`BTreeSet` rather than the hashed equivalents; and every tie is broken explicitly — equal-distance heap entries by node id, equal-gain Louvain moves by the lower community index. Any one of the three left as a `HashMap` reintroduces per-process variation, because Rust's default hasher is seeded per process. Distances additionally need a total order that `f64` does not have: `OrdF64` wraps `total_cmp`, so a NaN weight cannot silently corrupt the heap's invariant.

Write-back goes through the low-priority channel. `Subgraph::write_back_annotations` delegates to `Database::write_concepts` (`write_annotations` through 0.5.6 — [D-075](s13-decision-register.md#d-075)), which chunks at `chunk_rows::CONCEPTS` ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)) — so an analytics job that annotates 50,000 nodes yields the writer to the UI at every chunk boundary and accepts the fidelity boundary of [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) in exchange. The 50,000-label Louvain save is the case the 0.4.5 amendment was designed around.

**Where analytics output belongs (0.5.4, [D-041](s13-decision-register.md#d-041)).** An analytics result is an annotation, and an annotation is derivative state:

```rust
pub struct Annotation {
    pub concept_id: String,
    pub label: String,      // e.g. "louvain.community"
    pub value: String,      // JSON-encoded payload
}
```

They land in `analytics_annotations` ([§4.5](s4-schema.md#45-analytics-annotations--the-second-derivative-table-054-d-041)): a derivative table created by the migration runner alongside the normative schema, disposable and rebuildable by re-running the algorithm ([Doctrine VI](s0-s3-foundations.md#doctrine-vi), second category), and excluded from `transaction_log` by the same reasoning that excludes embeddings ([Doctrine VII](s0-s3-foundations.md#doctrine-vii)). No trigger is defined on it, so the ledger never sees it — and a reconstruction that needs community labels recomputes them, which is the only honest way to ask what a past graph's communities *were*.

`Subgraph::write_back_annotations(&db, label, &values)` builds one `Annotation` per node present in `values` and hands them to `Database::write_analytics_annotations`, which chunks at `chunk_rows::ANNOTATIONS` on the low-priority tier. The per-chunk fidelity boundary of [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) is acceptable here for a reason it is not acceptable for assertions: a partially written analytics pass is recoverable by rerunning, and a partially written history is not.

**What this replaced (0.5.4, [D-041](s13-decision-register.md#d-041)).** Until 0.5.4 the method built a `ConceptUpsert` per node and put the annotation value in the `content` field, so writing back a Louvain partition **overwrote every annotated concept's document text with a community label**. Two further consequences followed: the annotations entered `transaction_log` through the concept triggers, permanently inflating the ledger with derived data [Doctrine VII](s0-s3-foundations.md#doctrine-vii)'s reasoning excludes; and because a concept `UPDATE` requires a strictly advancing `recorded_at` ([§4.3](s4-schema.md#43-the-transaction-log)), rerunning the same algorithm produced a fresh version of every concept, so the ledger recorded repeated analytics passes as though the world had changed. The method's own doc comment defended the write as "a normal bitemporal write and not an edit of history," which was true of the mechanism and false of the intent: that mechanism is correct for a domain fact, and a community label is not one.

The full rationale, the rejected alternatives, and the differential-testing argument that replaces petgraph's track record are in [D-039](s13-decision-register.md#d-039).

### 5.5 temporal/replay.rs and temporal/snapshot.rs — reconstruction and snapshots

The replay fold derives belief-at-`ts` from the log using a window function:

```sql
SELECT seq_id, table_name, entity_id, operation, payload
FROM (
    SELECT seq_id, table_name, entity_id, operation, payload,
           ROW_NUMBER() OVER (
               PARTITION BY entity_id ORDER BY seq_id DESC
           ) AS rn
    FROM transaction_log
    WHERE recorded_at <= ?1
) WHERE rn = 1;
```

The `seq_id DESC` ordering resolves same-timestamp entries within a single transaction correctly by construction, and `idx_txlog_entity` lets SQLite stream each partition without a global sort. Where an anchored fold is used, the `seq_id > :anchor` predicate is an inequality, which correctly skips any gaps left by rolled-back transactions ([D-024](s13-decision-register.md#d-024) — see the [§4.3](s4-schema.md#43-the-transaction-log) monotonicity note). The fold routes rows on `table_name`, deserializes payloads with `serde_json` (branching on the payload version field, and raising `PayloadVersion` for anything newer than the crate understands), populates `ReplayCorrupt` with the actual offending `seq_id` rather than a placeholder, and builds an adjacency index as it goes — so the resulting `MaterializedState` answers node, edge and neighbour queries with the shape of the live `Database`. Callers do not need to know whether they are querying the present or the past. The fold is a read: it runs on `read_conn` and never touches the actor.

**The cold-database read path (0.5.2, [D-026](s13-decision-register.md#d-026)).** When `ts` predates the hot log's horizon, the delta needed to answer the question lives in the archive. `reconstruct` tests coverage first — is the oldest hot entry newer than `ts`? — and if the hot log does not cover it, ATTACHes the archive database, folds a `UNION ALL` of `main.transaction_log` and `cold.transaction_log` through the identical window query, and DETACHes:

```sql
SELECT seq_id, table_name, entity_id, operation, payload
FROM (
    SELECT seq_id, table_name, entity_id, operation, payload,
           ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) AS rn
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, recorded_at
          FROM main.transaction_log
        UNION ALL
        SELECT seq_id, table_name, entity_id, operation, payload, recorded_at
          FROM cold.transaction_log
    ) WHERE recorded_at <= ?1
) WHERE rn = 1;
```

The hot entry wins for entities present in both files because its `seq_id` is greater — the same last-writer-wins rule as snapshot composition, so the two paths agree by construction. ATTACH and DETACH bracket exactly one fold and never persist. The DETACH is unconditional, error paths included: ATTACH is not transactional and survives `ROLLBACK`, so a handle leaked by an early return would make every later `reconstruct` *and* every later `archive` fail with "database cold is already in use" — one corrupt payload would permanently poison the connection. `archive()` carries a note about the same failure mode, and the two share a shape.

**And a best-effort DETACH runs before each ATTACH (0.5.4, [D-044](s13-decision-register.md#d-044)).** Pairing covers every `Result` path and cannot cover a panic unwinding between the two, which skips the DETACH whatever the function would have returned. Recovering on the way *in* closes that hole without a destructor: in normal operation the statement fails harmlessly with "no such database: cold", and after a leak it turns permanent poisoning into one failed statement nobody sees. The `Drop` guard that suggests itself is not available — `execute` is `async`, `Drop` cannot await, and such a guard would discard the future and look like cleanup while doing none. [D-044](s13-decision-register.md#d-044) records why. If the cold file is absent or has been moved, the module returns a typed `ReplayCorrupt` naming the condition rather than a wrong answer ([R14](s11-s12-milestones-and-risks.md#r14)).

**The snapshot file is a versioned container (0.5.4, [D-043](s13-decision-register.md#d-043)).** Ten bytes precede the payload — magic `MACR`, a `u16` container format version, and the `u32` schema version this build understands — written uncompressed and checked before anything is decompressed:

```
offset 0  4  6        10
       MACR|fmt|schema| zstd(bincode(MaterializedState)) ...
```

A mismatch is `DbError::SnapshotIncompatible`, deliberately distinct from `ReplayCorrupt`: corruption is a fault to report, an incompatible snapshot is the ordinary consequence of upgrading, and the right response to the second is to discard the file and fold from the log. Without the header this failure is silent rather than loud — `bincode` is not self-describing, so a file written against a different shape of `MaterializedState` does not reliably fail to load, it loads into wrong values, and the newest snapshot is the first thing a restart reaches for. Files written before the container existed are caught by the same check, since their first bytes are zstd's magic rather than `MACR`.

Because an append-only log grows without bound, reconstruction is *designed* to compose with snapshots. A snapshot is a full `MaterializedState` serialized with bincode and compressed with zstd — JSON's readability buys nothing when both ends of the wire are owned by this crate — stored as a sidecar file named for the `transaction_log.seq_id` it reflects (`snapshots/0000000000000000123.snap.zst`). Snapshots are written every 10,000 log entries and on clean shutdown, with retention of the last five plus one daily for thirty days. Snapshot creation is a read-fold plus a file write; it never requires the write connection, and it runs entirely on the read side — a lightweight maintenance task watches `seq_id` through `read_conn`, and `close()` writes the final anchor after the actor has exited ([§5.1.7](s5-modules.md#517-shutdown-and-snapshot-coordination)). Keeping replay and snapshotting off the write connection is deliberate: the actor's loop must stay short enough that [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)'s latency bound holds.

`reconstruct(ts)` is specified to locate the newest snapshot `S` with `S.created_at <= ts`, fold the log delta (`seq_id > S.seq_id AND recorded_at <= ts`), and merge: for each entity the fold's row wins, because it is newer; for entities absent from the fold, the snapshot's row stands. The composition rule is last-writer-wins by `seq_id`, which is the same rule `links_current`'s upsert applies and the same rule the cold fold applies — three paths, one rule.

> **Implemented in 0.5.4 ([D-049](s13-decision-register.md#d-049)), with two carve-outs.** The composition above is what `reconstruct` does, and `Database::reconstruct(ts)` supplies both paths from the handle so it is the default. The merge collects *tombstones* — a winning `'D'` row, or a `retired = 1` concept — because onto a snapshot those must remove an entity the base carries, where folding from nothing they were indistinguishable from absence. Agreement between the composed and full-fold answers is a property test over generated histories, which is what the "three paths, one rule" sentence above had asserted since 0.4.5 without evidence.

**Corrected in 0.5.6 ([D-072](s13-decision-register.md#d-072)): there is no `'D'` row, and the fold now says so.** [Doctrine V](s0-s3-foundations.md#doctrine-v) permits no physical delete outside an archive session, all three hot tables carry delete guards, and the archive *moves* rows to the cold file rather than logging their removal — so nothing in the schema writes a `'D'` and nothing in the crate can produce one. Handling it as a tombstone was a claim that deletions are recorded and reconstructible, which is not true of this ledger. A `'D'` row now raises `ReplayCorrupt`, naming the sequence number, the table, and the rule it violates.

So exactly one thing populates a tombstone, and the asymmetry is worth stating rather than hiding:

* A **concept** disappears by being **retired** — a `'U'` row whose payload carries `retired = 1`. Onto a snapshot that is a genuine removal, and it is the case the paragraph above was really describing all along.
* An **edge never disappears.** Retiring one asserts a successor over the same interval key (same `source|target|type|valid_from`, later `recorded_at`), so the log row is an `'I'` under the *same* `entity_id` and last-writer-wins replaces the tuple in place. There is nothing to remove because nothing left — the interval closed. That is [Doctrine III](s0-s3-foundations.md#doctrine-iii) visible at the fold.

The `edges_gone` set that existed to serve the `'D'` branch is removed with it: it was populated from nowhere else, and closing one unreachable path by opening another is not a fix.
>
> **Carve-out 1 — closed in 0.5.5 ([D-052](s13-decision-register.md#d-052)).** Composition *was* disabled once an archive database existed, because archived log rows are scattered rather than a prefix: a row the delta needed could be gone while a newer row for the same entity — recorded after `ts` — kept it out of the hot log, and the composed answer would be silently wrong. The delta now folds hot and cold together (`ANCHORED_COLD_FOLD`), so the archived row is visible and the reason is gone rather than the symptom. Closing it also fixed a **live wrong-answer defect** in the same neighbourhood: `hot_log_covers` decided whether the cold file was needed by asking `MIN(recorded_at) <= ts`, which tests how far back the hot log *reaches* rather than whether it is *complete*. See [D-052](s13-decision-register.md#d-052).
>
> **Carve-out 2 — closed in 0.5.5 ([D-053](s13-decision-register.md#d-053)).** The maintenance task specified above now exists, read-side as specified, and the lifecycle question it was left open on is settled: `Database` owns it, a `watch` channel stops it (dropping the sender counts, so a handle dropped rather than closed does not leave it running), and `close()` stops and joins it **before** stopping the actor and taking the final snapshot — both it and `write_final` end by running retention, and retention deletes files. The trigger is a *distance* in log entries, not a schedule, so an idle database writes nothing however long it stays open. `Database::open_with_cadence(path, None)` opts out.
>
> **Retention caught up immediately after ([D-054](s13-decision-register.md#d-054)).** "The last five plus one daily for thirty days" is now what the code does. The divergence had cost nothing while snapshots were written once per shutdown — five anchors were five shutdowns — and a cadence turned it into a rule that defeated the feature it had just been given: five anchors can span minutes under load, so every older instant folded the whole log. "Today" is the newest snapshot's own day rather than the wall clock, so retention is a function of the directory's contents alone and a database left untouched for a year is not emptied by the first write after it wakes up. The container header gains the snapshot's instant (format v2) so bucketing costs eighteen bytes per file instead of decompressing each one.

**Snapshot file cleanup (0.5.1).** The retention policy must delete the corresponding `.snap.zst` files from the filesystem when a snapshot expires. The cleanup routine in `snapshot.rs` uses `std::fs::remove_file` for each expired sidecar and logs a `tracing::warn!` if deletion fails — a file locked by another process, typically. Expired snapshots that cannot be deleted are left in place and retried on the next pass; they do not affect reconstruction correctness, since the fold ignores snapshots older than the retention window.

> **Terminology note (0.5.0).** Prior versions of this document used "checkpoint" for this application-level concept. That collides with SQLite's WAL checkpoint (`PRAGMA wal_checkpoint`), the engine's own mechanism for flushing the write-ahead log into the main database file — an entirely different operation. To eliminate confusion in code review, commit messages and operational discussion, the application concept is called a **snapshot** throughout. The file extension is `.snap.zst`; the module is `snapshot.rs`. Where this document means the engine's mechanism, it says "WAL checkpoint" explicitly.

### 5.6 temporal/as_of.rs — valid-time queries and attribute hydration

`as_of` is a filtered read of live tables; it never touches the log. Its edge query applies the half-open window directly to `links_current`:

```sql
SELECT source_id, target_id, edge_type, valid_from, valid_to
FROM links_current
WHERE valid_from <= ?1 AND ?1 < valid_to;
```

This is the same predicate the traversal CTE carries ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity)), against the same table, which is the point: `as_of` and a traversal at `ts` agree because they ask the same question of the same rows, not because two query builders were kept in step by hand.

The module also owns attribute hydration, which is the mechanism behind `AttributeMode` ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), [§4.4](s4-schema.md#44-asymmetric-versioning-deliberately)). `hydrate_attributes(conn, node_ids, ts, mode)` is the single entry point and dispatches on the mode: `Omit` returns nothing and reads nothing; `Current` reads the live `concepts` rows; `AtTime` resolves the latest `transaction_log` entry per entity with `recorded_at <= ts` and deserializes attributes from the payload. It is bounded by the id list handed to it — the result set — rather than by the log, which is what keeps the [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budget of 30 ms for 100 nodes independent of history size.

The honest cost of [§4.4](s4-schema.md#44-asymmetric-versioning-deliberately)'s asymmetry lands here. `Current` is the default because it is what almost every caller wants and because the alternative taxes every query in the system; it is documented as wrong for historical text, and it emits a `tracing::warn!` when combined with `as_of`. A caller who needs belief-at-`ts` fidelity across retroactive assertions should not reach for `AtTime` at all — they should use `reconstruct(ts)` ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)), which answers a transaction-time question rather than patching attributes onto a valid-time answer.

### 5.7 temporal/archive.rs — cold storage

Archive moves closed intervals and superseded log rows into a separate database file via `ATTACH`. The session is one atomic transaction ([D-012](s13-decision-register.md#d-012)): create the session marker, copy rows to the cold file, verify counts, re-derive the affected materialization, record the horizon, drop the marker, commit. A crash anywhere rolls the whole thing back, leaving hot and cold mutually consistent.

**What "re-derive the affected materialization" costs, and which variable it scales with (0.6.0, [D-077](s13-decision-register.md#d-077)).** That step is `rebuild_within`, and it is not a touch-up of the archived rows — it is `DELETE FROM links_current` followed by a full window-function reprojection over **everything still in `links`**. Two consequences the sentence above hides. First, the archive's repair term grows with the *surviving* table, not with the batch being archived, so [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s "Archive, 100K closed intervals ≤ 30 s" is parameterised on the wrong quantity: archiving a fixed 100K intervals costs steadily more as the ledger grows. Second, until 0.6.0 that rebuild also ran `audit_current` on itself — two `EXCEPT` passes over the whole projection, O(E log E) each, inside the archive's write transaction. Measured at 4K/16K/40K rows in `links`, the audit alone costs **≈15 / 61 / 190 ms** — **roughly half the whole repair**. The audit's own figure is the stable one across runs (179–212 ms at 40K over five runs); the rebuild total is not (318–428 ms for identical work), so the *share* reads anywhere from 42% to 61% depending on the run and is quoted as "about half" rather than to a precision the harness does not have. That is [D-070](s13-decision-register.md#d-070)'s session-noise caution applied to this cycle's own numbers. It is now skipped on this path ([D-077](s13-decision-register.md#d-077)); `rebuild_current`, the operator-facing repair, still verifies.

None of this was hidden by the published figures so much as unattributed by them: the `archive` cost in [§5.1](s5-modules.md#51-connectionrs--the-handle-the-pragmas-and-the-write-actor)'s exemption table is measured end-to-end through `Database::archive`, so the re-derivation was always inside it. Nobody had asked what fraction it was.

**The session marker is committed state at no point ([D-008](s13-decision-register.md#d-008), revised 0.5.3).** `CREATE TABLE macrame_archive_session (x)` is the first statement *inside* the transaction and `DROP TABLE` is the last, so commit drops it and rollback discards it. There is no crash path that leaves the delete guards disarmed.

The marker is an ordinary table in `main`, not a TEMP table, and the correction matters because the original formulation was unimplementable rather than merely suboptimal. Pre-0.5.3 the document specified `CREATE TEMP TABLE archive_session` with the guards probing `temp.sqlite_master`. SQLite forbids a trigger in `main` from referencing objects in another database, `temp` included, so that guard fails at `CREATE TRIGGER` time — the schema would not install. The guards therefore probe `main.sqlite_master` for the marker's name, which preserves [D-008](s13-decision-register.md#d-008)'s actual point: probing the catalogue rather than selecting from the marker directly is what turns an illegal delete into a typed `ArchiveViolation` instead of a "no such table" error nobody can act on. The connection-locality argument the TEMP formulation rested on is no longer needed, but its conclusion still holds by construction: only the Write Actor can create the marker, because only the Write Actor can write.

**ATTACH is issued outside the transaction and DETACH unconditionally on the way out**, error paths included — the same reasoning as [D-026](s13-decision-register.md#d-026) ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)): ATTACH is not transactional and survives `ROLLBACK`, so a leaked handle poisons the connection for every later archive and every later cold-database reconstruct. A best-effort DETACH also runs before the ATTACH, which is what closes the one hole pairing cannot — a panic between the two (0.5.4, [D-044](s13-decision-register.md#d-044)).

**Archive scope.** The path targets exactly three tables, and the third was added in 0.5.3:

1. **`links`** — an assertion is archivable when `recorded_at < :cutoff` **and** it is either superseded by a later assertion for the same interval key, or it is the current belief for an interval that closed before the cutoff. The predicate is deliberately not "`valid_to` is in the past": it must keep every row `links_current` still projects, or [Doctrine VI](s0-s3-foundations.md#doctrine-vi)'s rebuildability is broken by the archive itself.
2. **`transaction_log`** — an entry is archivable when `recorded_at < :cutoff` and a later entry exists for the same entity. The newest entry per entity always stays hot, so `reconstruct(now)` never needs the cold file.
3. **`links_current`** — the rows projecting the intervals that were removed. `links_current` must remain equal to the latest-belief projection of what is left in `links` ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)), or `audit_current()` reports drift the moment an archive runs. **As of 0.5.4 this is done by re-derivation, not by a compensating DELETE ([D-035](s13-decision-register.md#d-035)):** the session calls `integrity::rebuild::rebuild_within(&tx)` inside the archive transaction. The prior hand-written compensation was a predicate over *valid* time standing in for one that also requires transaction time, and the two are not the same set — an interval closed at the cutoff but recorded at or after it survives in `links` while the compensation deleted it from `links_current`, producing permanent drift that the archive path itself caused. A description of a set can drift from the set; a derivation cannot.

**Concepts are never physically archived ([D-022](s13-decision-register.md#d-022)).** This is a structural decision, not an omission, and it rests on three things:

- **Foreign key integrity.** `embeddings_<model>.concept_id REFERENCES concepts(id)`. Deleting a concept violates the FK unless `ON DELETE CASCADE` is declared, and CASCADE silently destroys the embedding — a derived artifact [Doctrine VII](s0-s3-foundations.md#doctrine-vii) protects.
- **Portability of the cold file.** `F32_BLOB` and DiskANN are libSQL-specific. The cold database is a plain file opened via `ATTACH` and is deliberately trigger-free and FK-free — the delete guards must not exist on a file whose whole purpose is to receive rows, and a FK from `cold.links` to `concepts` could not be satisfied in any case.
- **Entity versus interval semantics.** Concepts are entities. They have no "closed" state analogous to a retired edge. A concept's lifecycle is `retired = 1` (application soft-delete) and `valid_to` (temporal expiry); physical removal is not part of it.

`trg_concepts_guard_delete` remains as a safety net against accidental deletion via raw SQL, a migration script, or a future code path that targets concepts without going through the session. In normal operation it never fires. Concept *archival* — as distinct from erasure — is deferred rather than ruled out, and [Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred) records the shape it would take; the constraint that survives is the third one, which turns archivability into a reachability predicate rather than an expiry predicate.

If a concept must be removed for legal or compliance reasons, that is a separate operation outside the archive path, requiring explicit handling of embeddings, log entries, and `links_current` rows. It is not designed here because it is not part of the ledger's normal lifecycle ([Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred)).

**Sizing (0.5.0, updated 0.5.1).** [D-012](s13-decision-register.md#d-012) correctly keeps the archive atomic, but the lock-hold cost must be acknowledged. The planning estimate of 1,000 rows/sec is deliberately conservative; actual throughput on NVMe for `INSERT INTO … SELECT` across an `ATTACH` boundary, including trigger amplification on the deletes, is likely 5,000–10,000 rows/sec. At 1,000 rows/sec a 1M-row archive holds the write lock for ~16 minutes; at 10,000 rows/sec, ~100 seconds.

Because the archive is a single atomic transaction, the cooperative chunking of [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) does not apply — there are no boundaries at which the actor can yield, and UI assertions queue behind the whole transaction ([§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) states this in the latency contract). The mitigation is at the scheduling layer: the scheduler estimates the transfer-set size before issuing the command, and if it exceeds 100,000 rows it logs a warning and defers to the next idle window with a smaller cutoff. Each window is still one atomic transaction; the scheduler bounds the window's scope rather than the transaction's.

**Crash recovery between scheduled windows (0.5.1).** If the application crashes between two windows, some rows are in the cold database (committed window) and some remain hot (unstarted window). This is safe: nothing is duplicated or lost. The cold database holds a complete copy of what it received; the hot database still holds what it did not send. The next run resumes from the horizon recorded in `cold.archive_horizon`. This is distinct from chunking *within* a transaction, which [D-012](s13-decision-register.md#d-012) rejects: the scheduling-layer windows are separate transactions, each atomic.

### 5.8 integrity/ — audit and rebuild

`audit_current()` runs on `read_conn`. It compares `links_current` against the latest-belief projection of `links` and returns a count of divergent intervals. Zero is the only acceptable answer in steady state.

**The audit computes a symmetric difference, and the way it does so is load-bearing (0.5.4, [D-030](s13-decision-register.md#d-030)).** The 0.4.5–0.5.3 query chained four compound-SELECT arms flatly — `A EXCEPT B UNION B EXCEPT A`. SQLite gives `EXCEPT` and `UNION` equal precedence and evaluates left-associatively, so that parses as `(((A EXCEPT B) UNION B) EXCEPT A)`, which reduces to a constant zero. The audit reported "no drift" for every possible state of the database, including a `links_current` the test suite had deliberately corrupted. An integrity check that cannot fail is worse than no integrity check, because it is trusted. Parentheses are not the fix — SQLite rejects a parenthesised compound-SELECT operand outright. The corrected query names both sides as CTEs and computes each direction inside its own scalar subquery, summing the two, so the grouping is structural rather than syntactic and no future edit can silently regroup it. Both directions are required: a row `links_current` lacks is a missed materialization, a row it holds that the projection does not is stale or spurious, and either alone is drift.

`rebuild_current()` is a high-priority command ([D-013](s13-decision-register.md#d-013)). It deletes every row of `links_current` and re-inserts from the projection **in a single transaction** — which, until 0.5.4, it did not do. It had been a bare `DELETE` followed by an `INSERT`, and the window between them is the whole of current belief: a failure across it, or a concurrent reader landing in it, sees a graph with no edges and no error. [D-023](s13-decision-register.md#d-023) claimed atomicity in a document whose code did not implement it. `rebuild_within(&tx)` is the transactional form, and it is also what the archive session calls ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage), [D-035](s13-decision-register.md#d-035)).

**Sizing and operational guidance (0.5.1, [D-023](s13-decision-register.md#d-023)).** Rebuild is a recovery operation, not a routine one. In steady state the triggers maintain `links_current` correctly and the audit returns zero; a nonzero result indicates a bug — a trigger failure, manual manipulation, or a restore from an inconsistent backup — that should be investigated rather than papered over.

**"The rebuild is atomic by necessity" was the claim here through 0.5.6, and 0.6.0 falsified it ([D-082](s13-decision-register.md#d-082)).** The argument ran: a chunked rebuild would create a window in which `links_current` is partially populated, concurrent traversals on `read_conn` would silently omit edges, and that is a correctness failure worse than a latency stall. Every step is true *of the shape it imagined* — chunking the `DELETE` and `INSERT` in place. It is not true of the operation, and the difference is where the partial state lives.

`rebuild_current_chunked` builds a **shadow table** and swaps it. `links_current` is not touched until the swap, which is one transaction, so there is no window in which it is partially populated and no traversal can observe one. The chunks build something nobody is reading. What the old paragraph established is therefore still exactly right — a partially populated `links_current` is actively harmful — and it is an argument against one implementation rather than against the goal.

The cost of the atomic form is real and is what motivated the work: at 1M edges the table above budgets a ~5 s hold, during which every other writer waits. The chunked form trades that for a longer wall-clock repair made of [`CHUNK_BUDGET`](s5-modules.md#515-cooperative-chunking--the-golden-rule)-sized holds, which is the golden rule applied to the one operation that had been exempt from it.

**The states are driven by the caller, and the actor remembers nothing between them.** The three `ShadowStep`s each answer with a `ShadowOutcome`. `Begin` drops any orphan shadow and creates a fresh one, answering `Started { build_start, epoch }` with the actor's archive epoch; `Fill { after }` projects the next `SOURCES_PER_CHUNK` (256) sources and answers `Filled { last }` with the last `source_id` it reached, or `None` when the table is exhausted; `Swap { build_start, epoch }` catches up on writes since `build_start` and swaps, in one transaction, answering `Swapped { rows }`. `Database::rebuild_current_chunked` is the loop over those three for callers who do not want to hold the state themselves.

*The epoch travels out to the caller and back rather than being remembered by the actor, and that is the load-bearing detail.* The actor is stateless per command by construction ([§5.1.4](s5-modules.md#514-the-actor-loop)); a single remembered slot would be shared — and silently corrupted — by two rebuilds running at once. It lives in `ActorShared` beside the metrics rather than in `ActorMetrics`, because it is not a measurement: it is a correctness interlock, and a build that compiles without the `metrics` feature must still have it ([§5.1.10](s5-modules.md#5110-actorshared--what-the-actor-shares-with-the-handle)).

**The interlock is against `archive`, and it fails closed.** An archive between `Begin` and `Swap` physically deletes rows the shadow was built from, so the shadow describes a `links` that no longer exists. The epoch check catches that and the swap is abandoned with [`DbError::RebuildInterrupted`](s6-s10-flows-to-dependencies.md#7-errors) — distinct from `RebuildFailed`, and the distinction is the whole point: `RebuildFailed` means the repair ran and did not repair, which is a reason to distrust the ledger. `RebuildInterrupted` means the repair **did not run**. `links_current` is untouched, whatever was true of it before is still true, and the action is to retry.

`links_current_shadow` is a transient table and carries no triggers ([§4.2](s4-schema.md#42-links-assertion-history-and-current-belief-materialization)). It is dropped and recreated by `Begin` rather than reused, so an orphan left by a process that died mid-rebuild costs one `DROP` and never a wrong answer.

| Edge count | Expected lock hold | Notes |
|---|---|---|
| 100K | ~500 ms | Acceptable even during active use |
| 1M | ~5 s | Noticeable UI stall; run at idle |
| 10M | ~50 s | Significant stall; run at startup only |

- **At startup:** run `audit_current()` before the UI is active. If drift is detected, rebuild immediately — the user has not yet interacted, so the stall is invisible.
- **During active use:** if the audit detects drift, log a `tracing::error!` with the count and defer the rebuild to the next idle window. Do not run a multi-second rebuild while the user is asserting edges.
- **The high-priority tier is retained** because it governs *ordering*, not urgency: it means "fix this before the next analytics chunk," not "fix this before the user's next click." [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028)'s latency contract is what tells a caller what the difference costs them.

### 5.9 vector/ — embeddings, the model registry, and search

Restored in 0.5.4 from the 0.4.5 document, where this material was **[§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration)**. The 0.5.x renumbering assigned [§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration) to `temporal/as_of.rs` and dropped the vector section without moving its content anywhere — so the whole vector module had no section at all for four releases, and `vector/search.rs`'s doc comments went on citing "[§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration)" for vector search while [§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration) had become valid-time attribute hydration. The content is restored here as [§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search) rather than renumbered, because [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity)–[§5.8](s5-modules.md#58-integrity--audit-and-rebuild) are cited throughout the decision register and the code; the three stale citations in `search.rs` are repointed at [§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search).

`ModelName` is a validated newtype: a model name becomes a table name (`embeddings_<model>`) and an index name, and a table name is an identifier, which cannot be a bind parameter. Validation is therefore the only option, and [D-037](s13-decision-register.md#d-037) records why that is the opposite of the edge-type case in [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), where binding removes the question. `register_model(conn, &model, dim)` creates the per-model table and its DiskANN index **in one transaction**, and there is no supported way to have one without the other: dimension enforcement is a property of the index rather than of the declared column type ([D-037](s13-decision-register.md#d-037), measured), so a per-model table without its index is a table with no dimension enforcement at all. `declared_dimension` reads `F32_BLOB(n)` back out of `PRAGMA table_info`, so the schema holds the single copy of the dimension and the Rust-side check cannot disagree with the engine-side one.

Plain vector search resolves the target model, then issues the DiskANN-backed query through `vector_top_k` — which yields base-table rowids, so `vector_distance_cos` is evaluated on the k rows the index selected rather than once per concept. An `ORDER BY vector_distance_cos(…)` over the whole table is linear in the corpus no matter how small k is, and would not meet [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s budget of top-10 over 100K concepts in ≤ 20 ms. Results are typed `VectorSearchResult` rows. The search is a read and is served from `read_conn`.

Hybrid search runs FTS5 keyword retrieval and vector top-k and fuses the two ranked lists with Reciprocal Rank Fusion at k = 60. The keyword half reads a `concepts_fts` shadow table maintained by trigger. RRF is some twenty lines of arithmetic and needs no dependency; the constant is documented and tunable, because retrieval-quality tuning is empirical rather than theoretical. Both retrievals are reads from `read_conn`.

**Why fuse ranks rather than scores.** Cosine distance and BM25 are not comparable numbers, and any scheme that adds or weights them is inventing a conversion nobody measured. RRF adds reciprocal *ranks*, which are comparable by construction, and k damps the top of each list so that agreement between the arms outweighs a single arm's confidence: at k = 60 a document both arms rank tenth beats one that is first in one list and absent from the other. That is the behaviour the two arms exist for — dense vectors find a paraphrase and miss an exact identifier they never saw; BM25 does the reverse.

**Depth is not `top_k`.** Fusing two top-k lists is not the top k of the fusion: a document ranked twelfth by *both* arms can outscore one ranked first by a single arm, and it is invisible if neither list was read past k. `HybridSearch` therefore reads each arm to a `depth` defaulting to `max(5 × top_k, 50)` and truncates after fusing, which costs a larger `LIMIT` per arm rather than a second round trip.

**The index is external-content**, declaring `content='concepts'`: the tokens are indexed and the text is not duplicated, so the index cannot disagree with the concept about what the concept says — a standalone copy would be a second description of data the ledger already holds, the failure class [D-030](s13-decision-register.md#d-030) and [D-035](s13-decision-register.md#d-035) exist to prevent. It also makes [D-036](s13-decision-register.md#d-036)'s rebuildability the engine's own operation (`INSERT INTO concepts_fts(concepts_fts) VALUES('rebuild')`) rather than code of ours that could drift from the triggers; `Database::rebuild_fts()` is that statement, on the low-priority tier. The price is that external-content tables do not maintain themselves: an `UPDATE` must retract the old terms using the *old* column values before inserting the new ones, which is what `trg_concepts_fts_update` does, and omitting it leaves an index that still matches text no concept contains — silently, and detectable only by searching for something that should be gone. There is deliberately no delete trigger, because `trg_concepts_guard_delete` is unconditional ([D-022](s13-decision-register.md#d-022)) and concepts are never physically deleted; if that guard ever becomes conditional, this index is stale until it gets one.

**That guard turns out to be load-bearing twice over, and the second way was not recorded until 0.5.6 ([D-071](s13-decision-register.md#d-071)).** External content is keyed on `content_rowid='rowid'`, and `concepts` has a `TEXT PRIMARY KEY`, so its rowid is *implicit* — and `VACUUM` renumbers implicit rowids. That is the standard external-content hazard: after a vacuum every `content_rowid` points at a different concept, and keyword search (and `HybridSearch` with it) returns the wrong concepts with no error at all.

**It does not arise here.** Concepts are never deleted, and upserts go through `ON CONFLICT(id) DO UPDATE`, which preserves the rowid rather than replacing the row — so `concepts.rowid` is dense `1..n` at all times and `VACUUM`'s renumbering is the identity map. Measured, not argued: `vacuum_does_not_disturb_the_fts_index`. **`VACUUM` is therefore safe on this schema exactly as long as concepts are never deleted.** [Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred) designs concept archival as deferred-but-feasible and names GDPR erasure as a separate operation outside the archive path; either makes rowids sparse and makes this real, and whichever lands owes a rebuild.

**FTS5's own `'integrity-check'` cannot detect this class of staleness**, which is why there is no `verify_fts()` beside `rebuild_fts()`. On libSQL 0.9.30 it verifies the index's internal consistency and not its agreement with the content table: after `'delete-all'` the index matches nothing where it matched ten rows, and the check still reports success. A verifier built on it would call an empty index healthy, so none was written; `an_emptied_fts_index_still_passes_integrity_check` pins the limitation as a tripwire, failing if a later engine starts cross-checking content. The exposure is bounded by [Doctrine VI](s0-s3-foundations.md#doctrine-vi) — the index is derivative and `rebuild_fts()` reconstructs it from the ledger, so nothing can be lost, only made wrong until someone rebuilds. Restoring a backup that skipped the shadow tables remains the case that method was written for.

**Arbitrary text is escaped before it reaches FTS5.** MATCH syntax is a language — `AND`, `OR`, `NOT`, `NEAR`, prefix `*`, column filters — and passing a search box through to it fails two ways. A malformed expression raises `SQLITE_ERROR`, so a user who typed a query gets an exception; and a query containing `NOT` silently *means* something, quietly excluding documents, which is a wrong answer rather than an error. Each run of alphanumeric characters therefore becomes one quoted term with implicit AND between them. `HybridSearch::raw_match(true)` opts back in to the query language for a caller building the expression themselves.

**The write path (0.5.4, [D-048](s13-decision-register.md#d-048)).** `Database::register_model(model, dim)` creates the table and its index on the high-priority tier; `Database::upsert_embeddings(model, rows)` stores vectors on the low-priority tier, chunked at `chunk_rows::EMBEDDINGS`, atomic per chunk, with the declared dimension resolved once per chunk. Both go through the Write Actor, because the write connection is actor-owned and `read_conn` is `query_only` ([D-019](s13-decision-register.md#d-019)) — before 0.5.4 neither had a route to it, so an application could search vectors it had no way to store. Reads are unchanged and still go direct to `read_conn`, never traversing the actor.

**Implementation status (0.5.5): implemented ([D-051](s13-decision-register.md#d-051)).** The 0.5.4 text recorded that hybrid search did not exist — `reciprocal_rank_fusion` was a pure function with nothing producing the keyword half and no `concepts_fts` table anywhere in [§4](s4-schema.md#4-schema), while [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budgeted the path at ≤ 50 ms. The schema decision it declined to take is now taken: `concepts_fts` and its two triggers arrive on a `v4 → v5` rung, derivative and additive under [D-036](s13-decision-register.md#d-036), and the rung backfills — unlike [D-041](s13-decision-register.md#d-041)'s annotations, the index is a pure function of text the ledger already holds, so `'rebuild'` reconstructs exactly what the triggers would have written had they always existed. `HybridSearch` is the public surface, mirroring `TraversalBuilder` and `FilteredVectorSearch`.

One correction landed with it: `reciprocal_rank_fusion` sorted on the fused score alone, leaving ties in `HashMap` iteration order. Ties are the *common* case here — two documents at the same pair of ranks score identically by construction — so the same query could return the same set in a different order on the next run. The sort now breaks ties by id. This is the procedural-versus-structural determinism trap [D-047](s13-decision-register.md#d-047) names, arriving as a search result that will not sit still.

### 5.10 metrics.rs — what the actor holds the lock for

**Off by default, and the default is the argument (0.6.0, [D-079](s13-decision-register.md#d-079)).** Every latency claim in this crate before 0.6.0 was a `cargo bench` figure. A [`CHUNK_BUDGET`](s5-modules.md#515-cooperative-chunking--the-golden-rule) that cannot be checked *in situ* is an aspiration, not a bound: it describes what a benchmark measured on one machine, not what the actor is doing in an application under real contention.

`--features metrics` records, per [`CommandKind`](s5-modules.md#513-the-two-tier-command-channel), a histogram of how long the actor held the write connection. `MetricsSnapshot::budget_violations()` returns the kinds whose holds exceeded the budget, which is the question an operator actually has.

*The instrumentation is arranged as two impls of one type rather than `#[cfg]` inside the actor loop.* With the feature off, `HoldTimer::start()` reads no clock and `ActorMetrics`'s methods are empty on a zero-sized type, so the loop compiles to what it compiled to before. The alternative — conditional compilation inside the loop — makes the loop's shape depend on a feature flag, and the loop is the one piece of this crate where an accidental early return or a missed `select!` arm is a deadlock rather than a wrong answer. One shape, always.

Buckets are fixed at `BUCKET_BOUNDS_MICROS` rather than computed. A histogram whose bucket edges move between builds cannot be compared across them, and comparison across builds is the only reason to keep the numbers.

### 5.11 util/ — ids, clocks, timestamps, and engine ceilings

`ids.rs` generates and validates ULIDs. Validation is not cosmetic: a link's `transaction_log.entity_id` concatenates `source|target|type|valid_from`, so an id containing the separator makes the row unattributable on replay ([D-061](s13-decision-register.md#d-061)). A rejected id is [`DbError::InvalidId`](s6-s10-flows-to-dependencies.md#7-errors) — *refused*, not *missing*, which is why it is not `NotFound`: telling a caller the thing does not exist invites them to create it with the same id and be refused again.

`clock.rs` is the `Clock` trait, `SystemClock` (monotonic floor plus strict parser) and `FakeClock`. [§5.1.2](s5-modules.md#512-handle-shape-and-the-clock-contract) has the contract.

`timestamp.rs` owns the canonical form — `YYYY-MM-DDTHH:MM:SS.ffffffZ`, exactly 27 bytes ([D-029](s13-decision-register.md#d-029)). `normalize` accepts the canonical form and the legacy second-precision one and **refuses everything else rather than guessing**: an offset, a missing `Z`, millisecond precision. `parse` additionally validates the calendar, because `2026-02-30T…` has canonical *shape* and is not a date, and accepting it would let a timestamp exist that no round trip can reproduce. `OPEN_SENTINEL` is the open-interval end.

`util/limits.rs` is the one that needs its existence explained, because it looks like the tuning constants in `connection::chunk_rows` and is the opposite of them (0.6.0, T3.1). **`chunk_rows` holds choices; `limits` holds ceilings the engine imposes.** `HYDRATE_CHUNK` (400) is how many ids go into one `IN (…)` list, and it is not a latency budget — these are reads, and `CHUNK_BUDGET` bounds what the *writer* holds. It is a bind-variable ceiling: `SQLITE_MAX_VARIABLE_NUMBER` is 999 on a stock build, and hydrating a budget-sized subgraph in one statement would fail at the driver with an error that says nothing about node count. The margin is doubled rather than exact, because a hydrate query carries other bound parameters too and a query that gains one should not be what discovers the ceiling.

It was defined twice before T3.1 — in `graph::subgraph` and `temporal::as_of`, both 400, one carrying the reasoning and the other a `// See as_of::HYDRATE_CHUNK` comment. That works until someone tunes one of them, at which point two constants that must be equal silently are not, and the symptom is a driver error on one code path and not the other. The cross-reference comment was evidence the duplication was known and being managed by convention; a shared constant is what replaces a convention.

The relationship is asserted at **compile time**, not in a test: `const _: () = assert!(HYDRATE_CHUNK * 2 < SQLITE_MAX_VARIABLE_NUMBER)`. A `#[test]` was the first form, and clippy was right to object — both sides are constants, so the assertion has a constant value. A const block fails the build rather than a test run, which matters because the failure it guards is someone raising `HYDRATE_CHUNK`, plausibly in a release build, plausibly without running the suite.
