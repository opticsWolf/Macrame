# Macrame

[![CI](https://github.com/opticsWolf/Macrame/actions/workflows/ci.yml/badge.svg)](https://github.com/opticsWolf/Macrame/actions/workflows/ci.yml)
[![Python](https://github.com/opticsWolf/Macrame/actions/workflows/python.yml/badge.svg)](https://github.com/opticsWolf/Macrame/actions/workflows/python.yml)
[![crates.io](https://img.shields.io/crates/v/macrame-db.svg)](https://crates.io/crates/macrame-db)
[![docs.rs](https://img.shields.io/docsrs/macrame-db)](https://docs.rs/macrame-db)
[![PyPI](https://img.shields.io/pypi/v/macrame-db.svg)](https://pypi.org/project/macrame-db/)
[![Python versions](https://img.shields.io/pypi/pyversions/macrame-db.svg)](https://pypi.org/project/macrame-db/)
[![MSRV](https://img.shields.io/crates/msrv/macrame-db.svg)](#minimum-supported-rust-version)
[![License](https://img.shields.io/crates/l/macrame-db.svg)](#license)
[![Website](https://img.shields.io/badge/website-Macrame-blue)](https://opticswolf.github.io/Macrame/)

**A bitemporal graph ledger for knowledge management — embedded, single-file, no server.**

Macrame stores concepts linked by typed, weighted relationships — where both concepts and relationships change over time, and the history of those changes is itself a first-class asset. Everything lives in one `.db` file on disk. No database server, no network protocol, no external service.

---

## Why Macrame

| Strength | What it means |
|---|---|
| **Bitemporal by design** | Two independent clocks per row — *valid time* (when a fact held in the world) and *transaction time* (when the database learned it), each addressable on its own. `as_of_valid(ts)` answers "what did the world look like?"; `as_of_recorded(ts)` and `reconstruct(ts)` answer "what did we believe?"; setting both asks what we believed then about what was true then ([D-174](docs/architecture/s13-decision-register.md#d-174), 0.13.2). Historical subgraph loads hydrate belief-at-instant when the mode asks for it ([D-289](docs/architecture/s13-decision-register.md#d-289), 0.19.1). `reconstruct_as_of` folds the whole state on the valid axis, and a concept holds one belief per valid start with at most one open interval ([D-291](docs/architecture/s13-decision-register.md#d-291), [D-292](docs/architecture/s13-decision-register.md#d-292), 0.20.0). |
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
macrame-db = "0.20"
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
| `wal_autocheckpoint` | `Default` / `Disabled` / `EveryPages(n)`. Disabling it is only correct paired with an explicit `checkpoint()`, and the cost is deferred rather than removed: ~8,400–9,100 frames in 41–45 ms at the end, against ~860 in 5.5–6.2 ms along the way ([D-157](docs/architecture/s13-decision-register.md#d-157)). **On a bulk import the threshold is the dial** (measured, [D-275](docs/architecture/s13-decision-register.md#d-275)): a 16,000-edge random-order import runs 5.0 s at the default, 3.7 s at `EveryPages(10_000)` with a ~43 MB WAL, 3.6 s disabled with a ~554 MB WAL — the recipe, numbers and shape caveat live in `Database::bulk_import`'s docs |
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
| v15 | `links`' primary key gains `branch_id`, last, and `cold.links` takes the same key ([D-232](docs/architecture/s13-decision-register.md#d-232)) |
| v16 | `log_integrity` — one row, one bit: *has anything ever been deleted from `transaction_log`?* — seeded from the log it describes and maintained by `trg_txlog_mark_gap`, so the reach guard reads a bit instead of scanning the log on every recorded-time read ([D-249](docs/architecture/s13-decision-register.md#d-249)) |
| v17 | `idx_txlog_fold_partition`, the index the fold's own partition asks for and **not** the one the review asked for ([D-254](docs/architecture/s13-decision-register.md#d-254)) |
| v18 | `idx_links_branch` and `idx_txlog_branch`, **partial** over `branch_id <> 'main'`, so the indexes `archive_branch` scans hold only what branches wrote and the trunk's write path pays nothing ([D-273](docs/architecture/s13-decision-register.md#d-273)) |
| v19 | `trg_links_single_open`'s branch predicate carries a unary `+`, so on a database without statistics the probe seeks `idx_lc_open_interval` instead of scanning a lineage. Trigger-only ([D-274](docs/architecture/s13-decision-register.md#d-274)) |
| v20 | `kv_store`: operational state with no log trigger, no archive membership and no `branch_id` ([D-280](docs/architecture/s13-decision-register.md#d-280)) |
| v21 | `concepts.extra`, a JSON object logged like every other column; concepts payload v3 ([D-278](docs/architecture/s13-decision-register.md#d-278)) |
| v22 | `blobs` and `idx_blobs_put_at`, with an update guard and an archive-gated delete guard; `cold.blobs` arrives with the first archive session ([D-281](docs/architecture/s13-decision-register.md#d-281), [D-288](docs/architecture/s13-decision-register.md#d-288)) |
| v23 | `idx_txlog_concept_starts`, partial over the log's concept rows, so the concept interval guard seeks one row per valid start; `idx_txlog_entity` dropped. Index-only ([D-300](docs/architecture/s13-decision-register.md#d-300), [D-301](docs/architecture/s13-decision-register.md#d-301)) — **current** |

`open()` climbs the ladder one rung at a time, each in its own `BEGIN IMMEDIATE` transaction. A database stamped newer than the build is refused, never downgraded.

**v15 is the last rung that changed a *primary key* before the 1.0 freeze, and v8 was the one before it.** Since then, v16, v20 and v22 add tables, v21 adds a column, v19 redefines a trigger, and v17, v18 and v23 change indexes. None of them touches a key. D-036 forbids a primary-key diff after 1.0, and D-032 is what reserves the pre-1.0 window for exactly this: v15 widens `links` because two lineages are *allowed* to believe different things about one edge, and the old key refused that pair with a bare `UNIQUE` error naming a storage key the caller has never seen. `branch_id` goes **last**, so the five-column covering seek the archive sweep runs per candidate row survives the change. On v8: `rowid_pk INTEGER PRIMARY KEY` costs `id` the primary key, and D-036 forbids a primary-key change after 1.0 (D-119). It also drops `idx_annotations_label` and `idx_lc_tgt_active`, which shipped in the v7 baseline with no query that seeks on them — measured at −7.9% off `assert_edge` (D-089, D-118).

---

## Rust Implementation

| Detail | Value |
|---|---|
| Edition | Rust 2021 |
| MSRV | **1.88** (verified, not declared) |
| Runtime | tokio async, single process |
| Engine | libSQL 0.9.30 (MIT, unmodified) |
| Schema version | **23** |
| Test suite | 873 Rust · 851 with `--no-default-features` · 695 Python (2 skipped) — all green (measured 2026-10-30 at 0.20.0 on one Windows box, each Rust count on the runner's R15 retry, and **not replicated on CI** at the cut, so [D-234](docs/architecture/s13-decision-register.md#d-234) and [D-262](docs/architecture/s13-decision-register.md#d-262) apply to them exactly as they applied to every one-box figure before them. 0.19.0 closed at 817 / 795 / 681. 0.18.0 closed at 797 / 775 / 666, and 0.19.0's +20 / +20 / +15 are all P4's ([D-281](docs/architecture/s13-decision-register.md#d-281)); 0.18.0's per-item deltas — 769 → 770 → 781 → 794 → 797 — are in the register at [D-279](docs/architecture/s13-decision-register.md#d-279), [D-280](docs/architecture/s13-decision-register.md#d-280) and [D-278](docs/architecture/s13-decision-register.md#d-278) and [D-286](docs/architecture/s13-decision-register.md#d-286), each measured at its own commit. **The last replicated figures are 0.17.0's** — 769 / 747 / 616, measured 2026-09-11 and replicated on all three CI runners at that release commit itself — run `34626309278`: 769 on ubuntu and macOS at attempt 1/3, 769 on Windows at attempt 2/8, the feature-off 747 on ubuntu at attempt 1/3, and the quarantined property arm **793 across 64 targets** at attempt 1/6 on ubuntu and macOS with `crashed-R15` at 6/6 on Windows, which is [D-236](docs/architecture/s13-decision-register.md#d-236)'s read and the reason that step is non-blocking. **Those figures are replicated; the ones this line carried through 0.16.3 were the 0.16.0 release's** — 757 / 736 / 607, run `34155656904` — and the patch releases between carried their own counts in the register ([D-274](docs/architecture/s13-decision-register.md#d-274)…[D-277](docs/architecture/s13-decision-register.md#d-277)) rather than re-arguing this line per release. Read [D-234](docs/architecture/s13-decision-register.md#d-234) and [D-262](docs/architecture/s13-decision-register.md#d-262) before quoting *any earlier* release's version of this line as replicated: D-243 … D-261 shipped with no CI run of any kind, because the branch filter did not reach `dev/**` and the draft pull request the policy relies on was never opened). `metrics` is a **default** feature since 0.12.11, so the first figure is a plain `cargo test`; the second is the same suite with the counters compiled out, and the **22-test gap** is `actor_metrics_tests` (14, the whole target), `src/metrics.rs`'s five unit tests, one gated test in `checkpoint_tests`, one gated test in `bulk_deferred_tests` (D-277's exemption assertion; D-276's exemption assertions are gated *inside* `vector_tests`, whole-test count unchanged), and the doc-test on `Database::metrics` — enumerated at 0.14.23 by diffing `cargo test -- --list` against the same list under `--no-default-features`, which is the only way this sentence stays true as the metrics target grows. **The second figure is a CI gate since 0.14.24** ([D-241](docs/architecture/s13-decision-register.md#d-241)): the feature-off suite is *run* on ubuntu, not merely compiled, because until then nothing could falsify the number this line publishes. Regenerate rather than trust this line: `python scripts/run_rust_suite.py`, and `python scripts/run_rust_suite.py --no-default-features` for the second — a bare `cargo test --no-default-features` stops at the first R15 crash and reports the partial count as the total |
| Dependencies | libsql, tokio, serde, serde_json, bincode, zstd, thiserror, tracing, ulid, sha2 |

### Module Map

| Module | Responsibility |
|---|---|
| `schema` | DDL, triggers, migrations |
| `graph` | CTE compilation, subgraph loading, vector filters |
| `temporal` | `as_of_valid()`, `as_of_recorded()`, `reconstruct()`, `reconstruct_as_of()`, snapshots, archive, rehydrate |
| `vector` | Model registration, embedding upsert, DiskANN search, hybrid RRF |
| `integrity` | Audit, atomic rebuild, chunked shadow-swap rebuild |
| `branch` | `BranchId`, `Branch`, `BranchView`, `fork`, `diff`, lineage resolution |
| `connection` | `Database` handle, Write Actor, priority channels |
| `error` | `DbError` enum, error classification |

---

## Python Bindings (v0.16.0)

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
- **Every error is typed** — 58 exception classes under `MacrameError` (59 including the base), of which seven are intermediate groups for catching sets — `IntegrityError`, `ValidationError`, `VectorError`, `TemporalError`, `WriterError`, `BudgetError`, `BranchError` — leaving 51 leaves. `BranchError` is 0.15.0's, over the six refusals a lineage can raise. `DbError::kind()` returns one of twelve `ErrorKind` values — the same seven groups plus the five ungrouped failures, so the Rust and Python taxonomies are one thing rather than two — and its match has no wildcard arm, so a variant added without a classification does not compile ([D-242](docs/architecture/s13-decision-register.md#d-242)). The *exception class* is a step further: a variant added without one is caught by `binding_parity_tests`, which **runs** rather than compiles: `DbError` is `#[non_exhaustive]` since 0.13.34, so the mapping's wildcard arm is mandatory and the compiler no longer checks completeness ([D-207](docs/architecture/s13-decision-register.md#d-207)).
- **`metrics` shipped on** — The wheel ships with the `metrics` feature enabled because feature flags do not survive into binary artifacts. It has been a Rust default since 0.12.11 too, so the two sides no longer differ ([D-154](docs/architecture/s13-decision-register.md#d-154)).
- **Parity is a wave, not a side effect** — 0.13.0 closed six gaps at once (W6): the archive-session and chunk-row constants, `registered_models()` / `declared_dimension()`, the maintenance calls and tuning keywords above, and a clock seam for tests. A gap opened in the release that created the feature is the one that never becomes a convention — the constants had gone eight releases for want of being anyone's next task.
- **The chunk-row constants are ceilings, not sizes** — `CHUNK_ROWS_EDGES` and its three siblings bound the adaptive loop from above; a populated database converges below them. Dividing a batch by one to predict transaction count reads a 0.11.0 fact ([D-161](docs/architecture/s13-decision-register.md#d-161)).
- **Three methods are deliberately absent** — `raw()`, `read_conn()` and `shadow_step`. The first two would dissolve the single-writer property or hand out a shared connection; the third is safe in Rust but its epoch obligation would cross as a convention where it is currently a type, on a method whose failure mode is a stale projection swapped over a live one without erroring ([D-165](docs/architecture/s13-decision-register.md#d-165)). A test asserts all three stay absent, so adding one means answering the register rather than deleting a line.

---

## Performance (measured, not gated)

Measured at **0.20.0** on the reference machine (Windows 11, NVMe SSD, 32 GB RAM, release build):
the median of three criterion sessions of `benches/budgets.rs`, with the `control/select_1` row
reading 1.42–1.57 µs in every group. Each figure is the operation alone; fixture setup and
teardown sit outside the timed region
([D-302](docs/architecture/s13-decision-register.md#d-302)). The bench fixtures are smaller than
the populations some budgets name (2,000 concepts, a few thousand edges and log rows), so a met row
is met at bench scale.

| Operation | Budget | Measured | |
|---|---|---|---|
| Single edge assertion | ≤ 5 ms | 181 µs | Met |
| Single edge assertion into 0 / 2,000 / 8,000-edge tables | ≤ 5 ms | 306 / 242 / 279 µs | Met; flat in out-degree |
| Single concept upsert | ≤ 3 ms | 363 µs | Met; flat in the concept's history |
| Chunk commit, edges, 90 rows | ≤ 3 ms | 3.99 ms | **Missed**; see below |
| Chunk commit, edges, 90 rows into an 8,000-edge table | ≤ 3 ms | 11.8 ms | **Missed**; see below |
| Chunk commit, concepts, 70 rows | ≤ 3 ms | 2.56 ms | Met |
| Chunk commit, annotations, 600 rows | ≤ 3 ms | 1.33 ms | Met |
| Chunk commit, embeddings, 30 rows | ≤ 3 ms | 1.33 ms | Met |
| Three-hop traversal, warm | ≤ 10 ms | 1.53 ms | Met |
| `as_of_valid` traversal | ≤ 15 ms | 742 µs | Met |
| Vector top-10 | ≤ 20 ms | 248 µs | Met |
| Hybrid top-10 (vector + FTS5 + RRF) | ≤ 50 ms | 1.71 ms | Met |
| Full fold (`reconstruct`) | ≤ 100 ms | 16.1 ms | Met |
| Snapshot + delta composition | ≤ 100 ms | 3.04 ms | Met |
| `audit_current` | ≤ 200 ms | 11.9 ms | Met |
| `rebuild_current` | ≤ 500 ms | 28.8 ms | Met |
| Snapshot write | ≤ 2 s | 1.97 ms | Met |
| Rehydrate, 1 concept | ≤ 5 ms | 2.64 ms | Met |
| Rehydrate, per concept after the first | ≤ 300 µs | ~68 µs to n=1,000; 107 µs at n=10,000 | Met |

**The 90-row edge chunk is over its budget, and bulk import holds the bound anyway.** 90 is the
ceiling `bulk_import`'s chunk loop starts from, not the size it commits: the actor times each
transaction and sizes the next from it, settling near 35 rows on a populated table
([D-143](docs/architecture/s13-decision-register.md#d-143),
[D-146](docs/architecture/s13-decision-register.md#d-146)). The first chunk of an import can run
over; the ones after it are sized to the bound. What a 90-row chunk costs is index maintenance on
`links`, `links_current` and `transaction_log`, and each of those indexes serves a read that
needs it. The per-index prices, and the one index that was dropped for costing the most while
serving the least, are in [D-301](docs/architecture/s13-decision-register.md#d-301).

**Three costs are shapes rather than points, and those are what to rely on.** A single edge
assertion does not grow with out-degree: the rows above span hub out-degrees of 0, 666 and 2,666.
Its real cost is the number of versions per edge key, which archival caps
([D-134](docs/architecture/s13-decision-register.md#d-134)). A concept upsert does not grow with the
concept's history: `examples/concept_guard_cost_probe.rs` measures 0.27 ms for a concept with
10,000 versions ([D-300](docs/architecture/s13-decision-register.md#d-300)). Rehydration is linear
to about 1,000 concepts per call and dearer per concept beyond that, because FTS5's merge cost is
per transaction.

**How to read the numbers.** Session-to-session spread on this machine reaches ~29%
([D-070](docs/architecture/s13-decision-register.md#d-070)), so read shapes and orders of
magnitude, and treat a smaller move between two runs as noise. The budgets are deliberately not
CI gates ([D-055](docs/architecture/s13-decision-register.md#d-055)): an absolute `≤ 5 ms` on a
shared runner is a claim about whichever machine picked up the job. Regressions are found by
comparing a machine with itself:

```bash
cargo bench --bench budgets -- --save-baseline before
```

then `--baseline before` after the change. Hardware-independent properties, such as plan shape,
operation counts and growth ratios, are gated in `tests/`. The full budget table, with each row's
mechanism and the figures earlier releases measured, is
[§9 of the architecture docs](docs/architecture/s6-s10-flows-to-dependencies.md#9-performance-budgets).
Figures before 0.20.0 include fixture teardown and do not compare directly with these.

**A second harness answers a different question.** The budgets above are about operations this crate bounds; [`benchmarks/`](benchmarks/README.md) runs a **TPC-BiH-style workload taxonomy** (TPCTC'13 T/K/R classes) over a generated bitemporal history, on the trunk and on a divergent lineage, in Python and in Rust — 13 correctness assertions gate every run, because a benchmark that can time a wrong answer is worse than none. It is a separate package: nothing there is built by an ordinary `cargo test` here, and none of it ships in the crate. Its headline shapes are that fork latency is flat across 3× the data (empirical O(1)), that transaction-dimension work is an order pricier than valid-dimension work, that branch cost is **ancestry rather than divergence** — a fork with no writes costs what a diverged one costs — and that chain reads stay flat from depth 1 to 5 and cost 15% more at depth 10. A third instrument lives beside them: `benchmarks/diagnostics/` is the spike ladder that attributed and fixed the fresh-file bulk-import defect ([D-274](docs/architecture/s13-decision-register.md#d-274), [D-275](docs/architecture/s13-decision-register.md#d-275)), and it is where the WAL recipe on the bulk path was measured; the vector-build dissection that produced `bulk_embeddings` ([D-276](docs/architecture/s13-decision-register.md#d-276)) ran as `examples/vector_build_probe.rs`. The materialization-skipping bulk ([D-277](docs/architecture/s13-decision-register.md#d-277)) was scoped by `examples/bulk_skip_probe.rs` — the mirror trigger is 2.2× of the random-pair bulk, and the chunked rebuild re-derives the projection for 191 ms per 16k.

---

## Known Risks

| Risk | Mitigation |
|---|---|
| **R15: cumulative `connect()` → access violation** (libSQL 0.9.30) | An intermittent `STATUS_ACCESS_VIOLATION` in the engine after many opens in one process; it is the engine, not the tests, and it reproduces through Python. Keep one open per database. The test runners classify each run and retry only a crash; a binary that fails when run alone is a real failure ([D-147](docs/architecture/s13-decision-register.md#d-147)). In the 0.20.0 suite runs it took out one or two test binaries per attempt and never failed an assertion. `.cargo/config.toml` documents the R15 measurements and the harness settings they led to |
| **Property tests fault mid-suite** | Each generated case opens a database, the most R15-exposed shape in the repository, so `--features property-tests` runs as its own CI step behind the feature gate ([D-148](docs/architecture/s13-decision-register.md#d-148)) |
| **A backfill overwrites the live `concepts` projection** | The interval guard admits a non-overlapping past interval, but the projection keeps one row per id, so after a backfill current-mode reads show the backfilled version. `reconstruct_as_of` answers correctly. Re-assert the current interval after a backfill. The fix moves the log write off the trigger, which is a schema change |
| **Overlaps already in a ledger are reported, not repaired** | A ledger written before the interval guard, or by raw SQL, keeps its overlaps. `verify_concept_intervals()` names them, and a read refuses only the instants both beliefs claim |
| **The 90-row edge chunk exceeds 3 ms** | 3.99 ms on the bench. `bulk_import` sizes chunks on elapsed time from that ceiling, so only an import's first chunk runs over ([D-143](docs/architecture/s13-decision-register.md#d-143), [D-301](docs/architecture/s13-decision-register.md#d-301)) |
| **Covering index wins over selective** | `EXPLAIN QUERY PLAN` assertions on every index-sensitive query, plus pins on the plans `ANALYZE` statistics have moved |
| **Snapshot chain divergence** | `verify_snapshot_chain()` reports but does not repair (snapshots are disposable) |

**`--all-features` is not a configuration this project supports or gates**, and 0.10.0 stopped publishing a test count for it. `--all-features` is `metrics` + `property-tests` together, which puts the R15-prone binaries back inside the main run — the exact arrangement the step above exists to avoid. Measured 2026-08-07: **4 of 4 runs crashed at one attempt, and 4 of 5 still went red at the six-attempt retry budget** the quarantined step uses. A required job that fails four times in five is not a gate, it is noise that teaches people to re-run CI without reading it. Run `--features metrics` and `--features property-tests` as the two separate steps CI does ([D-140](docs/architecture/s13-decision-register.md#d-140)).

---

## Minimum Supported Rust Version

**1.88**, verified rather than declared — `cargo +1.88.0 check --all-features --all-targets` passes and 1.85 does not. The constraint comes from `libsql-ffi`'s build dependency chain (`bindgen → which → home`), not from this crate's own code (which needs only 1.73).

---

## Documentation

- [Architecture specification](docs/architecture/README.md) — normative surfaces: §4 (schema) and Appendix A (API)
- [Architecture Quick Reference](docs/quickref.md) — API, schema, decisions, performance. Current to **v0.20.0**: the header and footer name the release, the schema ladder §4 reads **v23** against `SCHEMA_VERSION` in `src/schema/migrations.rs`, and §5's API prose carries the surface through the valid axis ([D-289](docs/architecture/s13-decision-register.md#d-289)…[D-295](docs/architecture/s13-decision-register.md#d-295)). `tests/doc_currency_tests.rs` gates those four claims — header, footer, ladder rung, and how far the newest decision cited may trail the register — and `tests/doc_sync_tests.rs` and `tests/perf_claim_tests.rs` pin the analytics types, the `Subgraph` accessors and the performance numbers. None of them can check that a passage is *true*, so where this disagrees with the architecture set, the architecture set wins — it is the normative one.
- [Python bindings](docs/architecture/s14-python-bindings.md) — §14: async→sync boundary, error tree, stubs
- [Decision register](docs/architecture/s13-decision-register.md) — D-001…D-302 with rationale
- [Release notes](docs/releases) — one document per minor, most recently [v0.20.0](docs/releases/v0.20.0.md) (the valid axis for state: [D-291](docs/architecture/s13-decision-register.md#d-291)…[D-302](docs/architecture/s13-decision-register.md#d-302), `reconstruct_as_of`, the concept interval guard and `verify_concept_intervals`, one runtime refusal that is new), [v0.19.0](docs/releases/v0.19.0.md) (the blob store: [D-281](docs/architecture/s13-decision-register.md#d-281), [D-287](docs/architecture/s13-decision-register.md#d-287) and [D-288](docs/architecture/s13-decision-register.md#d-288), content-addressed bytes reclaimed by the archive rather than a refcount) and [v0.18.0](docs/releases/v0.18.0.md) (the flexible ledger: [D-278](docs/architecture/s13-decision-register.md#d-278)…[D-286](docs/architecture/s13-decision-register.md#d-286), `concepts.extra`, the KV sidecar, the relaxed edge-kind charset and opt-in attributes on the subgraph loader)

---

## Naming

Distribution `macrame-db`, import `macrame` — on both crates.io and PyPI. The Rust side has no caveat: a crate's `[lib] name` is namespaced per build graph, so `macrame-db` providing `macrame` collides with nothing. `site-packages` is flat.

The PyPI package `macrame` is an unrelated, effectively abandoned build tool (0.0.1, 2021). If it installs a top-level `macrame/`, then installing both leaves two distributions contending for one directory — `pip` warns on file conflicts, so this is a known and non-silent risk. Importing as `macrame_db` is the fallback if it ever matters.

---

## License

See [LICENSE](LICENSE) for details.
