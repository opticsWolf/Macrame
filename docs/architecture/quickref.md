# Macrame — Architecture Quick Reference

**v0.6.0 · A Bitemporal Graph Ledger on libSQL**

---

## 1. Overview

Macrame is a domain-specific embedded database layer for a knowledge-ledger application: a system in which concepts are linked by typed, weighted relationships, both concepts and relationships change over time, and the history of those changes is itself a first-class asset.

Delivered as a single Rust crate that an application links directly. The entire database is one file on the local filesystem — no server, no network protocol, no external service.

### Five Capabilities

| Capability | Mechanism |
|---|---|
| Graph storage & traversal | Recursive CTEs over relational edge tables, compiled from a typed builder |
| Bitemporal semantics | Two independent clocks per row — valid time and transaction time — enforced by engine triggers |
| Native vector search | Per-model `F32_BLOB` tables with auto-maintained DiskANN indexes; `vector_top_k` + `vector_distance_cos` |
| In-memory graph analytics | Dijkstra, A*, SCC, k-core, Louvain — native adjacency-list `Subgraph`, no external graph dependency |
| Point-in-time reconstruction | Append-only `transaction_log` folded with window functions; snapshot composition for fast replay |

### Two Semantic Operations

- **`as_of(ts)`** — *valid-time* question answered under current belief. Reports what the world looked like at `ts` given everything we know now, including corrections recorded after `ts`. A filtered read of live tables — cheap.
- **`reconstruct(ts)`** — *transaction-time* question. Replays the log and reports what the database actually believed at `ts`, before later corrections arrived. A fold over history — costs what history costs.

Both are correct answers to different questions. Conflating them is a defect.

---

## 2. Principles (Doctrine)

Before any mechanism, this architecture is defined by eight invariants. Every design decision derives from them; every code review should begin by asking which of them a change touches.

### I. The boundary is sacred
Everything above libSQL is ours — schema generation, query compilation, temporal logic, the API. Everything below it is upstream. We never patch the engine, never fork the C core, never depend on undocumented engine internals.

**Code:** `src/schema/ddl.rs`, `src/connection.rs`

### II. Two clocks, never mixed
Every row carries time on two independent axes: valid time (`valid_from`/`valid_to` — when a fact held in the world) and transaction time (`recorded_at` — when the database learned it). No trigger, no default, no code path may ever derive one from the other.

**Code:** `src/util/clock.rs`, `src/temporal/`

### III. Assertions are immutable
Rows in `links` are assertions — statements of belief about an interval — and are never updated in place. Changing belief means inserting a new assertion with a fresh `recorded_at`. The past is never rewritten; it is only ever superseded.

**Code:** `src/schema/ddl.rs` (triggers), `src/connection.rs`

### IV. The ledger is a table, not the log
Transaction-time reconstruction reads exactly one structure: `transaction_log`, an append-only table captured by engine triggers. We do not read libSQL's WAL, replication frames, or any CDC facility.

**Code:** `src/schema/ddl.rs`, `src/temporal/replay.rs`

### V. No physical deletion in hot tables
Rows leave the hot database only through the archive path, which runs inside a declared archive session and is verified before anything is removed. An ad-hoc `DELETE` issued from any other client aborts at the trigger layer.

**Code:** `src/schema/ddl.rs` (guards), `src/temporal/archive.rs`

### VI. Derivative state is disposable
`links_current` is a materialization — a cache of current belief — and is rebuildable from `links` at any moment by a single deterministic query. Because it can be rebuilt, it can be trusted: drift is detectable by audit, recoverable by rebuild.

**Code:** `src/integrity/shadow.rs`, `src/schema/ddl.rs`

### VII. Embeddings are immutable per version and excluded from the ledger
A vector is a derived artifact of a specific model applied to specific content. It never appears in `transaction_log` payloads; it lives in per-model tables so that a model migration can never produce a row whose dimension violates its type.

**Code:** `src/vector/registry.rs`, `src/vector/mod.rs`

### VIII. Fidelity is a parameter, never a silent default
Queries that mix time axes say so in their signatures. `as_of(ts)` means valid time under current belief; `reconstruct(ts)` means belief as of `ts`. The gap between the two — retroactive assertions made after `ts` — is documented, pinned by tests, and surfaced at the type level.

**Code:** `src/temporal/as_of.rs`, `src/temporal/replay.rs`

---

## 3. Architecture

