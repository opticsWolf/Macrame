# Macrame — Quick Reference

**v0.6.0 · A Bitemporal Graph Ledger on libSQL**

---

## 1. Doctrine (8 Invariants)

| # | Invariant | What it means | Code location |
|---|---|---|---|
| I | **Boundary is sacred** | Never patch libSQL; build above it or carry the gap | `src/schema/ddl.rs`, `src/connection.rs` |
| II | **Two clocks, never mixed** | `valid_from`/`valid_to` (valid time) and `recorded_at` (transaction time) are independent | `src/util/clock.rs`, `src/temporal/` |
| III | **Assertions are immutable** | Rows in `links` are never updated; superseded by new rows | `src/schema/ddl.rs` (triggers), `src/connection.rs` |
| IV | **Ledger is a table** | Reconstruction reads `transaction_log`, not WAL or CDC | `src/schema/ddl.rs`, `src/temporal/replay.rs` |
| V | **No physical deletion** | Rows leave only through archive path; `DELETE` aborted by triggers | `src/schema/ddl.rs` (guards), `src/temporal/archive.rs` |
| VI | **Derivative state is disposable** | `links_current` is rebuildable from `links`; drift detectable and recoverable | `src/integrity/shadow.rs`, `src/schema/ddl.rs` |
| VII | **Embeddings excluded from ledger** | Vectors live in per-model tables; never in `transaction_log` payloads | `src/vector/registry.rs`, `src/vector/mod.rs` |
| VIII | **Fidelity is a parameter** | `as_of(ts)` ≠ `reconstruct(ts)`; gap documented and tested | `src/temporal/as_of.rs`, `src/temporal/replay.rs` |

---

## 2. Schema Overview

### Core Tables

| Table | Purpose | Key columns |
|---|---|---|
| `concepts` | Mutable entities with attributes | `id TEXT PK`, `valid_from/to`, `recorded_at`, `retired` |
| `links` | Full bitemporal edge history | PK: `(source_id, target_id, edge_type, valid_from, recorded_at)` |
| `links_current` | Materialized current belief (rebuildable) | Latest assertion per interval |
| `transaction_log` | Append-only replay log | `seq_id`, `entity_id`, `operation`, `recorded_at`, `payload` |
| `analytics_annotations` | Second derivative (analytics output) | `concept_id`, `label`, `value` |
| `concepts_fts` | FTS5 external-content index | Tokenized text, no duplication |
| `embeddings_*` | Per-model vector tables | `F32_BLOB(n)` with DiskANN index |

### Timestamp Form (normative)

Every temporal column is exactly 27 characters: `YYYY-MM-DDTHH:MM:SS.ffffffZ`

- Fixed width ensures lexicographic ordering equals chronological ordering
- Open-interval sentinel: `9999-12-31T23:59:59.999999Z`
- Enforced by `CHECK` (GLOB pattern) on all four tables
- Second-precision input is widened at the boundary (`util::timestamp::normalize`)

### Schema Versioning

| Version | Feature |
|---|---|
| v1 | Legacy baseline (refused by migration runner) |
| v2 | Legacy-free baseline |
| v3 | `analytics_annotations` table |
| v4 | `concepts_fts` external-content index |
| v5 | `idx_lc_open_interval` (overlap guard index) |
| v6 | Overlapping closed intervals refused in actor |
| v7 | `CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')` |

---

## 3. Architecture at a Glance

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

┌──────────────────────────────────────────┐
│           libSQL engine (unmodified)      │
│   SQLite core · DiskANN · F32_BLOB       │
│   JSON1 · window functions · ATTACH      │
│   user_version migration hook             │
└──────────────────┬───────────────────────┘

