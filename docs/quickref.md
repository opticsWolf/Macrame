# Macrame — Architecture Quick Reference

**v0.12.0 · A Bitemporal Graph Ledger on libSQL**

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

- **`as_of_valid(ts)`** — *valid-time* question answered under current belief. Reports what the world looked like at `ts` given everything we know now, including corrections recorded after `ts`. A filtered read of live tables — cheap.
- **`as_of_recorded(ts)`** — *transaction-time* question on a traversal (0.13.2, [D-174](architecture/s13-decision-register.md#d-174)). Folds `transaction_log` to `ts` and walks the topology the ledger held then. Setting it together with `as_of_valid` asks the bitemporal question — *what did we believe at `r` about what was true at `v`* — which nothing in the crate could express before. Raises `RecordedInstantUnreachable` once the hot log has been archived; `reconstruct` takes the archive path.
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
`links_current` is a materialization — a cache of current belief — and is rebuildable from `links` at any moment by a single deterministic query. Because it can be rebuilt, it can be trusted: drift is detectable by audit, recoverable by rebuild (atomic via `rebuild_current()` or chunked via `rebuild_current_chunked()` with shadow-swap).

**Code:** `src/integrity/rebuild.rs`, `src/integrity/shadow.rs`, `src/schema/ddl.rs`

### VII. Embeddings are immutable per version and excluded from the ledger
A vector is a derived artifact of a specific model applied to specific content. It never appears in `transaction_log` payloads; it lives in per-model tables so that a model migration can never produce a row whose dimension violates its type.

**Code:** `src/vector/registry.rs`, `src/vector/mod.rs`

### VIII. Fidelity is a parameter, never a silent default
Queries that mix time axes say so in their signatures. `as_of_valid(ts)` means valid time under current belief; `as_of_recorded(ts)` and `reconstruct(ts)` mean belief as of `ts`. The gap between them — retroactive assertions made after `ts` — is documented, pinned by tests, and surfaced at the type level. Through 0.13.1 a single `as_of(ts)` carried both clocks, so this doctrine was stated and not met ([D-174](architecture/s13-decision-register.md#d-174)).

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
- **Cooperative chunking** — low-priority transactions bounded to 3 ms, sized adaptively from each chunk's measured hold, under per-path ceilings (90 edges, 70 concepts, 600 annotations, 30 embeddings) and a 35-row floor
- **`PRAGMA query_only = ON`** — read connection enforced at engine level (not just Rust ownership)

### 3.3 Module Map

| Module | File(s) | Responsibility |
|---|---|---|
| `schema` | `schema/ddl.rs`, `schema/migrations.rs` | DDL generation, trigger/index creation, `user_version` migration runner |
| `graph` | `graph/builder.rs`, `graph/subgraph.rs`, `graph/vector_filter.rs` | CTE compilation, subgraph loading, vector filter strategies, byte budget |
| `temporal` | `temporal/replay.rs`, `temporal/snapshot.rs`, `temporal/as_of.rs`, `temporal/archive.rs` | `reconstruct()`, `as_of()`, snapshot cadence/retention, archive, `archivable_concepts()`, `rehydrate()` |
| `vector` | `vector/mod.rs`, `vector/registry.rs`, `vector/model.rs`, `vector/hybrid.rs` | Model registration, embedding upsert, DiskANN search, hybrid RRF fusion |
| `integrity` | `integrity/shadow.rs`, `integrity/rebuild.rs` | `audit_current()`, `rebuild_current()` (atomic), `rebuild_current_chunked()` (shadow-swap), `ShadowStep`/`ShadowOutcome` |
| `metrics` | `metrics.rs` | `ActorMetrics`, `HoldTimer`, `CommandKind`, `MetricsSnapshot` — feature-gated and zero-cost when off, except `HoldTimer`, which the chunk loop needs in every build (0.12.0) |
| `util` | `util/clock.rs`, `util/timestamp.rs`, `util/ids.rs`, `util/limits.rs` | `Clock` trait, timestamp normalization/parsing, ULID generation, `HYDRATE_CHUNK` (`CHUNK_BUDGET` is in `connection.rs`) |
| `connection` | `connection.rs` | `Database` handle, Write Actor, priority channels, `low_chunked()`, `ActorShared` |
| `error` | `error.rs` | `DbError` enum, error classification |
| `prelude` | `prelude.rs` | Re-exports `AttributeMode`, `EdgeAssertion`, `TraversalBuilder` (not `Subgraph` or algorithms) |

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
| v8 | `concepts.rowid_pk INTEGER PRIMARY KEY` + `id TEXT NOT NULL UNIQUE`; `concepts_fts` re-keyed to `content_rowid='rowid_pk'`; `trg_concepts_fts_delete` installed **inert**; `idx_annotations_label` and `idx_lc_tgt_active` dropped. Sets `suspends_foreign_keys` — the only rung that does (D-117, D-118, D-119) |
| v9 | `trg_concepts_guard_delete` becomes conditional on the archive-session marker, which is what lets a concept leave the hot table at all — and makes v8's inert FTS delete trigger fire. Trigger-only; no table touched (D-129) |
| v10 | `trg_concepts_log_insert` becomes conditional on the same marker, so a rehydration mints no transaction-time facts. Trigger-only. Required because the fold resolves by `seq_id`, not `recorded_at` (D-131) |

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

**`verify_snapshot_chain(ts)`**: Folds from genesis by withholding the snapshot directory, compares against the composed answer. Reports and does not repair — under Doctrine VI a snapshot is disposable. `seq_anchor` is reported but never compared (the composed answer and the fold legitimately differ); edges are compared as a *set*; results capped at `SAMPLE_LIMIT = 32` with a `truncated` flag (D-092).

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

**`write_concepts()`**: Low-priority chunked write for analytics write-back. Each chunk commits under its own `recorded_at`; not transaction-time atomic. Since 0.12.0 the chunk boundaries are machine-dependent, so the number of stamps is not reproducible across runs (§5.1.6).

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

**`bulk_import()`**: Same as `write_bulk_atomic` but low-priority and chunked, at most `chunk_rows::EDGES` rows at a time. Not transaction-time atomic overall, and the boundaries — hence the `recorded_at` stamps — depend on how fast the machine was.

### 5.4 Traversal & Subgraph

```rust
pub enum AttributeMode { Current, AtTime, Omit }

pub struct TraversalBuilder {
    pub start_node: String,
    pub max_depth: usize,
    pub edge_types: Vec<String>,
    pub min_weight: f64,
    pub attribute_mode: Option<AttributeMode>,
    pub as_of_valid: Option<String>,
    pub as_of_recorded: Option<String>,
}

impl TraversalBuilder {
    pub fn new(start_node: impl Into<String>) -> Self
    pub fn max_depth(mut self, depth: usize) -> Self
    pub fn edge_types(mut self, types: Vec<String>) -> Self
    pub fn min_weight(mut self, weight: f64) -> Self
    pub fn attribute_mode(mut self, mode: AttributeMode) -> Self
    pub fn as_of_valid(mut self, ts: impl Into<String>) -> Self
    pub fn as_of_recorded(mut self, ts: impl Into<String>) -> Self
    pub fn build_sql(&self) -> String
    pub async fn execute_ids(&self, conn: &libsql::Connection, ts: Option<String>) -> Result<Vec<String>>
    pub async fn execute(&self, conn: &libsql::Connection, ts: Option<String>) -> Result<MaterializedState>
}

// Fields are PRIVATE since 0.8.0 (B1, D-114). The representation — BTreeMap,
// String keys, two adjacency maps — was never a promise anyone meant to make,
// and interning the keys (D-087) is impossible while `EdgeRef::node` is a
// public String. Accessors return borrowed views, so nothing costs an
// allocation that field access did not.
pub struct Subgraph { /* nodes, out_adj, in_adj */ }
pub struct NodeData { /* title, content, embedding_model, valid_from, valid_to */ }
pub struct EdgeRef  { /* node, edge_type, weight, valid_from, valid_to */ }

impl NodeData {
    pub fn new(title, valid_from, valid_to) -> Self      // content absent
    pub fn with_content(self, content) -> Self
    pub fn with_embedding_model(self, model: Option<String>) -> Self
    pub fn title(&self) -> &str
    // NOT loaded by default since 0.8.0 (B3, D-116). None means NOT LOADED,
    // never empty — no algorithm reads content, and at 20 KB/concept it is 95%
    // of the byte budget. Ask via `TraversalBuilder::content(true)`.
    pub fn content(&self) -> Option<&str>
    pub fn embedding_model(&self) -> Option<&str>
    pub fn valid_from(&self) -> &str
    pub fn valid_to(&self) -> &str
}

// INTERNED since 0.8.0 (B2, D-115): {u32,u32,f64,u32,u32}, size_of 24, no heap.
// Every field but the weight indexes the Subgraph's string pool, so reading one
// needs the graph. Measured win 5.8x-6.8x bytes/edge, not the 7.1x-9.5x the
// plan projected from `2 * size_of` — the pool is not free (D-063 was right by
// about 20%).
impl EdgeRef {
    pub fn node<'a>(&self, g: &'a Subgraph) -> &'a str   // FAR end: target in out_edges
    pub fn edge_type<'a>(&self, g: &'a Subgraph) -> &'a str
    pub fn weight(&self) -> f64                          // not interned; already 8 bytes
    pub fn valid_from<'a>(&self, g: &'a Subgraph) -> &'a str
    pub fn valid_to<'a>(&self, g: &'a Subgraph) -> &'a str
}

impl Subgraph {
    pub fn contains_node(&self, id: &str) -> bool
    pub fn node(&self, id: &str) -> Option<&NodeData>
    pub fn node_ids(&self) -> impl ExactSizeIterator<Item = &str>
    pub fn node_count(&self) -> usize
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = (&str, &NodeData)>
    pub fn out_adjacency(&self) -> impl Iterator<Item = (&str, &[EdgeRef])>
    pub fn in_adjacency(&self) -> impl Iterator<Item = (&str, &[EdgeRef])>
    pub fn insert_node(&mut self, id, data: NodeData) -> Option<NodeData>
    pub fn add_edge(&mut self, source, target, edge_type, weight, valid_from, valid_to) -> usize
        // maintains BOTH indices; returns the bytes added, so the budget check is O(1)
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

**`as_of_valid(ts)`** (D-085, D-174): Sets a valid-time query on the *topology*. The traversal returns edges valid at `ts` under current belief. When either instant is set and `attribute_mode` is left unspecified, returns `DbError::AttributeModeUnstated` — the crate no longer silently defaults to `Current`.

**`as_of_recorded(ts)`** (0.13.2, D-174): Sets a *transaction-time* query. The walk leaves `links_current` and reads a fold of `transaction_log` bounded at `ts`, so it sees the topology the ledger held then — including edges a later correction has since closed. Setting both instants is the bitemporal cell. Raises `DbError::RecordedInstantUnreachable` once `archive` has removed rows from the hot log, because a traversal has no archive path and a partial fold returns *nearly* the right topology.

**`attribute_mode`**: `Current` returns live attributes, ignoring both instants (fast, and a *stated* choice). `AtTime` follows the instants — live `concepts` bounded by their own valid interval when only `as_of_valid` is set, the payload believed at `ts` when `as_of_recorded` is, both when both are. `Omit` returns topology only. The instants fix *when*; `attribute_mode` fixes *whether the text follows*. Conflating them was the silent wrong answer D-085 corrected; one instant serving two clocks was the one D-174 corrected.

**`TraversalBuilder` uses `UNION` not `UNION ALL`** (D-076): The recursive step dedupes on entry, bounding walk rows at `V × (depth+1)` rather than `V × (depth+1) × branching_factor`. The old `UNION ALL` form was a walk (one row per path); the current form is a traversal (one row per node).

**`load_subgraph()`**: Walks `links_current` under the same bounded CTE shape as traversal, hydrates node attributes, returns a `Subgraph`. Enforces byte budget (`SubgraphTooLarge`) and negative/NaN weight refusal.

**Algorithms**: Dijkstra, A*, SCC, k-core, Louvain — all operate on `Subgraph`. Deterministic via `BTreeMap`/`BTreeSet`; ties broken explicitly. **Louvain is local-moving only, by measurement** (D-122): two-phase diverges well inside the byte budget, but it does so by *merging* true communities — the modularity resolution limit — while local moving recovers the ground truth exactly. A higher Q is not a better partition here.

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
    pub concepts_archived: usize,   // 0.9.0, C2 -- always 0 before schema v9
    pub log_entries_archived: usize,
    pub horizon: Option<i64>,
}

// The move back (0.9.0, C3, D-131). Rehydration mints no transaction-time facts:
// it runs inside a declared archive session, which is what suppresses the
// concept insert log trigger (marker-gated at schema v10).
pub struct RehydrateReport {
    pub concepts_rehydrated: usize,
    pub rowids_reassigned: usize,   // could not keep the original rowid_pk;
}                                   //   the FTS mapping was corrected to match

// Which concepts an archive at `cutoff` would be entitled to move: retired,
// both clocks behind the cutoff, and no surviving hot `links` row naming it in
// either direction (0.9.0, D-128). Read-only -- nothing archives concepts yet.
pub async fn archivable_concepts(
    conn: &libsql::Connection,
    cutoff: &str,
) -> Result<Vec<String>>;

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
pub async fn archive_windowed(cutoff, window) -> Result<Vec<ArchiveReport>>
pub async fn save_snapshot(snapshots_dir, state) -> Result<PathBuf>
pub async fn load_snapshot(path) -> Result<MaterializedState>
pub async fn write_final(read_conn, snap_dir) -> Result<()>
pub async fn cleanup_expired_snapshots(snapshots_dir) -> Result<usize>
pub async fn audit_current(conn) -> Result<i64>
pub async fn rebuild_current() -> Result<RebuildReport>
pub async fn rebuild_current_chunked() -> Result<RebuildReport>
pub async fn hydrate_attributes(conn, ids, as_of: &AsOf, mode) -> Result<Vec<NodeAttributes>>

// Handle methods
impl Database {
    pub async fn archive_windowed(&self, cutoff: &str, window: Duration) -> Result<Vec<ArchiveReport>>
    pub async fn shadow_step(&self, step: ShadowStep) -> Result<ShadowOutcome>
}
```

**`query_as_of_edges()`**: Valid-time query over `links_current`. Returns topology at `ts` under current belief.

**`hydrate_attributes()`**: Attributes for a result set, per `AttributeMode`. Under `AtTime` with `as_of.recorded` set it folds the hot `transaction_log`, so it raises `RecordedInstantUnreachable` once rows have been archived out of it (0.13.16, [D-189](architecture/s13-decision-register.md#d-189)) — the same refusal `as_of_recorded` makes, at the surface that fixes the *text* rather than the topology. The other three cells read live `concepts` and are unaffected.

**`reconstruct()`**: Transaction-time replay from `transaction_log`. Composes from newest snapshot at or before `ts` plus anchored fold. Requires `archive_path` if history extends before the archive cutoff.

**`archive(cutoff)`**: Moves closed intervals to cold database. One atomic session — copy-then-delete.

**`archive_windowed(cutoff, window)`** (D-080): N small atomic sessions instead of one. Refuses rather than clamps — a window that never advances, or one implying more than `MAX_ARCHIVE_SESSIONS` (4,096), raises `DbError::ArchiveWindow`. Longest hold reduced (3,326→768 ms at 8K keys). Session skips rebuild when nothing archived.

**`save_snapshot()` / `load_snapshot()`**: Compose and deserialize `MaterializedState` to/from disk. Format v2 header carries snapshot instant for retention bucketing.

**`write_final()`**: Called on `close()` — folds the tail from the read side and writes the final snapshot.

**`audit_current()`**: Returns symmetric difference between `links` and `links_current`. Zero means in sync.

**`rebuild_current()`** (D-023): One act, audits itself, works inside a caller's transaction. O(E) delete + O(E log E) window reprojection + two audit passes. Single atomic transaction — a crash mid-rebuild leaves `links_current` partially populated.

**`rebuild_current_chunked()`** (D-082): Shadow-swap with catch-up. Builds `links_current_shadow` across many small transactions and swaps it in under one, so `links_current` is never partially populated. Longest hold 353→47 ms (7.6×). 2.3× cheaper total. Interlock against archive interleaving via `ActorShared::archive_epoch` — an archive between `Begin` and `Swap` raises `DbError::RebuildInterrupted`, meaning the repair *did not run* and the action is to retry. Drives the full state machine via `shadow_step(ShadowStep)` or calls `rebuild_current_chunked()` for the all-in-one path.

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
    pub async fn register_model(&self, model: &ModelName, dim: usize) -> Result<()>
    pub async fn upsert_embeddings(&self, model, rows) -> Result<usize>
    pub async fn rebuild_fts(&self) -> Result<()>
}

// Free functions over a read connection — not `Database` methods.
pub async fn registered_models(conn) -> Result<Vec<ModelName>>
pub async fn declared_dimension(conn, model) -> Result<usize>
pub async fn upsert_embedding(conn, model, concept_id, vector: &[f32]) -> Result<()>
pub async fn search_vector(conn, query_vec, model, top_k,
                           as_of_valid: Option<&str>, half_life: Option<Duration>)
    -> Result<Vec<VectorSearchResult>>

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
    pub fn as_of_valid(mut self, ts: impl Into<String>) -> Self
    pub fn half_life(mut self, half_life: Duration) -> Self
    pub async fn execute(&self, conn) -> Result<Vec<HybridHit>>
}

pub fn reciprocal_rank_fusion(vector_ranks, keyword_ranks, rrf_k) -> Vec<(String, f64)>
pub async fn keyword_search(conn, query, top_k,
                            as_of_valid: Option<&str>, half_life: Option<Duration>)
    -> Result<Vec<(String, f64)>>
pub fn escape_fts5_query(input: &str) -> String
```

**`register_model()`**: Creates per-model embedding table + DiskANN index in one transaction. Model names are validated as SQL identifiers.

**`search_vector()`**: Calls `vector_top_k` and `vector_distance_cos`. Returns cosine distances. Joins `concepts` and excludes retired ones (0.13.18, [D-191](architecture/s13-decision-register.md#d-191)); `top_k` remains a count, so the index is re-asked for a larger k′ when the filter takes rows out of a pass. `hybrid_search` inherits this because its vector arm *is* `search_vector`; `PreFilterCTE` splices the same predicate rather than inheriting it.

**`as_of_valid`** (0.13.19, [D-192](architecture/s13-decision-register.md#d-192)): with an instant, the same join also bounds `c.valid_from <= t AND t < c.valid_to`; without one, the statement is unchanged. `HybridSearch::as_of_valid` applies it to **both** arms. `FilteredVectorSearch` has no such parameter — it reads the traversal's `as_of_valid`, so a filtered search cannot rank a past neighbourhood against the present corpus. Valid time only: the index keeps one row per concept, so there is no past vector to read at an `as_of_recorded`.

**`half_life`** (0.13.20, [D-193](architecture/s13-decision-register.md#d-193)): weights a hit by the age of what it matched, `0.5 ^ (age / half_life)`, measured from `as_of_valid` — which it therefore requires, raising `HalfLifeWithoutInstant` rather than defaulting to now. **The sign lands on one surface only**: `search_vector` converts its distance to a similarity and back, `keyword_search` multiplies bm25 directly because bm25 is a negated similarity already. `HybridSearch` decays each arm *before* the fusion, since RRF reads ranks. A decaying surface reads `max(5×top_k, 50)` before reordering. `search_filtered` does not take it: the two strategies hold different pools, so it would make the answer a function of the byte estimate.

**`FilteredVectorSearch`**: Two strategies — `PostFilter` (retrieve generous k′, then post-filter) and `PreFilterCTE` (materialize candidate set, then exact distance scan). `TwoPhaseTempTable` removed (D-050). Strategy chosen by `CostEstimator` based on byte budget. Strategy may never change the answer.

**`HybridSearch`**: Fuses vector and keyword arms by RRF at k=60. Each arm read to `max(5×top_k, 50)`. Ties broken by id. FTS5 syntax escaped before reaching MATCH.

### 5.7 Integrity

```rust
pub enum ShadowStep {
    Begin,
    Fill { after: i64 },
    Swap { build_start: i64, epoch: u64 },
}

pub enum ShadowOutcome {
    Started { build_start: i64, epoch: u64 },
    Filled { last: i64 },
    Swapped { edges_rebuilt: usize },
    Interrupted { reason: String },
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

**`MetricsSnapshot`**: Queue depth (mean + high-water), per-kind hold histogram (bucket boundary at `CHUNK_BUDGET`), over-budget count, longest hold with command kind. Feature-gated — `ActorMetrics` is an empty ZST and `record_hold` a no-op when the feature is off. `HoldTimer` is **not** gated since 0.12.0: its reading sizes the next chunk, so it is a control signal in every build ([D-146](architecture/s13-decision-register.md#d-146)).

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
pub const CHUNK_BUDGET: Duration = Duration::from_millis(3)
pub const BULK_ATOMIC_WARN_HOLD: Duration = Duration::from_millis(250)
pub const SAMPLE_LIMIT: usize = 32
pub const MAX_ARCHIVE_SESSIONS: usize = 4_096

pub fn estimated_bulk_hold(edges: &[EdgeAssertion]) -> Duration  // ~33 ms / 500 rows
```

**`Clock` trait**: Injectable clock for deterministic testing. `SystemClock` enforces monotonicity by flooring to `MAX(recorded_at)`. `FakeClock` advances explicitly.

**`normalize()`**: Widens second-precision timestamps to canonical form. Rejects offsets, missing Z, millisecond precision.

**`util/limits.rs`**: Holds only the ceilings **SQLite imposes**, which is the distinction the module exists for — `HYDRATE_CHUNK` (400) is a bind-variable ceiling and `SQLITE_MAX_VARIABLE_NUMBER` (999) is the limit it stays under. Nothing here is a tuning choice, and no amount of measurement will move either number.

**The tuning constants are in `connection.rs`**, beside the actor they bound: `CHUNK_BUDGET` (3 ms), the duration bound all chunking is derived from; `CHUNK_FLOOR` (35), the smallest chunk the adaptive loop will fall to; `chunk_rows` (90/70/600/30), the per-path ceilings; `BULK_ATOMIC_WARN_HOLD` (250 ms), above which `write_bulk_atomic` warns; and `MAX_ARCHIVE_SESSIONS` (4,096), bounding `archive_windowed`. `SAMPLE_LIMIT` (32), which caps `ChainCheck` disagreement lists, is in `temporal/replay.rs`. *This paragraph read "`util/limits.rs` centralises chunking and operational constants" and listed all of the above as living there; none of them ever did.*

---

## 6. Performance

### 6.1 Chunking Constants (per path)

| Path | Ceiling (the size, before 0.12.0) | Measured at that size | Per-row cost |
|---|---|---|---|
| Edges (`bulk_import`) | 90 | ~2.39 ms | ~11 µs (empty db), ~135 µs at 8K edges |
| Concepts (`write_concepts`) | 70 | ~2.35 ms | ~2.5 µs |
| Annotations (`write_analytics_annotations`) | 600 | ~2.36 ms | ~2.5 µs |
| Embeddings (`upsert_embeddings`) | 30 | ~2.06 ms | ~135 µs (DiskANN insertion) |

**Bound**: 3 ms (`CHUNK_BUDGET`). Per-transaction overhead: ~0.8 ms (BEGIN, COMMIT, fsync). The four sizes are derived from measurement ([D-058](s13-decision-register.md#d-058)), not one shared constant. Two paths are superlinear in *table size* (edges via `trg_links_single_open`'s wrong index — fixed in v6 by `idx_lc_open_interval`; embeddings via DiskANN graph growth) — chunking costs throughput on every path.

**The four sizes are ceilings as of 0.12.0, not sizes.** No row count can bound a duration on a path whose per-row cost grows with the table ([D-143](s13-decision-register.md#d-143)), so the loop stopped picking one ahead of time: the actor times its own transaction, and each chunk's measured hold sizes the next. Shrink proportionally when over budget, grow by a quarter when comfortably under, hold in between — fast off a bound you are exceeding, slow toward one you are not. `CHUNK_FLOOR` (35) stops the descent, measured at **3.11–3.43 ms** on the four D-088 shapes ([D-146](s13-decision-register.md#d-146)): a knowing 0.1–0.4 ms miss, defended by the frame it protects (~3.2 + 5 = ~8.2 ms against 16.7 ms) rather than by the bound. Feedback, not preemption — the chunk in flight always commits in full, and a single-chunk batch gets no protection at all.

**What it bought, measured**: mean hold **3.4–3.7 ms against 7.1–8.3 ms** fixed, a 2.1–2.4× shorter typical stall, for **9–20%** throughput. The *worst* stall is unimproved (7.7–10.2 vs 7.6–15.2 ms) because it is the first chunk, at the ceiling, before feedback exists. On the edge path at 8,000 edges the trace is `[90, 35, 35, …]` — one step, and no size between the two is ever chosen.

**`Database::low_chunked`** (D-086): The four bulk paths are one deduplicated function, taking the whole batch, a ceiling and a closure that names the command. Four copies of a yield-critical loop are four places for the yield to be lost — and, since 0.12.0, four places to get the control law's clamps wrong.

### 6.2 Performance Budgets (§9)

| Operation | Budget | Measured | Notes |
|---|---|---|---|
| Single assertion | ≤ 5 ms | 224 µs | **Flat in out-degree** (D-134): measured into tables of 0 / 2,000 / 8,000 edges — hub out-degree 0 / 666 / 2,666 — with no rise. The old "not met at high out-degree" caveat described the pre-v6 access path (D-059). Real cost is O(version count per edge key), capped by archival |
| Chunk commit, edges 90 rows | ≤ 3 ms | ~2.39 ms | Fully amplified (triggers included) |
| Chunk commit, concepts 70 rows | ≤ 3 ms | ~2.35 ms | |
| Chunk commit, annotations 600 rows | ≤ 3 ms | ~2.36 ms | |
| Chunk commit, embeddings 30 rows | ≤ 3 ms | ~2.06 ms | |
| Three-hop traversal | ≤ 10 ms | 1.66 ms | On `star_of_stars` fixture |
| `audit_current` | ≤ 200 ms | 13.8 ms | |
| Vector top-10 | ≤ 20 ms | 246 µs | |
| Hybrid top-10 | ≤ 50 ms | 1.77 ms | |
| Full fold (reconstruct) | ≤ 100 ms | 16.9 ms | |
| Composition | ≤ 100 ms | 2.18 ms | Snapshot + delta fold |
| Archive (100K closed intervals) | ≤ 30 s | ~26.8 ms for 2K | One session; windowed trades total for latency |
| Rehydrate, 1 concept | ≤ 5 ms | 3.71 ms | ATTACH + one `BEGIN IMMEDIATE` + commit (D-132) |
| Rehydrate, per concept after the first | ≤ 300 µs | ~74 µs | The rate the archive row above implies, applied in reverse. **Linear to n=1,000 only** — 114 µs at n=10,000, and FTS5 index maintenance is why |
| Rebuild (10M edges) | ~50 s | — | Chunked: 7.6× less hold, 2.3× cheaper total |

**Re-measured at 0.8.0 (D-127) and extended at 0.9.0 (D-132); this table carried the 0.7.0 figures until 2026-08-07.** The read-path rows above moved 12–36% faster when three 0.8.0 items changed how a `Subgraph` is represented and what a load carries, and two controls say that is the code rather than the machine — D-090's fixed `control/select_1` unchanged, and an untouched chunk-commit path unchanged.

**Measurement caveat**: Absolute timings are hardware-dependent. All budgets measured on named reference hardware. Criterion baselines detect regression; machine against itself. **Budgets are measured, not CI gates** (D-055): absolute durations on arbitrary hardware are the wrong shape for a CI check — regression detection compares a machine against itself.

---

## 7. Known Risks & Mitigations

| Risk | Severity | Mitigation | Status |
|---|---|---|---|
| **R15: cumulative `connect()` → `STATUS_ACCESS_VIOLATION`** | High | `RUST_TEST_THREADS = "1"`; `property-tests` quarantined; soak test defends the claim | ⚠️ Mitigated; upstream report open. **Not a concurrency fault** — one task on one thread still faults 2/10 ([D-148](architecture/s13-decision-register.md#d-148)) |
| **Property test binaries fault in suite** | Medium | `property-tests` feature gate; serialised runs | ✅ Quarantined |
| **Fixture shape bias** | Medium | Four-shape fixture matrix; every decision names fixture | ✅ D-088 |
| **Covering index wins over selective** | High | `EXPLAIN QUERY PLAN` assertions on every index | ✅ D-042, D-059, D-064 |
| **Superlinear chunk cost on large tables** | Medium | Index on `(source_id, target_id, edge_type, valid_to, valid_from)` shipped as v5→v6 | ✅ D-059 |
| **Snapshot chain divergence** | Low | `verify_snapshot_chain()` reports but does not repair | ✅ D-092 |
| **Rebuild interrupted by archive during shadow-swap** | Medium | `ActorShared::archive_epoch` interlock; `RebuildInterrupted` error (not `RebuildFailed`) | ✅ D-082 |
| **Unreadable indices cost per-insert** | Low | Both dropped by the `v7 → v8` rung; the unread set is now asserted **empty**. Measured −7.9% off `assert_edge` | ✅ D-089, D-118 |
| **FTS index keyed on a rowid `VACUUM` may renumber** | Low | `rowid_pk INTEGER PRIMARY KEY` in v8. Never actually live — `VACUUM` renumbers only unindexed tables (measured) | ✅ D-071, D-119, D-120 |

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
| **Doc sync tests** | API surface matches architecture | `tests/doc_sync_tests.rs` (build failure on mismatch) |
| **Fixture matrix** | Four-shape fixture; every decision names fixture | `tests/fixtures.rs` (D-088) |
| **Plan-pinning** | Index plans asserted by `EXPLAIN QUERY PLAN`; every index names the query that reads it | `tests/index_plan_tests.rs` (D-089) |
| **Claim registry** | Every published performance figure names a bench and a decision, and one `(operation, fixture, metric)` carries one value | `tests/perf_claim_tests.rs` (D-139) |
| **Bench controls** | Control row distinguishes machine shift from baseline shift | `benches/budgets.rs` (D-090) |

**Totals are not reproduced here** (0.10.0, W4.6). They were, as four hard numbers with a measurement date, and they went stale every release that added a test — which is every release. `python scripts/run_rust_suite.py` prints the current count for whatever feature set you pass it, and `python tests_py/run_suite.py` does the same on the Python side; a number you can regenerate in one command does not need a copy in a document that nothing checks. The README carries a dated snapshot for readers who want a rough size, and that one is a snapshot on purpose.

The surface itself *is* pinned: `tests/doc_sync_tests.rs` fails the build when the public API diverges from this document.

**How to run it so the answer means something** (D-110). Use `python scripts/run_rust_suite.py --features metrics`, not bare `cargo test`. Under R15 a crashed target prints no `test result:` line while every other target prints its own, so the run comes back with a *smaller pass count and zero failures* — green to anything summing passes. The script classifies each run as `CRASH`, `FAILED`, `INCOMPLETE`, `TEARDOWN` or `BUILD` and retries only `CRASH`; a genuine failure is named on attempt 1. Both CI steps go through it, at three attempts for the main suite and six for the quarantined property step, which faults far more often. Same shape as `tests_py/run_suite.py` (D-107), deliberately not the same implementation.

**The suite is not the whole gate, and one gap shipped** (0.11.0, [D-144](architecture/s13-decision-register.md#d-144)). CI also runs `cargo doc --no-deps --features metrics` under `RUSTDOCFLAGS: -D warnings`, and rustdoc's checks are **not tests**: a broken intra-doc link is a warning that flag escalates, so no `cargo test` run and no assertion this project could write will see it. 0.10.0 was tagged and released with one. Run it locally the same way as the suite: `python scripts/run_rust_suite.py --docs --features metrics`. Nothing is retried — rustdoc opens no database, so a failure there is always real.

---

## 9. Decision Reference

| Decision | Reference | Rationale |
|---|---|---|
| **One writer, not one entry point** | D-016 | A caller-held closure could hold the write lock arbitrarily long |
| **`UNION` not `UNION ALL` in CTE** | D-076 | Bounds walk at `V × (depth+1)`; simple-path reachability equals walk reachability within D |
| **`PostFilter` + `PreFilterCTE`, no `TwoPhaseTempTable`** | D-050 | `CREATE TEMP TABLE` fails on `query_only` connection; `vector_top_k` refuses 4th arg |
| **an instant + unstated `attribute_mode` is an error** | D-085 | `Current` returns live text; `AtTime` returns historical text; conflating them is wrong |
| **`as_of` split into `as_of_valid` / `as_of_recorded`** | D-174 | one parameter compared against two different clocks; the name promised valid time and the attribute half delivered transaction time |
| **`raw()` is `#[doc(hidden)]`** | D-091 | Leaves three §4.7 gaps open; provoking a guard is its legitimate use |
| **`write_bulk_atomic` uncapped** | D-014 | Capping breaks the guarantee the method exists to provide |
| **Archive windowing not default** | D-080 | Windowing costs more total work; only pays when backlog is large |
| **Snapshot chain: report, don't repair** | D-092 | Under Doctrine VI a snapshot is disposable; repair evidence would be destroyed |
| **Metrics feature-gated, zero cost when off** | D-079 | `ActorMetrics` is an empty ZST and `record_hold` a no-op. `HoldTimer` left the gate in 0.12.0 — the reading is now a control signal, and `Duration::ZERO` reads as "under budget" |
| **Four chunk sizes, not one** | D-058 | Per-row costs span 60×; one constant cannot express one duration across paths |
| **The chunk size is measured, not chosen** | D-143 | No row count bounds a duration on a path whose per-row cost grows with the table; the constants become ceilings |
| **A chunk floor that misses the bound** | 0.12.0 | Without one the loop shrinks until the import stops finishing; 35 rows is 9.1 ms worst case against a 16.7 ms frame |
| **Chunking is ~11% slower as throughput** | D-059 | Smaller chunks buy latency and cost throughput on every path |
| **`low_chunked` deduplicates four bulk loops** | D-086 | Four copies of a yield-critical loop are four places for the yield to be lost |
| **`RebuildInterrupted` ≠ `RebuildFailed`** | D-082 | The repair *did not run* is not *the repair did not repair*; action is to retry |
| **`OverlappingInterval` boxed (168 bytes)** | D-075 | Only variant that is boxed; keeps `DbError` under `clippy::result_large_err` threshold |
| **NaN is not a schema gap** | D-078 | `weight REAL NOT NULL` rejects NaN; listing it as a gap claimed the schema was silent where it is strict |
| **`weight >= 0.0` via `CHECK`** | D-083 | Schema v7 closes the third §4.7 invariant; negative and text weights refused at engine level |
| ~~**Deferred: `rowid_pk` (D-084)**~~ | D-084 → **D-119** | **Both halves landed.** `rowid_pk` shipped on the `v7 → v8` rung in 0.8.0, *ahead* of its stated trigger, because D-036 forbids a primary-key change after 1.0; concept archival — the trigger — shipped in 0.9.0 (D-128…D-132) |
| **Deferred: `Subgraph` interning (D-087)** | D-087 | Deferred to 0.7.0 — cost/benefit assessed post-baseline, `hydrate`'s N+1 buries it |

---

## 10. Python Bindings (v0.12.0)

A synchronous Python binding built on pyo3 0.29 and maturin, delivered as a wheel alongside the Rust crate. The binding is **synchronous** (D-095): the Write Actor serialises every write through one channel, so exposing `await` on the write path advertises concurrency the architecture does not grant. A mixed async/sync surface is worse than either pure form.

**Runtime boundary.** Every `Database` method runs inside `Python::detach` around `Runtime::block_on`, releasing the GIL for the duration of the call. A single process-global multi-threaded runtime is behind a `OnceLock`; per-handle runtimes would mean N thread pools and a panic risk (tokio `Runtime::drop` panics from inside a runtime). The `PyDatabase` struct is `#[pyclass(frozen)]` over `RwLock<Option<Database>>` — reads take a read lock and run concurrently; `close()` takes the write lock and waits. The lock must be acquired *inside* the GIL-released closure, not outside, or `close()` blocks on the GIL and deadlocks. A `fork()` guard poisons the runtime on Linux `multiprocessing` children, converting a silent hang into an exception.

**Error mapping.** Every `DbError` variant maps to its own Python exception class with its fields as attributes — `MacrameError` is the base, with trees under `IntegrityError`, `ValidationError`, `VectorError`, `TemporalError`, `WriterError`, etc. Completeness is enforced by an exhaustive `match` over `DbError` with no wildcard arm; adding a variant fails to compile `macrame-py` before a wheel is built. The `#[error]` rendering survives as `str(e)`, so callers who only want the sentence get it.

**Value types and coercion.** Timestamps accept both `str` (passes through) and aware `datetime` (converted); naive datetimes and bare `date` objects are rejected rather than assumed UTC. Outbound timestamps are always `datetime` with `tzinfo=utc`. An open interval (`9999-12-31T23:59:59.999999Z`) crosses as `None`, not as a sentinel datetime — `datetime.max` cannot survive `.astimezone()` east of UTC. `macrame.OPEN` is the stored string for callers who need to name it. Embeddings accept `bytes` (fast path, 60.8 µs for 768 dims) or any sequence of floats (94.9 µs). Absent `content` also crosses as `None`: `load_subgraph` does not fetch document text unless `content=True` is passed, and `""` cannot mark *not loaded* because it is a valid value of the type (D-116, D-123). `Subgraph` stays opaque — a `#[pyclass]` with forwarded accessors, with an explicit `.to_dict()` for callers who want the copy, whose node values are `NodeData` objects rather than nested dicts. Opacity paid for itself in 0.8.0: the crate interned `EdgeRef` and no binding signature moved. Value types validate in their constructor (not at the point of use), so a `write_bulk_atomic` failure points at the line that built the offending value.

**What is not exposed.** `Database::raw()`, `Database::read_conn()`, the bare-connection `register_model`/`upsert_embedding`, and `open_with_clock` are all deliberately unexposed. `diagnostic_conn()` *is* exposed, but as methods that run a query and return rows — `db.explain(sql)` and `db.diagnostic_query(sql, params)` — not as a connection object. Opening per call measured clean at 500 sequential opens — which D-148 now reads as *under the threshold* rather than as *safe by shape*, since the fault counts cumulative `connect()` and faults sequentially at ~10,000. A caller who opens diagnostic connections in a loop is in the exposed regime; one who opens a few is not. `FakeClock` is available only in a separate `macrame.testing` submodule, gated and documented as unsupported.

**Lifecycle.** `__enter__`/`__exit__` are the supported path because Python's GC is non-deterministic and `close()` (which takes `self` by value in Rust) cannot be called from `#[pymethods]`. A dropped-without-close handle emits a `ResourceWarning` via `__del__`.

**Packaging.** Distribution `macrame-db`, import `macrame`. Wheels: `manylinux_2_28` x86_64 + aarch64, macOS universal2, Windows x86_64. `abi3-py310` (D-094): one wheel per platform rather than one per Python minor version. The wheel ships with `metrics` on (D-093) because a feature flag does not survive into a binary artifact. Smoke test asserts `engine_linked()` and `metrics().turns > 0` — a wheel that imports but has no engine or no counters would pass a bare import test. Cold build ~54–62 s; wheel 4.3 MiB compressed. Uploads use Trusted Publishing (OIDC), no token stored.

**Testing.** `tests_py/` runs single-process (no xdist), because `pytest-xdist` opens a database per worker — exactly the concurrent-open shape that reproduces R15. R15 is transparent to the boundary: `block_on` releases the GIL, so 48 concurrent opens from 48 threads fault 2/12, matching the Rust control arm. The gate (`run_suite.py`) checks summary, failure count, collected count and exit code against each other, naming four outcomes (`CRASH`, `FAILED`, `INCOMPLETE`, `TEARDOWN`) and retrying only `CRASH`. The reporting hazard differs from Rust: pytest runs one process, so a mid-run crash gives exit code 3 with no summary line (exit code *is* sufficient), but a fault during interpreter teardown after a green summary gives a green summary with non-zero exit (exit code alone is wrong).

**Stubs.** Hand-written `_macrame.pyi`, compared to the live extension both ways and to `errors.rs`, verified by five injection tests. `mypy --strict` in CI. `py.typed` marker ensures stubs are consulted. Stub conventions: timestamps **in** are `str | datetime` (aware only), **out** are always aware UTC `datetime`; open interval is `None`; `astar`'s heuristic is `Callable[[str, str], float]`.

**R15 through the boundary.** The concurrent-open fault reproduces through Python at the same rate as Rust — `block_on` releases the GIL, so threads are genuinely concurrent inside `open`. The boundary is transparent. The pytest suite's reporting hazard differs from Rust: see Testing above.

---

## 11. Deferred Decisions

| Decision | Reference | Trigger |
|---|---|---|
| ~~**`rowid_pk` on `concepts`**~~ | D-084 → **D-119** | **Delivered in 0.8.0**, ahead of its trigger: D-036 forbids a primary-key change after 1.0, so waiting for the release that needed it would have waited past the last release allowed to make it |
| ~~**`Subgraph` key interning**~~ | D-087 → **D-115** | **Delivered in 0.8.0** — 24-byte `EdgeRef` against a per-graph pool, measured 5.8×–6.8× rather than the projected 7.1×–9.5× |
| ~~**Concept archival**~~ | D-022 → **D-128**…**D-132** | **Delivered in 0.9.0, and both stated triggers were answered rather than waived.** *Identity semantics*: rehydration is a physical move back, so a concept reacquires its old identity — derived from Doctrine III, not chosen, and it needed schema v10 because the fold resolves by `seq_id`. *Rehydration cost*: 3.71 ms fixed, ~74 µs per concept, linear to n=1,000 (D-132) |
| **Automatic writer restart** | D-015, Appendix C | Deferred — containment errors support it; operational experience will decide |
| **Crate-level write cancellation** | D-028, Appendix C | Deferred — application-layer `CancellationToken` checked before `send` |
| **Graph-neural-network features** | Appendix C | Deferred — belongs to the application layer, not the ledger |
| **`Subgraph` as opaque handle** | D-101 | Delivered in P4.2 — converting eagerly doubles peak memory |
| **`astar` heuristic** | D-104 | Resolved: does not release GIL; raising heuristic captured and re-raised; `NaN` refused by name |
| **`traverse` vs `traverse_ids`** | D-102, D-103 | `OMIT` on `traverse` is refused (points to `traverse_ids`); unstated `min_weight` is `-inf`, not `0.0` |
| **`ChainCheck` anchors** | D-105 | `composed_anchor` and `folded_anchor` may legitimately differ and must never be compared — `diverged()` is the method |

---

*Last updated: 2026-08-08 · v0.12.0 · Synced against architecture (s4–s14, appendices A–C) · Pinned by `tests/doc_sync_tests.rs` and `tests/perf_claim_tests.rs`*
