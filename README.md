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
| **Bitemporal by design** | Two independent clocks per row — *valid time* (when a fact held in the world) and *transaction time* (when the database learned it), each addressable on its own. `as_of_valid(ts)` answers "what did the world look like?"; `as_of_recorded(ts)` and `reconstruct(ts)` answer "what did we believe?"; setting both asks what we believed then about what was true then ([D-174](docs/architecture/s13-decision-register.md#d-174), 0.13.2). |
| **Single file, embedded** | The entire database is one file on the local filesystem. Link it directly into your application. Run on Windows desktop, Linux, or macOS — the Rust suite runs on all three in CI. |
| **Graph + vectors + search** | Recursive CTE traversal, native DiskANN vector search, FTS5 keyword search, and hybrid RRF fusion — all in one crate, no external graph library. |
| **Five in-memory analytics** | Dijkstra, A*, SCC, k-core, and Louvain — operating on a typed `Subgraph` with zero external dependencies. |
| **Branching** | A fork is a lineage of belief, not a copy: one row in `branches`, no ledger table read or written. A branch reads its ancestry as of its fork point, writes its own rows *beside* its parent's rather than over them, answers `diff(a, b)` against another lineage, and is abandoned in one transaction that takes its links, its concepts, its log entries and its own register row. Merge is refused, in writing — Doctrine III has no neutral answer to which assertion supersedes which ([D-213](docs/architecture/s13-decision-register.md#d-213) … [D-232](docs/architecture/s13-decision-register.md#d-232), 0.15.0). |
| **Rebuildable materialization** | `links_current` is a cache of current belief, always rebuildable from the append-only `transaction_log`. Drift is detectable by audit, recoverable by atomic or chunked rebuild. |
| **Archival path** | Closed intervals move to a cold database inside atomic sessions. Point-in-time reconstruction composes from snapshots plus anchored folds — fast because it doesn't fold from genesis. |
| **Runtime safety** | One Write Actor serialises all writes; read connections carry `PRAGMA query_only = ON` enforced at the engine level. No raw SQL escapes the guard. |

**`as_of` became `as_of_valid` and `as_of_recorded` in 0.13.2, and this is a break.**
Through 0.13.1 a single `ts` was compared against `links.valid_from`/`valid_to` — valid
time — for the graph's shape and against `transaction_log.recorded_at` — transaction time —
for concept attributes under `AttributeMode::AtTime`. So `as_of("2020-06-01")` returned the
edges that held in 2020 labelled with *what was believed in 2020*: a title corrected today
to fix a 2020 typo came back uncorrected. Right answer to "what did we believe"; wrong
answer to "what was true", and the name promised the second.

The semantics were written down a release before they were changed, deliberately, so the
change could be reviewed against a stated position rather than argued in the commit that
made it ([D-160](docs/architecture/s13-decision-register.md#d-160), 0.12.17 →
[D-174](docs/architecture/s13-decision-register.md#d-174), 0.13.2).

**To migrate:** `as_of(t)` for topology-at-`t` under current belief is now
`as_of_valid(t)`. `as_of(t)` paired with `AttributeMode::AtTime`, if what you wanted was
*belief as of `t`*, is now `as_of_recorded(t)`. Setting both is new and asks the bitemporal
question. `reconstruct()` is unchanged.

`as_of_recorded` folds the hot `transaction_log`, so it raises `RecordedInstantUnreachable`
on a database whose log has been archived — it takes a connection and no archive path, and
answering from a partial fold would return *nearly* the right topology. `AttributeMode::AtTime`
folds the same log for the *text* and raises the same error for the same reason (0.13.16).
`reconstruct()` takes the path and answers the same question.

---

## Quick Start

### Rust

```toml
[dependencies]
macrame-db = "0.15"
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
8. **Fidelity is a parameter, never a silent default** — `as_of_valid(ts)`, `as_of_recorded(ts)` and `reconstruct(ts)` say what they mean in their signatures.

### Concurrency Model

- **One writer** — a dedicated Tokio task holds the sole write-capable connection
- **Many readers** — WAL journaling; readers never block on writer
- **Two-tier priority channels** — high-priority (user-driven) preempts low-priority (background)
- **Shared as `Arc<Database>`, not cloned** — every method but `close()` takes `&self`, and `close()` takes `self` because shutdown has exactly one owner; `Arc::into_inner(db).expect("last handle").close().await` is the pattern. `Clone` is refused rather than missing: `cadence_stop` is a cloneable `watch::Sender` whose *drop* is what stops the snapshot task, so a second copy keeps it running against a closing database with nothing raising an error ([D-203](docs/architecture/s13-decision-register.md#d-203))
- **Cooperative chunking** — bounded to ~3 ms per chunk, sized from each chunk's measured hold rather than from a constant, under four per-path ceilings (90 edges, 70 concepts, 600 annotations, 30 embeddings) and a 35-row floor

### Operational Controls (0.13.0)

Everything below defaults to *leave it alone*. None of them spells "off" as the absent
value, which is the failure the tri-state types exist to prevent
([D-155](docs/architecture/s13-decision-register.md#d-155)) — a default that silently
disables a mechanism reaches every caller who never heard of the knob.

| Control | What it does |
|---|---|
| `Tuning` / `open_tuned` | One struct for the open-time options, so the set can grow without breaking callers. The three existing constructors still compile unchanged. Python takes the same options as keyword arguments — it has defaults in the language, so there is no type to import ([D-164](docs/architecture/s13-decision-register.md#d-164)) |
| `checkpoint()` | Moves WAL frames back into the main file and **reports what it moved**. Runs `FULL` then `TRUNCATE`, because a truncating checkpoint reports zeros on success and cannot describe its own work. Check `busy` before treating the file as self-contained ([D-156](docs/architecture/s13-decision-register.md#d-156)) |
| `wal_autocheckpoint` | `Default` / `Disabled` / `EveryPages(n)`. Disabling it is only correct paired with an explicit `checkpoint()`, and the cost is deferred rather than removed: ~8,400–9,100 frames in 41–45 ms at the end, against ~860 in 5.5–6.2 ms along the way ([D-157](docs/architecture/s13-decision-register.md#d-157)) |
| `writer_cache_size` / `reader_cache_size` | Split, because the writer is one connection and the readers are several — one number would mean starving the writer or multiplying the readers' footprint ([D-158](docs/architecture/s13-decision-register.md#d-158)) |
| `analyze()` / `optimize()` | Planner statistics. Before 0.12.4 nothing here ever ran `ANALYZE`, so every query was costed against SQLite's built-in guesses. `optimize()` is a no-op when nothing has moved, which is why `close()` calls it ([D-149](docs/architecture/s13-decision-register.md#d-149)) |

**`analyze()` misses the ~3 ms chunk budget by ~6× on a populated ledger, and that is
reported rather than hidden.** Measured: **19.1 ms at 40,000 edges**. `ANALYZE` is one
indivisible statement, and `PRAGMA analysis_limit = 400` damps its cost 3–4× without making
it independent of the table — the claim that it did was corrected in
[D-166](docs/architecture/s13-decision-register.md#d-166) after §8's acceptance asked for a
measurement instead of an argument. `metrics().budget_violations()` names `analyze` after
every call and is meant to. The exemption could not be decided while one kind covered both
`analyze()` and the `optimize()` that `close()` runs unprompted
([D-168](docs/architecture/s13-decision-register.md#d-168)); **that split shipped in 0.13.24**,
and both kinds stayed budget-counted for opposite reasons — `Analyze`'s miss is structural and
constant, `Optimize`'s is rare and therefore the informative one
([D-197](docs/architecture/s13-decision-register.md#d-197)).

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
| v10 | `trg_concepts_log_insert` becomes conditional on the same marker, so rehydration mints no transaction-time facts (D-131) |
| v11 | `idx_links_recorded_at` and `idx_links_target` on the `links` ledger, so neither archive predicate scans it (D-151) |
| v12 | The `branches` register, `branch_id` on all four ledger tables with a real foreign key, `links_current` re-keyed per lineage, three log triggers redefined and four guards added ([D-214](docs/architecture/s13-decision-register.md#d-214)…[D-217](docs/architecture/s13-decision-register.md#d-217)) |
| v13 | `trg_branches_frozen_delete` becomes conditional on an archive session, so a lineage can be abandoned, and `cold.branches` arrives with it ([D-230](docs/architecture/s13-decision-register.md#d-230)) |
| v14 | `idx_lc_lineage_cut` on `links_current`, the index the branched read seeks and the trunk walk does not ([D-231](docs/architecture/s13-decision-register.md#d-231)) |
| v15 | `links`' primary key gains `branch_id`, last, and `cold.links` takes the same key ([D-232](docs/architecture/s13-decision-register.md#d-232)) — **current** |

**v15 is the last rung that changed a *primary key* before the 1.0 freeze, and v8 was the one before it.** D-036 forbids a primary-key diff after 1.0, and D-032 is what reserves the pre-1.0 window for exactly this: v15 widens `links` because two lineages are *allowed* to believe different things about one edge, and the old key refused that pair with a bare `UNIQUE` error naming a storage key the caller has never seen. `branch_id` goes **last**, so the five-column covering seek the archive sweep runs per candidate row survives the change. On v8: `rowid_pk INTEGER PRIMARY KEY` costs `id` the primary key, and D-036 forbids a primary-key change after 1.0 (D-119). It also drops `idx_annotations_label` and `idx_lc_tgt_active`, which shipped in the v7 baseline with no query that seeks on them — measured at −7.9% off `assert_edge` (D-089, D-118).

---

## Rust Implementation

| Detail | Value |
|---|---|
| Edition | Rust 2021 |
| MSRV | **1.88** (verified, not declared) |
| Runtime | tokio async, single process |
| Engine | libSQL 0.9.30 (MIT, unmodified) |
| Schema version | **15** |
| Test suite | 717 Rust · 698 with `--no-default-features` · 584 Python (2 skipped) — all green (measured 2026-09-05, 0.15.13, one Windows box; the branch's CI history is [D-234](docs/architecture/s13-decision-register.md#d-234) and worth reading before quoting this line as replicated). `metrics` is a **default** feature since 0.12.11, so the first figure is a plain `cargo test`; the second is the same suite with the counters compiled out, and the 19-test gap is `actor_metrics_tests` (12, the whole target), `src/metrics.rs`'s five unit tests, one gated test in `checkpoint_tests`, and the doc-test on `Database::metrics` — enumerated at 0.14.23 by diffing `cargo test -- --list` against the same list under `--no-default-features`, which is the only way this sentence stays true as the metrics target grows. It read *16* for two releases while the subtraction said 19. **That second figure was published for two releases against a configuration that did not build** — three examples called `Database::metrics()` with no `required-features` entry, which is [D-169](docs/architecture/s13-decision-register.md#d-169) recurring and is fixed in 0.13.36 ([D-209](docs/architecture/s13-decision-register.md#d-209)). The three `property-tests` binaries (23 tests) are **run as their own step** — see below. **`--all-features` is not a supported configuration**, see below. **The second figure is a CI gate since 0.14.24** ([D-241](docs/architecture/s13-decision-register.md#d-241)): the feature-off suite is *run* on ubuntu, not merely compiled, because until then nothing could falsify the number this line publishes. Regenerate rather than trust this line: `python scripts/run_rust_suite.py`, and `python scripts/run_rust_suite.py --no-default-features` for the second — a bare `cargo test --no-default-features` stops at the first R15 crash and reports the partial count as the total |
| Dependencies | tokio, serde, bincode, zstd, thiserror, tracing, ulid |

### Module Map

| Module | Responsibility |
|---|---|
| `schema` | DDL, triggers, migrations |
| `graph` | CTE compilation, subgraph loading, vector filters |
| `temporal` | `as_of()`, `reconstruct()`, snapshots, archive, rehydrate |
| `vector` | Model registration, embedding upsert, DiskANN search, hybrid RRF |
| `integrity` | Audit, atomic rebuild, chunked shadow-swap rebuild |
| `branch` | `BranchId`, `Branch`, `BranchView`, `fork`, `diff`, lineage resolution |
| `connection` | `Database` handle, Write Actor, priority channels |
| `error` | `DbError` enum, error classification |

---

## Python Bindings (v0.15.0)

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
- **Every error is typed** — 49 exception classes under `MacrameError` (50 including the base), of which seven are intermediate groups for catching sets — `IntegrityError`, `ValidationError`, `VectorError`, `TemporalError`, `WriterError`, `BudgetError`, `BranchError` — leaving 42 leaves. `BranchError` is 0.15.0's, over the six refusals a lineage can raise. `DbError::kind()` returns one of twelve `ErrorKind` values — the same seven groups plus the five ungrouped failures, so the Rust and Python taxonomies are one thing rather than two — and its match has no wildcard arm, so a variant added without a classification does not compile ([D-242](docs/architecture/s13-decision-register.md#d-242)). The *exception class* is a step further: a variant added without one is caught by `binding_parity_tests`, which **runs** rather than compiles: `DbError` is `#[non_exhaustive]` since 0.13.34, so the mapping's wildcard arm is mandatory and the compiler no longer checks completeness ([D-207](docs/architecture/s13-decision-register.md#d-207)).
- **`metrics` shipped on** — The wheel ships with the `metrics` feature enabled because feature flags do not survive into binary artifacts. It has been a Rust default since 0.12.11 too, so the two sides no longer differ ([D-154](docs/architecture/s13-decision-register.md#d-154)).
- **Parity is a wave, not a side effect** — 0.13.0 closed six gaps at once (W6): the archive-session and chunk-row constants, `registered_models()` / `declared_dimension()`, the maintenance calls and tuning keywords above, and a clock seam for tests. A gap opened in the release that created the feature is the one that never becomes a convention — the constants had gone eight releases for want of being anyone's next task.
- **The chunk-row constants are ceilings, not sizes** — `CHUNK_ROWS_EDGES` and its three siblings bound the adaptive loop from above; a populated database converges below them. Dividing a batch by one to predict transaction count reads a 0.11.0 fact ([D-161](docs/architecture/s13-decision-register.md#d-161)).
- **Three methods are deliberately absent** — `raw()`, `read_conn()` and `shadow_step`. The first two would dissolve the single-writer property or hand out a shared connection; the third is safe in Rust but its epoch obligation would cross as a convention where it is currently a type, on a method whose failure mode is a stale projection swapped over a live one without erroring ([D-165](docs/architecture/s13-decision-register.md#d-165)). A test asserts all three stay absent, so adding one means answering the register rather than deleting a line.

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

**There is no 0.11.0 or 0.12.0 column, and the second absence needs a word.** 0.11.0 changed no
code that runs, so a column would have been a second measurement of the same crate. 0.12.0 does
change one of these rows — **`bulk_import` no longer commits 90-row chunks.** 90 is now the
ceiling it starts from and it settles at 35 within a chunk or two
([D-146](docs/architecture/s13-decision-register.md#d-146)), so *"chunk commit, edges, 90 rows"*
still names a real measurement of a real transaction and no longer names what that method does.
A 0.12.0 column is not published rather than half-published: this table is a per-release series
measured as a whole under one control, and adding one row measured in a different session is the
practice [D-070](docs/architecture/s13-decision-register.md#d-070) and
[D-145](docs/architecture/s13-decision-register.md#d-145) exist to prevent. What 0.12.0 measured
instead is in [§5.1.5](docs/architecture/s5-modules.md#515-cooperative-chunking--the-golden-rule),
against the fixed size rather than against previous releases.

**0.10.0's column is a full re-measurement, median of three sessions, controls published below.**
Every row is inside its budget. Eight of the ten are within ±8% of 0.9.0 — below the ~11%
single-arm variance [D-134](docs/architecture/s13-decision-register.md#d-134) measured and far
below [D-070](docs/architecture/s13-decision-register.md#d-070)'s ~29% session spread — which is
the expected answer, because 0.10.0 changed no traversal, no search, no fold and no write path.
`control/select_1` reads **1.55–1.69 µs** per group against
[D-090](docs/architecture/s13-decision-register.md#d-090)'s recorded **1.589–1.639 µs**, so the
machine is where it was.

**One row read high, and 0.12.0 explained it: the machine, not the chunk**
([D-145](docs/architecture/s13-decision-register.md#d-145)). Chunk commit has published
2.39 / 2.40 / 2.38 ms for three releases and this column reads **2.71 ms** — five measurements at a
1.1% spread, which was taken at the time as evidence that a 14% rise could not be session variance.
It was not evidence about that at all: a tight spread *within* a session says nothing about the
spread *between* sessions, which [D-070](docs/architecture/s13-decision-register.md#d-070) had
already measured at ~29%. Re-run over six sessions, the arm tracks `control/select_1`
monotonically — with the control at or below D-090's recorded band it reads **2.356 / 2.358 /
2.365 ms**, a 0.4% spread agreeing with 2.39; with an elevated control it reads 2.54-2.73.
**The cell is left at 2.71 because that is what was measured here**, and this table is a
per-release series rather than a statement of current cost. The current cost is 2.39 ms.

**It never overturned `chunk_rows::EDGES`, and by 0.11.0 it could not have.**
[D-058](docs/architecture/s13-decision-register.md#d-058) solved the 90-row constant against the
3 ms bound *from* the 2.39 ms figure, which is why a 14% move in that figure looked consequential.
It is not any more: [D-143](docs/architecture/s13-decision-register.md#d-143) re-derived all four
constants against the fixture matrix, on grounds that never mention this number. The constant stays
at 90 for those reasons, and would have whichever way this row resolved.

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
| **R15: cumulative `connect()` → access violation** (libSQL 0.9.30) | One open per database; R15 reproduces transparently through Python. **`--features property-tests` is run as its own step**, not folded into the suite: `integrity_property_tests` needs a database per case, and inside the full run it faults often enough that the classifier's three retries are routinely exhausted. Alone it crashes on **93 of 100 attempts** and is green when it completes — measured 2026-08-08 under sustained load, and it is the engine rather than the tests. This row said *"~50/50"* until then, on no measurement of this quantity; `.cargo/config.toml` is where the rate lives and what to read before quoting it ([D-147](docs/architecture/s13-decision-register.md#d-147)) |
| **Property test binaries fault mid-suite** | `property-tests` feature gate; serialised runs; CI classifies each run rather than counting failures, and retries only a crash. A property case opens a database per generated case, which is the highest cumulative-`connect()` shape in the repo and so the worst-exposed by construction ([D-148](docs/architecture/s13-decision-register.md#d-148)) |
| **Covering index wins over selective** | `EXPLAIN QUERY PLAN` assertions on every index-sensitive query |
| **Snapshot chain divergence** | `verify_snapshot_chain()` reports but does not repair (snapshots are disposable) |
| **Retired concepts reachable through vector search** (0.13.1, **closed 0.13.18**) | `search_vector` joined only the embeddings table, so it had no `retired` column in scope and returned soft-deleted concepts; `hybrid_search` inherited it through its vector arm. Closed by one `concepts` join and a single spliced visibility predicate, applied at all three readers of an embedding table rather than at the one the finding named (W9.3, [D-191](docs/architecture/s13-decision-register.md#d-191)) |
| **Search reads today's corpus whatever `as_of` says** (0.13.1, **closed 0.13.19**) | No search surface bounded a concept's valid interval, so `as_of` could not reach retrieval and `search_filtered` mixed a past topology with the present corpus. `search_vector`, `keyword_search` and `HybridSearch` now take an optional `as_of_valid`, and a filtered search reads the traversal's (W9.4, [D-192](docs/architecture/s13-decision-register.md#d-192)) |

**`--all-features` is not a configuration this project supports or gates**, and 0.10.0 stopped publishing a test count for it. `--all-features` is `metrics` + `property-tests` together, which puts the R15-prone binaries back inside the main run — the exact arrangement the step above exists to avoid. Measured 2026-08-07: **4 of 4 runs crashed at one attempt, and 4 of 5 still went red at the six-attempt retry budget** the quarantined step uses. A required job that fails four times in five is not a gate, it is noise that teaches people to re-run CI without reading it. Run `--features metrics` and `--features property-tests` as the two separate steps CI does ([D-140](docs/architecture/s13-decision-register.md#d-140)).

---

## Minimum Supported Rust Version

**1.88**, verified rather than declared — `cargo +1.88.0 check --all-features --all-targets` passes and 1.85 does not. The constraint comes from `libsql-ffi`'s build dependency chain (`bindgen → which → home`), not from this crate's own code (which needs only 1.73).

---

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API)
- [Architecture Quick Reference](docs/quickref.md) — API, schema, decisions, performance. Marked **v0.12.0** and current to [D-148](docs/architecture/s13-decision-register.md#d-148); it does not yet carry the 0.13.0 wave (D-149…D-169) the 0.13.x series toward 1.0 (D-170…[D-212](docs/architecture/s13-decision-register.md#d-212)), which includes the public-surface changes of D-205…D-208, or the branching wave (D-213…[D-242](docs/architecture/s13-decision-register.md#d-242)) — so its API section names paths this crate no longer offers. **Refreshing it is not scheduled**, and that is recorded rather than glossed: it is a derived document, and the architecture set below is the one kept true by gates. This README said "v0.9.0 reference" until 0.12.25, which was wrong about its own pointer. Where it disagrees with the architecture set, the architecture set wins — it is the normative one.
- [Python bindings](docs/architecture/s14-python-bindings.md) — §14: async→sync boundary, error tree, stubs
- [Decision register](docs/architecture/s13-decision-register.md) — D-001…D-255 with rationale
- [Release notes](docs/releases) — one document per minor, most recently [v0.15.0](docs/releases/v0.15.0.md) (branching) and [v0.14.0](docs/releases/v0.14.0.md)

---

## Naming

Distribution `macrame-db`, import `macrame` — on both crates.io and PyPI. The Rust side has no caveat: a crate's `[lib] name` is namespaced per build graph, so `macrame-db` providing `macrame` collides with nothing. `site-packages` is flat.

The PyPI package `macrame` is an unrelated, effectively abandoned build tool (0.0.1, 2021). If it installs a top-level `macrame/`, then installing both leaves two distributions contending for one directory — `pip` warns on file conflicts, so this is a known and non-silent risk. Importing as `macrame_db` is the fallback if it ever matters.

---

## License

See [LICENSE](LICENSE) for details.
