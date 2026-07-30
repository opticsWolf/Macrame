# Macrame

**A Bitemporal Graph Ledger on libSQL · Embedded knowledge database**

Macrame is a domain-specific embedded database layer for a knowledge-ledger application: a system in which concepts are linked by typed, weighted relationships, both concepts and relationships change over time, and the history of those changes is itself a first-class asset.

Delivered as a single Rust crate that an application links directly. The entire database is one file on the local filesystem — no server, no network protocol, no external service.

## Core Stack

- **libSQL** (MIT, unmodified) — engine with WAL, F32_BLOB, DiskANN, JSON1, window functions
- **Rust, async** — tokio runtime, safe Rust above the engine boundary
- **Target platform** — Windows desktop, embedded, single-file

## Five Capabilities

| Capability | Mechanism |
|---|---|
| Graph storage & traversal | Recursive CTEs over relational edge tables, compiled from a typed builder |
| Bitemporal semantics | Two independent clocks per row — valid time and transaction time — enforced by engine triggers |
| Native vector search | Per-model `F32_BLOB` tables with auto-maintained DiskANN indexes; `vector_top_k` + `vector_distance_cos` |
| In-memory graph analytics | Dijkstra, A\*, SCC, k-core, Louvain (phase-one) — native adjacency-list `Subgraph`, no external graph dependency. Runs on a `Subgraph` whose adjacency is closed over its node set, so a retired concept removes its edges rather than leaving the algorithms to disagree about a dangling one |
| Point-in-time reconstruction | Append-only `transaction_log` folded with window functions; snapshot composition for fast replay (carve-outs: off across archive boundary, no cadence yet) |

## Two Semantic Operations

The distinction between these two runs through the entire design:

- **`as_of(ts)`** — *valid-time* question answered under current belief. Reports what the world looked like at `ts` given everything we know now, including corrections recorded after `ts`. A filtered read of live tables — cheap.
- **`reconstruct(ts)`** — *transaction-time* question. Replays the log and reports what the database actually believed at `ts`, before later corrections arrived. A fold over history — costs what history costs.

Both are correct answers to different questions. Conflating them is a defect.

## Eight Doctrine Invariants

Every design decision derives from these invariants:

1. **The boundary is sacred** — Everything above libSQL is ours; everything below it is upstream. Never patch the engine.
2. **Two clocks, never mixed** — Valid time and transaction time are independent. No trigger or default may derive one from the other.
3. **Assertions are immutable** — Rows in `links` are never updated in place. The past is never rewritten; it is only ever superseded.
4. **The ledger is a table, not the log** — Transaction-time reconstruction reads `transaction_log`, not WAL or CDC frames.
5. **No physical deletion in hot tables** — Rows leave through the archive path only. Ad-hoc `DELETE` aborts at the trigger layer.
6. **Derivative state is disposable** — `links_current` is a rebuildable materialization. Drift is detectable by audit, recoverable by rebuild.
7. **Embeddings are immutable per version, excluded from the ledger** — Vectors live in per-model tables; they never appear in `transaction_log` payloads.
8. **Fidelity is a parameter, never a silent default** — `as_of(ts)` and `reconstruct(ts)` say what they mean in their signatures.

## Architecture at a Glance

```
Application (Rust, async)
│  typed API — no SQL visible
▼
┌──────────────────────────────────────────┐
│              macrame crate               │
│                                          │
│  schema/  graph/  temporal/  vector/     │
│  DDL      CTE     as_of     DiskANN      │
│  triggers replay  snapshot  top-k        │
│  migratns algorithms archive  per-model   │
│                                          │
│         connection.rs                    │
│  (Write Actor, priority channels, clock) │
└──────────────────┬───────────────────────┘
│  libsql crate — dependency, never a fork
▼
libSQL engine (MIT, unmodified)
├── transaction_log  (append-only, trigger-captured)
├── macrame.db       (hot: current belief, recent log)
└── macrame_archive.db (cold: superseded history)
```

### Concurrency Model

One process, one writer, many readers under WAL journaling:

- **Write Actor** — the sole write-capable connection lives inside a dedicated Tokio task. No other code path can name it.
- **Two-tier priority channels** — high-priority (UI-driven work) preempts low-priority (background jobs) at every transaction boundary via biased `select!`.
- **Cooperative chunking** — low-priority workers chunk at 500–1,000 rows, bounding lock hold to 2–3 ms per chunk.
- **Read guard** — the read connection carries `PRAGMA query_only = ON`, converting Rust ownership into runtime enforcement.

## Schema

| Table | Role |
|---|---|
| `concepts` | Entities with mutable attributes; updated in place, history in log |
| `links` | Full bitemporal assertion history; 5-column PK including `recorded_at` |
| `links_current` | Trigger-maintained materialization of current belief; traversals read only this table |
| `transaction_log` | Append-only ledger captured by engine triggers; the sole transaction-time mechanism |
| `analytics_annotations` | Derivative table for algorithm results; disposable, excluded from ledger |
| `embeddings_<model>` | Per-model vector tables with DiskANN indexes; created by `register_model()` |

All temporal columns use a canonical 27-character timestamp form: `YYYY-MM-DDTHH:MM:SS.ffffffZ`, enforced by `CHECK` constraints.

## Crate Layout

```
src/
├── lib.rs                  # public re-exports, prelude
├── error.rs                # DbError (thiserror)
├── connection.rs           # Database handle, Write Actor, priority channels, clock
├── schema/
│   ├── ddl.rs              # all DDL as const strings
│   ├── migrations.rs       # user_version-driven runner
│   └── seed.rs             # optional bootstrap
├── graph/
│   ├── builder.rs          # Traversal builder → CTE; AttributeMode hydration
│   ├── edge.rs             # assert / retire / re-assert lifecycle
│   ├── vector_filter.rs    # strategies, byte-budget cost model
│   ├── subgraph.rs         # DB → Subgraph loader, byte budget
│   └── algorithms.rs       # dijkstra · astar · scc · k_core · louvain
├── temporal/
│   ├── interval.rs         # Interval, overlap arithmetic
│   ├── as_of.rs            # valid-time filters under current belief
│   ├── replay.rs           # window-function reconstruction; cold-DB ATTACH
│   ├── snapshot.rs         # bincode + zstd snapshots, seq-anchored
│   └── archive.rs          # ATTACH-based cold storage
├── vector/
│   ├── embedding.rs        # Vec ↔ F32_BLOB codec
│   ├── model.rs            # ModelName newtype (validated identifier)
│   ├── registry.rs         # register_model, declared_dimension
│   └── search.rs           # top-k, RRF fusion
├── integrity/
│   ├── audit.rs            # audit_current() — read-side
│   └── rebuild.rs          # rebuild_current() — high-priority command
└── util/
    ├── ids.rs              # ULID generation & validation
    └── clock.rs            # Clock trait; SystemClock; FakeClock
```

## Testing

```bash
# Full test suite (unit, integration, scenario)
cargo test --no-fail-fast

# Property tests (generated-history binaries, run serially)
cargo test --features property-tests --no-fail-fast
```

`--no-fail-fast` is not optional for reading the totals: without it cargo stops at the first failing binary and everything alphabetically behind it never runs. That is how the archive defect U sat unnoticed behind a known-red `concurrency_tests`.

### Test Layers

| Layer | What it proves |
|---|---|
| Unit tests | CTE builder output, interval arithmetic, RRF fusion, embedding codec roundtrips. Note that the interval-arithmetic test is the only caller of `Interval::overlaps` — defect **AG** |
| Integration tests | Full API against real database files; WAL crash recovery |
| Property tests | Random assertion/retirement streams never produce overlapping **open** intervals; `links_current` == latest-belief projection. The open-interval restriction is load-bearing, not incidental — nothing tests the closed case, which is defect **AA** |
| Scenario tests | Attribute fidelity across `AttributeMode` values; corrupt-then-rebuild roundtrip |
| Regression tests | `tests/wave1_regression_tests.rs` — one per Wave 1 defect, each verified to fail against the pre-fix tree. Its header names the three that pass either way and says why, rather than letting them look like coverage they are not |