### 3.1 System Context

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

### 3.2 Concurrency Model

- **One process, one file** — embedded, no server
- **One writer** — the Write Actor task holds the sole write-capable connection
- **Many readers** — WAL journaling; readers never block on writer
- **Two-tier priority channels** — high-priority (user-driven) preempts low-priority (background)
- **Cooperative chunking** — low-priority transactions bounded to per-path constants (90 edges, 70 concepts, 600 annotations, 30 embeddings)
- **`PRAGMA query_only = ON`** — read connection enforced at engine level (not just Rust ownership)

### 3.3 Module Map

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

## 4. Schema

### 4.1 Core Tables

| Table | Purpose | Key columns |
|---|---|---|
| `concepts` | Mutable entities with attributes | `id TEXT PK`, `valid_from/to`, `recorded_at`, `retired` |
| `links` | Full bitemporal edge history | PK: `(source_id, target_id, edge_type, valid_from, recorded_at)` |
| `links_current` | Materialized current belief (rebuildable) | Latest assertion per interval |
| `transaction_log` | Append-only replay log | `seq_id`, `entity_id`, `operation`, `recorded_at`, `payload` |
| `analytics_annotations` | Second derivative (analytics output) | `concept_id`, `label`, `value` |
| `concepts_fts` | FTS5 external-content index | Tokenized text, no duplication |
| `embeddings_*` | Per-model vector tables | `F32_BLOB(n)` with DiskANN index |

### 4.2 Timestamp Form (normative)

Every temporal column is exactly 27 characters: `YYYY-MM-DDTHH:MM:SS.ffffffZ`

- Fixed width ensures lexicographic ordering equals chronological ordering
- Open-interval sentinel: `9999-12-31T23:59:59.999999Z`
- Enforced by `CHECK` (GLOB pattern) on all four tables
- Second-precision input is widened at the boundary (`util::timestamp::normalize`)

### 4.3 Schema Versioning

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

## 5. Public API

### 5.1 Database Handle

```rust
pub struct Database { /* opaque */ }

// Open / close
impl Database {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self>
    pub async fn open_with_cadence(path, cadence: Option<SnapshotCadence>) -> Result<Self>
    pub async fn open_with_clock(path, clock: Arc<dyn Clock>) -> Result<Self>
    pub async fn close(self) -> Result<()>
    pub fn read_conn(&self) -> &libsql::Connection       // WAL reader
    pub fn path(&self) -> &Path                           // File path
    pub fn diagnostic_conn(&self) -> Result<Connection>   // OS-level read-only
    pub fn raw(&self) -> &libsql::Database                // #[doc(hidden)]
    pub fn clock(&self) -> &Arc<dyn Clock>                // Clock reference
    pub fn schema_version(&self) -> u32                   // Current schema version
    pub fn archive_path(&self) -> &Path                   // Archive file path
    pub fn snapshots_dir(&self) -> &Path                  // Snapshot directory
    pub fn metrics(&self) -> MetricsSnapshot              // Actor metrics (feature: metrics)
    pub async fn verify_snapshot_chain(ts: &str) -> Result<ChainCheck>
}
```

**`diagnostic_conn()`**: Opens an OS-level read-only connection. Stronger than `read_conn()` because `PRAGMA query_only` can be reversed; `diagnostic_conn()` cannot.

**`raw()`**: `#[doc(hidden)]` — exposes the raw `libsql::Database` handle. Left public to provoke a guard (§4.7 invariant 2).

**`verify_snapshot_chain(ts)`**: Folds from genesis by withholding the snapshot directory, compares against the composed answer. Reports and does not repair — under Doctrine VI a snapshot is disposable.

### 5.2 Concepts

```rust
pub struct ConceptUpsert {
    pub id: String,
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
    pub valid_from: String,
    pub valid_to: String,
    pub retired: bool,
}

impl ConceptUpsert {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self
    pub fn content(mut self, content: impl Into<String>) -> Self
    pub fn embedding_model(mut self, model: impl Into<String>) -> Self
    pub fn valid_from(mut self, ts: impl Into<String>) -> Self
    pub fn valid_to(mut self, ts: impl Into<String>) -> Self
    pub fn retired(mut self, retired: bool) -> Self
    pub fn normalized(mut self) -> Result<Self>
}

impl Database {
    pub async fn upsert_concept(concept: ConceptUpsert) -> Result<()>
    pub async fn write_concepts(concepts: Vec<ConceptUpsert>) -> Result<usize>
}
```

