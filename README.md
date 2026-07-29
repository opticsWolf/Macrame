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
| In-memory graph analytics | Dijkstra, A\*, SCC, k-core, Louvain (phase-one) — native adjacency-list `Subgraph`, no external graph dependency |
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
cargo test

# Property tests (generated-history binaries, run serially)
cargo test --features property-tests
```

### Test Layers

| Layer | What it proves |
|---|---|
| Unit tests | CTE builder output, interval arithmetic, RRF fusion, embedding codec roundtrips |
| Integration tests | Full API against real database files; WAL crash recovery |
| Property tests | Random assertion/retirement streams never produce overlapping open intervals; `links_current` == latest-belief projection |
| Scenario tests | Attribute fidelity across `AttributeMode` values; corrupt-then-rebuild roundtrip |

### Known test gaps

| Gap | Detail |
|---|---|
| Concurrency tests | `tests/concurrency_tests.rs` is `assert!(true)` — reports green, tests nothing. Priority interleaving, prefix visibility, writer containment, and shutdown coordination are all uncovered. |
| `FakeClock` injection | Constructed in `harness.rs` but never injected into any test. The compiler warns about the dead field on every build. |
| Benchmark gates | §9 performance budgets are not yet implemented as CI gates. |
| `RecordedAtRegression` | Mapped by the error classifier but unreachable through the public API — `SystemClock` is strictly increasing by contract. |
| `seq_id` gap tolerance (D-024) | No fold in the crate carries a `seq_id > :anchor` term, so the guarantee is vacuous rather than satisfied. Becomes writable when snapshot cadence lands. |

### Known defects

| # | Location | Defect |
|---|---|---|
| J | `util/ids.rs` | `validate_id` returns `NotFound` for a malformed ULID — wrong semantics |
| S | `vector_filter.rs` | `CostEstimator` selects among strategies with no implementations; the selector is a candidate-count heuristic carrying the name of a byte-budget cost model |
| T | Hybrid search | `reciprocal_rank_fusion` exists as a pure function; no FTS5 table, no keyword retrieval, nothing fuses them |
| R15 | libSQL 0.9.30 | Intermittent `STATUS_ACCESS_VIOLATION` when local databases are opened concurrently in one process; mitigated by `RUST_TEST_THREADS = "1"` and the `property-tests` feature gate |

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
| Snapshot composition (D-049) | **Delivered** — anchored fold + tombstone merge; two carve-outs open (archive boundary, cadence) |
| Phase 5 — Test matrix | **Delivered but for one item** — Doctrine VIII divergence (both directions), archive crash safety (D-012), Doctrine VII property suite. Empirical cost estimates (D-007) remain blocked behind `VectorFilterStrategy` |
| `VectorFilterStrategy` implementations | **Open** — blocked on `PRAGMA query_only` rejecting `CREATE TEMP TABLE` and no selectivity source |
| Hybrid search | **Open** — design specified (§5.9), waiting on go/defer call |
| `Subgraph` integer-index rewrite | **Deferred** — pending Louvain/Dijkstra benchmark on budget-sized graph |

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API). One file per section.
- [Implementation plan](docs/Macrame%20Implementation%20Plan%20v0.5.4.md) — delivery status, open items, defect register

## License

See [LICENSE](LICENSE) for details.
