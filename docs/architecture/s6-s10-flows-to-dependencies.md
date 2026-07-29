<!--nav-->
← [previous](s5-modules.md) · [index](README.md) · [next](s11-s12-milestones-and-risks.md) →
<!--/nav-->

## §6 Data Flows

### 6.1 Edge assertion

The application builds an `EdgeAssertion` and calls `db.assert_edge(edge)`. The value is normalized at the boundary — edge type validated against `[A-Z0-9]+`, timestamps widened to the canonical 27-character form ([D-029](s13-decision-register.md#d-029)) — so a malformed edge type or a second-precision timestamp is a typed error at the call site rather than an engine CHECK failure surfacing from the far side of an actor with no context attached ([D-034](s13-decision-register.md#d-034)). The normalized value crosses the high-priority channel as `HighPriCommand::AssertEdge`; the caller awaits its `oneshot`. The actor stamps `recorded_at` from the injected clock, opens `BEGIN IMMEDIATE`, inserts into `links`, and commits. Inside that transaction `trg_links_current_sync` upserts current belief and `trg_links_log_i` appends the log entry. The responder carries `Ok(())` or a typed `DbError` classified through the single boundary of [D-033](s13-decision-register.md#d-033).

### 6.2 Bulk analytics write-back

The application loads a subgraph, runs Louvain in memory ([§5.4](s5-modules.md#54-graphsubgraphrs-and-graphalgorithmsrs--native-in-memory-analytics)), and calls `db.write_annotations(concepts)`. The method chunks at `CHUNK_ROWS` (1,000) and sends each chunk as `LowPriCommand::WriteAnnotationsChunk`, awaiting each responder before sending the next — the await is what yields, and between chunks the actor's biased poll services any pending high-priority command. The job is atomic per chunk, not overall: a failure partway leaves earlier chunks committed, which is the tradeoff [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) documents. `db.write_bulk_atomic(edges)` is the all-or-nothing counterpart on the high-priority tier, one transaction and one stamp, at the cost of one stall.

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

## §7 Errors
```rust
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("engine: {0}")]
    Engine(#[from] libsql::Error),

    #[error("migration to v{to} failed: {reason}")]
    Migration { to: u32, reason: String },

    #[error("invalid edge type {0} (must match [A-Z0-9]+)")]
    InvalidEdgeType(String),

    #[error("{source} -> {target} ({edge_type}) already has an open interval; retire it first")]
    SingleOpenViolation { source: String, target: String, edge_type: String },

    #[error("node {0} not found")]
    NotFound(String),

    #[error("embedding dim {got}, expected {expected} for model {model}")]
    DimMismatch { got: usize, expected: usize, model: String },

    #[error("subgraph exceeds budget ({n} > {budget})")]
    SubgraphTooLarge { n: usize, budget: usize },

    #[error("replay corrupt at seq {seq}: {reason}")]
    ReplayCorrupt { seq: i64, reason: String },

    #[error("snapshot {path} is not readable by this build: {reason}")]
    SnapshotIncompatible { path: String, reason: String },

    #[error("payload v{got} unsupported (max {max})")]
    PayloadVersion { got: u8, max: u8 },

    #[error("physical delete blocked outside archive session ({table})")]
    ArchiveViolation { table: String },

    #[error("links_current drift detected: {n} intervals diverge")]
    CurrentDrift { n: usize },

    #[error("rebuild verification failed: {n} intervals still diverge")]
    RebuildFailed { n: usize },

    // -- 0.4.5: writer-actor containment --
    #[error("write actor is not running (reopen the Database)")]
    WriterUnavailable,

    #[error("write actor dropped the response channel mid-request")]
    WriterDroppedResponder,

    // -- 0.5.0: concept integrity --
    #[error("recorded_at must advance on concept update (got {got}, had {had})")]
    RecordedAtRegression { got: String, had: String },
}
```

The error philosophy is threefold. Nothing panics across the API boundary — every public method returns Result, and internal invariant breaches are debugassert!-only. Trigger-raised aborts are parsed at the connection layer into their typed variants, so a caller catching SingleOpenViolation never string-matches a SQLite message. And errors that describe data carry the coordinates of that data — ReplayCorrupt names its seq_id, CurrentDrift names its count — because an error a maintainer cannot act on is decoration.

The 0.4.5 variants encode one policy: the failure of the writer task must be containable. An in-flight oneshot whose actor has panicked resolves to WriterDroppedResponder rather than a panic of its own; every subsequent operation resolves to WriterUnavailable. The application learns precisely what happened and what to do — reopen — and the cascade stops at the crate boundary. The actor's death itself is reported through tracing with the underlying cause, so the crash report exists even when the user-facing error is deliberately terse.

The 0.5.0 RecordedAtRegression variant surfaces the concept monotonicity trigger ([§4.3](s4-schema.md#43-the-transaction-log)) as a typed error rather than a raw engine abort, carrying both the rejected and existing timestamps so the caller can diagnose the clock or code path at fault.

## §8 Testing Strategy

Testing is layered by what each layer can prove. Unit tests cover the pure machinery: CTE builder output against golden strings, interval overlap arithmetic, RRF fusion, the embedding codec's roundtrips and dimension rejection, and the byte-budget planner's strategy choices against synthetic statistics. Integration tests run the full API against real database files in temp directories, including WAL crash recovery — transactions dropped without commit must leave the file consistent on reopen. Property tests are the acceptance gates for the invariants themselves: random assertion/retirement streams must never produce overlapping open intervals, links_current must equal the latest-belief projection of links row for row after every stream, and reconstruct(now) must equal live-table reads for every entity. Fuzz tests attack the temporal machinery specifically: replay at every recorded_at in a random stream must equal an independent log-fold oracle, and retroactive assertions must respect the documented fidelity boundary between as_of and reconstruct. Scenario tests pin the human-facing contracts, above all the Monday/Wednesday/Friday attribute-fidelity case across all three AttributeMode values, and the corrupt-then-rebuild roundtrip: damage links_current through a raw connection, call rebuild_current(), and require audit_current() == 0. Benchmark gates run under criterion in CI against the [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) budgets, including the cold-page-cache variants, so a performance regression is a failing build rather than a discovered incident.

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
- **Cold-DB absence** — delete the archive file, call reconstruct(ts) for a pre-horizon ts, and require ReplayCorrupt with the "archive database not found" reason rather than a panic or a silently-wrong state.
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

The injectable clock is the keystone of the entire suite. Every temporal test is deterministic because time is a parameter, and the one place nondeterminism is tolerated — the wall-clock stamping inside archive triggers — is tested structurally (session marker lifecycle) rather than temporally. As of 0.4.5, FakeClock gains a Send + Sync interior (a mutex over its instant) so the harness and the actor share one deterministic clock — the keystone extends cleanly to the actor, because time was already a parameter.

## §9 Performance Budgets

All targets are measured on the reference hardware (Windows 11, NVMe SSD, 32 GB RAM, release build) under criterion, with cold-page-cache variants measured after PRAGMA shrinkmemory and OS cache flush. Trigger amplification is included: a single edge assertion produces three writes (the links row, the links_current upsert, the transaction_log entry).

| Operation | Target | Mechanism |
|---|---|---|
| Single edge assertion (incl. trigger writes) | ≤ 5 ms | One BEGIN IMMEDIATE … COMMIT; three table writes |
| Single edge retirement | ≤ 5 ms | Same shape as assertion |
| Single concept upsert | ≤ 3 ms | One table write + one log entry |
| 3-hop traversal, warm cache (1K edges) | ≤ 10 ms | Recursive CTE over links_current, indexed |
| 3-hop traversal, cold cache (1K edges) | ≤ 50 ms | Same CTE; I/O-bound |
| as_of(ts) traversal (1K edges) | ≤ 15 ms | CTE + two predicate rewrites; no log access |
| AtTime hydration (100 result nodes) | ≤ 30 ms | Window query over idx_txlog_entity, bounded by result set |
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
| Chunk commit, 500 rows, trigger-amplified | ≤ 3 ms | [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) golden rule calibration |
| Archive, 100K closed intervals | ≤ 30 s | Single atomic transaction; idle-scheduled ([D-012](s13-decision-register.md#d-012)) |
| Snapshot write (100K-edge state) | ≤ 2 s | Read-fold + bincode + zstd; read-side only |

These budgets are CI gates ([§8](s6-s10-flows-to-dependencies.md#8-testing-strategy)): a regression beyond the target is a failing build. **Corrected in 0.5.5 ([D-055](s13-decision-register.md#d-055)): they are now measured and they are deliberately not gates.** `benches/budgets.rs` covers twelve of the rows above under criterion, so nothing here is unfalsifiable any more. They are not CI gates because these numbers are stated for *named reference hardware* and CI is not that machine — an absolute `≤ 5 ms` becomes an assertion about whichever runner picked up the job, which is the flaky red this project refuses elsewhere by name. Regression detection compares a machine against itself (`cargo bench -- --save-baseline`/`--baseline`), and where a hardware-independent gate is possible it lives in `tests/` as an assertion about *shape* rather than duration — [D-042](s13-decision-register.md#d-042)'s plan shape and [D-047](s13-decision-register.md#d-047)'s growth ratio remain the model.

**First measurement, at reduced scale (2K concepts, 1–2K edges, 5K log entries), on a developer laptop rather than the reference machine.** Eleven of twelve rows land inside budget with room to spare — three-hop traversal 2.1 ms against 10 ms, `audit_current` 13.8 ms against 200 ms, vector top-10 294 µs against 20 ms, hybrid top-10 2.0 ms against 50 ms, full-fold reconstruction 21 ms against 100 ms, and composition 3.4 ms against it, which is the first direct evidence that [D-049](s13-decision-register.md#d-049)'s snapshot path is worth having.

**One row misses, and it is the load-bearing one.** `Chunk commit, 500 rows, trigger-amplified` measures **≈62 ms against a ≤ 3 ms budget — roughly 20×**. That row is not decoration: it is [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)'s golden-rule calibration, the number the cooperative-chunking argument rests on. If a chunk takes 62 ms then an interactive assertion arriving just behind one waits that long, and [D-011](s13-decision-register.md#d-011)'s "yields promptly at every chunk boundary" is 20× weaker than stated. The immediate suspect is identified: `write_edges_atomic` calls `tx.execute(INSERT_LINK, …)` once per row, so each of the 500 rows re-prepares the statement, and `links` carries two triggers that are compiled into every preparation. A prepared statement hoisted out of the loop is the obvious next move and is **not** done here — instrumenting and optimising in one change would leave neither reviewable. Cold-page-cache variants are measured separately and gated at 5× the warm target. The archive budget is measured but not CI-gated, because it is idle-scheduled and its duration is bounded by the scheduling-layer self-chunking ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)). The rebuild_current() budgets at 1M and 10M edges are measured but not CI-gated, because rebuild is a recovery operation that should not occur in normal operation ([D-023](s13-decision-register.md#d-023)).

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