**`normalized()`**: Validates and normalizes the concept — edge types are uppercased alphanumeric, identifiers are validated, timestamps are canonicalized.

**`write_concepts()`**: Low-priority chunked write for analytics write-back. Each chunk commits under its own `recorded_at`; not transaction-time atomic.

### 5.3 Edges

```rust
pub struct EdgeAssertion {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    pub valid_from: String,
    pub valid_to: String,
    pub weight: f64,
    pub properties: String,  // JSON
}

impl Database {
    pub async fn assert_edge(edge: EdgeAssertion) -> Result<()>
    pub async fn retire_edge(source, target, edge_type, valid_from, valid_to) -> Result<()>
    pub async fn write_bulk_atomic(edges: Vec<EdgeAssertion>) -> Result<usize>
    pub async fn bulk_import(edges: Vec<EdgeAssertion>) -> Result<usize>
}
```

**`assert_edge()`**: High-priority, single transaction. Latency: < 5 ms normal, up to ~50 s if a rebuild or archive is in flight.

**`retire_edge()`**: Inserts a successor assertion with `valid_to` set to the retirement time. The original row is preserved; the new row supersedes it.

**`write_bulk_atomic()`**: One transaction, one stamp, one stall. Uncapped — the caller sizes it. Warns above `BULK_ATOMIC_WARN_HOLD` (250 ms). Use when the batch must be visible all-at-once or not at all.

**`bulk_import()`**: Same as `write_bulk_atomic` but low-priority and chunked. Not transaction-time atomic overall.

### 5.4 Traversal & Subgraph

```rust
pub enum AttributeMode { Current, AtTime, Omit }

pub struct TraversalBuilder {
    pub start_node: String,
    pub max_depth: usize,
    pub edge_types: Vec<String>,
    pub min_weight: f64,
    pub attribute_mode: Option<AttributeMode>,
    pub as_of: Option<String>,
}

impl TraversalBuilder {
    pub fn new(start_node: impl Into<String>) -> Self
    pub fn max_depth(mut self, depth: usize) -> Self
    pub fn edge_types(mut self, types: Vec<String>) -> Self
    pub fn min_weight(mut self, weight: f64) -> Self
    pub fn attribute_mode(mut self, mode: AttributeMode) -> Self
    pub fn as_of(mut self, ts: impl Into<String>) -> Self
    pub fn build_sql(&self) -> String
    pub async fn execute_ids(&self, conn: &libsql::Connection) -> Result<Vec<String>>
    pub async fn execute(&self, conn: &libsql::Connection) -> Result<MaterializedState>
}

pub struct Subgraph {
    pub nodes: BTreeMap<String, NodeData>,
    pub out_adj: BTreeMap<String, Vec<EdgeRef>>,
    pub in_adj: BTreeMap<String, Vec<EdgeRef>>,
}

pub struct NodeData {
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
    pub valid_from: String,
    pub valid_to: String,
}

pub struct EdgeRef {
    pub node: String,
    pub edge_type: String,
    pub weight: f64,
    pub valid_from: String,
    pub valid_to: String,
}

impl Subgraph {
    pub fn out_edges(&self, node: &str) -> &[EdgeRef]
    pub fn in_edges(&self, node: &str) -> &[EdgeRef]
    pub fn degree(&self, node: &str) -> usize
    pub fn weighted_degree(&self, node: &str) -> f64
    pub fn total_weight(&self) -> f64
    pub fn edge_count(&self) -> usize
    pub fn is_closed(&self) -> bool
    pub fn estimated_bytes(&self) -> usize
    pub async fn write_back_annotations(&self, db, label, values) -> Result<usize>
    pub async fn load_subgraph(start_node, max_hops, ts, byte_budget) -> Result<Self>
    pub async fn load_subgraph_with(filter) -> Result<Self>
}

// Algorithms
pub fn dijkstra(graph, source, target, max_cost) -> Result<Option<(f64, Vec<String>)>>
pub fn astar(graph, source, target, heuristic) -> Result<Option<(f64, Vec<String>)>>
pub fn scc(graph) -> Result<Vec<BTreeSet<String>>>
pub fn k_core(graph, k) -> Result<Subgraph>
pub fn louvain(graph) -> Result<BTreeMap<String, String>>
pub fn modularity(graph, partition) -> Result<f64>

impl Database {
    pub fn traverse() -> TraversalBuilder
}
```