macrame.db (hot)          macrame_archive.db (cold)
open intervals            closed intervals
current belief            superseded history
recent log                detached on archive
```

### Concurrency Model

- **One process, one file** — embedded, no server
- **One writer** — the Write Actor task holds the sole write-capable connection
- **Many readers** — WAL journaling; readers never block on writer
- **Two-tier priority channels** — high-priority (user-driven) preempts low-priority (background)
- **Cooperative chunking** — low-priority transactions bounded to 500–1000 rows
- **`PRAGMA query_only = ON`** — read connection enforced at engine level (not just Rust ownership)

---

## 4. Module Map

| Module | File(s) | Responsibility |
|---|---|---|
| `schema` | `schema/ddl.rs`, `schema/migrations.rs` | DDL generation, trigger/index creation, `user_version` migration runner |
| `graph` | `graph/builder.rs`, `graph/subgraph.rs`, `graph/vector_filter.rs` | CTE compilation, subgraph loading, vector filter strategies, byte budget |
| `temporal` | `temporal/replay.rs`, `temporal/snapshot.rs`, `temporal/as_of.rs` | `reconstruct()`, `as_of()`, snapshot cadence/retention, archive |
| `vector` | `vector/mod.rs`, `vector/registry.rs`, `vector/model.rs`, `vector/hybrid.rs` | Model registration, embedding upsert, DiskANN search, hybrid RRF fusion |
| `integrity` | `integrity/shadow.rs` | `audit_current()`, `rebuild_current()`, shadow-swap rebuild |
| `util` | `util/clock.rs`, `util/timestamp.rs`, `util/ids.rs` | `Clock` trait, timestamp normalization/parsing, ULID generation |
| `connection` | `connection.rs` | `Database` handle, Write Actor, priority channels, chunking constants |
| `error` | `error.rs` | `DbError` enum, error classification |

---

## 5. Public API Surface

### Database Handle

```rust
pub struct Database { /* opaque */ }

// Open / close
impl Database {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self>
    pub async fn open_with_cadence(path, cadence: Option<SnapshotCadence>) -> Result<Self>
    pub async fn close(self) -> Result<()>
    pub async fn close_with_snapshot(self, snap_dir: impl AsRef<Path>) -> Result<()>
    pub fn read_conn(&self) -> &libsql::Connection       // WAL reader
    pub fn diagnostic_conn(&self) -> Result<Connection>   // OS-level read-only
    pub fn raw(&self) -> &libsql::Connection              // #[doc(hidden)]
}
```

### Concepts

```rust
pub struct ConceptUpsert { /* opaque */ }

impl Database {
    pub async fn upsert_concept(upsert: ConceptUpsert) -> Result<()>
    pub async fn upsert_concepts(rows: Vec<ConceptUpsert>) -> Result<()>
}
```

### Edges

```rust
pub struct EdgeAssertion { /* opaque */ }

impl Database {
    pub async fn assert_edge(assertion: EdgeAssertion) -> Result<()>
    pub async fn retire_edge(source, target, edge_type, valid_from, valid_to) -> Result<()>
    pub async fn write_bulk_atomic(rows: Vec<EdgeAssertion>) -> Result<()>
}
```

### Traversal & Subgraph

```rust
pub enum AttributeMode { Current, AtTime, Omit }

pub struct TraversalBuilder { /* opaque */ }

impl Database {
    pub fn traverse() -> TraversalBuilder
    pub async fn load_subgraph(start_node, max_hops, ts, byte_budget) -> Result<Subgraph>
    pub async fn load_subgraph_with(filter: FilteredSubgraph) -> Result<Subgraph>
}

// TraversalBuilder methods
TraversalBuilder::as_of(ts)          // valid-time query
TraversalBuilder::attribute_mode(mode) // Current / AtTime / Omit
TraversalBuilder::edge_type(...)     // bind parameters
TraversalBuilder::min_weight(f64)    // weight threshold
TraversalBuilder::build_sql()        // compiles CTE
```

### Temporal Queries

```rust
// as_of(ts) — valid time under current belief
pub async fn query_as_of_edges(conn, ts, filter) -> Result<MaterializedState>

// reconstruct(ts) — transaction-time replay from log
pub async fn reconstruct(conn, ts, archive_path, snapshots) -> Result<MaterializedState>

// Archive
pub async fn archive(cutoff, archived_at, archive_path) -> Result<ArchiveReport>
pub async fn archive_windowed(cutoff, window, archived_at, archive_path) -> Result<ArchiveReport>

// Snapshots
pub async fn save_snapshot(conn, state, snap_dir) -> Result<()>
pub async fn load_snapshot(conn, snap_path) -> Result<MaterializedState>
pub async fn audit_current(conn) -> Result<i64>
pub async fn rebuild_current(conn) -> Result<RebuildReport>
pub async fn rebuild_current_chunked(conn) -> Result<RebuildReport>

// Snapshot chain verification
impl Database {
    pub async fn verify_snapshot_chain(ts) -> Result<ChainReport>
}
```

### Vector Search

```rust
pub struct ModelName { /* opaque, validated as SQL identifier */ }

