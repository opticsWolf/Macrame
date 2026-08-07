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
macrame-db = "0.9"
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
| v8 | `concepts.rowid_pk`, the third FTS trigger, and the two unread indices dropped |
| v9 | `trg_concepts_guard_delete` becomes conditional on an archive session, so concepts can be archived (D-129) |
| v10 | `trg_concepts_log_insert` becomes conditional on the same marker, so rehydration mints no transaction-time facts (D-131) — **current** |

v8 is the last rung that could change a *primary key* before the 1.0 freeze: `rowid_pk INTEGER PRIMARY KEY` costs `id` the primary key, and D-036 forbids a primary-key change after 1.0 (D-119). It also drops `idx_annotations_label` and `idx_lc_tgt_active`, which shipped in the v7 baseline with no query that seeks on them — measured at −7.9% off `assert_edge` (D-089, D-118).

---

## Rust Implementation

| Detail | Value |
|---|---|
| Edition | Rust 2021 |
| MSRV | **1.88** (verified, not declared) |
| Runtime | tokio async, single process |
| Engine | libSQL 0.9.30 (MIT, unmodified) |
| Schema version | 10 |
| Test suite | 340 Rust · 349 with `metrics` · 355 Python — all green (measured 2026-08-07, 0.10.0). The three `property-tests` binaries (23 tests) are **run as their own step** — see below. **`--all-features` is not a supported configuration**, see below. Regenerate rather than trust this line: `python scripts/run_rust_suite.py --features metrics` |
| Dependencies | tokio, serde, bincode, zstd, thiserror, tracing, ulid |

### Module Map

| Module | Responsibility |
|---|---|
| `schema` | DDL, triggers, migrations |
| `graph` | CTE compilation, subgraph loading, vector filters |
| `temporal` | `as_of()`, `reconstruct()`, snapshots, archive, rehydrate |
| `vector` | Model registration, embedding upsert, DiskANN search, hybrid RRF |
| `integrity` | Audit, atomic rebuild, chunked shadow-swap rebuild |
| `connection` | `Database` handle, Write Actor, priority channels |
| `error` | `DbError` enum, error classification |

---

## Python Bindings (v0.10.0)

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
- **Opaque `Subgraph`** — A `#[pyclass]` with forwarded accessors; `.to_dict()` for callers who want the copy. It paid for itself in 0.8.0: the crate re-represented `EdgeRef` and **no binding signature moved**, because there is no converted copy whose layout had to follow (D-101, D-123).
- **Open intervals cross as `None`** — Not a sentinel datetime, because `datetime.max` cannot survive `.astimezone()` east of UTC.
- **Absent `content` crosses as `None`** — `load_subgraph` does not fetch document text unless asked (`content=True`). `""` cannot mark *not loaded*, because it is a valid value of the type (D-116, D-123).
- **Every error is typed** — 35 exception classes under `MacrameError`, with six intermediate groups for catching sets: `IntegrityError`, `ValidationError`, `VectorError`, `TemporalError`, `WriterError`, `BudgetError`.
- **`metrics` shipped on** — The wheel ships with the `metrics` feature enabled because feature flags do not survive into binary artifacts.

---

## Performance (measured, not gated)