**`as_of(ts)`**: Sets a valid-time query. The traversal returns topology at `ts` under current belief.

**`attribute_mode`**: `Current` returns live attributes (fast, wrong for historical text). `AtTime` hydrates from `transaction_log` (correct for historical text). `Omit` returns topology only. When `as_of` is set and mode is defaulted, returns `DbError::AttributeModeUnstated`.

**`load_subgraph()`**: Walks `links_current` under the same bounded CTE shape as traversal, hydrates node attributes, returns a `Subgraph`. Enforces byte budget (`SubgraphTooLarge`) and negative/NaN weight refusal.

**Algorithms**: Dijkstra, A*, SCC, k-core, Louvain — all operate on `Subgraph`. Deterministic via `BTreeMap`/`BTreeSet`; ties broken explicitly.

### 5.5 Temporal Queries

```rust
pub struct MaterializedState {
    pub seq_anchor: i64,
    pub timestamp: String,
    pub concepts: HashMap<String, NodeAttributes>,
    pub edges: Vec<(String, String, String, String, String)>,
}

pub struct ArchiveReport {
    pub links_archived: usize,
    pub log_entries_archived: usize,
    pub horizon: Option<i64>,
}

pub struct ChainCheck {
    pub timestamp: String,
    pub composed_anchor: i64,
    pub folded_anchor: i64,
    pub composed_concepts: usize,
    pub folded_concepts: usize,
    pub composed_edges: usize,
    pub folded_edges: usize,
    pub concept_disagreements: Vec<String>,
    pub edge_disagreements: Vec<String>,
    pub truncated: bool,
    pub fn diverged(&self) -> bool,
}

pub struct SnapshotCadence {
    pub every_entries: i64,
    pub poll_interval: Duration,
}

// Standalone functions
pub async fn query_as_of_edges(conn, ts, filter) -> Result<MaterializedState>
pub async fn reconstruct(conn, ts, archive_path, snapshots) -> Result<MaterializedState>
pub async fn archive(cutoff: &str) -> Result<ArchiveReport>
pub async fn archive_windowed(cutoff, window) -> Result<ArchiveReport>
pub async fn save_snapshot(snapshots_dir, state) -> Result<PathBuf>
pub async fn load_snapshot(path) -> Result<MaterializedState>
pub async fn write_final(read_conn, snap_dir) -> Result<()>
pub async fn cleanup_expired_snapshots(snapshots_dir) -> Result<usize>
pub async fn audit_current(conn) -> Result<i64>
pub async fn rebuild_current() -> Result<RebuildReport>
pub async fn rebuild_current_chunked() -> Result<RebuildReport>
pub async fn hydrate_attributes(conn, ids, ts, mode) -> Result<HashMap<String, NodeAttributes>>
```

**`query_as_of_edges()`**: Valid-time query over `links_current`. Returns topology at `ts` under current belief.

**`reconstruct()`**: Transaction-time replay from `transaction_log`. Composes from newest snapshot at or before `ts` plus anchored fold. Requires `archive_path` if history extends before the archive cutoff.

**`archive(cutoff)`**: Moves closed intervals to cold database. One atomic session — copy-then-delete.

**`archive_windowed(cutoff, window)`**: N small atomic sessions instead of one. Longest hold reduced (3,326→768 ms at 8K keys). Session skips rebuild when nothing archived.

**`save_snapshot()` / `load_snapshot()`**: Compose and deserialize `MaterializedState` to/from disk. Format v2 header carries snapshot instant for retention bucketing.

**`write_final()`**: Called on `close()` — folds the tail from the read side and writes the final snapshot.

**`audit_current()`**: Returns symmetric difference between `links` and `links_current`. Zero means in sync.

**`rebuild_current()`**: One act, audits itself, works inside a caller's transaction. O(E) delete + O(E log E) window reprojection + two audit passes.

**`rebuild_current_chunked()`**: Shadow-swap with catch-up. Longest hold 353→47 ms (7.6×). 2.3× cheaper total. Interlock against archive interleaving via `RebuildInterrupted`.

### 5.6 Vector Search