impl Database {
    pub async fn register_model(name: ModelName, dimension: usize) -> Result<()>
    pub async fn registered_models() -> Result<Vec<ModelName>>
    pub async fn upsert_embedding(model, concept_id, vector: &[f32]) -> Result<()>
    pub async fn upsert_embeddings(model, rows: Vec<(String, Vec<f32>)>) -> Result<()>
    pub async fn search_vector(query: &[f32], model, k) -> Result<Vec<VectorSearchResult>>
}

// Filtered vector search (through TraversalBuilder pattern)
pub struct FilteredVectorSearch { /* opaque */ }

FilteredVectorSearch::top_k(k)
FilteredVectorSearch::execute(conn, ts) -> Result<Vec<VectorSearchResult>>
// Returns CostEstimate with chosen strategy (PostFilter / PreFilterCTE)
```

### Hybrid Search

```rust
pub async fn keyword_search(conn, query, model, k, ts) -> Result<Vec<HybridHit>>
pub fn reciprocal_rank_fusion(vector_ranks, keyword_ranks, corpus_size) -> Result<Vec<HybridHit>>
// RRF fusion at k=60, each arm read to max(5×top_k, 50)
```

### Analytics

```rust
pub struct Annotation { /* opaque */ }

impl Database {
    pub async fn write_concepts(rows: Vec<ConceptUpsert>) -> Result<()>
    // Previously called write_analytics_annotations (renamed Wave 5)
}
```

### Metrics (feature: `metrics`)

```rust
impl Database {
    pub fn metrics(&self) -> Result<MetricsSnapshot>
}

// MetricsSnapshot fields
struct MetricsSnapshot {
    queue_depth_mean, queue_depth_hwm: f64
    hold_histogram: /* per-kind histogram */
    over_budget_count: /* per-kind */
    longest_hold_ms: f64
    longest_hold_kind: CommandKind
    turns, depth_samples: u64
}
```

---

## 6. Chunking Constants (per path)

| Path | Chunk Size | Measured at | Per-row cost |
|---|---|---|---|
| Edges (`write_edges_atomic`) | 90 | ~2.39 ms | ~11 µs (empty db), superlinear on degree |
| Concepts (`write_concepts_atomic`) | 70 | ~2.35 ms | ~2.5 µs |
| Annotations (`write_annotations_atomic`) | 600 | ~2.36 ms | ~2.5 µs |
| Embeddings (`upsert_embedding_chunk`) | 30 | ~2.06 ms | ~135 µs (DiskANN insertion) |

**Bound**: 3 ms (`CHUNK_BUDGET`). Per-transaction overhead: ~0.8 ms (BEGIN, COMMIT, fsync).

---

## 7. Performance Budgets (§9)

| Operation | Budget | Measured | Notes |
|---|---|---|---|
| Single assertion | ≤ 5 ms | — | Empty db; degrades on high-degree nodes without index |
| Chunk commit (90 edges) | ≤ 3 ms | ~2.39 ms | Fully amplified (triggers included) |
| Three-hop traversal | ≤ 10 ms | 2.1 ms | On `star_of_stars` fixture |
| `audit_current` | ≤ 200 ms | 13.8 ms | |
| Vector top-10 | ≤ 20 ms | 294 µs | |
| Hybrid top-10 | ≤ 50 ms | 2.0 ms | |
| Full fold (reconstruct) | ≤ 100 ms | 21 ms | |
| Composition | ≤ 100 ms | 3.4 ms | Snapshot + delta fold |
| Archive (2000 edges) | ≤ 30 s | 26.8 ms | One session; windowed trades total for latency |
| Rebuild (10M edges) | ~50 s | — | Chunked: 7.6× less hold, 2.3× cheaper total |

**Measurement caveat**: Absolute timings are hardware-dependent. All budgets measured on named reference hardware. Criterion baselines detect regression; machine against itself.

---

## 8. Hardening Plan v0.6.0 — Summary

### Tier 0 — Measured Defects (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T0.1 — Traversal enumerates paths, not nodes** | ✅ D-076 | CTE changed from `UNION ALL` + `path` column + `INSTR` cycle check → `UNION` (dedupes on entry). 1,600× improvement on clustered graphs, free on trees. `/` no longer reserved in identifiers. |
| **T0.2 — Repair costs more than damage** | ✅ D-077 | Post-rebuild `audit_current` made opt-in for archive path (archive already knows projection is correct). Projection extracted to single constant. |
| **T0.3 — §4.7 invariant 3 misstated** | ✅ D-078 | NaN is refused by `REAL NOT NULL` storage layer. §4.7 corrected to claim only "non-negative" (not "not NaN"). |

### Tier 1 — Bounded Actor Latency (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T1.1 — Window the archive** | ✅ D-080 | `archive_windowed(cutoff, window)` — N small atomic sessions instead of one. Longest hold 3,326→768 ms at 8K keys. Session skips rebuild when nothing archived. |
| **T1.2 — Chunked shadow rebuild** | ✅ D-082 | `rebuild_current_chunked` — shadow-swap with catch-up. Longest hold 353→47 ms (7.6×). 2.3× cheaper total. Interlock against archive interleaving. |
| **T1.3 — `write_bulk_atomic` stays uncapped** | ✅ D-081 | Warns above `BULK_ATOMIC_WARN_HOLD` (250 ms). `estimated_bulk_hold()` for caller prediction. Quadratic fan-out model with measured coefficients. |
| **T1.4 — Make actor observable** | ✅ D-079 | `Database::metrics()` — queue depth, per-kind hold histogram, over-budget count, longest hold with command kind. Feature-gated, zero cost when off. |

### Tier 2 — Schema (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T2.1 — `CHECK (weight >= 0.0)` on `links`** | ✅ D-083 | Schema v7: `CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')`. Migration rebuilds `links`. Guard stays for cold files. |
| **T2.2 — `rowid_pk INTEGER PRIMARY KEY` on `concepts`** | ⏸ Deferred | Bundled with concept archival/erasure. Not inert — blocks dense-rowid argument for FTS5. |
| **T2.3 — Overlap guard stays in actor** | ✅ Confirmed | Actor probe confirmed; moving to trigger would add second probe on hot path. §4.7 closed. |