Re-measured at 0.8.0, because [B2](docs/architecture/s13-decision-register.md#d-115) changed how a
`Subgraph` is represented, [B3](docs/architecture/s13-decision-register.md#d-116) changed what a
load carries, and [B4](docs/architecture/s13-decision-register.md#d-118) dropped an index — three
reasons a table of 0.7.0 numbers would have been describing a different crate.

| Operation | Budget | 0.7.0 | 0.8.0 | 0.9.0 | 0.10.0 |
|---|---|---|---|---|---|
| Single assertion | ≤ 5 ms | — | 258 µs, published with an **O(out-degree)** caveat (D-059) | 224 µs, and the caveat is **retired on measurement** (D-134) | 220 µs |
| Single concept upsert | ≤ 3 ms | — | — | 198 µs | 193 µs |
| Chunk commit (edges, 90 rows) | ≤ 3 ms | 2.39 ms | 2.40 ms | 2.38 ms | **2.71 ms — see below** |
| Three-hop traversal | ≤ 10 ms | 2.1 ms | **1.66 ms** | 1.61 ms | 1.72 ms |
| Vector top-10 | ≤ 20 ms | 294 µs | **246 µs** | 248 µs | 264 µs |
| Hybrid top-10 | ≤ 50 ms | 2.0 ms | **1.77 ms** | 1.77 ms | 1.79 ms |
| Full fold (reconstruct) | ≤ 100 ms | 21 ms | **16.9 ms** | 17.1 ms | 16.5 ms |
| Composition (snapshot + delta) | ≤ 100 ms | 3.4 ms | **2.18 ms** | 2.22 ms | 2.06 ms |
| Rehydrate, 1 concept | ≤ 5 ms | — | n/a | 3.71 ms | 3.41 ms |
| Rehydrate, per concept after the 1st | ≤ 300 µs | — | n/a | ~74 µs to n=1,000; **114 µs at n=10,000** | ~71 µs to n=1,000; **105 µs at n=10,000** |

**0.10.0's column is a full re-measurement, median of three sessions, controls published below.**
Every row is inside its budget. Eight of the ten are within ±8% of 0.9.0 — below the ~11%
single-arm variance [D-134](docs/architecture/s13-decision-register.md#d-134) measured and far
below [D-070](docs/architecture/s13-decision-register.md#d-070)'s ~29% session spread — which is
the expected answer, because 0.10.0 changed no traversal, no search, no fold and no write path.
`control/select_1` reads **1.55–1.69 µs** per group against
[D-090](docs/architecture/s13-decision-register.md#d-090)'s recorded **1.589–1.639 µs**, so the
machine is where it was.

**One row is not noise, and it is not explained.** Chunk commit has published 2.39 / 2.40 / 2.38 ms
for three releases and now reads **2.71 ms** — five measurements across today's sessions at 2.65,
2.69, 2.70, 2.71, 2.73, a **1.1% spread** with a normal control beside it. A 14% rise that stable is
not session variance. Nothing in 0.10.0 touches the edge write path: W2's check runs at `open`, and
W4.8's asserts are `debug_assert`s a release bench compiles out. **It is reported as measured and
attributed to nothing**, which is the same standard
[D-136](docs/architecture/s13-decision-register.md#d-136) applied to the 3× budget miss on the
populated-table arm.

**It does not overturn `chunk_rows::EDGES`.** [D-058](docs/architecture/s13-decision-register.md#d-058)
solved the 90-row constant against the 3 ms bound *from* the 2.39 ms figure, and one afternoon on one
machine with no mechanism is not grounds to re-derive a load-bearing constant. The reconciliation is
already owned: it is item 1 of
[Appendix C's 0.11.0 list](docs/architecture/appendices.md#named-for-0110-in-this-order), which
existed before this measurement and now has a second reason to run.

**Two controls, or the read-path numbers would mean nothing.** A uniform improvement across
unrelated paths is what a faster *machine* looks like, so: the fixed `control/select_1` row reads
**1.51–1.62 µs** against the **1.589–1.639 µs**
[D-090](docs/architecture/s13-decision-register.md#d-090) recorded, and the chunk-commit path —
which 0.8.0 did not touch — is **2.39 → 2.40 ms**. The machine has not moved and an untouched path
has not moved, so the 12–36% on the read paths is the code.

**0.9.0 re-measured the same rows and the answer is "nothing moved", which is the result rather
than the absence of one.** 0.9.0 changed the archive path and two triggers; it touched no traversal,
no search and no fold, so a table that showed a change would be evidence of a problem. Every
carried-over row but one is within **3.2%** of its 0.8.0 figure — the largest being three-hop
traversal at −3.1% — with `control/select_1` at **1.51–1.54 µs** across every group.

**The new row is the one 0.9.0 could plausibly have cost something.** The `v9 → v10` rung puts a
`WHEN NOT EXISTS (SELECT 1 FROM sqlite_master …)` clause on the concepts insert log trigger, and it
is evaluated on **every concept write**, not only during an archive. At **198 µs** against a 3 ms
budget the gating is not measurable on this fixture — worth stating, because "we added a subquery to
the hot write path" is the kind of change that is usually paid for somewhere.

**The single-assertion row reads 13% lower and that is not claimed as an improvement.** Nothing in
0.9.0 touches the `links` write path, and no mechanism explains it. It is reported as measured and
attributed to nothing. The 0.9.0 text added a second reason to distrust the figure — that the row is
complexity-bound rather than a stable constant, "since it remains linear in out-degree" — and that
half is now withdrawn: it was never measured, and
[D-134](docs/architecture/s13-decision-register.md#d-134) measured it. What remains is an
unexplained 13%, which is the smaller and more honest claim.

**Figures are the median of three runs, and the reason is a 21% excursion that the control did not
catch.** The first pass read the full fold at **20.4 ms** — with `control/select_1` sitting normal at
1.59 µs — and two repeats returned 16.96 and 17.09 ms. A `SELECT 1` round trip bounds machine,
scheduler and engine-overhead noise; it does not bound page-cache state or fsync variance, so an
I/O-bound row needs repetition *as well as* a control. [D-070](docs/architecture/s13-decision-register.md#d-070)
put this project's session-to-session noise at ~29%, which is exactly the size of the thing that
almost got written down here as a regression.

**The single-assertion row's caveat is retired, and it was wrong for four minor versions.** This
paragraph used to say the row "remains linear in out-degree, so a high-degree hub still exceeds it".
`overlap_guard` now measures the assertion into tables of 0, 2,000 and 8,000 edges — hub out-degree
0, 666 and 2,666 — at 983 / 920 / 882 µs, median of three sessions against a 1.52 µs control, so
out-degree rises by thousands and latency does not move
([D-134](docs/architecture/s13-decision-register.md#d-134)). The claim described the access
path as it stood in 0.5.5 and has been false since the `v5 → v6` rung shipped `idx_lc_open_interval`
([D-059](docs/architecture/s13-decision-register.md#d-059)) — it outlived the defect by four
releases because nothing measured it. The real cost is O(version count per edge key), which
archival caps. Dropping `idx_lc_tgt_active` bought −7.9% on that path
([D-118](docs/architecture/s13-decision-register.md#d-118)); the complexity claim it was said not to
change was not there to change.

Those figures are a shape, not a decimal: session-to-session spread on this path is ~11%, and
normalising by the control does not remove it ([D-070](docs/architecture/s13-decision-register.md#d-070)).

All budgets measured on named reference hardware, and deliberately **not** CI gates
([D-055](docs/architecture/s13-decision-register.md#d-055)) — an absolute `≤ 5 ms` on a shared
runner is an assertion about whichever machine picked up the job. Regression detection uses
criterion baselines, machine against itself. See [§9 of the architecture docs](docs/architecture/s6-s10-flows-to-dependencies.md#9-performance-budgets) for full table.

---

## Known Risks

| Risk | Mitigation |
|---|---|
| **R15: Concurrent open → access violation** (libSQL 0.9.30) | One open per database; R15 reproduces transparently through Python. **`--features property-tests` is run as its own step**, not folded into the suite: `integrity_property_tests` needs a database per case, and inside the full run it faults often enough that the classifier's three retries are routinely exhausted. Alone it is ~50/50 and green when it completes — measured 2026-08-07, and it is the engine rather than the tests |
| **Property test binaries fault mid-suite** | `property-tests` feature gate; serialised runs; CI classifies each run rather than counting failures, and retries only a crash |
| **Covering index wins over selective** | `EXPLAIN QUERY PLAN` assertions on every index-sensitive query |
| **Snapshot chain divergence** | `verify_snapshot_chain()` reports but does not repair (snapshots are disposable) |

**`--all-features` is not a configuration this project supports or gates**, and 0.10.0 stopped publishing a test count for it. `--all-features` is `metrics` + `property-tests` together, which puts the R15-prone binaries back inside the main run — the exact arrangement the step above exists to avoid. Measured 2026-08-07: **4 of 4 runs crashed at one attempt, and 4 of 5 still went red at the six-attempt retry budget** the quarantined step uses. A required job that fails four times in five is not a gate, it is noise that teaches people to re-run CI without reading it. Run `--features metrics` and `--features property-tests` as the two separate steps CI does ([D-140](docs/architecture/s13-decision-register.md#d-140)).

---

## Minimum Supported Rust Version

**1.88**, verified rather than declared — `cargo +1.88.0 check --all-features --all-targets` passes and 1.85 does not. The constraint comes from `libsql-ffi`'s build dependency chain (`bindgen → which → home`), not from this crate's own code (which needs only 1.73).

---

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API)
- [Architecture Quick Reference](docs/quickref.md) — v0.9.0 reference: API, schema, decisions, performance
- [Python bindings](docs/architecture/s14-python-bindings.md) — §14: async→sync boundary, error tree, stubs
- [Decision register](docs/architecture/s13-decision-register.md) — D-001…D-109 with rationale

---

## Naming

Distribution `macrame-db`, import `macrame` — on both crates.io and PyPI. The Rust side has no caveat: a crate's `[lib] name` is namespaced per build graph, so `macrame-db` providing `macrame` collides with nothing. `site-packages` is flat.

The PyPI package `macrame` is an unrelated, effectively abandoned build tool (0.0.1, 2021). If it installs a top-level `macrame/`, then installing both leaves two distributions contending for one directory — `pip` warns on file conflicts, so this is a known and non-silent risk. Importing as `macrame_db` is the fallback if it ever matters.

---

## License

See [LICENSE](LICENSE) for details.
