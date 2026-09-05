<!--nav-->
← [previous](s5-modules.md) · [index](README.md) · [next](s11-s12-milestones-and-risks.md) →
<!--/nav-->

## §6 Data Flows

### 6.1 Edge assertion

The application builds an `EdgeAssertion` and calls `db.assert_edge(edge)`. The value is normalized at the boundary — edge type validated against `[A-Z0-9]+`, timestamps widened to the canonical 27-character form ([D-029](s13-decision-register.md#d-029)) — so a malformed edge type or a second-precision timestamp is a typed error at the call site rather than an engine CHECK failure surfacing from the far side of an actor with no context attached ([D-034](s13-decision-register.md#d-034)). The normalized value crosses the high-priority channel as `HighPriCommand::AssertEdge`; the caller awaits its `oneshot`. The actor stamps `recorded_at` from the injected clock, opens `BEGIN IMMEDIATE`, inserts into `links`, and commits. Inside that transaction `trg_links_current_sync` upserts current belief and `trg_links_log_i` appends the log entry. The responder carries `Ok(())` or a typed `DbError` classified through the single boundary of [D-033](s13-decision-register.md#d-033).

### 6.2 Bulk analytics write-back

The application loads a subgraph, runs Louvain in memory ([§5.4](s5-modules.md#54-graphsubgraphrs-and-graphalgorithmsrs--native-in-memory-analytics)), and calls `db.write_concepts(concepts)` (`write_annotations` through 0.5.6 — [D-075](s13-decision-register.md#d-075)). The method chunks at up to `chunk_rows::CONCEPTS`, sizing each chunk from the previous one's measured hold ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule); this line said `CHUNK_ROWS` (1,000) from 0.4.5 until 0.12.0, two re-derivations after that constant stopped existing), and sends each chunk as `LowPriCommand::WriteConceptsChunk`, awaiting each responder before sending the next — the await is what yields, and between chunks the actor's biased poll services any pending high-priority command. The job is atomic per chunk, not overall: a failure partway leaves earlier chunks committed, which is the tradeoff [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) documents. `db.write_bulk_atomic(edges)` is the all-or-nothing counterpart on the high-priority tier, one transaction and one stamp, at the cost of one stall.

### 6.3 Reconstruction

`reconstruct(conn, ts, archive_path)` reads on `read_conn`: test whether the hot log covers `ts`; fold it if so; otherwise ATTACH the cold database, fold hot and cold together, and DETACH unconditionally ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots), [D-026](s13-decision-register.md#d-026)). Where a snapshot at or before `ts` exists, the fold is anchored to it and the two merge under last-writer-wins by `seq_id`. The write actor is never involved, so a reconstruction can run concurrently with a full-speed write-back without either slowing the other.

### 6.4 Archive session

The application's idle scheduler calls `db.archive(cutoff)`, which normalizes the cutoff and sends `LowPriCommand::Archive`. The actor ATTACHes the cold file, opens `BEGIN IMMEDIATE`, creates the session marker, ensures the cold schema exists, copies archivable `links` and `transaction_log` rows, verifies counts, re-derives `links_current` via `rebuild_within` ([D-035](s13-decision-register.md#d-035)), records the horizon in `cold.archive_horizon`, drops the marker, commits, and DETACHes on the way out regardless of outcome. Deletion is legal only inside the marker window; a crash anywhere rolls the transaction back, leaving hot and cold mutually consistent. Concepts are never archived ([D-022](s13-decision-register.md#d-022)).

### 6.5 Priority interleaving under bulk write

The flow the 0.4.5 amendment exists for. The user clicks "Assert Edge" while an analytics worker is saving 50,000 results:

1. The UI task sends a `HighPriCommand`; the send completes against the bounded queue in microseconds.
2. The worker's current chunk finishes — 2–3 ms — and commits.
3. The actor loop restarts, and the biased poll sees the high-priority message before the next chunk's send can land; the assertion executes in ~1 ms.
4. The UI receives its response and stays fluid. Observed latency is bounded by one chunk commit, not by the 50,000-row job.
5. The loop returns to the low-priority queue and takes the next chunk; the write-back finishes in about the wall-clock time it would have taken alone, interleaved across ~50 actor iterations.

```
writer actor    | chunk k |.| assert edge |.| chunk k+1 |.| chunk k+2 | ...
UI thread       --click--> send ----------> <-response-> render
analytics       ---await rx_k--> send k+1 ---await rx_{k+1}--> ...
```

The invariant, stated precisely: a high-priority command sent at any instant commits before any low-priority chunk accepted after that instant. The chunk already in flight is the irreducible cost — one transaction's worth of lock time — and [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) is the rule that keeps it one transaction's worth. The one operation that has no chunk boundaries is the archive ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)), and [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) is where a caller is told what that costs them.

**The four bulk paths are one function (0.6.0, [D-086](s13-decision-register.md#d-086)).** `bulk_import`, `write_concepts`, `write_analytics_annotations` and `upsert_embeddings` had a chunk loop each — four copies of *split, send, await, sum, stop on the first error* differing only in the constant and the command they built. `Database::low_chunked` is that loop once, taking the chunks and a closure that names the command. It is not a tidying: four copies of a loop that must yield between chunks are four places for the yield to be lost, and a lost yield is a latency regression no test asserts on because every chunk still commits.

**And in 0.12.0 that deduplication paid for itself a second time.** The loop now takes the *batch* and a ceiling rather than pre-split chunks, and decides each chunk's size from the previous one's measured hold ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)). That is a control law with a floor, two clamps and a dead band; written four times it would have been four chances to get the clamps wrong, in code whose failure mode is a latency miss rather than a wrong answer. The four callers are unchanged except that each now passes its `chunk_rows` constant as an argument instead of slicing with it.

**`archive_windowed` is the same argument applied to the archive (0.6.0, [D-080](s13-decision-register.md#d-080)).** `archive(cutoff)` is one session and one unbounded hold; `archive_windowed(cutoff, window)` walks the same range in bounded sessions, each its own transaction, so the actor returns to its `select!` between them. It refuses rather than clamps: a window that never advances, or one implying more than `MAX_ARCHIVE_SESSIONS` (4,096), is [`DbError::ArchiveWindow`](s6-s10-flows-to-dependencies.md#7-errors) — rounding a narrow window up would archive over boundaries the caller did not choose, and the caller cannot see it happen.

## §7 Errors

**Reproduced from `src/error.rs`, doc comments elided; that file is the authority.**
This block was hand-maintained through 0.6.0 and had fallen **eleven variants behind** —
`InvalidModelName`, `ModelNotRegistered`, `NegativeEdgeWeight`, `AttributeModeUnstated`,
`DiagnosticConn`, `ArchiveWindow`, `InvalidTimestamp`, `InvalidId`, `OverlappingInterval`,
`RebuildInterrupted` and `WriterStopped` were all missing — while also naming
`SingleOpenViolation`'s fields `source` / `target`, which the code cannot use because
`source` is reserved by `thiserror`. A copy that drifts is worse than a pointer, so
`tests/doc_sync_tests.rs` now fails the build when the variant set here stops matching
the enum.

```rust
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("engine: {0}")]
    Engine(#[from] libsql::Error),

    #[error("migration to v{to} failed: {reason}")]
    Migration { to: u32, reason: String },

    #[error("invalid edge type {0} (must match [A-Z0-9]+)")]
    InvalidEdgeType(String),

    // NOTE: the spec (§7) names these fields `source` / `target`. `source` is a
    // reserved field name for thiserror (it is inferred as the error source and
    // requires `std::error::Error`), so the schema column names are used instead.
    #[error(
        "{source_id} -> {target_id} ({edge_type}) already has an open interval; retire it first"
    )]
    SingleOpenViolation {
        source_id: String,
        target_id: String,
        edge_type: String,
    },

    #[error("node {0} not found")]
    NotFound(String),

    #[error("embedding dim {got}, expected {expected} for model {model}")]
    DimMismatch {
        got: usize,
        expected: usize,
        model: String,
    },

    #[error("invalid embedding model name {0:?}: expected [a-z][a-z0-9_]* up to 48 characters")]
    InvalidModelName(String),

    #[error("embedding model {model} is not registered (no {table} table)")]
    ModelNotRegistered { model: String, table: String },

    #[error(
        "invalid branch id {0:?}: expected 1-128 characters, \
         no control characters, no leading or trailing whitespace"
    )]
    InvalidBranchId(String),

    #[error("branch {0} is not registered")]
    UnknownBranch(String),

    #[error("branch {0} already exists")]
    BranchExists(String),

    // Three conditions, one answer: the lineage stays. An unregistered name is
    // `UnknownBranch` above instead, so a typo reads the same on every surface
    // (0.14.13, D-230).
    #[error("branch {branch} cannot be archived: {reason}")]
    BranchNotArchivable { branch: String, reason: String },

    // A rehydrate that needs a lineage `archive_branch` forgot: the concept
    // carries the `branch_id` it was minted on and the row it references left
    // with the lineage. Refused before the insert, because after it the
    // foreign key answers first — as an engine fault naming the table being
    // written rather than the one that is missing (0.15.11, D-253).
    #[error(
        "concept {concept} was minted on branch {branch}, which the ledger has \
         forgotten; re-register the lineage before rehydrating the concept"
    )]
    BranchArchived { branch: String, concept: String },

    #[error(
        "branch {branch} would fork from {parent} at {forked_at}, \
         before {parent} itself was cut at {parent_forked_at}"
    )]
    ForkPrecedesParent {
        branch: String,
        parent: String,
        forked_at: String,
        parent_forked_at: String,
    },

    #[error(
        "concept {id} belongs to lineage {held_by} and {attempted} may not          restate it; a branch inherits concepts"
    )]
    CrossLineage {
        id: String,
        held_by: String,
        attempted: String,
    },

    #[error("view of branch {view} was handed a write naming {named}")]
    BranchMismatch {
        view: String,
        named: String,
    },

    #[error("subgraph exceeds budget ({n} > {budget})")]
    SubgraphTooLarge { n: usize, budget: usize },

    #[error("edge {source_id} -> {target_id} has weight {weight}, which shortest-path analytics cannot use")]
    NegativeEdgeWeight {
        source_id: String,
        target_id: String,
        weight: f64,
    },

    #[error("replay corrupt at seq {seq}: {reason}")]
    ReplayCorrupt { seq: i64, reason: String },

    #[error("snapshot {path} is not readable by this build: {reason}")]
    SnapshotIncompatible { path: String, reason: String },

    #[error("snapshot {path} is damaged: {reason}")]
    SnapshotCorrupt { path: String, reason: String },

    #[error("snapshot {path} could not be saved: {reason}")]
    SnapshotWriteFailed { path: String, reason: String },

    #[error("payload v{got} unsupported (max {max})")]
    PayloadVersion { got: u8, max: u8 },

    #[error("physical delete blocked outside archive session ({table})")]
    ArchiveViolation { table: String },

    #[error(
        "the archive-session marker table {marker:?} is present as committed \
         state. While it exists, the delete guards on concepts, links and \
         transaction_log are disarmed and concept inserts write no \
         transaction_log row. An archive session creates and drops this table \
         inside one transaction, so it should never be visible here — \
         something wrote it outside the write actor. Drop it (DROP TABLE \
         {marker}) and audit for deletions and missing log rows since it \
         appeared"
    )]
    ArchiveSessionLeaked { marker: String },

    #[error(
        "traversal {instants} did not state an attribute mode: that topology \
         would be returned with attributes as they are *now*. Call \
         .attribute_mode(AttributeMode::AtTime) for attributes as believed at \
         the stated instant, or .attribute_mode(AttributeMode::Current) to \
         confirm live attributes are intended"
    )]
    AttributeModeUnstated { instants: StatedInstants },
    #[error(
        "transaction-time instant {ts} cannot be answered from the hot log: rows \
         have been archived out of it and this read has no archive path. Use \
         macrame::temporal::reconstruct(conn, ts, archive_path, snapshots_dir), \
         which does"
    )]
    RecordedInstantUnreachable { ts: String },
    #[error(
        "a half-life was given without a valid-time instant to measure age \
         from: decay ranks a hit by how old what it matched is, and \"old\" is \
         relative to the instant the search reads at. State it — \
         `as_of_valid(t)` on the same search — or drop the half-life"
    )]
    HalfLifeWithoutInstant,
    #[error("cannot open {path} read-only for diagnostics: {reason}")]
    DiagnosticConn { path: String, reason: String },
    #[error("archive window {window:?} is unusable: {reason}")]
    ArchiveWindow {
        window: std::time::Duration,
        reason: String,
    },

    #[error("timestamp {value:?} is not canonical: {reason}")]
    InvalidTimestamp { value: String, reason: String },

    #[error("invalid identifier {id:?}: {reason}")]
    InvalidId { id: String, reason: String },

    #[error(
        "edge {} -> {} ({}): the asserted [{}, {}) overlaps [{}, {}), which {}",
        .overlap.source_id, .overlap.target_id, .overlap.edge_type,
        .overlap.existing_from, .overlap.existing_to,
        .overlap.valid_from, .overlap.valid_to
    )]
    OverlappingInterval { overlap: Box<Overlap> },

    #[error("links_current drift detected: {n} intervals diverge")]
    CurrentDrift { n: usize },

    #[error("rebuild verification failed: {n} intervals still diverge")]
    RebuildFailed { n: usize },
    #[error("chunked rebuild abandoned: {reason}")]
    RebuildInterrupted { reason: String },

    // -- 0.4.5: writer-actor containment --
    #[error("write actor is not running (reopen the Database)")]
    WriterUnavailable,

    #[error("write actor dropped the response channel mid-request")]
    WriterDroppedResponder,

    #[error("write actor did not shut down cleanly: {0}")]
    WriterStopped(String),

    // -- 0.5.0: concept integrity --
    #[error("recorded_at must advance on concept update (got {got}, had {had})")]
    RecordedAtRegression { got: String, had: String },

    // -- 0.13.5: the clock floor --
    #[error("the newest recorded_at in this database is {stamp}, past the limit {limit} …")]
    FutureRecordedAt { stamp: String, limit: String },

    // -- 0.13.8: the caller's own stop --
    #[error("the bulk write was cancelled between chunks")]
    BulkCancelled,
}
```

**The four chunked paths do not return this enum bare (0.13.8, W7.6, [D-181](s13-decision-register.md#d-181)).** `bulk_import`, `write_concepts`, `upsert_embeddings` and `write_analytics_annotations` are atomic per chunk and not overall, so a failure partway leaves a committed prefix — and until 0.13.8 they said only *that* it failed. The count existed inside the chunk loop, was used to size the next chunk, and was discarded at the `?`. They now return `Result<usize, BulkInterrupted>`, where `BulkInterrupted` is that same `DbError` plus the `written` count; `From<BulkInterrupted> for DbError` keeps `?` working in a function returning `Result<T>`, at the cost of the count, which is the caller's decision to take rather than the crate's to make for them. `BulkCancelled` is the one variant a caller produces deliberately — a `CancelToken` read at a chunk boundary — and it is a stop, not a fault: nothing rolls back, and `written` is where the import got to.

The error philosophy is threefold. Nothing panics across the API boundary — every public method returns Result, and internal invariant breaches are debugassert!-only. Trigger-raised aborts are parsed at the connection layer into their typed variants, so a caller catching SingleOpenViolation never string-matches a SQLite message. And errors that describe data carry the coordinates of that data — ReplayCorrupt names its seq_id, CurrentDrift names its count — because an error a maintainer cannot act on is decoration.

The 0.4.5 variants encode one policy: the failure of the writer task must be containable. An in-flight oneshot whose actor has panicked resolves to WriterDroppedResponder rather than a panic of its own; every subsequent operation resolves to WriterUnavailable. The application learns precisely what happened and what to do — reopen — and the cascade stops at the crate boundary. The actor's death itself is reported through tracing with the underlying cause, so the crash report exists even when the user-facing error is deliberately terse.

The 0.5.0 RecordedAtRegression variant surfaces the concept monotonicity trigger ([§4.3](s4-schema.md#43-the-transaction-log)) as a typed error rather than a raw engine abort, carrying both the rejected and existing timestamps so the caller can diagnose the clock or code path at fault.

**`FutureRecordedAt` is refused at open, and it is the only variant that refuses the whole database rather than an operation (0.13.5, W7.4, [D-178](s13-decision-register.md#d-178)).** The clock floors itself at `MAX(recorded_at)` so stamps stay strictly increasing across restarts. That makes one row from the future — a skewed host, a bad import, a fixture that escaped — this process's floor, and every stamp it issues lands at or after it; those rows are written, so the next open reads the same floor back. **The damage is permanent and it spreads**, which is why the refusal is proportionate and why it is placed at open: that is the last point where the crate can still tell a stamp it wrote from one it did not. A *corrupt* stamp is still a `warn!` and no floor ([D-027](s13-decision-register.md#d-027)) — an unparseable value cannot be inherited, so it cannot spread. `Tuning::default().future_stamps(FutureStampPolicy::Allow)` opens the file to be read; it does not repair it, and every write made under it inherits the floor.


**The 0.5.6 and 0.6.0 variants exist to name the right subject, and that is a policy rather than a habit ([D-069](s13-decision-register.md#d-069)).** An error that names the wrong thing sends a caller to fix the wrong thing, so each of these was split out of a variant that already covered the case badly. `InvalidTimestamp` and `InvalidId` were `ReplayCorrupt { seq: 0 }` and `NotFound` — the first claiming the ledger was damaged when the caller's input was malformed, carrying a sequence number that cannot exist because `AUTOINCREMENT` starts at 1; the second telling a caller the thing is missing and inviting them to create it with the same id, which would be refused again. `DiagnosticConn` is its own variant because a file is not a node ([D-091](s13-decision-register.md#d-091)). `RebuildInterrupted` is distinct from `RebuildFailed` because *the repair did not run* is not *the repair did not repair*: `links_current` is untouched, whatever was true of it before is still true, and the action is to retry ([D-082](s13-decision-register.md#d-082)). `AttributeModeUnstated` was a `tracing::warn!` until 0.6.0, which is invisible in any application that has not configured a subscriber — it is now a value the caller cannot miss ([D-085](s13-decision-register.md#d-085)).

**Four variants, four subjects, and the last two arrived a wave apart ([D-185](s13-decision-register.md#d-185), [D-240](s13-decision-register.md#d-240)).** `SnapshotIncompatible` says *another build wrote this*, which is ordinary after an upgrade. `ReplayCorrupt` says **the ledger is damaged**, which is the worst thing this library can report about itself. `SnapshotCorrupt` says the *cache* is damaged and the ledger is not — deleting the file restores correctness and costs a slower reconstruction, because [Doctrine VI](s0-s3-foundations.md#doctrine-vi) makes a snapshot derivative and disposable. Until v3 there was no third name, so every failure of `load_snapshot` — a bad read, a failed decompression, a refused deserialization — came back as `ReplayCorrupt { seq: 0 }`: the wrong subject, carrying a sequence number that cannot exist because `AUTOINCREMENT` starts at 1. That is the same placeholder the paragraph above records [D-069](s13-decision-register.md#d-069) removing from `InvalidTimestamp`, left in the snapshot path because nothing had cause to look at it. Nothing in normal operation raises the new variant to a caller: a damaged snapshot is skipped by the scan and the fold runs from the log.

**The fourth subject is the same repair applied to the other direction, ten releases later (0.14.23, [D-240](s13-decision-register.md#d-240), §14.1 C-2).** `SnapshotWriteFailed` says *the cache could not be written* — a full disk, a read-only directory, a rename that lost its race — and until it existed every failure inside `save_snapshot` came back as `ReplayCorrupt`, the same wrong subject the paragraph above records being removed from the *read* path. It is the one member of the family where **nothing is damaged and nothing is lost**: the previous anchor still stands, the fold is still correct, and the entire cost is a slower start. `reason` names the step that failed, which includes the directory flush — [D-186](s13-decision-register.md#d-186) placed that failure in the same class as the file's own `sync_all`, and this variant keeps it there.

**`AttributeModeUnstated` names the axis, and until 0.13.10 it named a method instead ([D-183](s13-decision-register.md#d-183)).** The variant carried `as_of: String` and rendered it as `as_of(…)` — the method [D-174](s13-decision-register.md#d-174) removed in 0.13.2 when it split the axes. Both instants reached that field through an `.or()`, so a caller who set `as_of_recorded` was told about `as_of`, a caller who set both was told about one of them, and neither learned which clock they had asked about. It now carries `StatedInstants` — `Valid`, `Recorded` or `Both`, and never neither, because this error exists *because* an instant was set — rendered as the calls that produce them. The remedy it offers is a keyword on the same call, so naming the keyword is the whole of its job.

**`HalfLifeWithoutInstant` is the fourth category's other shape: a knob that needs a companion (0.13.20, W9.5, [D-193](s13-decision-register.md#d-193)).** It is `AttributeModeUnstated`'s sibling and sits under `ValidationError` for the same reason — nothing is wrong with the ledger, the call is short one parameter. Decay ranks a hit by the age of what it matched, and *age* is relative to an instant; no read path in this crate reads a wall clock, which is what lets the suite pin these answers under a `FakeClock`. Defaulting to *now* would make every decayed search quietly a search about the present, which is F-35's shape. It carries no attributes, because there is nothing to carry: the remedy is a keyword on the same call and the message names it.

**`RecordedInstantUnreachable` names a question the surface cannot reach, which is a fourth category (0.13.2, W7.1, [D-174](s13-decision-register.md#d-174)).** `TraversalBuilder::as_of_recorded` folds `transaction_log`, and `archive` removes superseded rows from it; a traversal takes a `Connection` and not an archive path, so once anything has been archived it cannot go and get what was moved. The variant exists rather than a silent partial fold because a fold missing its superseded rows returns *nearly* the right topology — the failure mode a ledger can least afford and the one a non-empty-result assertion will not catch. It says in its message which operation can. **Through 0.15.3 it was conservative by one bit and that bit was the wrong one** ([D-246](s13-decision-register.md#d-246), review C-2): the test was whether anything had *ever* been archived, not whether this instant survived it, so one archive session took the whole surface away for the ledger's whole history — `as_of_recorded(now)` included, which the archive is guaranteed to answer. The justification on record was that the cutoff is not recorded hot-side ([D-132](s13-decision-register.md#d-132) refused that marker outright rather than deferring it), which is true and does not imply the conclusion: the cutoff is not needed, because `LOG_ARCHIVABLE` requires a later row at the same entity and so the newest row per entity never leaves. An instant at or after the newest surviving stamp is answered entirely from rows that are still hot. Below it the refusal stands unchanged.

**Two surfaces fold the log, and only one of them was asking (0.13.16, W9.1, [D-189](s13-decision-register.md#d-189)).** `hydrate_attributes` under `AttributeMode::AtTime` with a `recorded` instant folds `transaction_log` for the *text*, exactly as the traversal folds it for the topology, and it read what the archive had left — returning a `Vec` one element shorter, where absent is indistinguishable from retired and from never-existed. That is §3.2 of the review, and it is the same silence in the same mechanism as the paragraph above: the guard existed, and the second reader did not have it. It does now, and the message's *a traversal has no archive path* became *this read has no archive path* because the variant is no longer about traversals.

**One variant, two guards, and the message says which ([D-180](s13-decision-register.md#d-180), 0.13.7).** `reject_overlapping_interval` compares an assertion against committed rows; `reject_overlaps_within` compares a batch against itself before the transaction opens. Only the first is talking about the database, and the message said "already holds" for both — so a caller whose 20,000-row import was refused went looking for a row that had never been written and never would be, since the batch is refused whole. `Overlap::within_batch` carries the distinction and `Overlap::provenance` renders it, closing the message with either *is already recorded* or *this same batch also asserts*. It crosses to Python as a `within_batch` attribute.

**`OverlappingInterval` is boxed, and it is the only variant that is ([D-075](s13-decision-register.md#d-075)).** Seven `String`s is 168 bytes, which put `DbError` — and therefore every `Result` in the crate, on the `Ok` path too — over `clippy::result_large_err`'s threshold the moment [D-060](s13-decision-register.md#d-060) added it. Boxing the rarest variant keeps the whole error small rather than trimming what a caller is told, and `matches!(err, OverlappingInterval { .. })` is unaffected. A unit test pins the size at 128 bytes, because the failure mode is a warning in a build log rather than a broken test.

**Guard aborts are classified in exactly one place.** SQLite reports a `RAISE(ABORT)` as a generic constraint failure carrying the message, so the message is the only thing separating "you violated the single-open-interval rule" from "the disk is full". `error::abort_kind` matches on it, once, against the `schema::ddl` constants spliced into the triggers themselves, so guard and classifier cannot drift. Scattered across call sites, an upstream wording change would silently degrade an unknown number of typed errors into opaque ones.

**A guard vocabulary that covers every guard is not one that covers every failure (0.13.3, W7.2, [D-176](s13-decision-register.md#d-176)).** `write_annotations_atomic` returned its engine error raw, and the reason looked sound: `analytics_annotations` carries no triggers — which is why it is the cheapest bulk table and has the largest chunk ceiling ([D-058](s13-decision-register.md#d-058)) — so `abort_kind` can only answer `NotAGuard` and `classify` would hand back what it was given. Correct about guards, silent about the **foreign key** onto `concepts`, which the engine enforces itself. So the one failure a caller can cause here — an annotation naming a concept that is not there — arrived as `FOREIGN KEY constraint failed` out of a rolled-back chunk of up to `chunk_rows::ANNOTATIONS` rows, naming none of them. It is now `NotFound(concept_id)`, reached through `classify` with `WriteOp::Annotation` like every other write.

The detector matches the **extended result code**, not the message: libSQL reports statement failures through `SqliteFailure(extended_error_code(…), …)`, so a foreign key has a number of its own and this classification cannot be broken by a rewording. Extended and not primary, because `SQLITE_CONSTRAINT` (19) also covers the canonical-timestamp CHECK on the same table, and reporting a malformed `computed_at` as a missing concept would be a wrong answer rather than an opaque one. The one-place-for-text doctrine above is unweakened — this is not a second text matcher.

## §8 Testing Strategy

**Conjunction predicates require asymmetric fixtures (0.9.0, [D-128](s13-decision-register.md#d-128), [D-130](s13-decision-register.md#d-130)).** When a test exercises a conjunction — archivability, overlap refusal, delete-guard gating — the natural fixture satisfies every clause at once and cannot show which one is doing the work. Each clause must be defeatable independently, which means at least one fixture where **that clause fails while the others hold**. The archivability predicate required a target-only concept to defeat the reachability clause; the same pattern will recur for any predicate over the link graph's two directions. This is [D-088](s13-decision-register.md#d-088)'s lesson at the fixture level rather than the measurement level, and the same class as [D-030](s13-decision-register.md#d-030)'s always-zero audit: a check whose failure mode is *always says fine* cannot be validated by examples that are fine.

Testing is layered by what each layer can prove. Unit tests cover the pure machinery: CTE builder output against golden strings, interval overlap arithmetic, RRF fusion, the embedding codec's roundtrips and dimension rejection, and the byte-budget planner's strategy choices against synthetic statistics. Integration tests run the full API against real database files in temp directories, including WAL crash recovery — transactions dropped without commit must leave the file consistent on reopen. Property tests are the acceptance gates for the invariants themselves: random assertion/retirement streams must never produce overlapping open intervals, links_current must equal the latest-belief projection of links row for row after every stream, and reconstruct(now) must equal live-table reads for every entity. Fuzz tests attack the temporal machinery specifically: replay at every recorded_at in a random stream must equal an independent log-fold oracle, and retroactive assertions must respect the documented fidelity boundary between as_of_valid and as_of_recorded/reconstruct. Scenario tests pin the human-facing contracts, above all the Monday/Wednesday/Friday attribute-fidelity case across all three AttributeMode values, and the corrupt-then-rebuild roundtrip: damage links_current through a raw connection, call rebuild_current(), and require audit_current() == 0. Benchmark gates run under criterion in CI against the [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budgets, including the cold-page-cache variants, so a performance regression is a failing build rather than a discovered incident.

0.4.5 adds a concurrency layer to tests/concurrency_tests.rs, and one of its tests joins the two original pinning tests as a gate on the amendment itself:

- **Priority interleaving** — drive a 50K-row chunked write-back under FakeClock while firing assert_edge at randomized points, and require that every UI assertion commits before any chunk accepted after it. The invariant is stated as an ordering property over committed seq_ids, not a wall-clock timing measurement, so it is deterministic.
- **Prefix visibility** — call reconstruct() mid-write-back and require exactly a chunk-prefix of the annotations, pinning [D-014](s13-decision-register.md#d-014)'s fidelity boundary against silent drift in either direction.
- **Containment** — force a handler panic through a test-only command and require WriterDroppedResponder on the in-flight request and WriterUnavailable on every subsequent one, with the test process very much alive.
- **Shutdown** — call close() during a write-back and require every committed chunk durable, the final snapshot present and anchored, and the actor task joined.

**0.5.0 adds:**

- **Concept monotonicity** — issue a concept `UPDATE` with `recorded_at <= OLD.recorded_at` and require `RecordedAtRegression`; issue with a strictly advancing stamp and require success. Pin under `FakeClock` with controlled instants.
- **Read-connection write rejection** — attempt an `INSERT` through `read_conn` and require an engine error (`PRAGMA query_only = ON`), confirming the structural read-guard survives a routing mistake in Rust.

**0.5.1 adds:**

- **Assert → Retire → Re-assert (same `valid_from`)** — assert an edge at `valid_from = T0`, retire it, then re-assert the same interval with a newer `recorded_at`. Verify: `links` holds exactly three rows for the key; `links_current` holds one, open, at the newest stamp; `reconstruct` at the first stamp returns the closed interval and at the second the open one. This pins the `valid_from <> NEW.valid_from` predicate in `trg_links_single_open` and the upsert logic in `trg_links_current_sync` against regression.
- **Clock monotonicity across restart** — construct a SystemClock, issue now(), simulate a backward NTP correction (mock the wall clock), construct a new SystemClock against the same database, and require that the new now() is strictly greater than the last recorded_at in the database. Pin the max(wall_clock, last_db_ts + 1μs) floor.
- **seq_id gap tolerance** — **implemented in 0.5.4 ([D-049](s13-decision-register.md#d-049)), and by a different mechanism than this described.** The 0.5.1 recipe here was to roll a write back and expect the consumed sequence number to be lost; measured, it is not — `sqlite_sequence` is transactional and rolls back too. The hole is instead punched the way the archive punches one, by deleting a log row, and the test asserts that the anchored fold returns the entries on both sides of it. Pin the inequality-comparison requirement against a future maintainer who might write seq_id = :anchor + 1.

**0.5.2 adds:**

- **Cold-DB reconstruction roundtrip** — populate the hot log, run archive() to move history before the horizon into the cold file, then call reconstruct(ts) for a ts older than the horizon and require the result to equal an independent oracle fold over the unarchived data. Pin the ATTACH/UNION ALL/DETACH path ([D-026](s13-decision-register.md#d-026)), including the hot-entry-wins resolution for entities present in both files.
- **Cold-DB absence** — delete the archive file, call reconstruct(ts) for a pre-horizon ts, and require ReplayCorrupt with the "archive database not found" reason rather than a panic or a silently-wrong state. **Narrowed in 0.8.0 ([D-121](s13-decision-register.md#d-121)): the fixture must have actually archived something.** As written, this recipe was satisfied by a database that had never been archived at all, and that case is no longer an error — it is *before recorded history*, and the empty state is the answer. `a_missing_archive_is_an_error_when_rows_were_actually_archived` supersedes the old test, which had been pinning the defect rather than the guarantee.
- **Clock parse fallback** — write a corrupt recorded_at (e.g. "not-a-timestamp") directly into concepts via a raw connection, construct SystemClock::new(), and require it to return a working clock floored to the wall clock (no panic), with subsequent now() calls strictly increasing.

**0.5.4 adds**, and the pattern across them is that several were written, mutated, found to assert nothing, and rewritten — recorded because a gate that passes against the defect it guards is worse than no gate:

- **Model-based property suites** for [Doctrine VI](s0-s3-foundations.md#doctrine-vi) (`integrity_property_tests.rs`) and for the doctrine as a whole (`doctrine_property_tests.rs`), driven only through the public API. The first found a live bitemporal defect in `archive()` ([D-035](s13-decision-register.md#d-035)).
- **Snapshot composition equals folding from genesis**, over generated histories at every instant in the delta ([D-049](s13-decision-register.md#d-049)). Passed under mutation on its first run because the generator had no operation producing a tombstone; `Op::RetireConcept` was added and it then failed, shrinking to a two-operation history.
- **`seq_id` gap tolerance** — see the 0.5.1 entry above and the correction it carries.
- **The traversal plan shape** — `EXPLAIN QUERY PLAN` must report `COVERING INDEX … (source_id=? AND valid_from<?)` for both the filtered and unfiltered traversal. Asserting `COVERING` alone passes under a wrong column order; the seek text is what distinguishes them ([D-042](s13-decision-register.md#d-042)).
- **Loader growth rate** — an 8× input must not cost more than 16× the time, a ratio and not a duration, sized against a measured 21.3× for the quadratic form and 8.0× for the linear one ([D-047](s13-decision-register.md#d-047)).
- **Snapshot container versioning** — a bumped schema version with a byte-identical payload must be refused, so only the header can be doing the rejecting ([D-043](s13-decision-register.md#d-043)).
- **A leaked `cold` attachment does not poison the connection** ([D-044](s13-decision-register.md#d-044)), and **analytics annotations never reach `transaction_log`** ([D-041](s13-decision-register.md#d-041)).
- **The vector write path through the handle alone** — the assertion is what it *uses*, not what it checks: it touches `Database` and nothing else on the write side, so a return to caller-supplied connections stops it compiling ([D-048](s13-decision-register.md#d-048)).

**0.13.14 gives the word "fuzz" a referent, which it had not had (W8.4, [D-187](s13-decision-register.md#d-187)).** The paragraph above says *"Fuzz tests attack the temporal machinery specifically"* and then describes a differential oracle over generated histories. That is a **property** test, it is a good one, and it is `doctrine_property_tests` — but nothing in this repository generated *unstructured* input for anything, and the section had used the word since 0.4.0 for a layer that did not exist. The distinction is not pedantry: a property test explores the space its generator was written to explore, so it finds defects in the shapes someone thought of, and a fuzzer's entire value is the shapes nobody did.

`fuzz/` holds three `cargo-fuzz` targets over the snapshot container, one per layer of [§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)'s reader: the whole file, the plaintext behind a valid checksum, and an arbitrary frame under an arbitrary declared length. They run in CI on ubuntu, time-boxed, under a deliberately low `-malloc_limit_mb` — because *never an allocation storm* is the clause a fuzzer can assert and a fixed set of cases cannot.

**They run nowhere else, and that is the reason the deterministic half exists.** `cargo-fuzz` needs nightly and libFuzzer and does not support Windows, so on the machine this crate is developed on the fuzz targets are unrunnable — a gate that lives only in CI is a gate nobody sees fail until after they have pushed. The unit tests in `src/temporal/snapshot.rs` therefore assert the same property exhaustively rather than randomly: **every** single-bit flip of a real snapshot is refused, **every** truncation and extension is refused, every mutation of the plaintext behind a correct checksum is answered rather than survived, and a decompression bomb with a valid checksum stops at the declared length. Exhaustive is available here precisely because the fixture is small, and where it is available it is worth more than sampling: `every_single_bit_flip_in_a_snapshot_is_refused` names the bit and the byte when it fails.

**0.8.0 changes how the suite is *read*, not what it contains** ([D-110](s13-decision-register.md#d-110)). Both Rust CI steps ran under `for attempt in 1 2 3`, which counts failures without classifying them — and under [R15](s11-s12-milestones-and-risks.md#r15) the two things worth telling apart print almost identically. `scripts/run_rust_suite.py` now classifies every run as `CRASH`, `FAILED`, `INCOMPLETE`, `TEARDOWN` or `BUILD` and retries only `CRASH`, so a genuine failure is reported on attempt 1 with the test named instead of after three full suite runs with nothing named. It keys on the **presence of each target's `test result:` line**, which is what `.cargo/config.toml` instructs anything gating on this suite to do and what a pass-count sum cannot see. The Rust gate now has the same shape as the Python one ([D-107](s13-decision-register.md#d-107)) without sharing its implementation, because cargo runs one process per target and pytest runs one process for everything.

**0.8.0 also widens where the suite runs** ([D-112](s13-decision-register.md#d-112)). `ci.yml`'s matrix was `[ubuntu-latest, windows-latest]` while the README promised three platforms, and the only macOS evidence this project held arrived through `python.yml` — the binding's CI standing in for the crate's. `macos-latest` joins the Rust matrix, which also puts a number on something never measured: the [R15](s11-s12-milestones-and-risks.md#r15) rate on Apple silicon. Every figure in that row is from one Windows machine, and the fault not having been *seen* elsewhere is not the same as it being absent.

**0.8.0 makes one class of document executable** ([D-113](s13-decision-register.md#d-113)). `doc_sync_tests` already pinned §7's error enum and [Appendix A](appendices.md#appendix-a--public-api-normative)'s method list; it now also reads [§13](s13-decision-register.md#13-decision-register) as data. A decision that says it is waiting for a named release must, once that release has shipped, close it out by name — `DELIVERED in 0.8.0`, or `RESCHEDULED from 0.7.0`. [D-087](s13-decision-register.md#d-087) and [D-089](s13-decision-register.md#d-089) both said *Scheduled for 0.7.0*, 0.7.0 shipped without either, and nothing went red for a whole release, because a scheduled decision is prose and prose is not executed. The register is the last normative document whose claims about the *future* nothing checked.

**0.13.17 adds the round trip an error is only as good as (W9.2, [D-190](s13-decision-register.md#d-190)).** [D-189](s13-decision-register.md#d-189) chose to refuse a `recorded` hydrate past the archive horizon rather than union the cold log into the fold, and its message names `reconstruct` as the operation that can answer. Nothing checked that it does. `what_the_hot_log_refuses_the_archive_path_still_answers` archives across the horizon, requires the refusal, then requires `reconstruct` with the archive path to return **the value the hot reader returned before the archive ran** — taken as a variable and compared against, never written twice as a literal, because a test that hard-codes the expected title passes when both readers are wrong the same way. Mutation-discriminated: forcing `hot_log_reach` to answer `Covers` unconditionally makes it fail with `left: None`, which is [§3.2](../Macrame%20Codebase%20Review%20v0.12.0.md)'s own shape one layer further down.

**A redirection is a claim about another operation, and this is the general form.** `RecordedInstantUnreachable` is not only a refusal; it is an assertion that `reconstruct(conn, ts, archive_path, snapshots_dir)` answers the question the caller asked. An error that sends a caller somewhere is testable in the place it sends them, and that half is the half nobody writes — which is what the plan means by *the finding most likely to be "fixed" by a change nobody can demonstrate*.

The injectable clock is the keystone of the entire suite. Every temporal test is deterministic because time is a parameter, and the one place nondeterminism is tolerated — the wall-clock stamping inside archive triggers — is tested structurally (session marker lifecycle) rather than temporally. As of 0.4.5, FakeClock gains a Send + Sync interior (a mutex over its instant) so the harness and the actor share one deterministic clock — the keystone extends cleanly to the actor, because time was already a parameter.

## §9 Performance Budgets

**Edges per byte budget (0.8.0, [D-115](s13-decision-register.md#d-115)).** Interning `EdgeRef` cuts the per-edge cost from 342/378/454 bytes to **59/62/67** at 8/26/64-byte ids — **5.8×–6.8×**, measured on the real type by `examples/budget_density_diag.rs`, not derived from `size_of`. **Fixture: `star_of_stars`, `clustered`, `chain` and `dense_small` ([D-088](s13-decision-register.md#d-088))**, on which `estimated_bytes` falls 339,638 → 190,134, 2,168,375 → 432,791, 324,653 → 174,023 and 30,744,680 → 4,386,282. The spread is density: the same diagnostic shows edges are 80% of the budget with empty `content` and 5% at 20 KB of document text, so this is a claim about topology-heavy graphs and B3 is what addresses the other kind.

**Re-measured at 0.8.0 ([D-127](s13-decision-register.md#d-127)), because three of this release's items changed the read path and a table carried over unchanged would have described a different crate.** [D-115](s13-decision-register.md#d-115) changed how a `Subgraph` is represented, [D-116](s13-decision-register.md#d-116) changed what a load carries, and [D-118](s13-decision-register.md#d-118) dropped an index. Against the 0.7.0 figures: three-hop traversal **2.1 → 1.66 ms**, vector top-10 **294 → 246 µs**, hybrid top-10 **2.0 → 1.77 ms**, full fold **21 → 16.9 ms**, snapshot composition **3.4 → 2.18 ms**, single assertion **258 µs** on this fixture, published then with a *"still O(out-degree)"* caveat that [D-134](s13-decision-register.md#d-134) has since retired on measurement. **Two controls make those numbers mean something**, because a uniform gain across unrelated paths is exactly what a faster machine looks like: [D-090](s13-decision-register.md#d-090)'s fixed `control/select_1` reads **1.51–1.62 µs** against the **1.589–1.639 µs** recorded there, and the chunk-commit path that 0.8.0 did not touch is **2.39 → 2.40 ms**. Machine unmoved, untouched path unmoved, read paths 12–36% faster.

All targets are measured on the reference hardware (Windows 11, NVMe SSD, 32 GB RAM, release build) under criterion, with cold-page-cache variants measured after PRAGMA shrinkmemory and OS cache flush. Trigger amplification is included: a single edge assertion produces three writes (the links row, the links_current upsert, the transaction_log entry).

| Operation | Target | Mechanism |
|---|---|---|
| Single edge assertion (incl. trigger writes) | ≤ 5 ms | One BEGIN IMMEDIATE … COMMIT; three table writes. **Flat in out-degree, measured** ([D-134](s13-decision-register.md#d-134)): sub-millisecond into tables of 0 / 2,000 / 8,000 edges, at which the probed hub carries out-degree 0 / 666 / 2,666, so the "not met at high out-degree" caveat this row carried from 0.5.5 to 0.9.0 is retired — it described the pre-v6 access path ([D-059](s13-decision-register.md#d-059)). The cost is O(version count per edge key), which archival caps. **Measured against the budget rather than under it since 0.15.6** ([D-248](s13-decision-register.md#d-248)): `examples/edge_write_probe.rs` reports 0.099 ms on the trunk and 0.106 ms on a forked database, against 0.184 and 0.401 before the actor kept its statements. Both were inside 5 ms and the row was never in danger, which is why nothing had looked: the forked figure was 2.2× the trunk's for a reason that had nothing to do with the write |
| Single edge retirement | ≤ 5 ms | Same shape as assertion |
| Single concept upsert | ≤ 3 ms | One table write + one log entry |
| 3-hop traversal, warm cache (1K edges) | ≤ 10 ms | Recursive CTE over links_current, indexed |
| 3-hop traversal, cold cache (1K edges) | ≤ 50 ms | Same CTE; I/O-bound |
| as_of_valid(ts) traversal (1K edges) | ≤ 15 ms | CTE + two predicate rewrites; no log access |
| AtTime hydration (100 result nodes) | ≤ 30 ms | Window query over idx_txlog_entity, bounded by result set — **and by the reach guard in front of it, which is not** ([D-247](s13-decision-register.md#d-247)). The query is flat at ~0.14 ms from 2,000 to 500,000 log rows; the guard was 0.1 ms to 24 ms across the same range, so the budget was being spent almost entirely on deciding whether to run a read that costs nothing. Since 0.15.5 that holds only *below* the newest surviving stamp; at or after it the guard is one 3.4 µs seek. **Since 0.15.7 it holds nowhere** ([D-249](s13-decision-register.md#d-249), schema v16): below the stamp the guard reads one row of `log_integrity` in 0.033 ms instead of counting the log, so the row's "bounded by result set" is true of the whole operation at every instant and at every log size |
| reconstruct(ts), 10K log entries, no snapshot | ≤ 100 ms | Full fold from genesis |
| reconstruct(ts), 100K log entries, no snapshot | ≤ 500 ms | Full fold from genesis |
| reconstruct(ts), 1M log entries, no snapshot | ≤ 3 s | Full fold; snapshot composition expected at this scale |
| reconstruct(ts), 1M log entries, with snapshot | ≤ 200 ms | Snapshot load + delta fold. **Reachable as of 0.5.4** ([D-049](s13-decision-register.md#d-049)), except across the archive boundary, where composition is disabled and the full-fold row above applies |
| reconstruct(ts), pre-horizon, with cold DB | ≤ 2× hot-fold target | ATTACH + UNION ALL fold over hot + cold ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)) |
| audit_current() (100K edges) | ≤ 200 ms | Window projection vs. links_current comparison |
| rebuild_current() (100K edges) | ≤ 500 ms | Delete + re-insert in one transaction |
| rebuild_current() (1M edges) | ≤ 5 s | Single atomic transaction; run at idle ([D-023](s13-decision-register.md#d-023)) |
| rebuild_current() (10M edges) | ≤ 50 s | Single atomic transaction; run at startup only ([D-023](s13-decision-register.md#d-023)) |
| Vector top-10 search (100K concepts) | ≤ 20 ms | DiskANN index scan |
| Hybrid search, top-10 (100K concepts) | ≤ 50 ms | DiskANN + FTS5 + RRF fusion |
| Chunk commit, 500 rows, trigger-amplified | ≤ 3 ms | [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) golden rule calibration. **Superseded in 0.5.5 ([D-058](s13-decision-register.md#d-058))** — the duration is the budget and the row count is not part of it. See the four rows below |
| Chunk commit, edges, 90 rows | ≤ 3 ms | 2.39 ms **on an empty database**; **9.06 ms into an 8,000-edge table**, measured in 0.10.0 across two sessions (9.08, 9.05) against empty-arm controls of 2.69 and 2.65 ms beside them ([D-136](s13-decision-register.md#d-136)). **This row is missed on a populated database, by ~3×** — where the pre-index 47.7 ms published here until 0.10.0 said 16× — and the residual is **attributed in 0.11.0** to `trg_links_current_sync` — 89% of it secondary-index maintenance on `links_current`, none of it the single-open guard ([D-142](s13-decision-register.md#d-142)). Re-derived against all four [D-088](s13-decision-register.md#d-088) shapes, which agree that **the largest size within the bound is 20**, not 90 — and the constant is deliberately left at 90, because per-row cost grows with the table and 20 would be the same miss at a larger population ([D-143](s13-decision-register.md#d-143)). **In 0.12.0 the row count stopped being fixed at all**: 90 is now the ceiling the adaptive loop starts from, and the size it settles at is whatever meets this budget on the machine and table in front of it, down to a floor of 35 rows. `chunk_rows::EDGES` ([D-058](s13-decision-register.md#d-058)) |
| Chunk commit, concepts, 70 rows | ≤ 3 ms | 2.35 ms measured. `chunk_rows::CONCEPTS` |
| Chunk commit, annotations, 600 rows | ≤ 3 ms | 2.36 ms measured. `chunk_rows::ANNOTATIONS` |
| Chunk commit, embeddings, 30 rows | ≤ 3 ms | 2.06 ms measured. `chunk_rows::EMBEDDINGS` |
| Archive, 100K closed intervals | ≤ 30 s | Single atomic transaction; idle-scheduled ([D-012](s13-decision-register.md#d-012)) |
| Rehydrate, single concept | ≤ 5 ms | ATTACH, one `BEGIN IMMEDIATE`, commit, DETACH — one round trip through the write actor, the same shape as a single edge assertion and budgeted the same. **Fixture: `rehydrate/rehydrate/1`** ([D-132](s13-decision-register.md#d-132)) |
| Rehydrate, per concept beyond the first | ≤ 300 µs | **Not a fresh number**: it is the per-row rate the `Archive, 100K closed intervals` row above already implies (30 s ÷ 100K), applied to the reverse direction on the argument that a move back should not cost more per row than the move out. **Fixture: `rehydrate/rehydrate/{10,100,1000,10000}`**, a sweep rather than a point, because `rehydrate()` is a per-id loop where `archive()` is set-based |
| Snapshot write (100K-edge state) | ≤ 2 s | Read-fold + bincode + zstd; read-side only |

These budgets are CI gates ([§8](s6-s10-flows-to-dependencies.md#8-testing-strategy)): a regression beyond the target is a failing build. **Corrected in 0.5.5 ([D-055](s13-decision-register.md#d-055)): they are now measured and they are deliberately not gates.** `benches/budgets.rs` covers sixteen of the rows above under criterion (twelve as of [D-055](s13-decision-register.md#d-055), plus the four per-path chunk rows [D-058](s13-decision-register.md#d-058) split the single chunk row into), so nothing here is unfalsifiable any more. They are not CI gates because these numbers are stated for *named reference hardware* and CI is not that machine — an absolute `≤ 5 ms` becomes an assertion about whichever runner picked up the job, which is the flaky red this project refuses elsewhere by name. Regression detection compares a machine against itself (`cargo bench -- --save-baseline`/`--baseline`), and where a hardware-independent gate is possible it lives in `tests/` as an assertion about *shape* rather than duration — [D-042](s13-decision-register.md#d-042)'s plan shape and [D-047](s13-decision-register.md#d-047)'s growth ratio remain the model.

**First measurement, at reduced scale (2K concepts, 1–2K edges, 5K log entries), on a developer laptop rather than the reference machine.** Eleven of twelve rows land inside budget with room to spare — three-hop traversal 2.1 ms against 10 ms, `audit_current` 13.8 ms against 200 ms, vector top-10 294 µs against 20 ms, hybrid top-10 2.0 ms against 50 ms, full-fold reconstruction 21 ms against 100 ms, and composition 3.4 ms against it, which is the first direct evidence that [D-049](s13-decision-register.md#d-049)'s snapshot path is worth having.

**One row misses, and chasing it found that the budget is wrong ([D-056](s13-decision-register.md#d-056)).** `Chunk commit, 500 rows, trigger-amplified` measured ≈62 ms against ≤ 3 ms. Half of that was a real defect — `write_edges_atomic` re-prepared `INSERT_LINK` on every row, and `links`'s two triggers are compiled into each preparation — and hoisting the statement took it to **≈37 ms, a 41% saving**. The remainder is trigger amplification: the identical commit with `trg_links_log_insert` and `trg_links_current_sync` dropped takes **2.96 ms**, so the triggers are ~92% of what is left.

**The same defect was in the other three bulk paths, and none of them is a row in this table ([D-057](s13-decision-register.md#d-057)).** `write_concepts_atomic`, `write_annotations_atomic` and `upsert_embedding_chunk` all called `execute` per row; the `bulk_chunks` group now measures each at 500 rows. Prepared once: concepts **34.1 → 11.9 ms** (65%), annotations **4.60 → 2.13 ms** (54%), embeddings **73.4 → 67.4 ms** (8%). Two of those readings are worth carrying forward here. `analytics_annotations` has no triggers, so its 2.13 ms is a control for a bare 500-row upsert — it corroborates the 2.96 ms trigger-free figure above rather than leaving it resting on one scratch-database measurement. And the embedding chunk at ≈135 µs per vector is now the **worst** bulk path in the system by a wide margin, DiskANN maintenance rather than trigger amplification, which means the §5.1.5 question below is sharper for embeddings than for edges. §9 should probably budget a chunk per path instead of once; it is not amended here, because a budget written to match a fresh measurement is the failure [D-055](s13-decision-register.md#d-055) is about.

**And the row itself was malformed, which is what re-deriving §5.1.5 established ([D-058](s13-decision-register.md#d-058)).** `Chunk commit, 500 rows ≤ 3 ms` reads as two requirements and is one: the duration is the budget, the row count is an answer to it, and the answer differs per path because per-row costs span 60×. Sweeping chunk size showed a fixed ~0.8 ms per transaction (BEGIN/COMMIT/fsync — over a quarter of the bound before any row is written) and that two of the four paths are *superlinear* in chunk size, so their old 1,000-row chunks were the worst latency and the worst throughput at once. The table now carries four chunk rows, one per path, all measured inside the bound — and the ≤ 3 ms figure survives the re-derivation unchanged, which is more than the paragraph below expected of it.

That earlier reading now needs qualifying. **2.96 ms is the budget**, to within 1% — so ≤ 3 ms is the cost of the insert *without* the amplification this section's own preamble says is included, and no amount of optimisation reaches it while the ledger entry is still written, which [Doctrine IV](s0-s3-foundations.md#doctrine-iv) requires. The consequence lands on [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) rather than on the code — **though not in the way this paragraph originally said.** It estimated chunks of "about 40 rows" by extrapolating 74 µs per amplified row from the 500-row measurement, and that extrapolation assumed a linearity the sweep disproves: marginal cost at n≈90 is ~22 µs per row, so the answer is 90 and the fully-amplified chunk lands at 2.39 ms. The ≤ 3 ms budget is therefore *reachable*, and what was unreachable was the row count bundled with it. Cold-page-cache variants are measured separately and gated at 5× the warm target. The archive budget is measured but not CI-gated, because it is idle-scheduled and its duration is bounded by the scheduling-layer self-chunking ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)). The rebuild_current() budgets at 1M and 10M edges are measured but not CI-gated, because rebuild is a recovery operation that should not occur in normal operation ([D-023](s13-decision-register.md#d-023)).

**Rehydration measured (0.9.0, C4, [D-132](s13-decision-register.md#d-132)), which is the last of the two reasons [Appendix C](appendices.md) gave for deferring archival.** One session, `control/select_1` reading **1.560–1.571 µs** against [D-090](s13-decision-register.md#d-090)'s recorded 1.589–1.639 µs, so the machine is where it was and these figures may be compared with the ones above.

| n concepts | `rehydrate()` | with `trg_concepts_fts_insert` dropped |
|---|---|---|
| 1 | 3.71 ms | — |
| 10 | 4.42 ms | — |
| 100 | 10.89 ms | — |
| 1,000 | 77.70 ms | 52.61 ms |
| 10,000 | 1.105 s | 515.0 ms |

**The fixed cost is 3.71 ms and the marginal cost is ~74 µs per concept — until it isn't.** Between n=10 and n=1,000 the slope is flat (71.9 then 74.2 µs), and from 1,000 to 10,000 it rises to 114.2 µs: ten times the rows cost **14.2×** the time. That is the superlinearity [D-058](s13-decision-register.md#d-058) found in two of the four bulk-write paths, in a fifth place, and the reason the row above budgets a *rate* rather than a total.

**The cause is named rather than inferred, and the trigger-free column is what names it.** The only trigger still firing on a rehydration insert is `trg_concepts_fts_insert` — the log trigger is marker-gated at v10 ([D-131](s13-decision-register.md#d-131)) and there is nothing else — so dropping it isolates FTS5 index maintenance from the row movement, the same control [D-056](s13-decision-register.md#d-056) used to attribute 92% of the chunk-commit cost to triggers. Without it the path is **linear**: ten times the rows cost 9.79× the time, and the per-concept figure is 48.9 µs at n=1,000 against 49.4 µs at n=10,000. So the row movement scales and FTS5 does not. The index is **32%** of the cost at n=1,000 and **53%** at n=10,000.

**The round trip is asymmetric by 3.8×, and that is a fact about how the two directions are written.** Archiving the same 1,000 detached concepts takes **20.37 ms** against rehydration's 77.70 ms, measured on the identical fixture in the same session. `archive()` pays one `INSERT … SELECT` and one `DELETE` per table however much it moves; `rehydrate()` pays a `SELECT` from cold, a rowid-collision `COUNT`, an `INSERT` and a `DELETE` for each id named. The asymmetry is deliberate — the collision check is per-concept and has nowhere else to live ([D-131](s13-decision-register.md#d-131)) — but it means the two directions must not be reasoned about as though they were one operation reversed.

**Nothing above extrapolates to 100K, deliberately.** The trigger-free path would reach ≈5.1 s and the real path would not, because the real path is the one that departs — and [D-058](s13-decision-register.md#d-058) is this project's own record of what extrapolating across a slope change costs: the "about 40 rows" estimate it corrected was linear arithmetic on a superlinear path, and the true answer was 90. A 100K rehydration wants measuring, not projecting.

**The consequence lands on batching, which is exactly the question [Appendix C](appendices.md) left open after C3.** A 10,000-concept rehydration holds the write lock for 1.1 s, and under [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) every queued write waits that long — a channel wait `busy_timeout` does not bound. That is well inside what the contract already tolerates (`rebuild_current()` at 10M edges is ~50 s), so it is not a defect and no windowing is added here. What the measurement changes is that the question now has a number attached: windowing `rehydrate` the way `archive_windowed` windows the archive would cut the stall roughly in proportion **and** would recover the superlinear part, because FTS5's merge cost is per transaction rather than per row. It is not taken in 0.9.0 because no caller has asked for a rehydration of that size, and a windowed rehydration is not a free change — it gives up the single-transaction atomicity that makes a partial rehydration impossible.

**And the matrix cannot move any of it.** Rehydrating 100 concepts costs 11.38 ms on `star_of_stars`, 11.38 on `clustered`, 11.46 on `chain` and 12.04 on `dense_small` — a 5.8% spread, inside this project's session noise. This is measured rather than argued even though the argument is sound: an archivable concept is one no edge names ([D-128](s13-decision-register.md#d-128)), so the predicate severs the matrix's usual axis before the topology can reach the rows being moved, and all a surrounding graph can still do is make the hot `concepts` table bigger. Asserting it would have been [D-088](s13-decision-register.md#d-088)'s error inverted — a figure from one shape presented as a property of the operation — and the whole point of the matrix is that this project has made that mistake before.

## §10 Dependencies

| Crate | License | Role |
|---|---|---|
| libsql | MIT | Engine binding; WAL, F32\_BLOB, DiskANN, JSON1, window functions |
| tokio | MIT | Async runtime; actor task, channels |
| serde / serde_json | MIT / Apache-2.0 | Payload serialization |
| bincode | MIT | Snapshot serialization |
| zstd | BSD-3 | Snapshot compression |
| thiserror | MIT / Apache-2.0 | Error derive |
| tracing | MIT | Structured diagnostics |
| ulid | MIT / Apache-2.0 | Entity ID generation |

No GPL-licensed component appears in the dependency tree. The libSQL engine is used unmodified as a compiled dependency; no C source is vendored or patched ([Doctrine I](s0-s3-foundations.md#doctrine-i)). Timestamp parsing is implemented in-crate (~20 lines, [§5.1.2](s5-modules.md#512-handle-shape-and-the-clock-contract)); no chrono/time dependency is introduced for that single call site.