### Tier 3 — Read Path (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T3.1 — One `HYDRATE_CHUNK`** | ✅ Folded | Constant moved to `util::limits`. Margin check became `const` block. |
| **T3.2 — `AttributeMode::Current` + `as_of` is error** | ✅ D-085 | `DbError::AttributeModeUnstated` when `as_of` set and mode defaulted. `TraversalBuilder::as_of(ts)` + `attribute_mode: Option<AttributeMode>`. |
| **T3.3 — `Subgraph` interning** | ⏸ Deferred to 0.7.0 | Gate passed (3,066→21,845 edges/MiB). Breaking change to public type. |
| **T3.4 — Pipeline bulk paths** | ❌ Removed | Measured at ~1% gain. Breaks prefix-commit guarantee for nothing measurable. Shared `low_chunked` loop kept. |

### Tier 4 — Measurement Discipline (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T4.1 — Fixture matrix** | ✅ D-088 | Four shapes: `star_of_stars`, `chain`, `clustered`, `dense_small`. Every performance decision must name its fixture. |
| **T4.2 — Plan-pinning as test** | ✅ D-089 | `tests/index_plan_tests.rs` — every index must name its query. Found 2 dead indexes (`idx_annotations_label`, `idx_lc_tgt_active`). |
| **T4.3 — Control row in every bench** | ✅ D-090 | `controlled_group` enforces control row. Absorbs ~90% of session noise. |

### Tier 5 — 1.0 API & Operations (✅ COMPLETE)

| Item | Status | What changed |
|---|---|---|
| **T5.1 — `diagnostic_conn()` + gated `raw()`** | ✅ D-091 | `diagnostic_conn()` opens OS-level read-only. `raw()` is `#[doc(hidden)]`. `CREATE TEMP TABLE` succeeds on read connection (not diagnostic). |
| **T5.2 — R15: defend load-bearing claim** | ✅ D-092 | Soak test: 0/16 faults at 8.7 min under load. Control arm (48 concurrent opens) faulted 2/10. R15 reworded. Upstream report still open. |
| **T5.3 — Snapshot chain cross-check** | ✅ D-092 | `verify_snapshot_chain(ts)` folds from genesis, compares to composed answer. Reports and does not repair. |

---

## 9. Architecture vs. Code — Comparison

### What the docs say vs. what the code does

