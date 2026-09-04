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

Migrations run through user_version, the engine's own schema-version slot, with each migration an idempotent step reviewed as ordinary DDL. **The ladder has two rung kinds** ([D-117](s13-decision-register.md#d-117), 0.8.0). The ordinary one runs inside `BEGIN IMMEDIATE` with the `user_version` stamp in the same commit. The second sets `suspends_foreign_keys`, for a rung that rebuilds a table with *inbound* foreign keys — `apply_step` toggles `PRAGMA foreign_keys` **around** the transaction, because inside one the pragma is silently ignored, and restores it on every path including the error one. Suspension is not permission: `PRAGMA foreign_key_check` runs inside the transaction before the stamp and any row it reports fails the rung. `v7 → v8` ([D-119](s13-decision-register.md#d-119)) and `v11 → v12` set it, for the two different reasons `Step::suspends_foreign_keys` records; no other rung does. The clock is injected here and threaded everywhere: production uses SystemClock (RFC 3339, microsecond precision, UTC); every test uses FakeClock, which is what makes the entire temporal test suite deterministic — including, as of 0.4.5, the actor, which receives the same Arc the harness holds.

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

**The floor is inherited, which makes a stamp from the future the one bad value in the file that spreads (0.13.5, W7.4, [D-178](s13-decision-register.md#d-178)).** The paragraph above describes the correction in one direction only. Read it the other way: a single row stamped in 2087 — a skewed host, a bad import, a `FakeClock` fixture that escaped its test — becomes this process's floor, every stamp the clock then issues lands at or after it, and those stamps are *written*. The next open reads the same floor back out of rows this crate itself produced. Nothing in the design recovers from that, because nothing in the design can tell the poisoned rows from legitimate ones once they are stored.

So `recorded_at_floor` bounds what it will absorb. Beyond `FutureStampPolicy`'s tolerance — a day by default, generous because what is being caught is out by *years* — the open returns [`DbError::FutureRecordedAt`](s6-s10-flows-to-dependencies.md#7-errors) rather than taking the floor. This is the only refusal in the crate that rejects a whole database rather than an operation, and it is proportionate for the reason above: it is the last point at which a stamp the crate wrote can still be told from one it did not.

**A corrupt stamp keeps the `warn!` it has always had** ([D-027](s13-decision-register.md#d-027)), and the asymmetry is the argument: a value that will not parse cannot become the floor, so it cannot propagate, and the monotonicity trigger contains it. A value that parses and is wrong is contained by nothing. `Tuning { future_stamps: FutureStampPolicy::Allow, .. }` opens a refused file so it can be *read*; it is not a repair, and writes made under it inherit the floor.

Timestamp parsing contract (0.5.2, [D-027](s13-decision-register.md#d-027)). Because recorded_at is stored as ISO-8601 text, SystemClock::new() must parse the stored string back into a SystemTime to use as its floor. This is the one place in the clock module where a panic is possible if handled carelessly, so the contract is explicit:
```rust
// util/clock.rs — shape only

impl SystemClock {
    pub async fn new(
        conn: &libsql::Connection,
        policy: FutureStampPolicy,          // 0.13.5, W7.4 — see below
    ) -> Result<Self> {
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
            // A stamp from the *future* is refused rather than absorbed
            // (0.13.5, W7.4, D-178): the floor is inherited, so absorbing it
            // manufactures more of it. Elided here — see the prose below.
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

> **These two sketches are 0.4.5 vintage and the crate has moved past them (0.5.4).** They are restored as written because the prose around them argues from their shape, but three things in them are no longer true: the payload fields are now typed values (`EdgeAssertion`, `ConceptUpsert`, `Annotation`) rather than loose columns, since a command carrying strings is an arbitrary-SQL channel ([D-034](s13-decision-register.md#d-034)); the wildcard `_ => LoopCtl::Continue` in `execute` below is the exact defect [D-034](s13-decision-register.md#d-034) removed, because it dropped the responder for nine of twelve commands; and the command set has since gained `RegisterModel`, `UpsertEmbeddingChunk` ([D-048](s13-decision-register.md#d-048)) and `WriteAnalyticsChunk` ([D-041](s13-decision-register.md#d-041)). [Appendix A](appendices.md#appendix-a--public-api-normative) is the current surface. A fourth, from 0.13.33: both enums are `pub(crate)` ([D-206](s13-decision-register.md#d-206)). The `pub` here was never a decision — Appendix A has never listed either enum — and leaving it would have committed 1.x to `tokio::sync::oneshot::Sender` in seventeen signatures.

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
) {
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
}
```

**It returns nothing, and returned a `Result<()>` it could not fail until 0.13.4 (W7.3, [D-177](s13-decision-register.md#d-177)).** The paragraph below is the reason: every command routes its `DbError` to that command's responder rather than to the loop, so there was never a third thing for an actor-level `Err` to carry, and none was ever constructed. The cost of leaving it was not the branch itself but what it concealed — `close()` read `Ok(res) => res?`, which looks like a failure path under review, while the branch that *does* fire, a **panicked** actor arriving as a `tokio::task::JoinError`, had no test. The mapping now lives in `writer_exit` and is tested against a real one.

Each execute runs exactly one transaction — BEGIN IMMEDIATE … COMMIT — and routes its DbError, if any, to the command's responder rather than to the loop; a failed assertion must not kill the writer. The loop is the only scheduler in the system, and its policy is one line: between any two transactions, re-check the UI queue before touching the background queue. The priority queue decides who goes next; it does not — cannot — interrupt a transaction already in flight, because SQLite's lock is not preemptible. That single fact is the reason [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) exists.

**The loop carries state between turns, and until 0.15.6 it carried none** (W14.3, [D-248](s13-decision-register.md#d-248), review A-3). Each command was handed `&conn` and nothing else, so every fact a turn established died with it: a single-edge assertion asked `branches` how many lineages exist, compiled the overlap guard, and compiled `INSERT_LINK`, having done all three on the previous call. `ActorState` — lineages, guard, insert statement — is owned by the loop and lent to each command, and it is invalidated by name rather than by epoch: `Fork` and `ArchiveBranch` forget the lineages, the three sessions that `ATTACH` a second database forget the statements. The whole safety argument is [D-014](s13-decision-register.md#d-014): the actor is the only writer, so a cache it is holding cannot go stale behind its own back. Measured on the single-edge write, best of 500: **0.184 → 0.099 ms** on the trunk and **0.401 → 0.106 ms** on a database that has been forked, where the guard's resolved form is the statement being compiled each time. The fork's 2.2× penalty on every write was almost entirely that compile.

**The preemption is strict, so low-priority starvation is unbounded — by design, and stated here because it had never been written down** (0.10.0, W4.5). "Re-check the UI queue before touching the background queue" has no ageing term, no fairness quota and no ceiling on how long a low-priority command may wait: a workload that keeps the high-priority queue non-empty holds every background job at the boundary indefinitely. That is the right trade for a desktop ledger — the alternative is admitting a UI stall to let an archive proceed, and a user who cannot type is a worse failure than an archive that runs later — but it is a real property and an unbounded one, not an approximation of fairness.

**It has a detector rather than a bound.** `MetricsSnapshot::low_depth_max` (`metrics.rs:455`, behind `--features metrics`, [D-079](s13-decision-register.md#d-079)) is the high-water mark of the low-priority queue depth. A depth that rises and does not come back down is starvation happening, and it is observable without inferring anything from latency. The choice to measure rather than to bound is the same one [D-055](s13-decision-register.md#d-055) makes about the budgets: a threshold this crate picked would be a threshold about someone else's workload.
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

**The other half of the diagnosis above — that the defect "slows every interactive `assert_edge` on a high-degree node" — went unmeasured until 0.10.0, and is now retired too** ([D-134](s13-decision-register.md#d-134)). The table here re-measured the *chunk*; the interactive path kept the caveat for four more releases on the strength of the same fixed index. Measured into tables of 0 / 2,000 / 8,000 edges — hub out-degree 0 / 666 / 2,666 on this same fixture — it does not move.

**Re-derived once more in 0.11.0, against all four [D-088](s13-decision-register.md#d-088) shapes rather than one, and this time one constant does not survive** ([D-143](s13-decision-register.md#d-143)). The figures above are empty-database figures — which is what [D-059](s13-decision-register.md#d-059) said needed fixing and what the matrix was built to fix. Populated to 8,000 edges, `concepts`, `annotations` and `embeddings` still meet the bound with 1.7–2.2× headroom and are unaffected by shape, because they never read `links`. The edge chunk takes **8.22 ms at 90 rows**, and all four shapes agree the largest size inside the bound is **20**.

*The constant stays at 90 anyway, and the reason is the finding.* 20 is not a fix — it is the same miss at a larger population, because per-row cost grows with `links_current` ([D-142](s13-decision-register.md#d-142)), so a row count fitted at 8,000 edges is wrong at 80,000. **This is the paragraph at the top of this section catching up with itself:** the rule is a bound on *duration*, the row count is "an answer to the rule", and an answer fitted at one population expires. The fix is for the chunk loop to stop on elapsed time with the constants as an upper bound — a write-actor design change, named for 0.12.0 in [Appendix C](appendices.md#named-for-0120-and-it-is-one-item) rather than taken as a side effect of measuring. [D-079](s13-decision-register.md#d-079)'s count of holds over `CHUNK_BUDGET` is the detector in the meantime.

**Delivered in 0.12.0. The row count is no longer chosen ahead of time.** The actor times its own transaction and reports the duration back with the row count; the caller-side loop sizes the next chunk from it. The four constants are now **ceilings** — the size the loop starts at and the largest it will ever send — and every derivation above still applies to them as such. The control law is deliberately asymmetric: over budget it shrinks proportionally, so a chunk 3× over converges in one or two steps; comfortably under, it grows by a quarter, so it cannot overshoot the ceiling and cannot oscillate across the bound. Back off fast from a bound you are exceeding, approach it slowly.

*Three things this does not do, and each is a consequence of the same fact.* The SQLite write lock is not preemptible — the founding fact of this whole section — so time-based chunking is **feedback, not preemption**. (1) The chunk in flight always commits in full; nothing is aborted mid-transaction, and a caller queued behind one still waits for all of it. (2) A batch small enough to be a single chunk gets no protection at all, because there is no next chunk to size; convergence costs one or two chunks and a one-chunk import is entirely the first, worst one. (3) The bound is met *after* the loop has been told it was missed, never before.

*And a floor, which is a knowing violation of the bound.* Feedback alone converges to whatever meets 3 ms, and on a populated `links` table there is no such size: per-row cost grows with the table ([D-142](s13-decision-register.md#d-142)), so the fixed ~0.8 ms of Correction 2 keeps taking a larger share until the loop reaches chunks of one or two rows and the import stops finishing. `CHUNK_FLOOR = 35` stops it. Measured against the loop that uses it rather than extrapolated from the sweep — a 900-edge `bulk_import` into each [D-088](s13-decision-register.md#d-088) shape at 8,000 edges, read from the actor's own per-transaction timings ([D-146](s13-decision-register.md#d-146)) — a 35-row chunk costs **3.11–3.43 ms** across the four shapes over two sessions. The floor misses the bound by 0.1–0.4 ms.

That miss is **steady state, not a transient on the way down**, and it is defended by the argument the bound answers to rather than by the bound itself. The first paragraph of this section derives 3 ms from a 60 Hz frame: an interactive assertion waits for the chunk in flight and then runs its own ≤ 5 ms write. At the floor that is ~3.2 + 5 = **~8.2 ms against a 16.7 ms frame** — comfortably inside it. The constant carries its measurement in a comment so the trade can be re-run rather than re-argued.

**Measured, the loop on this path is two-valued, and that is the finding worth carrying** ([D-146](s13-decision-register.md#d-146)). The size trace is `[90, 35, 35, …]` on all four shapes: the proportional shrink from a 90-row chunk proposes ~31 rows, which clamps, so the loop reaches its operating point in **one step** and never selects a size between the floor and the ceiling. The floor is therefore not a backstop under a controller finding an optimum — on the edge path at this population it *is* the operating point, and `CHUNK_FLOOR` is carrying more weight than the name suggests. The controller earns its keep where an in-budget size exists between the two, which is every other path and this one on a smaller table.

**And it does not improve the worst stall, which is the honest headline.** The longest hold in an adaptive import is **7.7–10.2 ms** against **7.6–15.2 ms** for fixed ceiling-sized chunks — 0.67–1.01×, an improvement on two shapes and nothing on the other two — because the worst chunk is the *first* one, at the ceiling, before any feedback exists. Point (3) above is not a footnote; on this path it is the dominant effect. What does improve is the *typical* stall: mean hold **3.4–3.7 ms against 7.1–8.3 ms**, a 2.1–2.4× shorter wait for an interactive write arriving at a random moment. The cost is **9–20% throughput**, which is [D-058](s13-decision-register.md#d-058)'s ~11% arriving where it was predicted.

#### The bound's scope: three operations are exempt

Recorded here because this is where a reader looks for it. Until 0.5.6 the exemptions lived in three separate rustdoc notes, so the rule read as though it had none:

| Path | Bound | Why it cannot be chunked |
|---|---|---|
| `write_bulk_atomic` | none — the caller sizes it | [D-014](s13-decision-register.md#d-014): the batch is *one act* under one stamp. Splitting it is the thing the method exists not to do |
| `archive()` | measured **26.8 ms** for 2,000 archivable edges | [D-012](s13-decision-register.md#d-012): copy-then-delete must be atomic, or a crash between the phases duplicates or loses rows |
| `rebuild_current()` | ~50 s per 10M edges | [D-023](s13-decision-register.md#d-023): the window between the `DELETE` and the `INSERT` is the whole of current belief |

**What the first row costs changed in 0.13.6 ([D-179](s13-decision-register.md#d-179)), though the exemption did not.** `write_edges_atomic` opened with a within-batch overlap guard ([D-060](s13-decision-register.md#d-060)) that compared every pair, so an uncapped batch carried an uncapped quadratic term — 20,000 corrections to one relationship's history held the actor for **18.1 s**. It sorts and sweeps now, and the same batch holds for **2.2 s**, which is what 20,000 inserts cost regardless of shape. The exemption is still [D-014](s13-decision-register.md#d-014)'s and still right; what it exempted the caller from is now a smaller number.

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

**As of 0.12.0 the chunk boundaries are machine- and load-dependent, and the guarantee above is therefore about the *shape* of what a reader sees, not about where the seams fall.** [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)'s loop sizes each chunk from the last one's measured hold, so the same 50,000-row write-back lands under a different number of stamps on a fast machine than on a busy one, and under a different number on two runs of the same machine. Everything this section promises survives that intact — each chunk is still exactly one transaction under exactly one `recorded_at`, `reconstruct(ts)` mid-write still observes a prefix and never a partial chunk, `seq_id` is still strictly monotonic, and [Doctrine III](s0-s3-foundations.md#doctrine-iii) and [Doctrine IV](s0-s3-foundations.md#doctrine-iv) are untouched. What is no longer promised is that two identical batches produce identical stamps.

That is a real loss and it is stated rather than absorbed: a caller who was implicitly relying on reproducible stamping — a test comparing two seedings, most obviously — now needs `write_bulk_atomic`, which is the escape hatch above and is exactly the case it exists for. The test that pins this section was rewritten for the same reason ([§8](s6-s10-flows-to-dependencies.md#8-testing-strategy)): it asserts that a contiguous prefix commits and that each stamp covers one contiguous run of it, and no longer asserts where the boundary falls, because that would be pinning the speed of the machine that ran it.

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

#### 5.1.11 Sharing the handle: `Arc<Database>`, and not `Clone`

**Decided in 0.13.30 ([D-203](s13-decision-register.md#d-203)), and decided in the negative.** Every method on `Database` but one takes `&self`, so `Arc<Database>` is a complete handle: reads run concurrently off `read_conn`, writes queue behind the actor's channel exactly as they do through a `&Database`, and nothing becomes serialised that the actor was not serialising already. The exception is `close()`, which takes `self` — the last owner closes with `Arc::into_inner(db).expect("last handle").close().await`, and the `None` arm is a caller being told a handle is still live rather than a snapshot going missing quietly.

`Clone` is absent because it would duplicate the *right to shut down*. `writer` is a `JoinHandle` and not cloneable at all, so a copy's `close()` could never check the actor's exit status; `closed` is per-handle, so two copies disagree about whether the ledger was closed. But the field that decides it is `cadence_stop`, which **is** `Clone`: its contract is that dropping it stops the snapshot task ([§5.1.7](s5-modules.md#517-shutdown-and-snapshot-coordination)), and a watch channel closes when the last sender goes — so one surviving copy keeps that task writing against a database that is going away, with nothing returning an error. And `close()`'s ordering — cadence stopped, actor joined, then the snapshot — is only enforceable while one handle can perform it.

The Python binding reached the same shape from the other side: `PyDatabase` holds a `RwLock<Option<Database>>` rather than a copy per caller, because Python cannot express a by-value `close()` and the uniqueness had to be rebuilt by hand. `a_database_is_shared_by_arc_and_is_deliberately_not_clone` holds both halves — a probe for the absent trait, and the `Arc` pattern executed end to end.

### 5.2 graph/builder.rs — traversal, valid time, and attribute fidelity

**`content` leaves the default load in 0.8.0 ([D-116](s13-decision-register.md#d-116)).** `load_subgraph_with` always hydrated `concepts.content`, and none of the six algorithms reads it — they touch topology and weight only — so the byte budget was being spent on document text nothing would look at. It is now `Option<String>`, off by default, requested with `TraversalBuilder::content(true)`. **`None` means *not loaded*, never *empty*:** a sentinel that is a valid value of the type cannot be told apart from the real thing, and the two differ exactly when a caller is deciding whether to go back to the database — the same refusal [D-096](s13-decision-register.md#d-096) made for the open interval. The query selects `NULL` in place of the column rather than fetching and discarding, so the engine never reads the text off disk. Complementary to interning rather than an alternative: edges are 80% of a loaded graph with empty content and 5% at 20 KB per concept.

**`EdgeRef` is interned as of 0.8.0 ([D-115](s13-decision-register.md#d-115)).** `{u32, u32, f64, u32, u32}`, `size_of` 24 against 104 plus roughly 250 bytes of strings, with every field but the weight an index into a pool the `Subgraph` holds. Only the *edges* are interned: `nodes` stays a `BTreeMap<String, NodeData>`, because node payload is not what scales and keeping that map is what keeps iteration order structural — so [D-063](s13-decision-register.md#d-063)'s warning that determinism would become procedural does not land. Measured **5.8×–6.8×** bytes per edge, not the 7.1×–9.5× projected from `2 × size_of`: the pool costs about 20%, which is D-063's own objection holding up under measurement. Reading an edge therefore needs the graph — `e.node(&g)` — and that is why the fields had to be private first.

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
    LIMIT ?7                                          -- only when limit is set
)
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
```

**The `LIMIT` is inside the recursion, and that is the whole content of the change (0.15.10, W13.5, [D-252](s13-decision-register.md#d-252), review C-8).** `FilteredVectorSearch::probe_cap` had bounded the *list* — the traversal ran to the end of the graph and the tail was dropped — so a name that read as a ceiling on cost was a ceiling on the size of the answer. The obvious repair, a `LIMIT` on the projection above, repeats the defect one line further down: that projection sorts, and a sort materialises the whole walk before a limit can apply. Measured on a hub graph whose walk visits 20,050 edges: **no limit 20,050; `LIMIT 20` on the outer `SELECT` 20,050; `LIMIT 20` inside the CTE 7,250; `LIMIT 5` inside 1,250.** SQLite halts the recursion once the recursive table reaches the limit, so the bound is the fan-out of the first `n` rows taken out of the queue — proportional, not absolute, and worth nothing until it is smaller than the expensive frontier.

`?7` is written here as the seventh slot; it is actually `edge_type_base + edge_types.len()`, **after** the variadic run, because it is the only parameter in this layout whose position depends on how many precede it rather than on which are present. The clause is empty when no limit is set, so every traversal written before 0.15.10 emits the byte-identical statement it always did.

**A limited walk answers with at most `n` ids, and the projection under a limit is a different one.** `n` counts *walk rows*: the walk holds `(node_id, depth)` and dedupes on the pair, so a node reachable at two depths spends two of them, and `c.retired = 0` then drops rows the walk has already paid for. So fewer than `n` does not mean the graph was smaller, and no id count can say whether the ceiling bit. `execute_ids_explained` returns a `WalkOutcome` taken from the walk's own row count in the same statement — and that count is the projection's **anchor**, left-joined to the ids rather than selected beside them, because a walk whose every concept is retired returns no rows to read a second column from and is exactly the case where the question matters most.

The subset is the near end: SQLite's recursive queue is FIFO, so the walk is breadth-first and a limit drops the farthest nodes first. Among nodes at the same depth the cut is arbitrary, which is the one thing `max_depth` — the crate's other stated bound — does not do.

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

**`as_of(ts)` read `ts` on two different clocks, and 0.13.2 splits it (W7.1, [D-174](s13-decision-register.md#d-174)). The statement of the problem is kept below because the fix is only reviewable against it.** [Doctrine VIII](s0-s3-foundations.md#doctrine-viii) says `as_of(ts)` means valid time under current belief and returns exactly that, and that a query mixing axes says so in its signature. This one mixes and does not say. Exactly what the single `ts` is compared against:

| half of the answer | column | axis |
|---|---|---|
| topology | `links.valid_from` / `links.valid_to` | **valid time**, current belief — Doctrine VIII's contract, met |
| attributes, `AtTime` | `transaction_log.recorded_at` | **transaction time** — belief as of `ts`, which is `reconstruct`'s axis |
| attributes, `Current` | none; `concepts WHERE retired = 0` | valid time **now**, belief now |

So `as_of(t).attribute_mode(AtTime)` — the pairing the rustdoc calls "usually what was meant" — answers *"the edges valid at `t`, labelled with what we believed at `t`"*: two questions under one timestamp. Concretely, a title corrected today to fix a 2020 typo does not appear in `as_of("2020-06-01")` with `AtTime`, because the correction was *recorded* after `ts` — the right answer to "what did we believe in 2020" and the wrong one to "what was true in 2020", which is what the method's name promises. A second mismatch in the same direction: `AtTime` filters on `recorded_at` and on the payload's `retired` flag and never consults the concept's **own valid interval**, so a concept whose validity ended before `ts` still hydrates, and the two halves of the answer do not agree about what "existed at `ts`" means.

Note that this is the *third* present-tense predicate on a valid-time traversal recorded in this section, alongside `c.retired = 0` above. They are not the same defect — that one is the application axis by design, this one was the transaction-time axis by accident — but they share a shape. Before 0.13.2 a caller who wanted an unmixed historical read had `reconstruct(ts)` for belief-as-of and nothing at all for *what was true then as best we now know*, because no attribute mode read concept attributes on the valid-time axis. `as_of_valid` is that missing read.

**Stated in 0.12.17, fixed in 0.13.2, and the order was the point.** Changing it changes answers callers already depend on, so it is a break and belonged to a release allowed to make one. Writing the semantics down a release early is what made the change reviewable against a stated position rather than argued in the commit that made it — and the table above is what the change is checked against.

#### The fix: two parameters, and a topology that can be folded (0.13.2, W7.1, [D-174](s13-decision-register.md#d-174))

`as_of` is gone. `as_of_valid(v)` is *what was true*; `as_of_recorded(r)` is *what we believed*; neither is a default for the other, and they compose:

| set | topology | attributes under `AtTime` |
|---|---|---|
| neither | `links_current` at `now_ts` | live `concepts` |
| `as_of_valid(v)` | `links_current` bounded at `v` | `concepts` bounded by their own valid interval at `v` |
| `as_of_recorded(r)` | `transaction_log` folded to `r`, bounded at `now_ts` | the payload believed at `r` |
| both | folded to `r`, bounded at `v` | believed at `r`, bounded by the validity *that payload* recorded |

The last row is the cell Jensen and Snodgrass's BCDM defines a bitemporal database as answering, and no surface in this crate could express it before. The third row is the capability that had to be *built* rather than renamed: `links_current` is a projection of current belief, so the row that stood before a correction is not in it. Folding `transaction_log` recovers it, and links are strictly append-only — every assertion and every correction is an `INSERT`, logged `'I'` under `entity_id = source|target|type|valid_from` — so the last log row per entity at or before `r` *is* what `links_current` held at `r`.

The fold partitions on `(entity_id, branch_id)`. `table_name` is not in it because `table_name = 'links'` is in the `WHERE` — the discriminator applied by the filter instead of by the partition, so the concept/link collision defect W is about cannot arise; the four folds in `replay.rs` carry it in the partition instead, and that difference is deliberate.

**`branch_id` was missing until 0.14.4, and that was a defect rather than a difference of style ([D-220](s13-decision-register.md#d-220)).** A link's `entity_id` is `source|target|type|valid_from` — the edge key, which is shared across lineages *by design*, because that is how a branch corrects an edge it inherited. So the partition put an ancestor's assertion and a descendant's correction in one group and kept whichever carried the higher `seq_id`: one belief gone before any resolution ran, and which one survived decided by write order. [D-216](s13-decision-register.md#d-216) fixed exactly this shape in `replay.rs` one release earlier and this fold was not in that sweep, because its own rustdoc argued the partition was sound — correctly, about the concept/link collision, and about nothing else. **A correct justification for the wrong claim reads exactly like a correct claim**, which is why the note now names what it does not cover.

**The second mismatch above closes with the same change.** `AtTime` never consulted the concept's own valid interval; it now does, against the interval the *payload* recorded rather than the live row's, because reading the live interval would answer today's belief about validity wearing the past's title — the conflation this whole item exists to end.

**Cost, and what is not claimed.** `links_current` is a projection maintained for exactly this read and covered by `idx_lc_traversal_cover`. The fold is a window function over `transaction_log` with a `json_extract` per column, materialised once and joined per hop. It is not the fast path and does not pretend to be; W10.6 measures it.

**W10.6 measured it, and the answer is that there is nothing to index (0.13.23, W10.6, [D-196](s13-decision-register.md#d-196)).** The transaction-time bound already seeks — `SEARCH transaction_log USING INDEX idx_txlog_time (recorded_at<?)`. Adding the valid-time instant changes the plan **not at all**, and that is the structural finding rather than a happy result: `recorded_at` is a column of `transaction_log`, while the valid-time bound is applied to the walk's join against `links_at_tx`, whose `valid_from` and `valid_to` come out of `json_extract` — one derivation past anything an index can be consulted for. The two axes never meet on a table, so F-33's question has no object rather than a difficult answer. Built and measured rather than argued: a composite over `(recorded_at, json_extract(payload, '$.valid_from'))` **is** picked, in preference to `idx_txlog_time`, and is used on `recorded_at<?` alone — a wider index with a dead second column and an index write per log row, forever. The plans' actual cost is elsewhere and no index reaches it either: `ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC)` sorts the whole bounded slice, and forcing the fold onto `idx_txlog_entity` trades the seek for a full scan and still leaves a partial sort. `tests/bitemporal_plan_tests.rs` pins all three so the closure is a gate rather than a paragraph. And it reads the *hot* log only — an instant below what `archive` has left raises `RecordedInstantUnreachable` ([§7](s6-s10-flows-to-dependencies.md#7-errors)) naming `reconstruct`, which takes the archive path, rather than folding what remains.

**`load_subgraph_with` ignored the instant entirely, and that was found here (F-35, 0.13.2).** It bound `now_ts` at the placeholder the builder bound the traversal's own instant at, so a historical builder passed to it silently returned the present — the same shape as defect Z and F-31: a surface that accepts a qualifier and does not apply it. Both call sites now take their parameters from one producer, `TraversalBuilder::bind_params`, and the placeholder offset from `edge_type_base`, so the two cannot bind different things at `?3` again. The previous arrangement was a hard-coded `5` in one file and a comment in the other saying both call sites push in the same order, which is the shape [D-030](s13-decision-register.md#d-030) and [D-035](s13-decision-register.md#d-035) are about.

**Edge types are bind parameters, not literals (0.5.4, [D-039](s13-decision-register.md#d-039)).** An earlier version spliced them into the CTE with `format!("'{t}'")`, which made any caller-supplied string a SQL fragment on the read path. The only validation in the crate, `validate_edge_type`, runs from `EdgeAssertion::normalized` on the *write* path, so a traversal never passed through it — and that function's own doc comment cited the traversal CTE as its justification. Binding removes the question rather than answering it: unlike a table name, an edge type is a value, and values can be parameters. Contrast [D-037](s13-decision-register.md#d-037), where a model name is an identifier, cannot be bound, and validation was therefore the only option.

**The recursive step is index-only (0.5.4, [D-042](s13-decision-register.md#d-042)).** `idx_lc_traversal_cover` carries every column the walk reads, so the hop join never fetches a base-table row: `EXPLAIN QUERY PLAN` reports `SEARCH l USING COVERING INDEX idx_lc_traversal_cover (source_id=? AND valid_from<?)`, and `links_current` itself is not touched until the terminal join to `concepts`. That plan shape is asserted by test rather than assumed, including the seek constraint and not merely the word `COVERING` — the column order is what makes the difference, and an index in the wrong order still exists, still reports as covering, and still walks more of itself than it needs to. [D-042](s13-decision-register.md#d-042) records the ordering argument and the measurement that corrected it.

**This index did not change at v14, and that is the decision** ([D-231](s13-decision-register.md#d-231)). §15.4 has asked since 0.14.3 for `branch_id` to be added to it, on [D-219](s13-decision-register.md#d-219)'s measurement of three placements. Re-measured against the reader that shipped, the preferred placement buys nothing — the branched read does not consult this index at all — and every placement that *does* help the branched read produces `SEARCH l USING INDEX idx_lc_open_interval (source_id=?)` for the walk above: one bound column, not covering, the guarantee this paragraph describes silently gone. So the branched read got [its own index](#lineage-resolution) instead and this one was left exactly as it is. The third instance of D-042's lesson in the same table, and the first where the cost of getting the order wrong is a lost guarantee rather than a slower plan.

~~**Cycle-detection performance note (0.5.1).** `INSTR(w.path, CAST(l.target_id AS BLOB))` is O(path length) per hop… If a use case ever requires depth ≥ 20, benchmark this against a visited-set CTE (`json_each` over a JSON array of visited ids) and document the crossover.~~ **Obsolete as of 0.6.0 ([D-076](s13-decision-register.md#d-076)): there is no path column and no cycle check.** The note is struck rather than deleted because it is a good example of the trap it fell into — it costed the cycle check carefully, per hop, and concluded the design was "correct and fast for the target workload". It was correct. The cost it was measuring was not the one that mattered, and depth was not the variable: at depth 6 on a 328-edge *graph* the query took 403 ms, while the `INSTR` it warns about at depth 50 would still have been ~1,300 bytes of memchr. Depth bounded the path length; branching factor multiplied the number of paths, and nothing here was watching that.


<a id="lineage-resolution"></a>

#### graph/lineage.rs — which lineage a read returns, and as of when (0.14.4, [D-220](s13-decision-register.md#d-220); 0.14.6, [D-223](s13-decision-register.md#d-223))

v12 keyed `links_current` `(source_id, target_id, edge_type, valid_from, branch_id)`, so a branch correcting or retiring an edge it inherited writes its **own** row beside the ancestor's rather than over it — the only form of either that Doctrine III permits across lineages, since closing the ancestor's row is the parent corruption branching exists to prevent. This module is which of those rows a given lineage sees.

**Nearest-ancestor, not union.** `lineage(branch_id, dist)` walks `parent_id` from the reader to the root carrying its distance; `visible` reduces the edge relation with `ROW_NUMBER() OVER (PARTITION BY the edge key ORDER BY dist)` and keeps `rn = 1`. The partition is the edge *key* and not the edge: two lineages asserting the same `(source, target, type)` at different `valid_from` are two assertions in valid time and stay two rows; at the same `valid_from` they are one edge believed twice and resolve to one. That is also what makes shadow-retirement work — a branch writes its own row at the ancestor's key with a closed interval, the resolution prefers it, and the edge is gone from that lineage's view with the ancestor's row untouched. `branch_id IN (ancestry)`, which is what [§15.3](../Macrame%20Road%20to%201.0.md) describes, is not a resolution: probe §4b measures a shadowed retirement having **no effect at all** under it, 1,111 nodes against 1,000.

**Two shapes, and one probe of `branches` to pick between them** (a third since 0.15.2, below). `LineageShape::Trunk` emits the pre-0.14.4 SQL; `Resolved` emits the ancestry join. The condition is *one row in `branches`*, and it is exact rather than a heuristic: `branch_id` is `NOT NULL DEFAULT 'main' REFERENCES branches(branch_id)` with a real key on every ledger table ([D-214](s13-decision-register.md#d-214)), so a one-row register is a database in which every ledger row reads `'main'` — not by convention, but because nothing else could have been stored. Then the ancestry of `main` is `{main}`, every partition holds one member, and the two forms return the same rows by construction. Necessary rather than merely nice: probe §6 measures the resolved form at **3.02×** the plain one on a single-lineage database, which is every database this crate has written and stays the common case after `fork()`. This is the same pattern `temporal::replay::cold_lineage` uses at the archive boundary ([D-216](s13-decision-register.md#d-216)).

**The third shape is the root's, on a ledger that has forked (0.15.2, [D-244](s13-decision-register.md#d-244)).** `TrunkOnForked` is chosen when `branches` holds more than one row and the requested lineage has no parent — the same probe, three counts — and the lowering emits the trunk prelude plus one predicate: `AND +l.branch_id = ?{slot}` on the current-belief read, the same bound inside the fold's window on the transaction-time read. Exact for D-220's reason one level down: a root has no ancestors, so no cutoff and an empty churned set, and its resolved read reduces to its own rows. It is D-223's named escalation taken where the probe's answer is structural; the general probe of `links_current` for a post-cutoff row is still recorded and not built. The `+` is planner steering — the bare equality is served from `idx_lc_lineage_cut` and scans the trunk once per hop, which is [D-231](s13-decision-register.md#d-231)'s prediction arriving through the index it *did* build — and the walk's plan is pinned to seek `idx_lc_traversal_cover` and materialise nothing. Measured: the forked trunk's current read at **1.06×** the unforked trunk, against the 2.3× it paid as `Resolved`. The write path took the shape as `Resolved` until 0.15.8 lowered the guard ([D-250](s13-decision-register.md#d-250)), which is where the distinction stopped being free: exact either way, and **6.6%** apart on the single-edge write. `diff` lowers both its sides `Resolved` because two lineages are what it compares.

**And the fold was a co-routine.** `links_at_tx` — the transaction-time read's fold of the log, on every shape since 0.13.2 — is referenced once, by the walk's recursive step, and SQLite's default for a single-reference CTE re-evaluates it per outer row; inside a recursive step that is the whole fold per walk row. 10.6 s against 59 ms on 11,110 edges at depth 4. It is `AS MATERIALIZED` now and pinned on all three shapes. Why no probe from D-219 to D-231 saw it: the resolved shape's `visible` window forces materialisation by itself, so the branched read was already the fast form, and the trunk's folded read was the baseline nobody timed.

**Two refusals rather than two plausible answers.** A traversal naming a lineage that is not registered raises `UnknownBranch` naming it (`NotFound` until 0.14.7, whose `Display` says *node* — the wrong noun, and no better variant existed until `fork()` needed one, [D-224](s13-decision-register.md#d-224)), rather than answering for the trunk — the [D-069](s13-decision-register.md#d-069) shape, and the answer a caller is *least* able to detect, because on a database that has never forked the trunk's view is what they expected anyway. And a traversal naming no lineage resolves `main` rather than scanning every lineage: "no branch" has always meant the trunk, and the extra edges a union would return look entirely ordinary.

**The fork point is a cutoff, and the projection cannot serve it (0.14.6, [D-223](s13-decision-register.md#d-223)).** `lineage` carries a third column: the reader has none, and stepping to a parent takes the *stepping* branch's `forked_at`, clamped by a running minimum so inheritance narrows at every hop. What that bound cannot be applied to is `links_current`. The projection holds one belief per key per lineage and `trg_links_current_sync` carries `recorded_at` forward on conflict, so it answers *current as of now* and **structurally cannot answer *current as of t***: once an ancestor churns an edge, the version the branch inherited is not in the table, and a `recorded_at <= cutoff` predicate over it *removes* the edge rather than restoring it — with the subtree below it. Probe §3 measures four churn kinds and neither the pre-0.14.6 read nor the naive filter is right on all four; both lose a retired edge's whole subtree, which is the failure no result announces.

So the current-belief read is a **hybrid**, `links_cut`: the projection arm for rows each ancestor may still show directly, and a log fold — bounded per lineage, inside its own window rather than after it — for the `(edge key, lineage)` pairs whose projected row is younger than the cutoff. The two arms split `links_current ⋈ lineage` on one comparison, so they are disjoint by construction rather than by an argument about the archive, which is why `churned` is derived from the projection and not from `transaction_log` even though the log has the better index for it. A key an ancestor first asserted *after* the cutoff contributes nothing from either arm, and that is the answer rather than a gap. The transaction-time path needs no hybrid: `links_at_tx` already folds the log, so the cutoff is one more bound on rows it was going to read.

**Nearest-ancestor and latest-`recorded_at` coincide under cutoffs**, because each ancestor's visible window ends where its descendant's begins. That is what lets the fold bound per lineage with no cross-ancestor tiebreak. It is a consequence of the write path — a branch's own writes follow its fork — and not of the schema, which has no `CHECK` for it.

**Cost, against a prediction written first.** Expected ~1× on untouched keys and ~3× on churned ones; measured (probe §4, 11,110 trunk edges, one long-lived branch, depth 4) **1.45×** the 0.14.4 read at *zero* churn, 1.81× at 9%, 2.65× at 90%. The churn-linear half held and the fixed cost did not. Probe §6 puts it in the fold arm's machinery — `lineage` goes `CO-ROUTINE` → `MATERIALIZE`, the arm builds an automatic index and a temp b-tree whether or not it yields a row, and `links_cut` is a compound query the walk cannot index into — rather than in rows scanned, which is why probe §5 prices an index on `(branch_id, recorded_at)` at 3–13% and 0.14.6 declines it. The escalation on record is a *third shape*: probe whether anything on the ancestry is churned and emit the naive filter when nothing is, exact for the same reason the two existing shapes are. Recorded, not built.

**Where it degrades, and it is not where the first argument for it said.** `LINKS_ARCHIVABLE` and `LOG_ARCHIVABLE` both have superseded arms, so a pre-fork assertion that a post-fork correction supersedes *is* archivable — "an open row is never archived" is not the safety here. What holds is narrower: the projection arm never degrades, because `archive()` re-derives `links_current` from surviving `links` rather than deleting from it; the fold arm degrades exactly where main's own historical reads already do, on churned keys whose pre-fork log entry has gone cold. That is [§3.2](s0-s3-foundations.md)'s carried `AtTime` degradation reached from the branch side, and a cold arm belongs with that fix rather than with this one. Deliberately not guarded by `check_recorded_reach`: its bit is `hot_log_is_intact`, which any archive at all flips.

**The index the two arms seek on (0.14.14, [D-231](s13-decision-register.md#d-231), schema v14).** `churned` and `links_cut`'s projection arm are the only statements in the crate that read `links_current` **by lineage**, and five call sites emit them — the traversal, `query_as_of_edges_on`, `load_subgraph_with` and `diff`'s two tagged copies. Both bind `(branch_id, recorded_at)`, and until v14 nothing led on `branch_id`, so SQLite built the index itself on every execution: `AUTOMATIC PARTIAL COVERING INDEX (branch_id=?)`, twice per branched read. `idx_lc_lineage_cut` is the eight columns those two arms name, in that order — **a second index rather than a column on the traversal cover**, because the two shapes stopped sharing an access path the moment the resolved walk started joining a CTE instead of the table. Branched reads 1.20× on a fresh fork and 2.28× once the trunk churns; the trunk plan unchanged; every assertion +12.6%. A third `AUTOMATIC` index remains in the plan and no index can remove it: it is over the `links_cut` co-routine, which is not a table.

The three execution paths — `execute_ids`, `execute` and `Database::load_subgraph_with` — each ask `lineage_shape` and pass the answer down. `TraversalBuilder::build_sql` is a pure function and cannot; it emits the shape its own configuration implies and says so in its rustdoc, because it exists to explain the query rather than to run it. `temporal::as_of::query_as_of_edges` had the same gap and is fixed additively — it delegates to `query_as_of_edges_on(.., None)`, so its signature is unchanged and the two cannot drift.

#### graph/plan.rs — the lineage read, lowered once (0.15.1, [D-243](s13-decision-register.md#d-243))

The prelude above — `lineage`, then `links_at_tx` or `churned` + `links_cut`, then `visible` — is emitted by **one function**. `Resolution { shape, branch_slot, recorded_slot, tag }` is what a reader has decided before any SQL exists: where its branch binds, where its recorded instant binds if it has one, and the suffix that lets two lineages share a `WITH` list. `lower()` returns the CTEs in dependency order and the name of the relation the reader's own query joins — `visible{tag}` under `Resolved`, `links_current` or the fold under `Trunk`. The traversal, `query_as_of_edges_on` and `diff_sql` each construct a `Resolution` and splice the result; none of them names a lineage CTE any more, and `links_at_tx_cte` lives here rather than in the builder because the fold is prelude and not walk.

**Why it exists is [D-227](s13-decision-register.md#d-227).** Three readers assembling the same prelude from the same generators agreed at 0.15.0 because a repair made them agree, not because anything kept them so; the as-of reader had spelled its own form and missed the cutoff for four releases. A lineage shape that lands here lands in every reader on the same day, which is what the third shape ([D-223](s13-decision-register.md#d-223)'s escalation, W13.2) and the walk's `limit` (W13.5) require. The lowering is crate-private; the public `ReadPlan` road map §16 asks for comes once the SQL has been stable through the shapes that follow, so the public-API gate and the plan pins never fail in one release.

**Byte-identical by construction and by check.** `Lowered.ctes` is a `Vec` because the three readers glue the list three ways; twenty-seven captured texts matched after the move, and every plan pin and golden string passed unchanged.

**And the fourth reader is the writer (0.15.8, [D-250](s13-decision-register.md#d-250)).** `Resolution` carries `key: Option<KeySlots>` — three placeholder slots — and `lower()` places it by shape: on the trunk shapes the resolved relation *is* `links_current`, so the key joins the reader's own `WHERE`; under `Resolved` a CTE chain is in the way and it goes into the **base scans**, since narrowing the tail would narrow a relation already built over the whole ledger. A keyed resolution also carries `properties`, which `retire_from_resolved` restates on the shadow row and no traversal selects. `overlap_candidates_resolved` and `retire_from_resolved` are the lowering plus one line each now, `key_visibility_cte` is deleted, and the guard's plan is pinned over the generated bytes on all three shapes rather than over a copy of them in `migration_tests`.

#### branch.rs — the lineage's name and its one write (0.14.7, [D-224](s13-decision-register.md#d-224))

The write half, and the smallest module this wave has added: a validated name, a
row shape, and **one `INSERT`**. `Database::fork(name, from)` reads no ledger
table, copies nothing, and returns the `Branch` it wrote; `Database::branches()`
lists them trunk-first then in creation order. A branch inherits its parent's
history by *resolution at read* — `graph/lineage.rs` above — so a thousand forks
against a seeded ledger leave `links`, `links_current`, `concepts` and
`transaction_log` byte-identical, which is asserted as a count in both languages
rather than reasoned about.

**Why the read shipped three releases first.** A write that creates something
unreadable is the worse order. Had `fork()` landed at 0.14.2, every branch made
between then and 0.14.6 would have been readable only through a query that
silently absorbed its parent's later writes, and
[D-223](s13-decision-register.md#d-223) would have been a semantic break on
stored data rather than a correction to a path nothing could reach. That is
[D-160](s13-decision-register.md#d-160) → [D-174](s13-decision-register.md#d-174)
applied a third time.

**The cross-row invariant, and the one this module could not enforce.** `branches`
carries two `CHECK`s and both are within one row. Ordering a fork point against
the *parent's* row is not, so it lives here — and the rule the schema comment
promised since v12, `forked_at >= parent.created_at`, turned out to be
uncheckable rather than merely unenforced: the trunk's `created_at` is stamped
from the wall clock during migration, before the injected clock exists, so the
comparison is between two clocks and refuses every fork in the crate. What ships
is `forked_at >= parent.forked_at` — same clock by construction, and its
guarantee is that **fork points are non-decreasing down a root path**, which is
what `ancestry_cte`'s running minimum already assumed. §4's `branches` note
carries the full argument; the clamp stays either way, because raw SQL can still
write a row.

**Existence and fork point are read as two subqueries in one round trip**, because
the trunk's `forked_at` is legitimately `NULL` and one nullable column cannot
tell *no such branch* from *the root*. The pair of checks and the `INSERT` are
one actor turn, which is the reason `fork` is a `HighPriCommand` rather than a
write through the handle: the duplicate check is only sound if nothing can
register a colliding name between it and the insert. High priority for
`RegisterModel`'s reason rather than for its size — everything the caller does
next is work on this branch. `branches()` goes to the read connection.

**`BranchId` is not [`ModelName`](#59-vector--embeddings-the-model-registry-and-search),
and the difference is the justification rather than the rule.** `ModelName`
validates because a model name is *spliced into a table identifier* and SQLite
cannot bind an identifier ([D-037](s13-decision-register.md#d-037)). A
`branch_id` is a bound value at every call site, so there is no splice to protect
and `[a-z][a-z0-9_]*` would reject both shapes §15.5's use case generates — a
hyphenated UUID and a path-like `turn/17/alt/3`. The rule here is non-empty,
≤ 128 bytes, no control characters, no leading or trailing whitespace, and it
exists because `branches` is append-only under two unconditional guards: a name
is written once and can **never** be corrected, so a trailing space is not a typo
but a second lineage that prints as the first. Refused rather than trimmed, at
[D-034](s13-decision-register.md#d-034)'s boundary.

**Readable, and — since 0.14.8 — writable.** Through 0.14.7 `EdgeAssertion`
carried no lineage, so every write landed on the trunk and a fork was a *view* of
its parent's history as of an instant. The gap was invisible from the signatures
— `fork` returned a `Branch`, writes took none, nothing refused anything — so it
was stated in the rustdoc rather than left to be discovered. The subsection below
is that paragraph deleted.

#### The write path carries lineage (0.14.8, [D-225](s13-decision-register.md#d-225))

`EdgeAssertion::on_branch` and `ConceptUpsert::on_branch` name a lineage,
`Database::retire_edge_on` shadows an inherited edge, and `INSERT_LINK` and
`UPSERT_CONCEPT` bind `branch_id` — nine parameters each, where the ninth is the
one whose omission is *silent*, because the column defaults to `'main'`. §17's
second acceptance criterion, *a branch reads its parent's history **and its
own***, is assertable from this release in both languages.

**The finding is in the guard, and its repair is not a predicate.**
`reject_overlapping_interval` ([D-060](s13-decision-register.md#d-060), defect
AA) read `links_current` for the edge key with **no lineage predicate at all**,
which was exact for as long as every row in the table was `main`'s. The moment a
second lineage can write it is wrong in *both directions at once*: a branch is
refused for overlapping the parent belief it forked in order to supersede, and
the trunk is refused for overlapping a **branch's** belief it cannot see.
`AND branch_id = ?` fixes the trunk's direction and inverts the branch's — a
branch checked against only its own rows may assert `[10,20)` over an inherited
`[5,15)`, which is defect AA reintroduced across lineages *by the fix for it*.
That is the shape [D-223](s13-decision-register.md#d-223) found one release
earlier, where the obvious `WHERE recorded_at <= cutoff` made an inherited edge
vanish instead of appearing stale.

What ships is `lineage::key_visibility_cte` — the ancestry walk, the fork-point
cutoffs, the log-fold arm and the nearest-lineage `ROW_NUMBER()` of
[`visible_cte`](#lineage-resolution)
above, restricted to a single edge key. The rule it enforces is the read's own
definition applied to the write: **what a lineage may not overlap is what that
lineage can see**, and the trunk's case falls out of it rather than being
special-cased. `overlap_candidates_resolved` is the assertion arm
(`WHERE valid_from <> ?4`) and `retire_from_resolved` the retirement arm
(`WHERE valid_from = ?4`); both push `(source_id, target_id, edge_type)` into the
base scan, where `idx_lc_open_interval` leads with exactly those three columns,
so each is a seek. Calling the traversal's own CTEs instead would be O(rows) per
row on a branched bulk write; the cost of the second spelling is a fold written
twice, and what keeps it honest is that a divergence cannot be quiet — the write
would disagree with the read about what a branch can see, which is one named test.

**Retirement across a lineage boundary is a write, not an update.** Closing the
ancestor's row is the parent corruption [Doctrine
III](s0-s3-foundations.md#doctrine-iii) forbids, and `links` is append-only, so a
branch retires an inherited edge by writing its **own** row at the ancestor's key
with a closed interval; the read prefers it by `dist`. That answers the question
`CREATE_LINKS_SINGLE_OPEN` parked in v12 — the half a trigger able to see one row
genuinely could not answer went to the Rust layer, where the ancestry is
reachable.

**Every write asks `lineage_shape` before it takes the lock**, including trunk
writes. `check_lineages` calls the function the *read* path calls, for the reason
it calls it, so an unregistered branch is `UnknownBranch` naming the branch rather
than an unqualified foreign-key failure out of a rolled-back transaction — and on
a forked ledger the *trunk's* guard is the one that is wrong in the other
direction, so `None` resolves to `'main'` and is checked like any other name. The
shape is global, so a batch naming one lineage costs one round trip on a table
with no secondary indices, and a database that has never forked answers `Trunk`
and runs the pre-0.14.8 statements byte for byte.

**`DbError::CrossLineage` closes a guard three releases older than its caller.**
`trg_concepts_cross_lineage` has been in the schema since v12 and `AbortKind`
has recognised it since, but `classify` had **no arm** for that kind and fell
through to `DbError::Engine` — the opaque variant every other guard exists to
avoid. Nothing could reach it until a write could name a lineage, which is
[D-224](s13-decision-register.md#d-224)'s finding on a third kind of artefact:
machinery written for an unbuilt caller is exercised by nothing, so a gap in it
is invisible in a green suite. `concepts` is keyed by identity, so a branch
**inherits** its parent's concepts and may mint its own; what it may not do is
restate an inherited one.

<a id="branch-view"></a>

#### `BranchView` — the lineage threaded once instead of at every call (0.14.9, [D-226](s13-decision-register.md#d-226))

The last piece of §15.4's first bullet, and the smallest: a `Database` plus a
`BranchId`, so a caller who forked reads and writes through the fork. **Every
method on it exists on `Database` already and takes a lineage there** — the type
buys ergonomics and no capability, which is what made it one release rather than
a fifth of them. What the tests pin is exactly that: going through the view
produces the same rows, and the same errors, as naming the branch by hand.

**The `Arc` is the design.** `Database::close` takes `self` by value and an `Arc`
cannot surrender that while a clone survives, so *a view cannot end the handle it
reads through* — structural rather than documented. That is
[§5.1.11](#5111-sharing-the-handle-arcdatabase-and-not-clone)'s decision reaching
the use §15.4 reserved it for: `Database: Clone` was declined because a freely
cloned handle carrying `close()` puts every caller one call away from stopping
the actor, and a branch view was named at the time as the reason it should stay
declined. `Database::view` therefore takes `&Arc<Self>`, which asks for nothing a
caller sharing a handle did not already have, and `BranchView` derives `Clone`
because it owns no lifecycle.

**Construction does no I/O and cannot fail.** Whether the lineage is *registered*
is a question every operation on the view already asks — the read path since
0.14.4, the write path since [D-225](s13-decision-register.md#d-225) — answering
`UnknownBranch` by name. A checking constructor would buy one round trip's worth
of earlier notice and be stale by the next call.

**One read is wrapped and the rest are not.** `execute_ids`, `execute` and
`load_subgraph_with` take the lineage *from the builder*, so `BranchView::traversal`
seeding it once is the whole of the read side; what is wrapped is the two calls
that take the branch as a bare parameter — `load_subgraph`, whose sugar form has
no builder, and `query_as_of_edges_on`. `read_conn()` is lent so a seeded builder
runs without reaching back for the handle. `database()` is exposed because
`archive`, `checkpoint` and `verify` are properties of the *file*: duplicating
them onto a view would answer the same thing for every branch.

**A write naming a different lineage is refused, not relabelled** —
`DbError::BranchMismatch`, the thirty-ninth variant. An assertion naming *none*
is stamped, which is what building through the view produces; one naming a
different lineage is evidence the caller believed something about where the write
was going, and relabelling it discards the belief rather than contradicting it.
The failure is two views held at once with one's assertion passed to the other,
which nothing in the type system prevents — both have the same methods and the
call site reads correctly.

**`diff(a, b)` — the second read, and the one the plan mis-costed (0.14.11, [D-228](s13-decision-register.md#d-228)).** What `a` believes that `b` does not: `a` holds an edge key `b` does not hold at all, or holds it over a different interval, or at a different weight. §15.4 called that cheap "because divergence is exactly the set of rows carrying the branch's own id", and that rule is right for a fresh fork against an unchurned parent and wrong in both directions otherwise — a branch that writes nothing still diverges from a trunk that reweights after the fork, on a row the *trunk* wrote, and two siblings diverge through a row their **common ancestor** wrote; while a branch that re-asserts an inherited edge at its existing value writes a row and concludes nothing. So `branch_id` comes back on the `Divergence` and is not what selects it.

**It is one statement, and that is why the CTE builders take a name tag.** Two reads against `read_conn()` are two snapshots, and a write landing between them yields a reported difference that existed at no instant; a read transaction would close that and is not available, because the connection is public and `BranchView` lends the same one. So both lineages are resolved in a single `WITH RECURSIVE` — `lineage_a`/`churned_a`/`links_cut_a`/`visible_a` beside their `_b` twins — and the answer is `visible_a LEFT JOIN visible_b` on `(source, target, type, valid_from)`, with a row surviving when the join misses or the interval or the weight differs. The tag is a suffix rather than a second copy of the hybrid for [D-227](s13-decision-register.md#d-227)'s reason: `links_cut`'s two arms must partition, which is a property of one comparison written once. Every single-lineage caller passes `""` and emits exactly the text it emitted before.

**There is no instant parameter.** A shadow retirement *is* a divergence about an instant having passed — the branch's own row is closed at the ancestor's key, the ancestor's is open — so any `valid_from <= ts < valid_to` filter drops the branch's side and answers "no difference". `properties` is not compared because nothing in the crate reads edge properties back, and `weight` is compared exactly, because an epsilon would invent a tolerance the ledger does not have.

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

**`run_pre_filter` is the third reader of an embedding table, and the plan that closed F-31 named two (0.13.18, W9.3, [D-191](s13-decision-register.md#d-191)).** `PostFilter` composes `search_vector` and inherits its visibility predicate; `PreFilterCTE` scores candidate rows directly and inherits nothing. Fixing only `search_vector` would have moved the inconsistency rather than removed it — a filtered search would have answered differently depending on which strategy the byte estimate picked, which is worse than both arms being wrong, because it reproduces on one machine and not the next. `PreFilterCTE` now joins `concepts` and splices the same `VISIBLE_CONCEPT` constant. It needs no *k′* inflation: its filter and its `LIMIT` are in one statement, so the limit already applies to survivors.

**`FilteredVectorSearch` has no instant of its own; it reads the traversal's (0.13.19, W9.4, [D-192](s13-decision-register.md#d-192)).** W9.4 gives the vector surfaces an instant, and this one composes both of them, so it is the *fourth* place the predicate has to reach. A knob here would permit a filtered search whose filter is historical and whose ranking is not — a past neighbourhood scored against the present corpus, which is the axis confusion §3.1 exists to end rather than a configuration anyone wants. So the instant is `TraversalBuilder::as_of_valid`, propagated down both strategy arms, and the acceptance gate is the same one: `a_validity_that_ended_is_invisible_to_the_strategy_choice`. A traversal that states no instant reads the corpus even though `execute` always has a `now_ts` in hand — binding *that* would valid-time-bound every filtered search that has ever been written, which is the behaviour change [D-155](s13-decision-register.md#d-155) forbids.

**The strategy may never change the answer, and this is enforced rather than intended.** `PostFilter`'s failure mode is not a slow query but a short one: a top-ten that returns four rows and reports success. So when a post-filtered pass comes back short *and* its underlying index scan was saturated — it returned every row it asked for, so more existed — the planner cannot conclude the missing matches do not exist, and escalates to `PreFilterCTE`. The acceptance gate is that the two strategies agree on every query, swept across filter tightness and k. Strategy is then a performance decision and nothing else, which is the only form in which a planner is safe to have; a planner that picks between different answers is a fidelity leak of the kind [Doctrine VIII](s0-s3-foundations.md#doctrine-viii) names.

**Implementation status (0.5.5): implemented.** `FilteredVectorSearch` is the public surface — a builder mirroring [`TraversalBuilder`](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), taking a model, a query vector, a k and a traversal. Both strategies have execution bodies, `CostEstimator` reads its `byte_budget`, and the estimate is logged and returned. The strategy is planner-chosen; `FilteredVectorSearch::strategy` forces one, and exists for the agreement test above.

**The three premises of the 0.4.5 design, now all three measured (0.5.5).** They were recorded in 0.5.4 as unestablished. Measuring them is what removed a strategy:

1. **`CREATE TEMP TABLE` on the read connection fails** with `SQLITE_READONLY (8), "attempt to write a readonly database"`. `PRAGMA query_only = ON` ([D-019](s13-decision-register.md#d-019)) covers the TEMP database too. D-019 is the runtime half of the write-serialization guarantee and does not give way, so the staging mechanism is unavailable on the only connection a caller can reach. The reformulation the 0.5.4 note proposed — carry the candidates in the statement as bound parameters — is what `PreFilterCTE` now does, and it needs no write privilege at all, which makes it strictly better than the mechanism it replaces rather than a concession to it.
2. **The allow-list push-down does not exist.** `vector_top_k` refuses a fourth argument at runtime — *"too many arguments on vector_top_k() - max 3"* — and `vectorIndexSearch` in the bundled amalgamation rejects `argc != 3` before inspecting anything. So `TwoPhaseTempTable` had neither of its two mechanisms, and its row in the cost table priced an operation the engine cannot be asked to perform. It is removed ([D-050](s13-decision-register.md#d-050)).
3. **Selectivity has no source, so it is measured rather than estimated.** SQLite maintains no histograms and `sqlite_stat1` carries average rows-per-key, which estimates an equality predicate and not multi-hop reachability. The planner therefore runs a **bounded counting probe**: the traversal, under a cap (`DEFAULT_PROBE_CAP`, 10,000). It costs a fraction of what it prices and the cap bounds that fraction; above the cap the planner knows only "at least the cap", which is already enough to reject `PreFilterCTE`. The probe doubles as the candidate set, so the walk is paid for once rather than twice. `CandidateCount::Exact` and `::AtLeast` keep the distinction in the type, because a capped probe has not measured a count and must not be read as though it had.

### 5.4 graph/subgraph.rs and graph/algorithms.rs — native in-memory analytics

Superseded in 0.5.4 by [D-039](s13-decision-register.md#d-039), which replaced the petgraph bridge this section used to describe. The analytics surface is five algorithms — Dijkstra, A\*, strongly connected components, k-core, and Louvain — implemented in-crate over an adjacency list, with no external graph dependency.

**Louvain is local-moving only, and 0.8.0 replaced the reason it gave ([D-122](s13-decision-register.md#d-122)).** The aggregate-and-recurse phase is absent. That used to be justified by graph size — *"would matter on graphs far larger than the byte budget admits"* — and [D-115](s13-decision-register.md#d-115)'s interning raised the budget's reach by 5.8×–6.8×, so the claim was re-measured and found **false**: `examples/louvain_aggregation_probe.rs` has two-phase diverging at 6,144 nodes, well inside the budget, with the gap growing to +0.0049 Q at the 49,152-node ceiling. What settles it is *what* diverges. On `clustered`, whose ground truth is one community per clique, local moving recovers the truth **exactly** at every size, and two-phase buys its higher Q by **merging whole cliques** — four per community at the ceiling, scoring above the ground truth itself. That is the modularity resolution limit, a property of the objective: past a certain graph size Q prefers a partition coarser than the true one, so optimising it harder moves away from the answer. The phase is declined because it would change a correct result into a merged one, which is the opposite of the old reason and a stronger one.

`Database::load_subgraph(start_node, max_hops, now_ts, byte_budget)` walks `links_current` under the same bounded CTE shape as [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), hydrates node attributes, and returns a `Subgraph` holding `BTreeMap` node and adjacency maps. Two boundary conditions are enforced during the load rather than left to the algorithms:

- **The byte budget is measured and enforced, in linear time.** `DbError::SubgraphTooLarge` existed in the error enum from 0.4.0 and was constructed nowhere, so [D-007](s13-decision-register.md#d-007)'s budget was declared and unenforced through four releases. `load_subgraph` now carries a running payload total and refuses the moment it passes the budget — topology as edges arrive, attributes as they hydrate. The total is accumulated rather than recomputed: `estimated_bytes()` is O(V + E), and calling it per row made loading O(E²), so the check that bounds a load was itself the thing that did not scale ([D-047](s13-decision-register.md#d-047)). Both the agreement between the running total and the derivation, and the growth rate, are pinned by test.
- **Negative and NaN edge weights are refused at the boundary** with a typed `NegativeEdgeWeight`. Dijkstra and A\* are correct only for non-negative weights; a negative weight yields a shortest path that is merely a path. `links.weight` is a bare `REAL NOT NULL` with no CHECK, and adding one is a schema change against the [D-036](s13-decision-register.md#d-036) freeze that is not taken unilaterally — so the refusal sits at the load boundary instead. Note where that leaves it: `EdgeAssertion::normalized` does not check weight either, so the *write* path accepts a negative weight and only the read refuses it — the odd one of the three gaps [§4.7](s4-schema.md#47-what-this-schema-does-not-enforce) now gathers, and the only one still genuinely open ([D-074](s13-decision-register.md#d-074)).

Determinism is a property of the data structures, not of a sort applied at the end. `Subgraph` uses `BTreeMap`; the algorithms return `BTreeMap`/`BTreeSet` rather than the hashed equivalents; and every tie is broken explicitly — equal-distance heap entries by node id, equal-gain Louvain moves by the lower community index. Any one of the three left as a `HashMap` reintroduces per-process variation, because Rust's default hasher is seeded per process. Distances additionally need a total order that `f64` does not have: `OrdF64` wraps `total_cmp`, so a NaN weight cannot silently corrupt the heap's invariant.

**The interior is measured, and it is changing (0.13.27, W10.3, [D-200](s13-decision-register.md#d-200)).** §2.5 of the 0.12.0 review estimated a 5–20× gain from replacing the `String`-keyed adjacency with a dense index-based interior and said plainly that it had been read rather than benchmarked. `examples/subgraph_interior.rs` benchmarks it. The ratio holds — **9.6×–15.3× on Louvain, 12×–25× on Dijkstra**, from 48 to the 49,152-node budget ceiling, under both short and ULID-shaped ids — and the interior is **a third to two thirds** of what a caller waits for on realistic two-to-four-hop neighbourhoods, so it is not a small term. What the measurement changed is *where* the view is built: done at the boundary, as §2.5 proposed, the conversion costs one string lookup per edge endpoint and the whole operation is 1.8×–2.1× on Louvain and **a loss on Dijkstra**, which has one pass to earn the build back and does not. Done in-crate it costs one string lookup per *node*, because [D-115](s13-decision-register.md#d-115) already interned what an `EdgeRef` carries. **0.13.28 is that rewrite ([D-201](s13-decision-register.md#d-201)).** `Subgraph::build_dense` produces a borrowed CSR view — flat `(u32, f64)` edge arrays with an offset table per direction — and the six algorithms run on it; the three `BTreeMap`s, their order, and every public signature are untouched. At the budget ceiling `louvain` goes **675 ms → 75 ms**, `scc` **310 → 34**, `dijkstra` **125 → 28**, and `k_core` breaks even, because its own work was one degree count per node and it now pays a build proportional to the edges instead. On the realistic three-hop fixture the whole call goes from ~81 ms to ~33 ms. The view is built per call and **not** cached: caching it would roughly double the retained footprint of the one structure carrying an explicit byte budget. **`astar` came back off it in 0.13.29 ([D-202](s13-decision-register.md#d-202)).** It is the only one of the six that returns before it has seen the graph, so the build is not amortised by it but spent instead of it: at the ceiling a one-hop goal cost 16.3 ms on the dense view and 0.019 ms on the maps, settling six nodes either way, while the dense view's cost stayed flat — 16.3, 16.6, 17.2 ms — across goals one and four thousand hops away. Distant goals are 3×–10× slower for it, which is accepted: that case is a `dijkstra` call written as an `astar`. `a_near_goal_does_not_pay_for_the_whole_graph` holds it by ratio against `dijkstra` on the same graph rather than by a wall-clock number — pairing each timed run with the precondition walk it opens with, since in a debug build that walk is a thousand times the search and subtracting a separately measured one made the guard flake ([D-204](s13-decision-register.md#d-204)). What makes the change behaviour-preserving is that dense indices are `nodes`' key order, so index order and id order are the same relation and every tie the determinism contract breaks by id is broken identically by index — and `the_interior_may_change_but_these_answers_may_not`, written before the rewrite, pins all six algorithms' exact output including `scc`'s component order.

Write-back goes through the low-priority channel. `Subgraph::write_back_annotations` delegates to `Database::write_concepts` (`write_annotations` through 0.5.6 — [D-075](s13-decision-register.md#d-075)), which chunks at up to `chunk_rows::CONCEPTS` — a ceiling since 0.12.0, with each chunk's size measured rather than fixed ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)) — so an analytics job that annotates 50,000 nodes yields the writer to the UI at every chunk boundary and accepts the fidelity boundary of [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) in exchange. The 50,000-label Louvain save is the case the 0.4.5 amendment was designed around.

**Where analytics output belongs (0.5.4, [D-041](s13-decision-register.md#d-041)).** An analytics result is an annotation, and an annotation is derivative state:

```rust
pub struct Annotation {
    pub concept_id: String,
    pub label: String,      // e.g. "louvain.community"
    pub value: String,      // JSON-encoded payload
}
```

They land in `analytics_annotations` ([§4.5](s4-schema.md#45-analytics-annotations--the-second-derivative-table-054-d-041)): a derivative table created by the migration runner alongside the normative schema, disposable and rebuildable by re-running the algorithm ([Doctrine VI](s0-s3-foundations.md#doctrine-vi), second category), and excluded from `transaction_log` by the same reasoning that excludes embeddings ([Doctrine VII](s0-s3-foundations.md#doctrine-vii)). No trigger is defined on it, so the ledger never sees it — and a reconstruction that needs community labels recomputes them, which is the only honest way to ask what a past graph's communities *were*.

`Subgraph::write_back_annotations(&db, label, &values)` builds one `Annotation` per node present in `values` and hands them to `Database::write_analytics_annotations`, which chunks at up to `chunk_rows::ANNOTATIONS` on the low-priority tier — a ceiling since 0.12.0, with the size measured per chunk ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)). The per-chunk fidelity boundary of [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) is acceptable here for a reason it is not acceptable for assertions: a partially written analytics pass is recoverable by rerunning, and a partially written history is not.

**A rejected chunk names the concept it could not annotate (0.13.3, W7.2, [D-176](s13-decision-register.md#d-176)).** The absence of triggers cuts both ways: it is why this is the cheapest bulk table, and it is why the chunk writer was the one write path in the crate returning its engine error raw — no `RAISE(ABORT)` can fire here, so routing through `error::classify` looked like a no-op. It is not, because the table does carry a **foreign key** onto `concepts`. Annotating an id that is not a concept — an algorithm run against a graph read before an archive, or against ids that were never concepts — used to arrive as `FOREIGN KEY constraint failed` out of a rolled-back chunk of up to `chunk_rows::ANNOTATIONS` rows, identifying none of them. It is now `DbError::NotFound(concept_id)`, classified on the engine's extended result code rather than on its message.

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

The `seq_id DESC` ordering resolves same-timestamp entries within a single transaction correctly by construction, and `idx_txlog_entity` lets SQLite stream each partition without a global sort. Where an anchored fold is used, the `seq_id > :anchor` predicate is an inequality, which correctly skips any gaps left by rolled-back transactions ([D-024](s13-decision-register.md#d-024) — see the [§4.3](s4-schema.md#43-the-transaction-log) monotonicity note). The fold routes rows on `table_name`, deserializes payloads with `serde_json` (branching on the payload version field, and raising `PayloadVersion` for anything newer than the crate understands), populates `ReplayCorrupt` with the actual offending `seq_id` rather than a placeholder, and builds an adjacency index as it goes — so the resulting `MaterializedState` answers node, edge and neighbour queries with the shape of the live `Database`. Callers do not need to know whether they are querying the present or the past. The fold is a read: it runs on `read_conn` and never touches the actor — which is what decided the shape of [D-249](s13-decision-register.md#d-249): a verdict every recorded-time read needs, on a connection the write actor does not own, cannot be cached in the actor and cannot be cached in a temp table either, because `read_conn` is `query_only` and refuses to create one. It is kept in the database.

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

**Below the log's floor is a third case, and until 0.8.0 it was folded into the second ([D-121](s13-decision-register.md#d-121)).** `reconstruct` asked one boolean — *does the hot log cover `ts`?* — and sent everything else to cold storage. So a question about an instant before the ledger started, on a database that had never been archived, came back as `ReplayCorrupt` — the class meaning the ledger is damaged — naming an archive file the caller had never created. Asking what was believed before your data existed is ordinary, and *nothing yet* is the ordinary answer.

The obstacle was that *before recorded history* and *the cold file has been deleted* look identical on disk, and answering "empty" to the second would be inventing a fact. They are now told apart from the hot file alone, with no marker and no schema change: `transaction_log.seq_id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, a rolled-back transaction leaves no gap ([D-049](s13-decision-register.md#d-049), measured), and `trg_txlog_guard_delete` confines deletion to an archive session — so a log with `MIN(seq_id) = 1` and `COUNT(*) = MAX(seq_id)` has provably never had a row removed, and the implication runs both ways. Where it holds, the empty state is returned with `MaterializedState::predates_recorded_history` set, so a caller can tell it from a state that is empty because everything was retired. Where it does not, the cold file is genuinely needed and its absence stays an error.

**What the fold emits is a belief, not an edge (0.14.5, [D-222](s13-decision-register.md#d-222)).** `MaterializedState::edges` is `Vec<EdgeBelief>`: the five fields it carried as a tuple, plus the `branch_id` holding them. Until 0.14.5 it was the tuple, and that was the second half of the defect [D-221](s13-decision-register.md#d-221) records. [D-216](s13-decision-register.md#d-216) widened all four fold constants to `PARTITION BY (table_name, entity_id, branch_id)` so two lineages' beliefs about one edge would stay two rows — and then those constants did not **project** `branch_id`, so the label was lost on the way out of SQL, and `fold_delta` keyed its map on `entity_id` alone, which for a link is the edge key and is shared across lineages by design. **The widened partition was handing two rows to a container that could not hold two**, so one was overwritten and which one survived was decided by emission order. Both halves are fixed together, because either alone still collapses.

**Not resolved to one lineage's view, and deliberately.** `reconstruct` asks a whole-ledger question — *what did the ledger hold at `ts`* — and a forked ledger held both. Resolving here would need an ancestry, so a connection this type does not have, and would answer a narrower question than the one asked while looking like an answer to it. A caller wanting one lineage's view has `TraversalBuilder::on_branch` and `temporal::query_as_of_edges_on`, which resolve nearest-ancestor against the register ([D-220](s13-decision-register.md#d-220)) — and that is why filtering `MaterializedState::edges` by `branch_id` equality by hand is *not* the same thing and is not a supported substitute.

**`#[non_exhaustive]` and a constructor are one decision, not two.** The attribute is [D-207](s13-decision-register.md#d-207)'s call taken again so that a seventh field is additive. But `save_snapshot` is public and takes a `MaterializedState`, so the attribute alone would not have bought future flexibility — it would have spent present capability, making a public function uncallable from any other crate, and doing it silently, since the failure appears at a caller's compile rather than at ours. `EdgeBelief::new` takes the five fields that were the tuple and defaults the sixth to the trunk, with `on_branch` for the rest: `EdgeAssertion::new`'s shape, because the two are the same fact travelling in opposite directions.

**The snapshot file is a versioned container (0.5.4, [D-043](s13-decision-register.md#d-043); v3 in 0.13.12, [D-185](s13-decision-register.md#d-185); v4 in 0.14.5, [D-222](s13-decision-register.md#d-222)).** v4 is a *payload* change and the header layout below is still v3's — which is precisely what this number is for, because `bincode` is not self-describing: a v3 payload read as a v4 shape does not fail, it reads the next edge's `source_id` as this edge's `branch_id` and runs off the buffer somewhere later, reported as `ReplayCorrupt`. A fault to chase, and an upgrade is not one. A fixed header precedes the payload, written uncompressed and checked before anything is decompressed:

```
offset  0      4     6        10                18            26           34     38
        MACR | fmt | schema | taken_at_micros | payload_len | plain_len | crc32 | zstd(bincode(MaterializedState)) ...
        (4)    (2)   (4)      (8)               (8)           (8)         (4)
```

*This paragraph said "ten bytes" and showed a diagram ending at offset 10 until 0.13.12.* v2 added `taken_at_micros` in 0.5.5 and took the header to eighteen; the [D-054](s13-decision-register.md#d-054) note further down records that ("eighteen bytes per file") and this one was never brought into line, which is the [D-183](s13-decision-register.md#d-183) shape a third time — a correction filed beside the passage it corrects rather than into it.

A version or schema mismatch is `DbError::SnapshotIncompatible`, deliberately distinct from `ReplayCorrupt`: corruption is a fault to report, an incompatible snapshot is the ordinary consequence of upgrading, and the right response to the second is to discard the file and fold from the log. Without the header this failure is silent rather than loud — `bincode` is not self-describing, so a file written against a different shape of `MaterializedState` does not reliably fail to load, it loads into wrong values, and the newest snapshot is the first thing a restart reaches for. Files written before the container existed are caught by the same check, since their first bytes are zstd's magic rather than `MACR`.

**v3 makes the reader bounded, and gives damage its own name (0.13.12, W8.2, [D-185](s13-decision-register.md#d-185)).** `bincode`'s default limit is `Infinite`. serde's cautious-capacity blunts the single catastrophic `Vec::with_capacity`, so the practical failure was never one huge allocation — it was a deserializer working through a corrupt stream to exhaustion, on a file the crash path depends on. `payload_len` and `plain_len` are the declared lengths, `crc32` covers the first 34 bytes of the header **and** the payload, and the reader checks in that order: framing, then integrity, then the decompressed size — enforced *during* decompression, with the reader bounded to `plain_len + 1` bytes, so a frame that expands further stops at the bound rather than at whatever it decides to become. The bincode limit is then the buffer's own length.

Because the lengths sit under the checksum, a reader can trust them before acting on them; the checksum is what makes a declared bound a bound rather than a suggestion. What it is not is authentication — CRC-32 detects accidental damage, and anyone able to write a forged snapshot into the directory can compute the field and could equally overwrite the database file. The bounds after it therefore hold on their own, and the tests forge a valid checksum on purpose to prove it. A failure at any of these steps is `DbError::SnapshotCorrupt`, which is the *cache* being damaged rather than the ledger — see [§7](s6-s10-flows-to-dependencies.md#7-errors) for why that needed a third name.

**Atomic and durable are different words, and publishing a snapshot only had the first (0.13.13, W8.3, [D-186](s13-decision-register.md#d-186)).** The write is a temporary file, `fsync`, `rename` — and the `fsync` covers the file's *bytes* while the `rename` is a change to the *directory*. Until the directory's own metadata reaches the disk, a power loss can take the new name and leave everything else: the payload intact under a name nothing looks for, and the newest resolvable snapshot still the previous one. `save_snapshot` therefore flushes the directory after the rename, which is the standard POSIX close to the standard POSIX gap.

**What that gap costs is a slower start and never a wrong answer, and saying so is part of the entry.** A lost rename leaves the older anchor and the log, and folding from the older anchor is correct by construction — [Doctrine VI](s0-s3-foundations.md#doctrine-vi) makes a snapshot derivative and disposable, so no claim in this document rests on any particular snapshot being present. What does rest on it is [§5.1.7](s5-modules.md#517-shutdown-and-snapshot-coordination): `close()` promises the final anchor, and a promise that the file survives everything except the crash it exists for is not one worth making. A failure of the directory flush is reported rather than logged for the same reason — at that point the snapshot is on disk and readable, so the error does not say *this is missing*, it says *this function cannot promise the name outlives a power loss*.

Deletions get no such treatment, and the asymmetry is the argument: a deletion a crash undoes resurrects a *valid* snapshot, which the next retention pass deletes again, while a creation a crash undoes loses the anchor. Durability is owed to the name that has to be there, not to the name that has to be gone.

**Windows has no equivalent, and the branch says that rather than pretending (0.13.13, W8.3).** There is no directory `fsync`: a directory handle is obtainable with `FILE_FLAG_BACKUP_SEMANTICS`, but `FlushFileBuffers` wants write access the handle does not carry, and the call that does cover directory metadata takes a *volume* handle, needs administrative privileges, and flushes every open file on the volume — not something a library may do to its host's machine. What stands in for it is NTFS's metadata journal, which recovers a completed rename on the filesystem's own terms. That is a weaker guarantee than the POSIX branch's, it assumes NTFS or ReFS rather than FAT32 or a network share, and it is written into the function's documentation so that the no-op is a stated position instead of an absence nobody notices.

**The format W8.2 hardened is a format a fuzzer cannot get into, and W8.4 is mostly about that (0.13.14, [D-187](s13-decision-register.md#d-187)).** Coverage-guided mutation solves a four-byte magic in seconds. It does not solve a CRC-32 that has to agree with 34 header bytes *and* the whole payload — that is the same "needle in a 2^32 haystack" shape a fuzzer is famously bad at, and it is the shape W8.2 deliberately built. A fuzzer pointed at `load_snapshot` therefore spends its entire budget being turned away at step 2 and never reaches zstd or bincode, which are the two components §3.3 named and the two where a defect would actually live.

So there are three targets, one per layer, and the inner two are handed a container this crate builds around their input:

| target | input | what it explores |
| --- | --- | --- |
| `snapshot_container` | a candidate file | framing — magic, versions, declared lengths, arithmetic on numbers off a disk |
| `snapshot_payload` | plaintext, wrapped and checksummed by the harness | `bincode`'s decoder, under W8.2's `with_limit` |
| `snapshot_frame` | a declared length and arbitrary payload bytes, checksummed | zstd and the `plain_len` bound — the decompression bomb |

The third is the one with something the checksum cannot help with. A bomb's checksum is *correct*: the file is exactly what its author meant to write, and the only thing between the reader and the frame's full expansion is `take(plain_len + 1)`. That is why W8.2 put the bound after the checksum rather than trusting it, and it is why "never an allocation storm" is asserted here by libFuzzer's `-malloc_limit_mb` rather than by an assertion in a test.

The seed corpus is **generated and never committed** (`fuzz/src/bin/seed.rs`). A directory of valid v3 files is correct until `SNAP_FORMAT_VERSION` next moves, after which every seed is refused at the version check and the session starts from nothing — while still looking seeded, in a run whose only output is a coverage number nobody has a baseline for. Every seed is read back out of a snapshot `save_snapshot` genuinely wrote, so no part of the corpus is a second description of the writer.

Because an append-only log grows without bound, reconstruction is *designed* to compose with snapshots. A snapshot is a full `MaterializedState` serialized with bincode and compressed with zstd — JSON's readability buys nothing when both ends of the wire are owned by this crate — stored as a sidecar file named for the `transaction_log.seq_id` it reflects (`snapshots/0000000000000000123.snap.zst`). Snapshots are written every 10,000 log entries and on clean shutdown, with retention of the last five plus one daily for thirty days. Snapshot creation is a read-fold plus a file write; it never requires the write connection, and it runs entirely on the read side — a lightweight maintenance task watches `seq_id` through `read_conn`, and `close()` writes the final anchor after the actor has exited ([§5.1.7](s5-modules.md#517-shutdown-and-snapshot-coordination)). Keeping replay and snapshotting off the write connection is deliberate: the actor's loop must stay short enough that [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)'s latency bound holds.

**Off the write connection was never the same as off the thread (0.13.11, W8.1, [D-184](s13-decision-register.md#d-184)).** The paragraph above is about the *write connection*, and it is correct about it: snapshotting runs on `read_conn` and cannot lengthen the actor's loop. What it does not cover is the executor. Serializing a `MaterializedState`, compressing it with zstd, writing it and calling `fsync` are all synchronous, all unbounded in the size of the graph, and until 0.13.11 all of them ran on the tokio worker that happened to be polling `write_final` — the same worker pool the actor's own I/O is scheduled on. [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budgets that write at two seconds for 100K edges, which is two seconds during which one worker polls nothing. The load side has the same shape and is easier to miss: `snapshot_anchor` decompresses and deserializes a whole snapshot, on the async path of every `reconstruct` that composes.

Both now go to `spawn_blocking` — the write as one hop covering the save and the retention pass that follows it, the read as one hop covering the entire directory scan rather than one per candidate, since the loop stops at the first usable file and a hop per file would add scheduling to a path whose common case reads exactly one. `save_snapshot` and `load_snapshot` stay synchronous, because they are also the functions benches, tools and tests call from no runtime at all; the offload lives at the two async call sites, and the sync signatures say in their own docs that a caller inside a runtime needs the same wrapper.

The two ends do not treat a lost thread alike, and the asymmetry is the point. A `spawn_blocking` task cannot be cancelled once started, so a `JoinError` means the closure panicked. On the write side that is the file `close()` promised to have written, and it becomes `ReplayCorrupt` — the same class every other failure of `save_snapshot` already reports, so a caller needs no second arm for *it failed by panicking*. On the read side a snapshot is derivative and disposable ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)), so a panicking loader joins the incompatible and unreadable files the scan already skips: log it, return `None`, fold from genesis. That is strictly better than what it replaces, where the panic unwound through `reconstruct` and killed the caller's task — one corrupt file stopping a process that had a correct answer available the whole time. W8.4 fuzzes for those panics; this is what happens to the ones it has not found.


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

`as_of` is a filtered read of live tables **on its edge query**, which never touches the log. It applies the half-open window directly to `links_current`:

```sql
SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.branch_id
FROM links_current l
WHERE l.valid_from <= ?1 AND ?1 < l.valid_to;
```

This is the same predicate the traversal CTE carries ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity)), against the same table, which is the point: `as_of` and a traversal at `ts` agree because they ask the same question of the same rows, not because two query builders were kept in step by hand.

**And since 0.14.4 they agree about lineage as well ([D-220](s13-decision-register.md#d-220)).** The block above is the `Trunk` shape. On a forked ledger this read returned *every* lineage's rows — the most-called reader in the crate, quietly answering a question nobody asked — and it now resolves the same way the traversal does, through the same `graph::lineage` module rather than a second copy of the CTE. `query_as_of_edges_on(conn, ts, branch)` reads a named lineage; `query_as_of_edges(conn, ts)` delegates to it with `None` and keeps its signature, because a breaking change to this function is not what fixing its default is worth.

**Since 0.15.9 it does not spell that statement either** ([§5.12](s5-modules.md#512-planrs--what-a-read-asks-for), [D-251](s13-decision-register.md#d-251)). The block above is `plan::edges_at`'s, and this function is it with no recorded instant and the lineage dropped from each row — which is why the trunk arm now selects through an `l` alias and a sixth column it discards. Its own two-arm `match` over the shapes is gone. The paragraph below is the reason: this is the function that spelled its own SQL and missed the cutoff for four releases, and it is one release of work to stop it being able to happen again.

**It agreed about lineage and not about the fork point, for four releases (0.14.10, [D-227](s13-decision-register.md#d-227)).** [D-223](s13-decision-register.md#d-223) bounded the resolution by `branches.forked_at` and reached **two** of the three read paths: the traversal and `load_subgraph_with` share [`TraversalBuilder`](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), which carries the lineage and picks its own source through `resolved_source`, so a repair written there arrives at both. This function takes the branch as a bare parameter and spells its own SQL, so it kept emitting `visible` over `links_current`. It was wrong in both directions D-223 separates, and the second loses rows: a branch was handed a trunk edge recorded after it forked, and it *lost* an inherited edge the moment the trunk retired one — a reader without the fold shows **no** edge there rather than a stale one, and a branch that lost `b → c` is indistinguishable from a branch that never had it. It now assembles `churned` + `links_cut` + `visible(links_cut)` from the same `graph::lineage` functions the traversal uses rather than a second copy of them; the trunk's answer is unchanged, because `main` has no cutoff and `churned` is empty by its own clause. **The general shape is worth carrying:** a surface that does not go through the shared builder is the surface that misses every repair made to the shared builder, quietly, because the repair's tests are written against the builder. The same cause is why this was the one read surface D-220 never bound into Python ([§14.18](s14-python-bindings.md#fifth-read-surface)).

The module also owns attribute hydration, which is the mechanism behind `AttributeMode` ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), [§4.4](s4-schema.md#44-asymmetric-versioning-deliberately)). `hydrate_attributes(conn, node_ids, as_of, mode)` is the single entry point and dispatches on the mode and, under `AtTime`, on which of `AsOf`'s two axes are fixed (0.13.2, W7.1, [D-174](s13-decision-register.md#d-174)): `Omit` returns nothing and reads nothing; `Current` reads the live `concepts` rows and ignores both axes by definition; `AtTime` with neither axis is `Current`, with `valid` alone is the live row bounded by its own interval, and with `recorded` set resolves the latest `transaction_log` entry per entity at or before that instant and deserializes attributes from the payload. It is bounded by the id list handed to it — the result set — rather than by the log, which is what keeps the [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budget of 30 ms for 100 nodes independent of history size. **The query is; the operation was not** (0.15.5, [D-247](s13-decision-register.md#d-247)). The fold itself measures flat at ~0.14 ms from 2,000 to 500,000 log rows, and the reach guard in front of the `recorded` arm measured 0.1 ms to 24 ms over the same range — so the independence claim was true of the statement this paragraph describes and false of the call a caller makes. It is now true at or after the newest surviving stamp, where the guard is a single 3.4 µs seek, and **true below it as well since 0.15.7** ([D-249](s13-decision-register.md#d-249), schema v16, review C-5): there is still no exact cheaper *query* for "were rows removed", so the log stops being asked and keeps the answer instead — one row in `log_integrity`, maintained by a trigger on the only operation that can change it, read in 0.033 ms at every log size against 32.6 ms at 500,000.

**That fold partitions on `entity_id` alone, and unlike the link folds it is right to** (0.14.4). Written down rather than left as an omission that happens to be safe, because two sweeps — [D-216](s13-decision-register.md#d-216) and [D-220](s13-decision-register.md#d-220) — widened every *other* fold in the crate and left this one. A link's `entity_id` is the edge key and is shared across lineages by design; a concept's is the concept id, and under Option A there is exactly one concept row per id across the whole ledger — the guards refuse a second lineage restating one at all, and `branch_id` on `concepts` is provenance rather than identity ([D-214](s13-decision-register.md#d-214)). One row per id means one `branch_id` per partition, so adding it would change nothing.

**The `recorded` arm folds the hot log, so it carries the hot log's reach obligation** (0.13.16, W9.1, [D-189](s13-decision-register.md#d-189)). `archive` moves superseded rows out of `transaction_log`, and a superseded row is exactly what a past instant asks for; through 0.13.15 this arm read what was left and returned a shorter `Vec`, where a missing element is indistinguishable from *retired* and from *no such concept*. It now runs the same reach test [§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)'s traversal guard runs and raises `RecordedInstantUnreachable` ([§7](s6-s10-flows-to-dependencies.md#7-errors)), naming `reconstruct` — which takes the archive path. Since 0.15.4 that test consults the instant ([D-246](s13-decision-register.md#d-246)); before it, one archive session cost this arm every instant rather than the archived ones. The other three cells of that table read live `concepts` and an archive cannot shorten them, so the refusal is scoped to the one arm that reads the log.

The honest cost of [§4.4](s4-schema.md#44-asymmetric-versioning-deliberately)'s asymmetry lands here. `Current` is the mode a caller states when they want live text under a historical topology; it is documented as wrong for historical text, and since 0.6.0 leaving the mode unstated beside an instant is `AttributeModeUnstated` rather than a defaulted `Current` and a `tracing::warn!` nobody sees ([D-085](s13-decision-register.md#d-085)). A caller who needs belief-at-`ts` fidelity across retroactive assertions should not reach for `AtTime` at all — they should use `reconstruct(ts)` ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)), which answers a transaction-time question rather than patching attributes onto a valid-time answer, and which is the operation the refusal above names.

### 5.7 temporal/archive.rs — cold storage

Archive moves closed intervals and superseded log rows into a separate database file via `ATTACH`. The session is one atomic transaction ([D-012](s13-decision-register.md#d-012)): create the session marker, copy rows to the cold file, verify counts, re-derive the affected materialization, record the horizon, drop the marker, commit. A crash anywhere rolls the whole thing back, leaving hot and cold mutually consistent.

**What "re-derive the affected materialization" costs, and which variable it scales with (0.6.0, [D-077](s13-decision-register.md#d-077)).** That step is `rebuild_within`, and it is not a touch-up of the archived rows — it is `DELETE FROM links_current` followed by a full window-function reprojection over **everything still in `links`**. Two consequences the sentence above hides. First, the archive's repair term grows with the *surviving* table, not with the batch being archived, so [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s "Archive, 100K closed intervals ≤ 30 s" is parameterised on the wrong quantity: archiving a fixed 100K intervals costs steadily more as the ledger grows. Second, until 0.6.0 that rebuild also ran `audit_current` on itself — two `EXCEPT` passes over the whole projection, O(E log E) each, inside the archive's write transaction. Measured at 4K/16K/40K rows in `links`, the audit alone costs **≈15 / 61 / 190 ms** — **roughly half the whole repair**. The audit's own figure is the stable one across runs (179–212 ms at 40K over five runs); the rebuild total is not (318–428 ms for identical work), so the *share* reads anywhere from 42% to 61% depending on the run and is quoted as "about half" rather than to a precision the harness does not have. That is [D-070](s13-decision-register.md#d-070)'s session-noise caution applied to this cycle's own numbers. It is now skipped on this path ([D-077](s13-decision-register.md#d-077)); `rebuild_current`, the operator-facing repair, still verifies.

**Keyed since 0.15.3 ([D-245](s13-decision-register.md#d-245), review C-1).** The paragraph above is what the repair *was*, and it is kept because the reasoning that made it wrong is the reasoning that makes the fix exact. `links_current` is a function of `links` **per key**, so a session that deleted rows at some set of keys can only have disturbed the projection there. The session collects that set before its `DELETE` — with the delete's own predicate, so the two statements cannot describe different rows — and afterwards deletes those keys from `links_current` and re-inserts what the surviving rows project to. The re-insert yielding *no* row is the answer for a key whose last belief was archived, which is why the repair is two statements against the projection rather than one `DELETE` with a predicate: describing that case is how the pre-0.6.0 compensation drifted. Measured on a 200-key slice: **2.26 / 2.52 / 2.97 ms** against **13 / 133 / 701 ms** at 2K / 20K / 100K hot links — flat where the rebuild is linear. `rebuild_within` stays for `rebuild_current`, whose caller has no key set and wants none.

None of this was hidden by the published figures so much as unattributed by them: the `archive` cost in [§5.1](s5-modules.md#51-connectionrs--the-handle-the-pragmas-and-the-write-actor)'s exemption table is measured end-to-end through `Database::archive`, so the re-derivation was always inside it. Nobody had asked what fraction it was.

**The session marker is committed state at no point ([D-008](s13-decision-register.md#d-008), revised 0.5.3).** `CREATE TABLE macrame_archive_session (x)` is the first statement *inside* the transaction and `DROP TABLE` is the last, so commit drops it and rollback discards it. There is no crash path that leaves the delete guards disarmed.

The marker is an ordinary table in `main`, not a TEMP table, and the correction matters because the original formulation was unimplementable rather than merely suboptimal. Pre-0.5.3 the document specified `CREATE TEMP TABLE archive_session` with the guards probing `temp.sqlite_master`. SQLite forbids a trigger in `main` from referencing objects in another database, `temp` included, so that guard fails at `CREATE TRIGGER` time — the schema would not install. The guards therefore probe `main.sqlite_master` for the marker's name, which preserves [D-008](s13-decision-register.md#d-008)'s actual point: probing the catalogue rather than selecting from the marker directly is what turns an illegal delete into a typed `ArchiveViolation` instead of a "no such table" error nobody can act on. The connection-locality argument the TEMP formulation rested on is no longer needed, but its conclusion still holds by construction: only the Write Actor can create the marker, because only the Write Actor can write.

**ATTACH is issued outside the transaction and DETACH unconditionally on the way out**, error paths included — the same reasoning as [D-026](s13-decision-register.md#d-026) ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)): ATTACH is not transactional and survives `ROLLBACK`, so a leaked handle poisons the connection for every later archive and every later cold-database reconstruct. A best-effort DETACH also runs before the ATTACH, which is what closes the one hole pairing cannot — a panic between the two (0.5.4, [D-044](s13-decision-register.md#d-044)).

**Archive scope.** The path targets exactly three tables, and the third was added in 0.5.3:

1. **`links`** — an assertion is archivable when `recorded_at < :cutoff` **and** it is either superseded by a later assertion **of its own lineage** for the same interval key, or it is the current belief for an interval that closed before the cutoff **and no other lineage holds a hot row at that key** ([D-229](s13-decision-register.md#d-229), 0.14.12; both qualifiers were missing from v12 until then). The predicate is deliberately not "`valid_to` is in the past": it must keep every row `links_current` still projects, or [Doctrine VI](s0-s3-foundations.md#doctrine-vi)'s rebuildability is broken by the archive itself.
2. **`transaction_log`** — an entry is archivable when `recorded_at < :cutoff` and a later entry exists for the same entity **on the same lineage**. The newest entry per *fold partition* always stays hot, so `reconstruct(now)` never needs the cold file. This sentence read "per entity" until 0.14.12, written when the fold partitioned by entity; it has partitioned by `(table_name, entity_id, branch_id)` since v12 ([D-229](s13-decision-register.md#d-229)).
3. **`links_current`** — the rows projecting the intervals that were removed. `links_current` must remain equal to the latest-belief projection of what is left in `links` ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)), or `audit_current()` reports drift the moment an archive runs. **As of 0.5.4 this is done by re-derivation, not by a compensating DELETE ([D-035](s13-decision-register.md#d-035)):** the session calls `integrity::rebuild::rebuild_within(&tx)` inside the archive transaction. The prior hand-written compensation was a predicate over *valid* time standing in for one that also requires transaction time, and the two are not the same set — an interval closed at the cutoff but recorded at or after it survives in `links` while the compensation deleted it from `links_current`, producing permanent drift that the archive path itself caused. A description of a set can drift from the set; a derivation cannot.

**The two qualifiers above are 0.14.12's whole subject, and the reason they lasted the branch wave is that nothing could see them** ([D-229](s13-decision-register.md#d-229)). A link's `entity_id` is the edge key and carries no lineage by design, so "a later assertion for the same interval key" matched across branches: a branch writing at the trunk's key made the trunk's own open, current row archivable, and one `archive` left the trunk unable to reach a node it still believed — in both directions, since the predicate compares `recorded_at` and whichever lineage wrote second pruned the other. Separately, *a closed interval is history* is false of a **shadow**: archiving the branch's own closed row at an ancestor's key removes the branch's disbelief and lets the ancestor's open row win the resolution again, so an archive that mints no assertions un-retired an edge. **`audit_current` reports 0 throughout.** Doctrine VI's check asks whether `links_current` is the image of `links`, and item 3 above re-derives it from what survives — so the answer is yes whether or not the right rows survived. The drift audit can see a projection that disagrees with the ledger and cannot see a ledger that is missing rows; there is nothing outside the file to compare against, and the only instrument that shows this is asking what a lineage can still reach before and after. The second qualifier is **conservative rather than exact** — strictly it is an *ancestor's* surviving row that matters, and ancestry would mean resolving `graph::lineage`'s chain for every branch inside an operation that takes no branch parameter, while "surviving" is self-referential. Leaving rows hot costs bytes and is never wrong.

**Concepts are archived as of v9 ([D-129](s13-decision-register.md#d-129), [D-130](s13-decision-register.md#d-130)), and the three constraints below are what shaped the predicate rather than what refused it.** The paragraphs that follow are kept as written because they are the reasoning C2 had to answer, and each is answered in place. The archive session now has a fourth phase: after the `links` delete and before the log, every concept [`CONCEPTS_ARCHIVABLE`](s13-decision-register.md#d-128) admits moves to `cold.concepts` and its derived rows are disposed of. `ArchiveReport` carries `concepts_archived`.

**And the move back (0.9.0, C3, [D-131](s13-decision-register.md#d-131)).** `Database::rehydrate(&[…])` returns named concepts from `cold.concepts` to the hot table, on the same low-priority tier as `archive` and inside the same kind of declared session. It **mints no transaction-time facts**: the concept's log entries were never removed, so the ledger already says everything true about it, and the concept reacquires its old identity because the alternative would make the transaction-time axis lie about when it was learned ([Doctrine III](s0-s3-foundations.md#doctrine-iii)). That required schema v10 to be implementable at all — `trg_concepts_log_insert` is marker-gated, because the fold resolves last-writer-wins by **`seq_id` and not by `recorded_at`**, so a log row written at rehydration would take a new sequence number, outrank the concept's own retirement, and bring it back alive. Only the *insert* trigger is gated; nothing inside a session updates a concept.

`rowid_pk` is reinstated when it is still free and reassigned when it is not — and in the second case the stale `concepts_fts` entry at the old rowid is deleted, since the index is external-content keyed on that column ([D-119](s13-decision-register.md#d-119)). `RehydrateReport::rowids_reassigned` reports which exit was taken, because it is the one way a rehydrated row can differ from the row that was archived.

**Measured, and it does not window (0.9.0, C4, [D-132](s13-decision-register.md#d-132)).** 3.71 ms fixed and ~74 µs per concept, against `archive()`'s 20.37 ms for the same 1,000 rows — a **3.8× asymmetry** that is a fact about how the two directions are written rather than an inefficiency: the archive pays one `INSERT … SELECT` and one `DELETE` per table however much it moves, while rehydration is a per-id loop because the `rowid_pk` collision check has nowhere else to live. Above n=1,000 it turns superlinear, and the trigger-free control names the cause as FTS5 index maintenance rather than the row movement ([§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)). `rehydrate` nonetheless keeps its single transaction and gains no `rehydrate_windowed` twin: 10,000 concepts hold the write lock for 1.105 s, [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028)'s contract already tolerates ~50 s, and windowing would trade away the atomicity that makes a partial rehydration impossible. This is the one place the archive and its inverse deliberately differ in shape, and the reason is the size of the problem, not the principle.

**The second arm: forgetting a lineage (0.14.13, [D-230](s13-decision-register.md#d-230), schema v13).** `Database::archive_branch(branch)` is indexed by *lineage* where everything above is indexed by time, and it exists because the two cannot substitute: reclaiming an abandoned branch's recent history through `archive` means archiving the trunk's history of the same age with it. §15.4 justified the arm on "an abandoned branch's rows are a contiguous archivable set by construction". The predicate is indeed the cheapest in the crate — `branch_id = :branch` — and **contiguous by construction is false twice**. A concept is keyed by identity across the whole ledger ([D-214](s13-decision-register.md#d-214)), so a trunk or sibling edge may name one minted on the branch, which is measured rather than argued; and a branch's log rows are scattered through the sequence rather than forming a prefix. The first refutation becomes a **refusal** — a lineage other lineages still depend on is not abandoned — and the second forces the shape: links go, so the log must go (or `reconstruct(now)` yields the branch's open edges while `links_current` does not), so the **`branches` row** must go, because `hot_log_reach`'s soundness rests on *the newest row per entity is never archivable* and a whole-lineage predicate breaks that. Moving the lineage record is what makes a hot fold that omits the branch **correct rather than silently short**: afterwards the name is unknown, and a read naming it is refused instead of being handed its parent's view. Also refused for the trunk and for a branch with descendants, which read through it. The lineage record lands in `cold.branches` with an `archived_at`, which is the table [D-217](s13-decision-register.md#d-217) predicted this arm would need; no `archive_horizon` row is written, because that table records a cutoff and this session's boundary is a lineage.

**What was true through v8 ([D-022](s13-decision-register.md#d-022)), and how each part resolved:**

- **Foreign key integrity.** `embeddings_<model>.concept_id REFERENCES concepts(id)`. Deleting a concept violates the FK unless `ON DELETE CASCADE` is declared, and CASCADE silently destroys the embedding — a derived artifact [Doctrine VII](s0-s3-foundations.md#doctrine-vii) protects.
**`cold.links` is keyed by lineage since v15 (0.14.15, [D-232](s13-decision-register.md#d-232)), and it had to move with the hot table rather than after it.** The cold ledger carried the same lineage-blind primary key `links` did. Widening the hot one alone would have left `archive` as the single operation that still refuses the pair — two lineages' rows about one edge at one `recorded_at`, legal in `links` from the moment v15 landed, colliding on the way out. That is a maintenance failure on rows the crate had just started accepting, reported as raw engine text about a key the caller has never seen, and no read or write would have hinted at it. Existing cold files are carried across by `upgrade_cold_lineage`, the same function that already brings a pre-v12 file forward inside the session's own transaction; the new arm is a **rebuild** rather than an `ALTER` because SQLite cannot add a column to a key, which is safe here for the reason probe §12–13 established for the `ADD COLUMN` — `ROLLBACK` takes cold DDL with it, so a session that fails partway leaves the file as it found it. Detection is `PRAGMA cold.table_info`'s key-position column rather than the stored SQL, for the reason `cold_has_branch` gives: a cold file may be one this crate did not write, and matching its text would be matching someone else's formatting.

- **Portability of the cold file.** `F32_BLOB` and DiskANN are libSQL-specific. The cold database is a plain file opened via `ATTACH` and is deliberately trigger-free and FK-free — the delete guards must not exist on a file whose whole purpose is to receive rows, and a FK from `cold.links` to `concepts` could not be satisfied in any case.
- **Entity versus interval semantics.** Concepts are entities. They have no "closed" state analogous to a retired edge. A concept's lifecycle is `retired = 1` (application soft-delete) and `valid_to` (temporal expiry); physical removal is not part of it.

**Resolved, in order.** The first constraint dissolves: the embedding is *deleted* rather than cascaded, which [Doctrine VII](s0-s3-foundations.md#doctrine-vii) permits because it is recomputable from content — and the objection to `ON DELETE CASCADE` was always to the **silence** rather than to the destruction. The second dissolves with it: no vector crosses, so `F32_BLOB` and DiskANN never have to exist on the cold file. The third stands and became the predicate — [D-128](s13-decision-register.md#d-128) turns archivability into reachability rather than expiry.

`trg_concepts_guard_delete` remains the safety net against accidental deletion via raw SQL, a migration script, or a future code path that targets concepts without going through the session — but as of v9 it is **marker-gated** rather than unconditional ([D-129](s13-decision-register.md#d-129)), so it fires on everything except a declared session. Concept *archival* — as distinct from erasure — is deferred rather than ruled out, and [Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred) records the shape it would take; the constraint that survives is the third one, which turns archivability into a reachability predicate rather than an expiry predicate.

**The reachability predicate now exists, ahead of anything that acts on it (0.9.0, C1, [D-128](s13-decision-register.md#d-128)).** `CONCEPTS_ARCHIVABLE` states the third constraint above as SQL — `retired = 1`, **both** clocks behind the cutoff, and no surviving row of hot `links` naming the concept in either direction — and `temporal::archivable_concepts(conn, cutoff)` returns the ids it admits. Nothing is archived yet: this release ships the decision, not the move. `recorded_at < :cutoff` is in the predicate although the design named `valid_to` alone, for the reason the `links_current` compensation above gives, reached from the other side — a concept sent cold while the log entries describing it stay hot is the same two-clock mismatch.

**Two of the three constraints do *not* become clauses, and the difference is what a reader is most likely to get wrong.** The foreign keys from `analytics_annotations` and `embeddings_*` hold **derived** rows, so they do not block archivability; only the two `links` keys do, because those name ledger rows. The first bullet above is still right that `ON DELETE CASCADE` is unacceptable, but the objection is to the **silence**, not to the destruction: an embedding is recomputable from content by [Doctrine VII](s0-s3-foundations.md#doctrine-vii), so removing it as a declared step of an archive session and recomputing it on rehydration is sound, while having a foreign key quietly delete it as a side effect is not. C2 is where that disposal is actually specified.

If a concept must be removed for legal or compliance reasons, that is a separate operation outside the archive path, requiring explicit handling of embeddings, log entries, and `links_current` rows. It is not designed here because it is not part of the ledger's normal lifecycle ([Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred)).

**Sizing (0.5.0, updated 0.5.1).** [D-012](s13-decision-register.md#d-012) correctly keeps the archive atomic, but the lock-hold cost must be acknowledged. The planning estimate of 1,000 rows/sec is deliberately conservative; actual throughput on NVMe for `INSERT INTO … SELECT` across an `ATTACH` boundary, including trigger amplification on the deletes, is likely 5,000–10,000 rows/sec. At 1,000 rows/sec a 1M-row archive holds the write lock for ~16 minutes; at 10,000 rows/sec, ~100 seconds.

Because the archive is a single atomic transaction, the cooperative chunking of [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) does not apply — there are no boundaries at which the actor can yield, and UI assertions queue behind the whole transaction ([§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) states this in the latency contract). The mitigation is at the scheduling layer: the scheduler estimates the transfer-set size before issuing the command, and if it exceeds 100,000 rows it logs a warning and defers to the next idle window with a smaller cutoff. Each window is still one atomic transaction; the scheduler bounds the window's scope rather than the transaction's.

**Crash recovery between scheduled windows (0.5.1).** If the application crashes between two windows, some rows are in the cold database (committed window) and some remain hot (unstarted window). This is safe: nothing is duplicated or lost. The cold database holds a complete copy of what it received; the hot database still holds what it did not send. The next run resumes from the horizon recorded in `cold.archive_horizon`. This is distinct from chunking *within* a transaction, which [D-012](s13-decision-register.md#d-012) rejects: the scheduling-layer windows are separate transactions, each atomic.

### 5.8 integrity/ — audit and rebuild

`audit_current()` runs on `read_conn`. It compares `links_current` against the latest-belief projection of `links` and returns a count of divergent intervals. Zero is the only acceptable answer in steady state.

**The audit computes a symmetric difference, and the way it does so is load-bearing (0.5.4, [D-030](s13-decision-register.md#d-030)).** The 0.4.5–0.5.3 query chained four compound-SELECT arms flatly — `A EXCEPT B UNION B EXCEPT A`. SQLite gives `EXCEPT` and `UNION` equal precedence and evaluates left-associatively, so that parses as `(((A EXCEPT B) UNION B) EXCEPT A)`, which reduces to a constant zero. The audit reported "no drift" for every possible state of the database, including a `links_current` the test suite had deliberately corrupted. An integrity check that cannot fail is worse than no integrity check, because it is trusted. Parentheses are not the fix — SQLite rejects a parenthesised compound-SELECT operand outright. The corrected query names both sides as CTEs and computes each direction inside its own scalar subquery, summing the two, so the grouping is structural rather than syntactic and no future edit can silently regroup it. Both directions are required: a row `links_current` lacks is a missed materialization, a row it holds that the projection does not is stale or spurious, and either alone is drift.

`rebuild_current()` is a high-priority command ([D-013](s13-decision-register.md#d-013)). It deletes every row of `links_current` and re-inserts from the projection **in a single transaction** — which, until 0.5.4, it did not do. It had been a bare `DELETE` followed by an `INSERT`, and the window between them is the whole of current belief: a failure across it, or a concurrent reader landing in it, sees a graph with no edges and no error. [D-023](s13-decision-register.md#d-023) claimed atomicity in a document whose code did not implement it. `rebuild_within(&tx)` is the transactional form, and it is also what the archive session calls ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage), [D-035](s13-decision-register.md#d-035)).

**Two callers wanted a *restricted* projection, and for a while there were two spellings of it (0.15.3, [D-245](s13-decision-register.md#d-245)).** The window function has to rank inside a subquery, and a restriction has to go **inside** that subquery: outside it the query still ranks every partition in `links` and costs what the whole rebuild costs — correct, and pointless. `shadow.rs` worked this out in 0.6.0 for its chunk bounds and kept a private `projection_where`; `mod.rs` kept the unrestricted form as a const. One rule, written twice, in the module whose own header explains why that is the failure class [D-035](s13-decision-register.md#d-035) names. There is now a single generator, `projection_where(clause)`, with `latest_belief_projection()` as its `1 = 1` case.

`repair_keys_within(conn, keys)` is its third caller and the reason the unification happened when it did. It re-derives `links_current` at named keys only — a `DELETE` of those keys followed by the restricted projection over them — and it is what the archive session runs in place of `rebuild_within` ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)). It is exact rather than approximate because the projection is *pointwise*: one row per key, depending on that key's `links` rows and nothing else. The `INSERT` yielding no row is the answer for a key whose last belief was archived, which is the case a hand-written compensation has to describe and gets wrong. `rebuild_current` still calls `rebuild_within`; its caller is asking for a rebuild of everything and has no key set to offer.

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

**Every vector read joins `concepts` and applies one visibility predicate (0.13.18, W9.3, [D-191](s13-decision-register.md#d-191)).** Through 0.13.17 `search_vector` did not: `keyword_search` had carried `AND c.retired = 0` since it was written, the vector arm never gained it, and `hybrid_search` therefore fused one list that honoured a retirement with one that did not — F-31. The predicate is now a single spliced constant, `VISIBLE_CONCEPT`, bound to the alias `c`; the join is an inner join and is not a second filter, because the foreign key from each embedding table to `concepts` means a vector with no concept behind it cannot exist. The join costs *k* seeks on a `TEXT PRIMARY KEY` after `vector_top_k` has already reduced the corpus to *k*, not a scan.

**Search reads at a stated instant, and only at a stated one (0.13.19, W9.4, [D-192](s13-decision-register.md#d-192)).** F-32 was that no search surface bounded a concept's valid interval, so `as_of` could not reach retrieval at all and `search_filtered` mixed a past topology with the present corpus. `search_vector`, `keyword_search` and `HybridSearch` now take an optional valid-time instant, and with it set the `concepts` join W9.3 introduced also applies `c.valid_from <= t AND t < c.valid_to` — the crate's half-open interval, the same one `hydrate_current` and the traversal CTE use. Absent, the statement is byte-for-byte what 0.13.18 issued: an absent knob leaves the mechanism alone ([D-155](s13-decision-register.md#d-155)).

**The predicate became a function of the parameter index rather than a constant**, because the instant binds at `?4` in the vector search, `?3` in the keyword search, and after a variadic candidate chunk in `PreFilterCTE`. Passing the index keeps the timestamp a *bound parameter* on all three paths; splicing the value would put a literal into a statement already built per call, which is an injection surface and a prepared-statement cache miss for the sake of saving an argument. `keyword_search`'s own `AND c.retired = 0` — the copy W9.3 left behind — folds into the shared clause in the same edit, because W9.4 is precisely the release that would have made the two disagree.

**It is `as_of_valid`, and transaction time is deliberately not offered.** [D-174](s13-decision-register.md#d-174) split `as_of` into two axes; a search parameter called `as_of` would be a third spelling of the confusion §3.1 exists to end. Reading the *index* as it stood at a past `recorded_at` is a different question and this surface cannot answer it: the DiskANN index holds one row per concept and keeps no history of what the vector used to be. That question goes to the ledger.

**The requirement is stated once per surface, against one fixture, because "fixed by construction" is a claim about today's call graph (0.13.21, W9.6, [D-194](s13-decision-register.md#d-194)).** W9.3 and W9.4 both relied on composition: `hybrid_search`'s vector arm *is* `search_vector`, and `PostFilter` calls it too, so each was correct the moment the shared predicate was. That is true and it is not durable — an arm rewritten for speed or a second access path added for a planner removes the guarantee with every per-surface test still green, which is the exact history `run_pre_filter` already has. `tests/search_visibility_tests.rs` therefore asks all four surfaces, and `search_filtered` twice because its strategies share no query and no ordering mechanism, against a corpus where the retired concept and the expired one are ranked **first** by both arms before either is hidden. Absence is only evidence if the fixture could have returned them.

**The two are hidden by different mechanisms on purpose.** `retired` is a flag and an ended validity is an interval; they are separate terms of the same clause, and a surface can splice one and forget the other. The gate's companion asserts the other direction — with no instant stated the retirement still applies and the ended validity does not — so a build that valid-time-bounds every search whether or not one was asked for fails it ([D-155](s13-decision-register.md#d-155)).

**Decay ranks a hit by the age of what it matched, and the two surfaces do not share the arithmetic (0.13.20, W9.5, [D-193](s13-decision-register.md#d-193)).** One `half_life` parameter, off by default, applied at ranking and never at storage — nothing about an embedding changes because time passed, only its rank does. The factor is `0.5 ^ (age / half_life)`, and *age* is measured from the instant the search reads at, which is why this item follows W9.4 rather than standing alone.

**The sign is the trap, and it lands on one surface and not the other.** `search_vector` returns a **distance** and its list ascends; a factor in (0, 1] multiplied into a distance makes a stale row look *nearer*, so that surface converts — similarity is `(2 - d) / 2`, clamped non-negative before the multiply because scaling a negative similarity toward zero would improve it, and converted back so the score is still a distance. `keyword_search` returns bm25, which FTS5 gives **negative** with magnitude growing in relevance: it is a negated similarity already, so the plain multiply is correct there. **The operation that would have been the bug on the vector surface is the right one on the keyword surface**, which is exactly why the two are separate functions with separate tests rather than one shared helper that would have made one of them wrong.

**A half-life without an instant is refused** (`HalfLifeWithoutInstant`). Age is relative to something, and no read path in this crate reads a wall clock — that is what makes these answers pinnable under a `FakeClock` at all. Defaulting to *now* would make every decayed search quietly a search about the present, which is F-35's shape.

**Re-ranking a top-k is not the top-k of the re-ranking**, so a decaying surface reads to `rerank_depth(top_k) = max(5 × top_k, 50)` before it reorders — `HybridSearch::depth`'s rule, promoted to a shared function because decay needs it for the same reason fusion does. It is a bound rather than a guarantee, and the honest statement is that decay only ever *demotes*, so a row outside the pool enters the answer only if five times as many rows ahead of it were pushed below it. In `HybridSearch` the decay reaches each **arm** before the fusion, because RRF adds ranks and a factor applied to the fused score would leave both orderings — the only thing RRF reads — untouched.

**`search_filtered` does not take a half-life, and that is a decision.** The two strategies do not hold the same pool: `PostFilter` gets the *k′* the cost model priced, `PreFilterCTE` scores every candidate the traversal returned. Ranking by age inside each would make the answer a function of the byte estimate, which is the one thing [D-050](s13-decision-register.md#d-050) forbids and the property that makes having a planner safe at all.

**`top_k` stays a count, and that is the part with a decision in it.** The index chooses its rows before any predicate of ours can see them, so filtering afterwards returns fewer than *k* whenever a retired concept is among them — and letting `top_k` become a *ceiling* is a silent behaviour change for every caller that ever asked for ten. `search_vector` therefore re-asks the index for a larger *k′* when a pass comes up short, doubling until the index has been asked for the whole table, at which point what came back **is** every visible neighbour. It runs only on the short path, so a corpus with nothing retired pays one query and no row count — which is why this is a loop rather than the selectivity estimate [§5.3](s5-modules.md#53-graphvector_filterrs--strategies-and-the-byte-budget-cost-model) computes up front, an estimate here would price every search for a case that almost never arises.

Hybrid search runs FTS5 keyword retrieval and vector top-k and fuses the two ranked lists with Reciprocal Rank Fusion at k = 60. The keyword half reads a `concepts_fts` shadow table maintained by trigger. RRF is some twenty lines of arithmetic and needs no dependency; the constant is documented and tunable, because retrieval-quality tuning is empirical rather than theoretical. Both retrievals are reads from `read_conn`.

**Why fuse ranks rather than scores.** Cosine distance and BM25 are not comparable numbers, and any scheme that adds or weights them is inventing a conversion nobody measured. RRF adds reciprocal *ranks*, which are comparable by construction, and k damps the top of each list so that agreement between the arms outweighs a single arm's confidence: at k = 60 a document both arms rank tenth beats one that is first in one list and absent from the other. That is the behaviour the two arms exist for — dense vectors find a paraphrase and miss an exact identifier they never saw; BM25 does the reverse.

**Depth is not `top_k`.** Fusing two top-k lists is not the top k of the fusion: a document ranked twelfth by *both* arms can outscore one ranked first by a single arm, and it is invisible if neither list was read past k. `HybridSearch` therefore reads each arm to a `depth` defaulting to `max(5 × top_k, 50)` and truncates after fusing, which costs a larger `LIMIT` per arm rather than a second round trip.

**The index is external-content**, declaring `content='concepts'`: the tokens are indexed and the text is not duplicated, so the index cannot disagree with the concept about what the concept says — a standalone copy would be a second description of data the ledger already holds, the failure class [D-030](s13-decision-register.md#d-030) and [D-035](s13-decision-register.md#d-035) exist to prevent. It also makes [D-036](s13-decision-register.md#d-036)'s rebuildability the engine's own operation (`INSERT INTO concepts_fts(concepts_fts) VALUES('rebuild')`) rather than code of ours that could drift from the triggers; `Database::rebuild_fts()` is that statement, on the low-priority tier. The price is that external-content tables do not maintain themselves: an `UPDATE` must retract the old terms using the *old* column values before inserting the new ones, which is what `trg_concepts_fts_update` does, and omitting it leaves an index that still matches text no concept contains — silently, and detectable only by searching for something that should be gone. ~~There is deliberately no delete trigger, because `trg_concepts_guard_delete` is unconditional ([D-022](s13-decision-register.md#d-022)) and concepts are never physically deleted; if that guard ever becomes conditional, this index is stale until it gets one.~~ **Corrected 2026-08-07 — and the conditional it named is exactly what happened.** `trg_concepts_fts_delete` was installed **inert** on the `v7 → v8` rung ([D-119](s13-decision-register.md#d-119)), ahead of a guard that was still unconditional; the `v8 → v9` rung made the guard marker-gated ([D-129](s13-decision-register.md#d-129)) and the trigger has fired ever since, on every concept an archive session removes. The sentence predicted its own falsification and the prediction was acted on a release early, which is the only reason the index is not stale today.

**That guard turns out to be load-bearing twice over, and the second way was not recorded until 0.5.6 ([D-071](s13-decision-register.md#d-071)).** External content is keyed on `content_rowid='rowid'`, and `concepts` has a `TEXT PRIMARY KEY`, so its rowid is *implicit* — and `VACUUM` renumbers implicit rowids. That is the standard external-content hazard: after a vacuum every `content_rowid` points at a different concept, and keyword search (and `HybridSearch` with it) returns the wrong concepts with no error at all.

**It did not arise, and as of 0.9.0 it cannot** — but the reason has changed twice and the paragraph is corrected rather than replaced, because both revisions are instructive.

~~Concepts are never deleted, and upserts go through `ON CONFLICT(id) DO UPDATE`, which preserves the rowid rather than replacing the row — so `concepts.rowid` is dense `1..n` at all times and `VACUUM`'s renumbering is the identity map. **`VACUUM` is therefore safe on this schema exactly as long as concepts are never deleted.** [Appendix C](appendices.md#appendix-c--future-considerations-deliberately-deferred) designs concept archival as deferred-but-feasible and names GDPR erasure as a separate operation outside the archive path; either makes rowids sparse and makes this real, and whichever lands owes a rebuild.~~

**First revision (0.8.0, [D-120](s13-decision-register.md#d-120)): the density argument was never the thing protecting anyone.** `VACUUM` renumbers a sparse implicit rowid **only for a table with no index at all**, and `concepts` has always carried the `id` autoindex — so the hazard was never live, and the safety did not depend on density.

**Second revision (0.9.0, C2): the precondition failed, exactly as this paragraph said it would, and it no longer matters.** Concept archival landed, so concepts *are* physically deleted and rowids *are* sparse. The debt this paragraph recorded — *"whichever lands owes a rebuild"* — is **discharged rather than paid**: [D-119](s13-decision-register.md#d-119) had already replaced the implicit rowid with an explicit `rowid_pk INTEGER PRIMARY KEY` and re-keyed the index onto it, and an explicit `INTEGER PRIMARY KEY` is not renumbered at all. Measured on the case that now exists: `vacuum_after_an_archive_leaves_the_sparse_numbering_and_the_index_alone`, alongside the original `vacuum_does_not_disturb_the_fts_index`.

**What is worth keeping is the shape of it.** A paragraph named the precondition its safety rested on, named what would break it, and named what would then be owed. Two releases later the precondition broke — and the debt was already covered, by a rung taken for an unrelated reason. That is luck the second time and design the first, and it is only visible because the precondition was written down instead of assumed.

**FTS5's own `'integrity-check'` cannot detect this class of staleness**, which is why there is no `verify_fts()` beside `rebuild_fts()`. On libSQL 0.9.30 it verifies the index's internal consistency and not its agreement with the content table: after `'delete-all'` the index matches nothing where it matched ten rows, and the check still reports success. A verifier built on it would call an empty index healthy, so none was written; `an_emptied_fts_index_still_passes_integrity_check` pins the limitation as a tripwire, failing if a later engine starts cross-checking content. The exposure is bounded by [Doctrine VI](s0-s3-foundations.md#doctrine-vi) — the index is derivative and `rebuild_fts()` reconstructs it from the ledger, so nothing can be lost, only made wrong until someone rebuilds. Restoring a backup that skipped the shadow tables remains the case that method was written for.

**Arbitrary text is escaped before it reaches FTS5.** MATCH syntax is a language — `AND`, `OR`, `NOT`, `NEAR`, prefix `*`, column filters — and passing a search box through to it fails two ways. A malformed expression raises `SQLITE_ERROR`, so a user who typed a query gets an exception; and a query containing `NOT` silently *means* something, quietly excluding documents, which is a wrong answer rather than an error. Each run of alphanumeric characters therefore becomes one quoted term with implicit AND between them. `HybridSearch::raw_match(true)` opts back in to the query language for a caller building the expression themselves.

**The write path (0.5.4, [D-048](s13-decision-register.md#d-048)).** `Database::register_model(model, dim)` creates the table and its index on the high-priority tier; `Database::upsert_embeddings(model, rows)` stores vectors on the low-priority tier, chunked at `chunk_rows::EMBEDDINGS`, atomic per chunk, with the declared dimension resolved once per chunk. Both go through the Write Actor, because the write connection is actor-owned and `read_conn` is `query_only` ([D-019](s13-decision-register.md#d-019)) — before 0.5.4 neither had a route to it, so an application could search vectors it had no way to store. Reads are unchanged and still go direct to `read_conn`, never traversing the actor.

**Implementation status (0.5.5): implemented ([D-051](s13-decision-register.md#d-051)).** The 0.5.4 text recorded that hybrid search did not exist — `reciprocal_rank_fusion` was a pure function with nothing producing the keyword half and no `concepts_fts` table anywhere in [§4](s4-schema.md#4-schema), while [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budgeted the path at ≤ 50 ms. The schema decision it declined to take is now taken: `concepts_fts` and its two triggers arrive on a `v4 → v5` rung, derivative and additive under [D-036](s13-decision-register.md#d-036), and the rung backfills — unlike [D-041](s13-decision-register.md#d-041)'s annotations, the index is a pure function of text the ledger already holds, so `'rebuild'` reconstructs exactly what the triggers would have written had they always existed. `HybridSearch` is the public surface, mirroring `TraversalBuilder` and `FilteredVectorSearch`.

One correction landed with it: `reciprocal_rank_fusion` sorted on the fused score alone, leaving ties in `HashMap` iteration order. Ties are the *common* case here — two documents at the same pair of ranks score identically by construction — so the same query could return the same set in a different order on the next run. The sort now breaks ties by id. This is the procedural-versus-structural determinism trap [D-047](s13-decision-register.md#d-047) names, arriving as a search result that will not sit still.

### 5.10 metrics.rs — what the actor holds the lock for

**Off by default, and the default is the argument (0.6.0, [D-079](s13-decision-register.md#d-079)).** Every latency claim in this crate before 0.6.0 was a `cargo bench` figure. A [`CHUNK_BUDGET`](s5-modules.md#515-cooperative-chunking--the-golden-rule) that cannot be checked *in situ* is an aspiration, not a bound: it describes what a benchmark measured on one machine, not what the actor is doing in an application under real contention.

`--features metrics` records, per [`CommandKind`](s5-modules.md#513-the-two-tier-command-channel), a histogram of how long the actor held the write connection. `MetricsSnapshot::budget_violations()` returns the kinds whose holds exceeded the budget, which is the question an operator actually has.

*The instrumentation is arranged as two impls of one type rather than `#[cfg]` inside the actor loop.* With the feature off, `ActorMetrics`'s methods are empty on a zero-sized type, so the loop compiles to what it compiled to before. The alternative — conditional compilation inside the loop — makes the loop's shape depend on a feature flag, and the loop is the one piece of this crate where an accidental early return or a missed `select!` arm is a deadlock rather than a wrong answer. One shape, always.

**`HoldTimer` left the gate in 0.12.0, and the zero-cost claim is narrowed rather than abandoned** ([D-146](s13-decision-register.md#d-146)). It read no clock without the feature until then, because the reading existed only to feed the histogram. [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)'s chunk loop made that reading a *control signal* — the input to the next chunk's size — so it is needed in every build. Left gated it returned `Duration::ZERO`, which the controller reads as *comfortably under budget*: a default build would have grown every chunk to the ceiling, in exactly the builds nobody was measuring. What is still gated is the histogram, which is all of the cost that was ever worth gating; what is now paid unconditionally is one `Instant::now()` pair per actor turn, tens of nanoseconds against a turn measured in microseconds at best.

Buckets are fixed at `BUCKET_BOUNDS_MICROS` rather than computed. A histogram whose bucket edges move between builds cannot be compared across them, and comparison across builds is the only reason to keep the numbers.

**`Analyze` became two kinds, and that is what made the exemption answerable (0.13.24, W10.5, [D-197](s13-decision-register.md#d-197)).** From 0.12.4 to 0.13.23 one `CommandKind` covered both `analyze()` and `optimize()`, and [D-168](s13-decision-register.md#d-168) refused to decide its budget exemption *because* of that: `close()` calls `optimize()` unconditionally, so a judgement made about the explicit call would have landed on the automatic one without ever being made about it. That is [`CommandKind::Rehydrate`]'s defect from the other side — there a shared kind granted an exemption nobody had decided ([D-152](s13-decision-register.md#d-152)); here one would have laundered one.

**Both halves came back not exempt, and the reasons are different, which is why the split had to come first.** `Analyze` cannot fill in the exemption table's `Bound` column — 19.1 ms at 40,000 edges against a 3 ms budget, and the honest entry is "the size of the table, damped 3–4×", which is the absence of a bound rather than one ([D-166](s13-decision-register.md#d-166)). `Optimize` is the opposite case: measured at **90–220 µs whenever it declines to re-analyse**, which is nearly always, and over budget only when it actually did work — 10.7 ms on a never-analysed database, 460 ms once the table had grown past SQLite's staleness ratio. Its violations are a signal rather than noise, and exempting the kind would delete the only thing that distinguishes a `close()` that cost nothing from one that cost half a second.

**The measurement also falsified the premise of a scheduled item.** `PRAGMA optimize`'s staleness test is a *ratio*, not a row count, and it is a large one: read across the call rather than timed, `sqlite_stat1` was **untouched** after growth of 2× and 5× and only rewritten at 25×. So `optimize()` is not a cheap `analyze()` and calling it after a bulk load does not refresh the statistics the load invalidated. That is W10.2's input, recorded here because it came out of this measurement.

**The shadow rebuild became two kinds, and the exemption criterion got named (0.14.16, W12.16, [D-233](s13-decision-register.md#d-233)).** `CommandKind::ShadowRebuild` covered both halves of a chunked rebuild from 0.6.0: the `Begin` and `Fill` turns, which are *meant* to fit the budget, and the one `Swap` turn, which cannot — all three indexes are built there under the write lock, 46.8 ms against 3 ms ([D-082](s13-decision-register.md#d-082)). Since the kind was not exempt, every successful rebuild put a permanent entry in `budget_violations()`, so the method documented as *the one-line answer to whether the bound is holding* answered falsely on any database that had ever repaired its projection — and a fill regression, the thing worth catching, arrived as `+1` on a count that already read `N(rebuilds)`. `tests_py/test_end_to_end.py` had been carrying a hard-coded carve-out for exactly this since 0.14.9, which is the cost being paid in the open.

**The rule the exemptions had always followed, written down at the fourth application.** `Archive` ([D-012](s13-decision-register.md#d-012)), `Rehydrate` ([D-152](s13-decision-register.md#d-152)) and `Checkpoint` ([D-156](s13-decision-register.md#d-156)) are exempt; `Analyze` and `Optimize` are not, for the reasons two paragraphs up. What separates them is not size and not predictability but **applicability** (re-anchored 0.14.17, [D-234](s13-decision-register.md#d-234)'s sibling amendment under [D-233](s13-decision-register.md#d-233)): *exempt means the chunk bound does not apply, because the operation is atomic by necessity and has no smaller unit to chunk into; counted means the bound applies, so exceeding it is information.* This was first written here as *expected-on-healthy overages are exempt, workload-dependent ones are not*, and that form is refuted by `Optimize` — which runs on **every** close, is expected-on-healthy in the plainest sense, and is counted. Expectedness is the usual *symptom* of inapplicability, not its definition. An exempt kind's violation would be a constant; a non-exempt kind's violation discriminates. Split, the two halves of a rebuild land on opposite sides — the swap is `CommandKind::ShadowSwap` and exempt, the fill half keeps `ShadowRebuild` and stays counted, so a nonzero there is a regression and nothing else. Nothing is hidden by the exemption, because `over_budget` counts **occurrences and not magnitude**: it moves by one per over-budget turn however far over, so the swap's growth with table size was never visible there and remains visible where it always was, in the kind's histogram and `longest`.

**The starvation counter's companion decision, and the reason it changed (0.13.26, W10.4, [D-199](s13-decision-register.md#d-199)).** [D-153](s13-decision-register.md#d-153) left open whether its `run_max=63` said anything about production or only about a 64-task burst. W10.4's sweep answers it the other way: **four closed-loop writers** — each awaiting its own write, which is what application code does — starve the low tier for essentially all of their writes, and the run scales with the interactive work offered rather than with the number of callers. So the starvation is real on ordinary shapes, and the reason there is still no fairness floor is the floor's own price. "Take one low-priority command" cannot choose which one — the low queue is an mpsc channel with no peek — and `Archive` is exempt from [`CHUNK_BUDGET`](s5-modules.md#515-cooperative-chunking--the-golden-rule) **by contract**, measured at 3.3 s unwindowed on an 8,000-key backlog ([D-080](s13-decision-register.md#d-080)). The floor would add an unbounded term to the interactive path to unblock work whose exemption was granted because it is *not* on one. The lever that does work is think time and it belongs to the caller, which is what `low_starved_run_max` is now documented as the instrument for.

**And the call site W2.2 scheduled is not built, because nothing reads what it would write (0.13.25, W10.2, [D-198](s13-decision-register.md#d-198)).** The measurement went past the threshold the item asked for to the question underneath it. On a freshly loaded database an `optimize()` **does** fire — the first call is a full analysis, so [D-197](s13-decision-register.md#d-197)'s ratio never gets in the way — and it writes seven rows of `sqlite_stat1` for between 0.26% and 21% of the import's own cost, depending on run size. It then changes **no plan and no opcode count**: not at 90, 500, 5,000 or 40,000 edges, and not across the six queries the registry justifies an index with plus a join whose order the planner is free to choose. A maintenance call whose output nothing reads is [D-089](s13-decision-register.md#d-089)'s unread index wearing a different costume, and this crate has a rule about those.

**`tests/statistics_effect_tests.rs` is that result as a tripwire**, against a third fixture — the same rows with no `ANALYZE` — added so the comparison isolates statistics rather than confounding them with row count the way [`migrated`] against [`populated_and_analysed`] does. [D-149](s13-decision-register.md#d-149) is not overturned by any of this: `close()`'s call costs ~0.1 ms on an idle database and keeps the estimates honest for the query that has not been written yet, and [D-150](s13-decision-register.md#d-150)'s two fixtures exist precisely so the crate notices when the two planners diverge. What is refuted is that a *second, automatic* call site buys a caller anything today.

### 5.11 util/ — ids, clocks, timestamps, checksums, and engine ceilings

`ids.rs` generates and validates ULIDs. Validation is not cosmetic: a link's `transaction_log.entity_id` concatenates `source|target|type|valid_from`, so an id containing the separator makes the row unattributable on replay ([D-061](s13-decision-register.md#d-061)). A rejected id is [`DbError::InvalidId`](s6-s10-flows-to-dependencies.md#7-errors) — *refused*, not *missing*, which is why it is not `NotFound`: telling a caller the thing does not exist invites them to create it with the same id and be refused again.

`clock.rs` is the `Clock` trait, `SystemClock` (monotonic floor plus strict parser) and `FakeClock`. [§5.1.2](s5-modules.md#512-handle-shape-and-the-clock-contract) has the contract.

`timestamp.rs` owns the canonical form — `YYYY-MM-DDTHH:MM:SS.ffffffZ`, exactly 27 bytes ([D-029](s13-decision-register.md#d-029)). `normalize` accepts the canonical form and the legacy second-precision one and **refuses everything else rather than guessing**: an offset, a missing `Z`, millisecond precision. `parse` additionally validates the calendar, because `2026-02-30T…` has canonical *shape* and is not a date, and accepting it would let a timestamp exist that no round trip can reproduce. `OPEN_SENTINEL` is the open-interval end.

`util/limits.rs` is the one that needs its existence explained, because it looks like the tuning constants in `connection::chunk_rows` and is the opposite of them (0.6.0, T3.1). **`chunk_rows` holds choices; `limits` holds ceilings the engine imposes.** `HYDRATE_CHUNK` (400) is how many ids go into one `IN (…)` list, and it is not a latency budget — these are reads, and `CHUNK_BUDGET` bounds what the *writer* holds. It is a bind-variable ceiling: `SQLITE_MAX_VARIABLE_NUMBER` is 999 on a stock build, and hydrating a budget-sized subgraph in one statement would fail at the driver with an error that says nothing about node count. The margin is doubled rather than exact, because a hydrate query carries other bound parameters too and a query that gains one should not be what discovers the ceiling.

It was defined twice before T3.1 — in `graph::subgraph` and `temporal::as_of`, both 400, one carrying the reasoning and the other a `// See as_of::HYDRATE_CHUNK` comment. That works until someone tunes one of them, at which point two constants that must be equal silently are not, and the symptom is a driver error on one code path and not the other. The cross-reference comment was evidence the duplication was known and being managed by convention; a shared constant is what replaces a convention.

The relationship is asserted at **compile time**, not in a test: `const _: () = assert!(HYDRATE_CHUNK * 2 < SQLITE_MAX_VARIABLE_NUMBER)`. A `#[test]` was the first form, and clippy was right to object — both sides are constants, so the assertion has a constant value. A const block fails the build rather than a test run, which matters because the failure it guards is someone raising `HYDRATE_CHUNK`, plausibly in a release build, plausibly without running the suite.

`crc32.rs` is the checksum the v3 snapshot header carries (0.13.12, [D-185](s13-decision-register.md#d-185), [§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)): the standard polynomial, a table generated at compile time so there is nothing transcribed to get wrong, and the standard check vector asserted. It is forty lines here rather than `crc32fast` because the input is one payload per save and per load, on a path already dominated by zstd and bincode, and a dependency has to be audited and kept compatible for as long as the project lives.

**It was absent from this section and from [§3](s0-s3-foundations.md#3-crate-layout)'s tree from 0.13.12 until 0.13.15** ([D-188](s13-decision-register.md#d-188)). The heading above is an enumeration — *ids, clocks, timestamps, engine ceilings* — which is the kind of sentence that stops being true without anyone editing it, and nothing was checking. Something is now: `tests/doc_sync_tests.rs` fails the build when a module under `src/` is not named in §3.

### 5.12 plan.rs — what a read asks for

`ReadPlan` is what a read asks for, as one value: `branch`, `valid`, `recorded` and `limit`, every one of them `Option` and every `None` the ordinary read — the trunk, now, current belief, the whole answer (0.15.9, W13.4, [D-251](s13-decision-register.md#d-251), review F-34; `limit` 0.15.10, W13.5, [D-252](s13-decision-register.md#d-252)). `TraversalBuilder::plan` applies one, `read_plan` returns one, and `Database::edges` takes one.

**`limit` is the one field that does not narrow which rows are true.** The other three name a read; this one bounds what it costs, so two reads under the same plan can differ and a plan carrying a limit describes a prefix rather than the read. What "prefix" means is the surface's to say and each does: on a traversal it is [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity)'s ceiling on the walk, yielding the nodes nearest the start; on `edges` it is a plain `LIMIT` on one flat projection, whose rows are whichever the engine reaches first — that read states no order, and adding one so the truncation looked principled would put a sort on the largest statement in the crate. It needs nothing to report truncation, because nothing drops rows after the limit applies: `len() == n` is exact there where it is meaningless on the walk.

**Two modules are called `plan` and only one is a public path.** This one is *what was asked*; `graph/plan.rs` ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity), [D-243](s13-decision-register.md#d-243)) is *how it is answered*, stays crate-private, and a caller who never reads SQL never meets it. The division is what the naming costs and what it buys.

**A plan is inert.** It validates its branch — through [`BranchId`](s13-decision-register.md#d-224), which is the type that already asks that question — and canonicalises nothing else. An unregistered lineage is `lineage_shape`'s refusal at read time, an instant below the hot log's reach is [D-247](s13-decision-register.md#d-247)'s guard, a malformed stamp is `timestamp::normalize` at the read. Nothing moved to the constructor, because only the database knows which lineages exist and only the read knows which log rows survive.

`plan()` **replaces rather than amends**, and that is the argument for having it at all: a plan *is* the read, so applying one answers what the read is instead of patching what it was, and `ReadPlan::new()` clears an `as_of_recorded` set earlier. The three setters on the builder stay — `plan()` is additive, C-11 is breaking, and they do not share a release.

The module also owns `edges_at`, which is the statement in [§5.6](s5-modules.md#56-temporalas_ofrs--valid-time-queries-and-attribute-hydration)'s block: `lower()` a `Resolution` from the plan, filter the half-open window, return `EdgeBelief`. `Database::edges` is that with the handle's clock standing in for an unset `valid`; `query_as_of_edges_on` is that with no recorded instant. The placeholder layout is the traversal's with four slots removed — `?1` the valid instant, the branch next when the shape binds one, the recorded instant after it — and `BRANCH_SLOT` is a named constant here for [D-030](s13-decision-register.md#d-030)'s reason: the SQL and the parameter vector must agree exactly, and agreeing by comment is the failure mode.

**What it added that no reader had.** `query_as_of_edges_on` takes a valid instant and a lineage and has no third argument, so a bitemporal whole-ledger read — *what did we believe, in March, about how the graph stood in January* — meant walking from a start node the question does not have, or folding the entire log with `reconstruct` and filtering the result. `edges` reaches `links_at_tx` bounded by the ancestry's cutoffs ([D-223](s13-decision-register.md#d-223)), so the cell costs what reading it costs. And `EdgeBelief` carries `branch_id`, which the five-tuple could not: on a forked ledger the old reader could say *that* an edge is visible and not *whose* it is, and nearest-ancestor resolution is precisely what a caller cannot reconstruct by filtering.

The order of the result is **unspecified**, as it always was for `query_as_of_edges`. An `ORDER BY` here is a sort over the largest read in the crate, added so the answer looks tidy; a caller who needs an order knows which one and can sort a `Vec` they already own.