**Current baseline: 211 passing, 0 failing** on plain `cargo test`; 230 with `--features property-tests --no-fail-fast`.

Benchmarks are separate and are **measurements, not gates** — §9's numbers are stated for named hardware, so an absolute threshold in CI would gate on whichever runner picked up the job:

```bash
cargo bench --bench budgets
```

Two things to know before reading a red or a green here. **A green means "no test disagrees with the code"** — the 2026-07-30 review found ten defects living inside a green suite, and while nine are now closed by tests, one still is not. And **a red may not be yours**: R15 crashes an arbitrary test binary with `STATUS_ACCESS_VIOLATION` on roughly one full run in three, taking a different binary each time and passing when that binary is re-run alone. Re-run the named binary on its own before believing it.

### Known test gaps

| Gap | Detail |
|---|---|
| Raw-SQL overlap | The valid-time overlap guard lives in the write actor, so it binds callers of this API and not raw SQL against the same file. A trigger would bind both and would tax every insert on the path the v6 index was just added to make fast. |
| Unbenched read paths | `benches/budgets.rs` measures twenty rows and none of them is `load_subgraph`, one of the five algorithms, `archive()`, or `FilteredVectorSearch`. The deferred `Subgraph` rewrite is blocked on a benchmark in that gap. |
| Benchmark gates | §9 performance budgets are measured, not enforced as CI gates — deliberately (D-047, D-055). |
| `RecordedAtRegression` | Mapped by the error classifier but unreachable through the public API — `SystemClock` is strictly increasing by contract. |
| Plan-shape coverage | Three tests now pin a query *plan* rather than a result (**D-042**, **D-059**, **D-064**), because that class of defect returns the right answer and no correctness test can see it. Every index-sensitive query should have one; not all of them do. |

### Known defects

Severity rule: **a wrong answer outranks a crash.** Full evidence, reproductions and remediation order are in the [implementation plan §8.5](docs/Macrame%20Implementation%20Plan%20v0.5.4.md); the letters are that document's register.

**Open:**

| # | Location | Defect | Wave |
|---|---|---|---|
| Subgraph filters | `graph/subgraph.rs` | `load_subgraph` takes neither `edge_types` nor `min_weight` while `TraversalBuilder` takes both. A feature gap, not a wrong answer — but a reachability limit, because filtering after the fact cannot help a caller whose *unfiltered* neighbourhood exceeds the byte budget | — |
| Snapshot cross-check | `temporal/snapshot.rs` | Chains compound: `write_final` composes onto the previous snapshot, so an error propagates forward with nothing verifying a composed result against a fold from genesis | — |
| No `verify_fts()` | `connection.rs` | `rebuild_fts()` is the repair with no way to ask whether it is needed. FTS5's `integrity-check` cannot answer — it verifies the index's internal consistency, not its agreement with `concepts` — so none was written rather than one that would call an empty index healthy (D-071) | — |
| R15 | libSQL 0.9.30 | Intermittent `STATUS_ACCESS_VIOLATION` when local databases are opened concurrently in one process; mitigated by `RUST_TEST_THREADS = "1"` and the `property-tests` feature gate | — |

**Closed by Wave 4 (2026-07-30)** — hardening, recorded as D-066 … D-070:

| # | Was | Resolution |
|---|---|---|
| `load_subgraph` growth | Superlinear, 12.5× for 10× the nodes, unexplained | **Explained, not fixed** (D-070): an O(E log E) `DISTINCT` sort, load-bearing. Two fixes tried — deduplicating the walk first is 36% *slower*; removing the redundant second sort measures 85.95 vs 86.76 ms, i.e. nothing |
| ATTACH race | The cadence shared `read_conn`, so two folds could interleave in the unsynchronised `ATTACH cold … DETACH cold` region | The cadence gets its own connection (D-066) — removes the shared state rather than ordering access to it |
| `close()` swallowed failure | The writer's `Result` was discarded, so a `Database` whose actor had panicked closed "successfully" | Returns `DbError::WriterStopped`, *before* the final snapshot is written |
| Stale snapshots after upgrade | A `SCHEMA_VERSION` bump invalidates every snapshot and nothing wrote a replacement, so the first reconstruct folded from genesis | Re-anchored at open (D-067), gated on it being a real upgrade *and* the cadence being enabled |
| Errors naming the wrong subject | `timestamp::normalize` reported caller input as `ReplayCorrupt { seq: 0 }`; `archive_horizon.archived_at` was written with the cutoff | `DbError::InvalidTimestamp`; the archive time comes from the clock (D-069) |

**Closed by Wave 3 (2026-07-30)** — the measurement wave, recorded as D-063, D-064 and D-065:

| # | Was | Resolution |
|---|---|---|
| AI | The overlap guard's narrowing predicate made `idx_lc_traversal_cover` win as a *covering* index, so the guard bound one column and scanned the source's out-degree — **D-059's defect, reproduced by D-060's fix one wave later.** +9.8 ms per 90-edge chunk into a 2,000-edge hub | Predicate dropped; the query is now a three-column point lookup. **No correctness test could have caught this** — the guard answered correctly throughout — so the plan is pinned by a test |
| AF | `corpus_size` runs `COUNT(*)` per filtered query; the planner's input is O(corpus) | **Closed without a fix.** Measured at <1% of a search, and caching it would be unsound: it changes on every embedding write |
| D-047 | The `Subgraph` integer-index rewrite, deferred on a benchmark nobody had written | **Retired.** The load dominates any single analysis at every size measured |

**Closed by Wave 2 (2026-07-30)**, with three decisions recorded as D-060, D-061 and D-062:

| # | Was | Resolution |
|---|---|---|
| AA | `trg_links_single_open` guards only the open sentinel, so **overlapping closed intervals were accepted** and read back as two edges for one relationship | Refused in the write actor (D-060). Not in a trigger — so raw SQL can still write one, and §4.2 says so |
| AG | `Interval::overlaps` was dead code, the missing half of AA | It is the guard's decision procedure; the SQL beside it only narrows |
| AD | Nothing validated an identifier while three modules assumed ULIDs | Ids are opaque with `\|` and `/` reserved (D-061). Every existing id stays valid, and W's collision becomes unreachable rather than merely harmless |
| J | `validate_id` returned `NotFound` for a malformed id | `DbError::InvalidId { id, reason }` — a refusal is not an absence |
| K | `FakeClock` was constructed in the harness and injected nowhere | `Database::open_with_clock` (D-062), with the floor lifted into the `Clock` trait |
| D-059 | The single-open probe scanned its source's out-degree on every insert | `idx_lc_open_interval` on a v5 → v6 rung. **`SCHEMA_VERSION` is now 6** |

**Closed by Wave 1 (2026-07-30)**, each by a named test rather than by a commit — the distinction is deliberate, because defect **H** spent a full cycle marked fixed on the strength of a commit that changed nothing observable:

| # | Was | Resolution |
|---|---|---|
| V | Concept log payload omitted `embedding_model`, so **every temporal read returned `None`** | Payload v2, readers accept v1 and v2 |
| W | Folds partitioned on `entity_id` alone; a concept colliding with a link key **vanished from the reconstruction** | Partition on `(table_name, entity_id)` |
| Z | Adjacency could reference absent nodes: `louvain` **panicked**, `scc` emitted phantoms, `k_core` inflated degree | `Subgraph` carries a stated closure invariant; dangling entries dropped. No algorithm needed changing |
| AB | Three attribute readers, three retirement semantics | One rule: not returned if retired as of the instant asked about |
| AE | `hydrate` and `hydrate_attributes` issued one query per node | Batched at 400 ids per statement. **Not yet benchmarked** — Wave 3 |
| AC | `classify_archive_violation` called from nowhere, so `DbError::ArchiveViolation` was unconstructible (reopened **H**) | Deleted — `error::classify` already did the same job. `archive()`'s deletes routed through it |
| AH | Doctrine VII's static guard banned the substring `embedding`, refusing the V fix | Narrowed to permit `embedding_model` by name, with a test pinning that it is still a scalar column |