| Area | Documented | In Code | Match? |
|---|---|---|---|
| **Write Actor** | Single write connection inside dedicated Tokio task; two-tier biased channels | `src/connection.rs` — `run_writer_actor` with `high_pri_rx`/`low_pri_rx`, `tokio::select!` with `biased` | ✅ |
| **Clock injection** | `Clock` trait; `SystemClock` floors to `MAX(recorded_at)`; `FakeClock` for tests | `src/util/clock.rs` — `Clock` trait, `SystemClock::new` queries `MAX(recorded_at)`, `FakeClock` advances explicitly | ✅ |
| **Two-tier channels** | High-priority: user-driven. Low-priority: background. Bounded backpressure. | `src/connection.rs` — `HighPriCommand`/`LowPriCommand` enums, `mpsc::channel(256)`/`mpsc::channel(64)` | ✅ |
| **Cooperative chunking** | Low-priority workers chunk; golden rule: ≤3 ms per chunk | `src/connection.rs` — `CHUNK_BUDGET`, per-path constants (90/70/600/30), `low_chunked` shared loop | ✅ |
| **CTE traversal** | `UNION` (not `UNION ALL`) dedupes on entry; no path column | `src/graph/builder.rs` — `build_sql` uses `UNION`, no `path` column, no `INSTR` | ✅ |
| **Vector filter strategies** | `PostFilter` + `PreFilterCTE`; `TwoPhaseTempTable` removed | `src/graph/vector_filter.rs` — `VectorFilterStrategy::PostFilter`/`PreFilterCTE`, no `TwoPhaseTempTable` | ✅ |
| **Byte budget** | `CostEstimator` reads `byte_budget`; `SubgraphTooLarge` raised | `src/graph/vector_filter.rs` + `src/graph/subgraph.rs` — `CostEstimator.estimate` returns `CostEstimate`, `SubgraphTooLarge` constructed | ✅ |
| **Snapshot cadence** | `watch` channel; stops on drop; cadence at `MAX(recorded_at)` every 10K entries | `src/temporal/snapshot.rs` — `watch` channel, cadence triggered by entry count, writes at `MAX(recorded_at)` | ✅ |
| **Snapshot retention** | Newest-five-flat (not "last five plus one daily for 30 days") | `src/temporal/snapshot.rs` — retention keeps newest five; format v2 header carries snapshot instant for daily bucketing | ✅ (partially: format v2 daily tier implemented) |
| **Archive windowing** | `archive_windowed(cutoff, window)` — N small atomic sessions | `src/temporal/archive.rs` — `archive_windowed` with session loop, skips rebuild when nothing archived | ✅ |
| **Chunked shadow rebuild** | `rebuild_current_chunked` — shadow-swap with catch-up | `src/integrity/shadow.rs` — `rebuild_current_chunked`, catch-up by `recorded_at`, interlock against archive | ✅ |
| **Actor metrics** | Queue depth, per-kind hold histogram, over-budget count, longest hold | `src/metrics.rs` — `ActorMetrics` with `HoldTimer`, `CommandKind` histogram, feature-gated | ✅ |
| **Hybrid search** | RRF fusion at k=60; each arm to `max(5×top_k, 50)` | `src/vector/hybrid.rs` — `reciprocal_rank_fusion`, break ties by id, FTS5 escape | ✅ |
| **`diagnostic_conn()`** | OS-level read-only connection; stronger than `query_only` | `src/connection.rs` — `diagnostic_conn` opens with `SQLITE_OPEN_READONLY` | ✅ |
| **`raw()` hidden** | `#[doc(hidden)]` or gated behind feature | `src/connection.rs` — `raw()` is `#[doc(hidden)]` | ✅ |
| **Schema v7 weight check** | `CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')` | `src/schema/ddl.rs` — `CREATE_LINKS_TABLE` includes v7 CHECK | ✅ |
| **`AttributeMode::Current` + `as_of` error** | `DbError::AttributeModeUnstated` when mode defaulted with `as_of` set | `src/graph/builder.rs` — `attribute_mode: Option<AttributeMode>`, `as_of(ts)` method, error raised in execute | ✅ |
| **Fixture matrix** | Four shapes: tree, clustered, chain, dense | `tests/fixtures.rs` — `Shape::StarOfStars`, `Clustered`, `Chain`, `DenseSmall` | ✅ |
| **Index plan tests** | Every index must name its query | `tests/index_plan_tests.rs` — `every_index_is_justified` asserts against `CREATE_INDICES` | ✅ |
| **Snapshot chain verification** | `verify_snapshot_chain(ts)` folds from genesis, compares | `src/connection.rs` — `verify_snapshot_chain` with `ChainReport`, tampered snapshot test | ✅ |
| **R15 soak test** | One long-lived `Database`, heavy concurrent read load | `examples/r15_soak.rs` — `--arm claim`/`--arm control`, subprocess runner | ✅ |

### What's documented but not yet in code

