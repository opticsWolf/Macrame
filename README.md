# Macrame

[![CI](https://github.com/opticsWolf/Macrame/actions/workflows/ci.yml/badge.svg)](https://github.com/opticsWolf/Macrame/actions/workflows/ci.yml)
[![Python](https://github.com/opticsWolf/Macrame/actions/workflows/python.yml/badge.svg)](https://github.com/opticsWolf/Macrame/actions/workflows/python.yml)
[![crates.io](https://img.shields.io/crates/v/macrame-db.svg)](https://crates.io/crates/macrame-db)
[![docs.rs](https://img.shields.io/docsrs/macrame-db)](https://docs.rs/macrame-db)
[![PyPI](https://img.shields.io/pypi/v/macrame-db.svg)](https://pypi.org/project/macrame-db/)
[![Python versions](https://img.shields.io/pypi/pyversions/macrame-db.svg)](https://pypi.org/project/macrame-db/)
[![MSRV](https://img.shields.io/crates/msrv/macrame-db.svg)](#minimum-supported-rust-version)
[![License](https://img.shields.io/crates/l/macrame-db.svg)](#license)

**A bitemporal graph ledger for knowledge management — embedded, single-file, no server.**

Macrame stores concepts linked by typed, weighted relationships — where both concepts and relationships change over time, and the history of those changes is itself a first-class asset. Everything lives in one `.db` file on disk. No database server, no network protocol, no external service.

---

## Why Macrame

| Strength | What it means |
|---|---|
| **Bitemporal by design** | Two independent clocks per row — *valid time* (when a fact held in the world) and *transaction time* (when the database learned it). `as_of(ts)` answers "what did the world look like?" and `reconstruct(ts)` answers "what did we believe?" — both correct, both different. |
| **Single file, embedded** | The entire database is one file on the local filesystem. Link it directly into your application. Run on Windows desktop, Linux, or macOS — the Rust suite runs on all three in CI. |
| **Graph + vectors + search** | Recursive CTE traversal, native DiskANN vector search, FTS5 keyword search, and hybrid RRF fusion — all in one crate, no external graph library. |
| **Five in-memory analytics** | Dijkstra, A*, SCC, k-core, and Louvain — operating on a typed `Subgraph` with zero external dependencies. |
| **Rebuildable materialization** | `links_current` is a cache of current belief, always rebuildable from the append-only `transaction_log`. Drift is detectable by audit, recoverable by atomic or chunked rebuild. |
| **Archival path** | Closed intervals move to a cold database inside atomic sessions. Point-in-time reconstruction composes from snapshots plus anchored folds — fast because it doesn't fold from genesis. |
| **Runtime safety** | One Write Actor serialises all writes; read connections carry `PRAGMA query_only = ON` enforced at the engine level. No raw SQL escapes the guard. |

---

## Quick Start

### Rust

```toml
[dependencies]
macrame-db = "0.7"
```

```rust
use macrame::prelude::*;

async fn main() {
    let db = Database::open("knowledge.db").await?;

    db.upsert_concept(ConceptUpsert::new("quantum", "Quantum Computing")
        .valid_from("2026-01-01T00:00:00.000000Z"))
        .await?;

    db.upsert_concept(ConceptUpsert::new("entanglement", "Quantum Entanglement")
        .valid_from("2026-01-01T00:00:00.000000Z"))
        .await?;

    db.assert_edge(EdgeAssertion::new("quantum", "entanglement", "ENTAILS")
        .valid_from("2026-01-01T00:00:00.000000Z")
        .weight(1.0))
        .await?;

    let subgraph = db.traverse()
        .start_node("quantum")
        .max_depth(3)
        .execute(db.read_conn(), None)
        .await?;
}
```

### Python

```bash
pip install macrame-db
```

```python
import macrame

T0 = "2026-01-01T00:00:00.000000Z"

with macrame.Database.open("knowledge.db") as db:
    db.write_concepts([
        macrame.ConceptUpsert("quantum", "Quantum Computing", valid_from=T0),
        macrame.ConceptUpsert("entanglement", "Quantum Entanglement", valid_from=T0),
    ])
    db.assert_edge(
        macrame.EdgeAssertion("quantum", "entanglement", "ENTAILS", valid_from=T0)
    )
    graph = db.load_subgraph("quantum", 3, 1 << 20)
    print(graph.dijkstra("quantum"))
```

---

## Architecture Highlights

### Eight Doctrine Invariants

Every design decision derives from these invariants:

1. **The boundary is sacred** — Everything above libSQL is ours; everything below it is upstream. Never patch the engine.
2. **Two clocks, never mixed** — Valid time and transaction time are independent axes. No code path derives one from the other.
3. **Assertions are immutable** — Rows in `links` are never updated in place. The past is never rewritten; it is only ever superseded.
4. **The ledger is a table, not the log** — Transaction-time reconstruction reads `transaction_log`, not WAL or CDC frames.
5. **No physical deletion in hot tables** — Rows leave through the archive path only. Ad-hoc `DELETE` aborts at the trigger layer.
6. **Derivative state is disposable** — `links_current` is a rebuildable materialization. Drift is detectable, recoverable by rebuild.
7. **Embeddings are immutable per version, excluded from the ledger** — Vectors live in per-model tables; they never appear in `transaction_log` payloads.
8. **Fidelity is a parameter, never a silent default** — `as_of(ts)` and `reconstruct(ts)` say what they mean in their signatures.

### Concurrency Model

- **One writer** — a dedicated Tokio task holds the sole write-capable connection
- **Many readers** — WAL journaling; readers never block on writer
- **Two-tier priority channels** — high-priority (user-driven) preempts low-priority (background)
- **Cooperative chunking** — bounded to ~3 ms per chunk, four paths with different row counts (90 edges, 70 concepts, 600 annotations, 30 embeddings)

### Schema Versioning

| Version | Feature |
|---|---|
| v2 | Legacy-free baseline |
| v3 | `analytics_annotations` table |
| v4 | FTS5 external-content index |
| v5 | Overlap guard index |
| v6 | Overlapping closed intervals refused in actor |
| v7 | `CHECK (weight >= 0.0)` on `links.weight` |
| v8 | `concepts.rowid_pk`, the third FTS trigger, and the two unread indices dropped — **current** |

v8 is the last rung before the 1.0 freeze that could take it: `rowid_pk INTEGER PRIMARY KEY` costs `id` the primary key, and D-036 forbids a primary-key change after 1.0 (D-119). It also drops `idx_annotations_label` and `idx_lc_tgt_active`, which shipped in the v7 baseline with no query that seeks on them — measured at −7.9% off `assert_edge` (D-089, D-118).

---

## Rust Implementation

| Detail | Value |
|---|---|
| Edition | Rust 2021 |
| MSRV | **1.88** (verified, not declared) |
| Runtime | tokio async, single process |
| Engine | libSQL 0.9.30 (MIT, unmodified) |
| Schema version | 8 |
| Test suite | 296 Rust · 305 with `metrics` · 316 with `property-tests` · 344 Python — all green (measured 2026-08-02) |
| Dependencies | tokio, serde, bincode, zstd, thiserror, tracing, ulid |

### Module Map

| Module | Responsibility |
|---|---|
| `schema` | DDL, triggers, migrations |
| `graph` | CTE compilation, subgraph loading, vector filters |
| `temporal` | `as_of()`, `reconstruct()`, snapshots, archive |
| `vector` | Model registration, embedding upsert, DiskANN search, hybrid RRF |
| `integrity` | Audit, atomic rebuild, chunked shadow-swap rebuild |
| `connection` | `Database` handle, Write Actor, priority channels |
| `error` | `DbError` enum, error classification |

---

## Python Bindings (v0.7.0)

| Detail | Value |
|---|---|
| Engine | pyo3 0.29 + maturin |
| Surface | Synchronous (Write Actor serialises all writes) |
| GIL | Released via `Python::detach` around `Runtime::block_on` |
| Distribution | `macrame-db` on PyPI, import `macrame` |
| Wheels | `abi3-py310` — one per platform (Linux x86_64/aarch64, macOS universal2, Windows x86_64) |
| Python | CPython 3.10+ |
| Type stubs | Ship with wheel, `py.typed` set, `mypy --strict` in CI |

### Key design decisions

- **Synchronous surface** — The Write Actor serialises every write through one channel, so exposing `await` advertises concurrency the architecture does not grant.
- **Opaque `Subgraph`** — A `#[pyclass]` with forwarded accessors; `.to_dict()` for callers who want the copy.
- **Open intervals cross as `None`** — Not a sentinel datetime, because `datetime.max` cannot survive `.astimezone()` east of UTC.
- **Every error is typed** — 35 exception classes under `MacrameError`, with six intermediate groups for catching sets: `IntegrityError`, `ValidationError`, `VectorError`, `TemporalError`, `WriterError`, `BudgetError`.
- **`metrics` shipped on** — The wheel ships with the `metrics` feature enabled because feature flags do not survive into binary artifacts.

---

## Performance (measured, not gated)

| Operation | Budget | Measured |
|---|---|---|
| Single assertion | ≤ 5 ms | **not met at high out-degree** — O(out-degree), not O(1) (D-059) |
| Chunk commit (edges, 90 rows) | ≤ 3 ms | ~2.39 ms |
| Three-hop traversal | ≤ 10 ms | 2.1 ms |
| Vector top-10 | ≤ 20 ms | 294 µs |
| Hybrid top-10 | ≤ 50 ms | 2.0 ms |
| Full fold (reconstruct) | ≤ 100 ms | 21 ms |
| Composition (snapshot + delta) | ≤ 100 ms | 3.4 ms |

All budgets measured on named reference hardware. Regression detection uses criterion baselines — machine against itself. See [§9 of the architecture docs](docs/architecture/s6-s10-flows-to-dependencies.md#9-performance-budgets) for full table.

---

## Known Risks

| Risk | Mitigation |
|---|---|
| **R15: Concurrent open → access violation** (libSQL 0.9.30) | One open per database; R15 reproduces transparently through Python |
| **Property test binaries fault mid-suite** | `property-tests` feature gate; serialised runs; CI classifies each run rather than counting failures, and retries only a crash |
| **Covering index wins over selective** | `EXPLAIN QUERY PLAN` assertions on every index-sensitive query |
| **Snapshot chain divergence** | `verify_snapshot_chain()` reports but does not repair (snapshots are disposable) |

---

## Minimum Supported Rust Version

**1.88**, verified rather than declared — `cargo +1.88.0 check --all-features --all-targets` passes and 1.85 does not. The constraint comes from `libsql-ffi`'s build dependency chain (`bindgen → which → home`), not from this crate's own code (which needs only 1.73).

---

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API)
- [Architecture Quick Reference](docs/quickref.md) — v0.7.0 reference: API, schema, decisions, performance
- [Python bindings](docs/architecture/s14-python-bindings.md) — §14: async→sync boundary, error tree, stubs
- [Decision register](docs/architecture/s13-decision-register.md) — D-001…D-109 with rationale

---

## Naming

Distribution `macrame-db`, import `macrame` — on both crates.io and PyPI. The Rust side has no caveat: a crate's `[lib] name` is namespaced per build graph, so `macrame-db` providing `macrame` collides with nothing. `site-packages` is flat.

The PyPI package `macrame` is an unrelated, effectively abandoned build tool (0.0.1, 2021). If it installs a top-level `macrame/`, then installing both leaves two distributions contending for one directory — `pip` warns on file conflicts, so this is a known and non-silent risk. Importing as `macrame_db` is the fallback if it ever matters.

---

## License

See [LICENSE](LICENSE) for details.