Two defects this table used to carry, **S** (`CostEstimator` with no strategy implementations) and **T** (RRF with no keyword arm), are **fixed** — D-050 and D-051 respectively. They were listed as open here while the delivery table below recorded them as delivered.

## Dependencies

| Crate | Role |
|---|---|
| libsql 0.9.30 | Engine binding |
| tokio 1 | Async runtime; actor task, channels |
| serde / serde_json | Payload serialization |
| bincode | Snapshot serialization |
| zstd | Snapshot compression |
| thiserror | Error derive |
| tracing | Structured diagnostics |
| ulid | Entity ID generation |

No GPL-licensed components. No `chrono` or `time` dependency.

## Delivery Status

| Phase | Status |
|---|---|
| Phase 0 — Schema + migrations | **Delivered** — legacy-free baseline at v2, rungs v2→v3 (D-041) and v3→v4 (D-042) |
| Phase 1 — Write Actor + public write API | **Delivered** — exhaustive match, no wildcard; assert/retire/upsert/bulk-atomic/bulk-import/annotations/rebuild/archive |
| Phase 2 — Temporal core (replay, snapshots) | **Delivered** — ATTACH bracketed on all paths, self-healing (D-044), versioned container (D-043) |
| Phase 3 — Vector + graph | **Delivered** — `register_model` + `upsert_embeddings` through the actor (D-048); native `Subgraph` with five algorithms (D-039); edge types bound, not interpolated |
| Phase 4 — Document restoration | **Delivered** — §5.2–§5.9, §6, Appendix A de-corrupted and forward-ported |
| Snapshot composition (D-049) | **Delivered** — anchored fold + tombstone merge; composes across the archive boundary as of D-052 |
| Snapshot cadence (D-053) | **Delivered** — read-side maintenance task, triggered by log distance rather than a clock; `close()` stops and joins it before the final anchor |
| Snapshot retention (D-054) | **Delivered** — the newest five plus one per day for thirty days, as §5.5 always specified; container header v2 carries each snapshot's instant so bucketing costs 18 bytes, not a decompression |
| §9 benchmarks (D-055) | **Delivered** — `cargo bench --bench budgets` measures twelve budget rows. Measurement, not CI gates: absolute durations on arbitrary runners are the wrong shape (D-047's reasoning). **Eleven pass; chunk commit missed by 20×** |
| Chunk commit (D-056) | **Delivered** — statement prepared once per chunk instead of once per row: ≈62 → ≈37 ms at 500 rows. Measuring the residual showed the ledger triggers are ~92% of it, and that §9's ≤ 3 ms is the *un-amplified* cost — so §5.1.5's golden rule needs re-deriving, not the code |
| Bulk write paths (D-057) | **Delivered** — the same hoist in the three bulk paths §9 does not budget: concepts 34.1 → **11.9 ms**, annotations 4.60 → **2.13 ms**, embeddings 73.4 → **67.4 ms** at 500 rows, measured by a new `bulk_chunks` group rather than assumed from the edge chunk. The spread inverts D-056's reasoning: `analytics_annotations` has no triggers and saved 54%, the DiskANN tables saved 8%, so preparation is a near-constant per row and the saving is largest where the row is cheapest |
| Superlinearity diagnosed (D-059) | **Delivered** — and it corrects D-058. The sweep measured chunk size and table size as one variable; separated, a fixed 90-row chunk into a hub of 0/2,000/8,000 edges costs **4.4 / 18.4 / 47.7 ms**, so the cost is in the table. End to end, 1,000 edges are **85.5 ms as one transaction vs 94.7 ms as eleven chunks** — chunking is ~11% *slower*, not 3.3× faster. The edge path's growth is a **defect**: the single-open guard's `EXISTS` plans as a one-column scan over `idx_lc_traversal_cover`, so every insert scans its source's out-degree (90 rows into a 90,000-edge hub: **1.06 s**), and that hits interactive `assert_edge` too. Fix proven (**47.7 → 8.0 ms, flat**), not shipped — it needs a migration rung. Embeddings are the same shape and inherent to DiskANN. See `examples/chunk_diag.rs` |
| Golden rule (D-058) | **Delivered** — §5.1.5 re-derived from a chunk-size sweep. It is a bound on *duration*, so `CHUNK_ROWS = 1000` becomes `CHUNK_BUDGET` = 3 ms with four measured sizes — edges 90, concepts 70, annotations 600, embeddings 30 — at **2.39 / 2.35 / 2.36 / 2.06 ms** where the single constant gave **89 / 24 / 3.5 / 143 ms**. Two paths turned out superlinear in chunk size, so smaller chunks are **3.3× faster in total** there, not a trade; per-transaction overhead is ~0.8 ms, not "noise". Open: why the superlinearity |
| Archive read path (D-052) | **Delivered** — `hot_log_covers` tested how far the hot log *reached*, not whether it was *complete*, so a reconstruction before the archive cutoff could silently drop an entity |
| Phase 5 — Test matrix | **Delivered** — Doctrine VIII divergence (both directions), archive crash safety (D-012), Doctrine VII property suite, and empirical cost estimates via D-050 |
| Filtered vector search | **Delivered (D-050)** — `FilteredVectorSearch`; two strategies, both with bodies, held together by a test requiring them to agree; `TwoPhaseTempTable` removed, its two mechanisms being absent from libSQL 0.9.30 |
| Hybrid search | **Delivered (D-051)** — `concepts_fts` FTS5 external-content index on a `v4 → v5` rung; `HybridSearch` fuses vector and keyword arms by RRF; `rebuild_fts()` satisfies D-036 |
| `Subgraph` integer-index rewrite | **Deferred** — pending a Louvain/Dijkstra benchmark on a budget-sized graph, which does not exist; see Wave 3 |

Every item the previous plan sequenced is delivered. What comes next is set by the 2026-07-30 review rather than by that sequence.

## Roadmap

Four waves, ordered by the project's own severity rule — a wrong answer outranks a crash, and both outrank a slow one. Full detail, evidence and per-item acceptance tests are in the [implementation plan §9](docs/Macrame%20Implementation%20Plan%20v0.5.4.md).

| Wave | Theme | Scope | Size |
|---|---|---|---|
| **1** ✅ | The six silent defects | V, W, Z, AB, AC, AE, plus documentation drift. **No schema change.** Delivered 2026-07-30; thirteen new tests, 171 → 184 passing | done |
| **2** ✅ | Decisions deferred a full cycle | D-059's index shipped as the v5 → v6 rung; valid-time overlap (**AA**/**AG**), identity (**AD**/**J**) and the injectable clock (**K**) all decided. Delivered 2026-07-30; 184 → 202 passing | done |
| **3** ✅ | Measure what the bounds claim | Six bench groups covering every previously unmeasured path. Found one defect (**AI**), retired two proposed optimisations, confirmed the four chunk constants. Delivered 2026-07-30; 202 → 203 passing | done |
| **4** ✅ | Hardening | The ATTACH race, `close()`'s discarded error, `raw()`'s surface, the migration re-anchor, three mis-named errors, and Wave 3's unexplained superlinearity. Delivered 2026-07-30; 203 → 205 passing | done |

| **5** ⬅ | The last silent-wrong-answer paths | The unreachable `'D'` branch (done, D-072); the FTS5 `VACUUM` hazard (investigated, **nothing built** — it isn't reachable, D-071); then R15, the subgraph filter, one consolidated §4 statement, and the rename | in progress |

**Waves 1–4 are delivered; Wave 5 is under way.** What remains open is in the table above, and none of it returns a wrong answer.

Wave 5's first two items are worth reading as a pair, because both ended by *removing* something rather than adding it: an unreachable branch and the set that served it (D-072), and two mechanisms declined after measurement showed one hazard unreachable and one check unable to see it (D-071). The output was a smaller crate and three facts nobody had written down.

Why correctness preceded the performance work: D-059's index is the largest measured win in the tree and it moves `user_version`, so it wants a stable base and its own migration test. Wave 1's defects needed no schema movement, so they landed while that rung was still being decided — **Wave 2 is now what's next.**

### Contracts settled by Waves 1 and 2

Recorded here because they constrain future work rather than merely describing past work.

- **`Subgraph` is closed.** Every adjacency endpoint is a hydrated node, stated on the type. Algorithms may rely on it and none re-checks. A future loader that admits retired nodes has to revisit all five.
- **Retirement is uniform.** A concept retired as of the instant asked about is not returned, by any of the three readers. `Current` asks "retired now" and `AtTime` asks "retired then" — that difference is the two clocks and is meant to stay.
- **One error classifier, not two.** `error::classify` is the only path to a typed guard error; the archive's private duplicate is gone.
- **Identifiers are opaque, except that `|` and `/` are reserved.** Not ULIDs — the crate never required them and three modules only assumed them. The two characters delimit the transaction log's entity key and the traversal path, so an id carrying one is ambiguous rather than merely unusual. `generate_id()` is offered, not required.
- **Valid-time intervals for one relationship never overlap — through this API.** Two open intervals are `SingleOpenViolation` (the trigger), anything else overlapping is `OverlappingInterval` (the write actor). The two guards partition the space; they do not layer. Raw SQL against the same file can still write an overlap.
- **A `Clock` can be told its floor.** `raise_floor` is a required trait method, not a defaulted one, because "strictly increasing across restarts" depends on what the database already holds and no clock can see that by itself.
- **`CHUNK_BUDGET`'s 3 ms has exactly three exemptions**, and they are stated with the bound rather than scattered across rustdoc: `write_bulk_atomic`, `archive()` and `rebuild_current` are atomic by contract and cannot be chunked without breaking the guarantee each exists to provide. Callers who need the latency bound instead of the atomicity have `bulk_import`.
- **A narrowing predicate is not free if it changes the plan.** A covering index is chosen for containing the columns, not for discriminating between rows, so the more columns one carries the more queries it silently captures. This has now bitten the same codebase three times; index-sensitive queries get a test that asserts their `EXPLAIN QUERY PLAN`.
- **`close()` is how you learn the write actor died.** It propagates the writer's `Result` and writes the final snapshot; dropping instead is legal and costs one snapshot, which is slower rather than wrong (Doctrine VI), so `Drop` warns and does not assert.
- **Concepts are never deleted, and that guard protects more than the ledger.** `trg_concepts_guard_delete` is unconditional (D-022), so `concepts.rowid` stays dense — which is the only reason the external-content FTS index survives a `VACUUM`. Any future concept archival or erasure has to rebuild the index (D-071).
- **A `'D'` row in `transaction_log` is corruption.** Nothing writes one: Doctrine V forbids the delete and the archive moves rows rather than logging their removal. The fold refuses it rather than folding it as a tombstone (D-072).
- **`raw()` is an escape hatch, and actor containment above it is a convention.** `query_only` protects `read_conn()`; nothing protects a connection you open yourself. This is the same limit §4.2 states for the overlap guard — one fact, not two: the storage layer permits what this API refuses.

Independent of all four: the **R15 upstream report** against libSQL 0.9.30, outstanding longest and blocked by nothing.

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API). One file per section.
- [Implementation plan](docs/Macrame%20Implementation%20Plan%20v0.5.4.md) — delivery status, open items, defect register. Start at **§8.5** (the 2026-07-30 review, with reproductions), **§8.6** (bounds that are stated and not bounded) and **§9** (the four waves).
- [Decision register](docs/architecture/s13-decision-register.md) — D-001 … D-059, each with its rationale and the disqualifying flaw of the alternative

## License

See [LICENSE](LICENSE) for details.