```rust
pub struct ModelName(String);  // validated as SQL identifier

impl ModelName {
    pub fn new(raw: impl AsRef<str>) -> Result<Self>
    pub fn as_str(&self) -> &str
    pub fn table(&self) -> String
    pub fn index(&self) -> String
}

pub struct VectorSearchResult {
    pub concept_id: String,
    pub score: f64,  // cosine distance
}

pub struct HybridHit {
    pub concept_id: String,
    pub score: f64,
    pub vector_rank: Option<usize>,
    pub keyword_rank: Option<usize>,
}

impl Database {
    pub async fn register_model(model: &ModelName, dim: usize) -> Result<()>
    pub async fn registered_models(conn) -> Result<Vec<ModelName>>
    pub async fn declared_dimension(conn, model) -> Result<usize>
    pub async fn upsert_embedding(model, concept_id, vector: &[f32]) -> Result<()>
    pub async fn upsert_embeddings(model, rows) -> Result<usize>
    pub async fn search_vector(query, model, k) -> Result<Vec<VectorSearchResult>>
    pub async fn rebuild_fts() -> Result<()>
}

// Filtered vector search
pub enum VectorFilterStrategy { PostFilter, PreFilterCTE }
pub enum CandidateCount { Exact(usize), AtLeast(usize) }
pub struct CostEstimate {
    pub strategy: VectorFilterStrategy,
    pub candidates: CandidateCount,
    pub post_filter_bytes: usize,
    pub pre_filter_bytes: usize,
    pub k_prime: usize,
}
pub struct CostEstimator {
    pub byte_budget: usize,
    pub corpus: usize,
    pub vector_bytes: usize,
}

impl CostEstimator {
    pub fn new(byte_budget, corpus, vector_bytes) -> Self
    pub fn k_prime(k, candidates) -> usize
    pub fn estimate(k, candidates) -> Result<CostEstimate>
}

pub struct FilteredVectorSearch {
    // builder pattern
}

impl FilteredVectorSearch {
    pub fn new(model, query, traversal) -> Self
    pub fn top_k(mut self, k: usize) -> Self
    pub fn byte_budget(mut self, budget: usize) -> Self
    pub fn probe_cap(mut self, cap: usize) -> Self
    pub fn strategy(mut self, strategy: VectorFilterStrategy) -> Self
    pub async fn execute_explained(&self, conn, ts) -> Result<(Vec<VectorSearchResult>, CostEstimate)>
}

// Hybrid search
pub const RRF_K: usize = 60
pub struct HybridSearch { /* builder */ }

impl HybridSearch {
    pub fn new(model, query_text, query_vector) -> Self
    pub fn top_k(mut self, k: usize) -> Self
    pub fn depth(mut self, depth: usize) -> Self
    pub fn rrf_k(mut self, k: usize) -> Self
    pub fn raw_match(mut self, raw: bool) -> Self
    pub async fn execute(&self, conn) -> Result<Vec<HybridHit>>
}

pub fn reciprocal_rank_fusion(vector_ranks, keyword_ranks, corpus_size) -> Result<Vec<HybridHit>>
pub async fn keyword_search(conn, query, model, k, ts) -> Result<Vec<HybridHit>>
pub fn escape_fts5_query(input: &str) -> String
```

**`register_model()`**: Creates per-model embedding table + DiskANN index in one transaction. Model names are validated as SQL identifiers.

**`search_vector()`**: Calls `vector_top_k` and `vector_distance_cos`. Returns cosine distances.

**`FilteredVectorSearch`**: Two strategies — `PostFilter` (retrieve generous k′, then post-filter) and `PreFilterCTE` (materialize candidate set, then exact distance scan). `TwoPhaseTempTable` removed (D-050). Strategy chosen by `CostEstimator` based on byte budget. Strategy may never change the answer.

**`HybridSearch`**: Fuses vector and keyword arms by RRF at k=60. Each arm read to `max(5×top_k, 50)`. Ties broken by id. FTS5 syntax escaped before reaching MATCH.

### 5.7 Integrity

```rust
pub enum ShadowStep {
    BuildStart,
    BuildChunk { source_id_range: (i64, i64) },
    BuildIndex,
    CatchUp { recorded_at: String },
    Swap { triggers_dropped: bool },
    FinalCatchUp,
}

pub enum ShadowOutcome {
    Complete { edges_rebuilt: usize },
    Interrupted { archive_count_at_build: i64, archive_count_at_swap: i64 },
    Failed { reason: String },
}

pub struct RebuildReport {
    pub edges_rebuilt: usize,
    pub audit_drift: i64,
    pub steps: Vec<ShadowStep>,
    pub outcome: ShadowOutcome,
}

pub async fn audit_current(conn) -> Result<i64>
pub async fn rebuild_current() -> Result<RebuildReport>
```