| Item | Status | Notes |
|---|---|---|
| **T3.3 — `Subgraph` interning** | ⏸ Deferred to 0.7.0 | Gate passed; breaking change to public type |
| **T2.2 — `rowid_pk` on `concepts`** | ⏸ Deferred with archival/erasure | Blocks dense-rowid FTS5 argument |
| **Upstream R15 report** | Still open | Raw reproduction written; needs GitHub credentials |

### What's in code but not in docs

| Item | Location | Notes |
|---|---|---|
| **`write_bulk_atomic` quadratic fan-out model** | `src/connection.rs` | Measured coefficients: `73 µs · rows + 5.5 ns · mismatched + 86 ns · matching` |
| **`estimated_bulk_hold()` public method** | `src/connection.rs` | Caller can predict hold before committing |
| **`bulk_chunks` benchmark group** | `benches/budgets.rs` | Measures all four bulk paths at 500 rows |
| **`fixture_matrix` bench group** | `benches/budgets.rs` | Four shapes at comparable coverage |
| **`controlled_group` bench constructor** | `benches/budgets.rs` | Enforces control row in every bench group |
| **`tests/bench_control_tests.rs`** | Tests | Keeps back door to `BenchmarkGroup` shut |
| **`tests/index_coverage_probe.rs`** | Examples | Confirms dead indexes would be chosen by query of obvious shape |
| **`r15_soak.rs` control arm** | Examples | 48 concurrent opens against long-lived database |

---

## 10. Known Risks & Mitigations

| Risk | Severity | Mitigation | Status |
|---|---|---|---|
| **R15: Concurrent open → `STATUS_ACCESS_VIOLATION`** | High | `RUST_TEST_THREADS = "1"`; soak test defends claim | ⚠️ Mitigated; upstream report open |
| **Property test binaries fault in suite** | Medium | `property-tests` feature gate; serialised runs | ✅ Quarantined |
| **Fixture shape bias** | Medium | Four-shape fixture matrix; every decision names fixture | ✅ D-088 |
| **Covering index wins over selective** | High | `EXPLAIN QUERY PLAN` assertions on every index | ✅ D-042, D-059, D-064 |
| **Superlinear chunk cost on large tables** | Medium | Index on `(source_id, target_id, edge_type, valid_to, valid_from)` shipped as v5→v6 | ✅ D-059 |
| **Snapshot chain divergence** | Low | `verify_snapshot_chain()` reports but does not repair | ✅ D-092 |

---

## 11. Testing Strategy

| Layer | What it proves | Location |
|---|---|---|
| **Unit tests** | Pure functions, invariants, error shapes | `src/*/tests::` |
| **Integration tests** | End-to-end paths through the handle | `tests/*.rs` |
| **Property tests** | Generated histories; invariants hold under random mutation | `tests/*_property_tests.rs` (feature: `property-tests`) |
| **Benchmarks** | Performance budgets; regression detection via baselines | `benches/budgets.rs` |
| **Soak tests** | R15 claim defended under sustained load | `examples/r15_soak.rs` |
| **Diagnostic examples** | Measurement and diagnosis; not part of test suite | `examples/*.rs` |

**Total**: 240 tests (221 plain + 19 property)

---

## 12. Quick Decision Reference

| Decision | Reference | Rationale |
|---|---|---|
| **One writer, not one entry point** | D-016 | A caller-held closure could hold the write lock arbitrarily long |
| **`UNION` not `UNION ALL` in CTE** | D-076 | Bounds walk at `V × (depth+1)`; simple-path reachability equals walk reachability within D |
| **`PostFilter` + `PreFilterCTE`, no `TwoPhaseTempTable`** | D-050 | `CREATE TEMP TABLE` fails on `query_only` connection; `vector_top_k` refuses 4th arg |
| **`as_of(ts)` + `AttributeMode::Current` is error** | D-085 | `Current` returns live text; `AtTime` returns historical text; conflating them is wrong |
| **`raw()` is `#[doc(hidden)]`** | D-091 | Leaves three §4.7 gaps open; provoking a guard is its legitimate use |
| **`write_bulk_atomic` uncapped** | D-014 | Capping breaks the guarantee the method exists to provide |
| **Archive windowing not default** | D-080 | Windowing costs more total work; only pays when backlog is large |
| **Snapshot chain: report, don't repair** | D-092 | Under Doctrine VI a snapshot is disposable; repair evidence would be destroyed |
| **Metrics feature-gated, zero cost when off** | D-079 | `HoldTimer` reads no clock; `ActorMetrics` is empty ZST |

---

*Last updated: 2026-07-31 · v0.6.0*