### 5.8 Metrics (feature: `metrics`)

```rust
pub enum CommandKind {
    AssertEdge,
    RetireEdge,
    UpsertConcept,
    WriteBulkAtomic,
    WriteConceptsChunk,
    BulkImportChunk,
    Archive,
    RebuildCurrent,
    RegisterModel,
    UpsertEmbeddingChunk,
    WriteAnalyticsChunk,
    Shutdown,
}

pub struct MetricsSnapshot {
    pub turns: u64,
    pub depth_samples: u64,
    pub high_depth_mean: f64,
    pub high_depth_max: u64,
    pub low_depth_mean: f64,
    pub low_depth_max: u64,
    pub longest: Option<(CommandKind, Duration)>,
    pub kinds: Vec<KindSnapshot>,
}

pub struct KindSnapshot {
    pub kind: CommandKind,
    pub turns: u64,
    pub over_budget: u64,
    pub mean: Duration,
    pub longest: Duration,
    pub buckets: [u64; BUCKET_COUNT],
}

impl Database {
    pub fn metrics(&self) -> MetricsSnapshot
}
```

**`MetricsSnapshot`**: Queue depth (mean + high-water), per-kind hold histogram (bucket boundary at `CHUNK_BUDGET`), over-budget count, longest hold with command kind. Feature-gated — `HoldTimer` reads no clock when feature is off.

### 5.9 Utility

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> String;
}

pub struct SystemClock { /* floors to MAX(recorded_at) */ }
pub struct FakeClock { /* advances explicitly */ }

pub fn generate_id() -> String
pub fn validate_id(id: &str) -> Result<()>
pub fn is_canonical(s: &str) -> bool
pub fn normalize(s: &str) -> Result<String>
pub fn parse(s: &str) -> Result<SystemTime>
pub fn format(st: SystemTime) -> String

pub const TIMESTAMP_LEN: usize = 27
pub const OPEN_SENTINEL: &str = "9999-12-31T23:59:59.999999Z"
pub const HYDRATE_CHUNK: usize = 400
```

**`Clock` trait**: Injectable clock for deterministic testing. `SystemClock` enforces monotonicity by flooring to `MAX(recorded_at)`. `FakeClock` advances explicitly.

**`normalize()`**: Widens second-precision timestamps to canonical form. Rejects offsets, missing Z, millisecond precision.

---

## 6. Performance

### 6.1 Chunking Constants (per path)

| Path | Chunk Size | Measured at | Per-row cost |
|---|---|---|---|
| Edges (`write_edges_atomic`) | 90 | ~2.39 ms | ~11 µs (empty db), superlinear on degree |
| Concepts (`write_concepts_atomic`) | 70 | ~2.35 ms | ~2.5 µs |
| Annotations (`write_annotations_atomic`) | 600 | ~2.36 ms | ~2.5 µs |
| Embeddings (`upsert_embedding_chunk`) | 30 | ~2.06 ms | ~135 µs (DiskANN insertion) |

**Bound**: 3 ms (`CHUNK_BUDGET`). Per-transaction overhead: ~0.8 ms (BEGIN, COMMIT, fsync).

### 6.2 Performance Budgets (§9)

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

## 7. Known Risks & Mitigations

| Risk | Severity | Mitigation | Status |
|---|---|---|---|
| **R15: Concurrent open → `STATUS_ACCESS_VIOLATION`** | High | `RUST_TEST_THREADS = "1"`; soak test defends claim | ⚠️ Mitigated; upstream report open |
| **Property test binaries fault in suite** | Medium | `property-tests` feature gate; serialised runs | ✅ Quarantined |
| **Fixture shape bias** | Medium | Four-shape fixture matrix; every decision names fixture | ✅ D-088 |
| **Covering index wins over selective** | High | `EXPLAIN QUERY PLAN` assertions on every index | ✅ D-042, D-059, D-064 |
| **Superlinear chunk cost on large tables** | Medium | Index on `(source_id, target_id, edge_type, valid_to, valid_from)` shipped as v5→v6 | ✅ D-059 |
| **Snapshot chain divergence** | Low | `verify_snapshot_chain()` reports but does not repair | ✅ D-092 |

---

## 8. Testing Strategy

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

## 9. Decision Reference

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
