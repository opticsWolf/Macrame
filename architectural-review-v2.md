# Macrame — Architectural Review (v2)

**Date:** 2026-08-07
**Version Reviewed:** 0.9.0 (schema v10)
**Reviewer:** Independent deep review — documentation vs. code cross-check
**Status:** Supersedes `architectural-review.md`. Three of that file's findings were checked against the code and found to be **materially wrong** (they recommend fixes that are already shipped); this file corrects them and adds new findings the prior review missed.

---

## 1. Executive Summary

Macrame is an exceptionally well-disciplined embedded bitemporal graph ledger. The architecture documentation is unusually honest — it records its own corrections, dates them, and pins its claims with executable tests (`doc_sync_tests.rs`, `doc_link_tests.rs`, `index_plan_tests.rs`, `migration_tests`). The implementation tracks the specification closely.

This review cross-checked the documented claims against the source (`src/`, `tests/`, `benches/`, `bindings/`) and against `git`/on-disk state. The headline results:

- **The crate is closer to 1.0 than the prior review reported.** Two of the prior review's two "fix before 1.0" items are already implemented and shipped (the D-059 index and the concept-log payload v2). The 1.0 blocker list is, on current evidence, **empty of new work** — what remains is reconciliation of stale prose.
- **Several documentation locations are internally contradictory** — they describe the pre-fix behaviour of the single-open guard while the fix is applied, pinned, and tested. This is the single most important category of issue found: not a code defect, but a prose defect that already misled one reviewer into recommending a fix that exists.
- **One genuine binding-level risk** the prior review did not surface: the Python `diagnostic_query`/`explain` methods open a fresh `SQLITE_OPEN_READ_ONLY` connection on every call, and `PyDatabase` holds an `RwLock`, so concurrent calls from Python threads are the R15 concurrent-open shape the project itself measures as faulting ~2/12 at 48 threads.
- **No code-level invariant violation was found.** The eight doctrine invariants are enforced at the documented points.

---

## 2. Methodology

- Read every file under `docs/architecture/` and `docs/quickref.md`, plus `README.md`, `Cargo.toml`, `pyproject.toml`.
- Read the load-bearing source: `src/schema/ddl.rs`, `src/schema/migrations.rs`, `src/error.rs`, `src/connection.rs` (assertion / overlap / chunking paths), `src/temporal/replay.rs` & `as_of.rs` (payload handling), `src/temporal/archive.rs` (rehydrate), `benches/budgets.rs`.
- Verified specific documented claims against code: trigger DDL, payload version & fields, index list, migration rungs, plan-pinning registry, the D-059 index's presence, error enum size, `git ls-files` for the alleged committed `.pdb`.
- Checked the prior review's recommendations against the current tree to confirm or retract them.

---

## 3. Corrections to the Prior In-Repo Review (`architectural-review.md`)

These three items are the reason this file exists. The prior review's findings were taken at face value from the prose; cross-checked against the code, they are wrong.

### 3.1 ❌ Prior §3.1 / §9 row 1 — "D-059 `idx_lc_open_interval` not applied" — **WRONG**

The prior review's headline "High" finding says:

> *Apply D-059's proven fix — `idx_lc_open_interval ON links_current (source_id, target_id, edge_type, valid_to, valid_from)` — in a `v10→v11` rung… The fix costs a 0.5.6-era schema rung that has been documented as ready for several releases.*

**The fix has been shipped since 0.5.6 / 0.6.0.** Evidence:

| Where | Evidence |
|---|---|
| `src/schema/ddl.rs` `CREATE_INDICES` | `CREATE INDEX IF NOT EXISTS idx_lc_open_interval ON links_current (source_id, target_id, edge_type, valid_to, valid_from);` — in the **baseline** schema, so every fresh database gets it |
| `src/schema/migrations.rs` | rung `from: 5, to: 6, name: "single-open-interval-index"` → `add_open_interval_index`, so every upgraded database gets it |
| `tests/migration_tests.rs` | `a_v5_database_climbs_to_v6_and_gains_the_open_interval_index` drops the index, re-runs migrations, asserts it reappears |
| `tests/index_plan_tests.rs:81` | `idx_lc_open_interval` is in the plan-pinning registry, labelled *"the overlap guard and the single-open probe"* |
| `benches/budgets.rs:1508` | a bench **drops** `idx_lc_open_interval` to measure the pre-fix cost — proving it is normally present |
| `docs/architecture/s4-schema.md §4.2` | *"Applied in 0.5.6 as the `v5 → v6` rung, `idx_lc_open_interval` — index-only… 47.7 → 8.0 ms, flat"* |

The prior review was misled by **stale prose** (see §4.A below) that still describes the pre-fix scan. The recommendation to add a `v10→v11` rung is incorrect — applying it again would be a redundant no-op (`CREATE INDEX IF NOT EXISTS`).

**Action:** Drop this from the 1.0 blocker list. The real issue is the contradictory prose (§4.A).

### 3.2 ❌ Prior §3.2 / §9 row 2 — "Concept log payload missing `embedding_model`" — **WRONG**

The prior review says:

> *`trg_concepts_log_insert` payload includes `title`, `content`, `valid_from`, `valid_to`, and `retired` — but not `embedding_model`. … Recommendation: Add `embedding_model` to a v2 payload format, with the deserializer branching on version.*

**The payload is already at v2 and already carries `embedding_model`.** Evidence:

- `src/schema/ddl.rs`, `CREATE_CONCEPTS_LOG_INSERT`:
  `json_object('v', 2, 'title', NEW.title, 'content', NEW.content, 'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to, 'retired', NEW.retired, 'embedding_model', NEW.embedding_model)`
- `trg_concepts_log_update` (same file): also `v`, 2 with `embedding_model`.
- `src/temporal/replay.rs:841` reads `payload.get("v")` and branches (`unwrap_or(1)`); line 863–873 reads `payload.get("embedding_model")` into the folded concept.
- `src/temporal/as_of.rs:232` hydrates `embedding_model` from the payload under `AttributeMode::AtTime`.
- The README's revision history records this as **delivered in 0.5.6 Wave 1** (defect V), and the `CREATE_TRIGGERS` doc comment in `ddl.rs` explicitly documents the v1→v2 payload bump and forward-compat ("v1 is still accepted and folds with the field absent").

The prior review trusted a **stale restoration note in §4.3** (see §4.B) that describes the 0.5.4 state and was never reconciled when Wave 1 added the field.

**Action:** Drop this from the 1.0 blocker list. The real issue is the stale §4.3 note (§4.B).

### 3.3 ❌ Prior §4.3 / §9 row 4 — "`nul.pdb` committed to repo root" — **WRONG**

The prior review says:

> *The `nul.pdb` file (1.15 MB) in the repo root appears to be a debug symbol file that should not be committed. Recommendation: Add `nul.pdb` to `.gitignore` and remove it.*

`git ls-files | grep -iE '\.pdb$|nul'` returns **nothing** — `nul.pdb` (and `python/macrame/_macrame.pdb`) are **not tracked**. `.gitignore` already contains `*.pdb` (line 10). The files are local build artifacts present on the reviewer's disk, not repository content.

**Action:** No action. Retract the finding.

---

## 4. Documentation Discrepancies (the real findings)

The code is correct; the prose contradicts it. These are the items worth fixing.

### 4.A [High — misleading] "Single-open guard still scans out-degree / fix not applied" prose is stale and contradictory

Multiple authoritative locations assert that `trg_links_single_open`'s `EXISTS` still scans the source's whole out-degree and that the D-059 fix is not applied — **while the fix is applied, plan-pinned, and the same document set says it was applied**.

| Location | Stale claim | Contradicted by |
|---|---|---|
| `README.md` performance table | *"258 µs, and **still O(out-degree), not O(1)** (D-059)"* (0.7.0 column) + prose *"it remains linear in out-degree, so a high-degree hub still exceeds it"* | §4.2 doc: *"flattens the curve (47.7 → 8.0 ms)"*; the applied index |
| `docs/quickref.md §6.2` | *"the single-open guard scans the source's whole out-degree (D-059), so it is O(degree), not O(1)"* | same |
| `src/connection.rs` `chunk_rows::EDGES` doc comment | *"…because `trg_links_single_open`'s `EXISTS` is served by `idx_lc_traversal_cover` with only `source_id` bound… That is a schema defect with a proven fix, recorded in D-059 and **not applied here**."* | `CREATE_INDICES` (baseline) + v5→v6 rung; `tests/index_plan_tests.rs:81` pins the probe to `idx_lc_open_interval` |
| Decision register `D-118` entry (~line 1520) | *"the single-open guard still scans the source's whole out-degree (D-059), so a high-degree hub still exceeds 5 ms"* — written at 0.8.0, after the fix shipped in 0.6.0 | the fix it cites |

**Why this matters:** contradictory prose already produced one wrong review recommending a fix that exists. A reader of `connection.rs` will believe the hot path is defective. The exit gate `tests/doc_sync_tests.rs` pins the **API** surface, not performance prose, so nothing fails when these lines drift from the code.

**What is actually true on a v10 database:**
- `trg_links_single_open`'s `EXISTS` is a 4-equality-column lookup (`source_id`, `target_id`, `edge_type`, `valid_to=sentinel`) plus a `valid_from <>` residual, served by `idx_lc_open_interval`. It returns at most the open intervals for one edge key — a **version count**, not an out-degree. It is O(1) in graph size.
- The actor's `OVERLAP_CANDIDATES` (D-060 guard) is explicitly a 3-equality-column point lookup served by `idx_lc_open_interval` (the `valid_from <> ?4` residual excludes the row being re-asserted). `connection.rs:2254–2267` documents *why* the tempting `valid_from < :new_valid_to` narrowing was dropped (D-064): it let `idx_lc_traversal_cover` win as covering and rescanned out-degree. That defect was fixed and pinned.

**If a residual out-degree-proportional cost genuinely remains on `assert_edge`, it has not been located** — none of the three write-path statements (`OVERLAP_CANDIDATES`, `INSERT_LINK`, and the three triggers `trg_links_single_open`/`trg_links_current_sync`/`trg_links_log_insert`) scan out-degree on a v10 file. The honest options are:

1. **Preferred — correct the prose.** Update the README table, quickref §6.2, the `chunk_rows::EDGES` comment, and D-118 to state that the index shipped in v6 and the probe is now a point lookup; re-measure single-assertion on a high-degree hub and report the (now flat) number, or drop the "O(out-degree)" claim if unmeasured.
2. **If a residual scan is believed to exist — prove it.** Add a bench that measures `assert_edge` against an 8K-edge hub *with the index present* (the existing `benches/budgets.rs` arm that drops the index measures the pre-fix path, not the residual). If it is flat, the prose is simply stale. If it is not, the index is not being chosen and a new `EXPLAIN` assertion is the fix — not a v11 rung.

**Severity: High** — not because the code is wrong, but because the documentation is the project's most-cited product and a wrong claim here compounds (it already did).

### 4.B [Medium — stale] §4.3 "restoration note" still says concept payloads omit `embedding_model`

`docs/architecture/s4-schema.md §4.3` retains the 0.5.4 blockquote:

> ***Concept payloads do not carry `embedding_model`.** The prose below says they do. … a reconstruction cannot currently tell which model a concept was embedded under.*

This was true at 0.5.4 and **false since 0.5.6 Wave 1** (payload v2 — see §3.2). The note was never reconciled when the fix landed. `trg_concepts_log_insert` and `trg_concepts_log_update` both write `'v', 2, …, 'embedding_model', NEW.embedding_model`; `replay.rs` and `as_of.rs` both read it.

**Action:** Update the blockquote bullet to record that v2 restored the field (with the v1 forward-compat branch), or strike it. The README history already records the delivery; the §4.3 note just needs to catch up.

### 4.C [Low — stale] `quickref.md §8` test counts predate 0.8.0/0.9.0

- `docs/quickref.md §8`: *"296 Rust / 305 with metrics / 316 with property-tests / 344 Python (measured 2026-08-02)"*
- `README.md`: *"330 Rust · 339 with metrics · 362 with --all-features · 353 Python (measured 2026-08-07)"*

The quickref is labelled the *v0.9.0 reference* but carries 0.7.0-era counts. Minor, but a reference doc that names a date should match the version it claims.

**Action:** Re-sync quickref §8 counts to the README's 2026-08-07 figures (or drop the hard counts in favour of "see `scripts/run_rust_suite.py`" so they cannot drift again).

### 4.D [Low — cosmetic] `§3` crate-layout diagram vs. module map

The `quickref.md §3.3` module map is current and correct (it lists `integrity/shadow.rs`, `util/limits.rs`, `util/ids.rs`, and notes the analytics are native). If `docs/architecture/s0-s3-foundations.md §3` still draws an ASCII crate tree showing `integrity/` with only `audit.rs`/`rebuild.rs` and labelling analytics "petgraph", it should match the quickref. (The prior review flagged this; verify and reconcile if still present.)

---

## 5. New Findings the Prior Review Missed

### 5.1 [Medium — binding risk] Python `diagnostic_query`/`explain` are a concurrent-open (R15) exposure

**What.** `bindings/python/src/database.rs` exposes `diagnostic_query` and `explain` as `#[pymethods]` on `PyDatabase`, which is `#[pyclass(frozen)]` over `RwLock<Option<Database>>`. `with_db` takes the **read** lock (`inner.read()`), so multiple Python threads can be inside these methods simultaneously. Each call opens a **fresh** `SQLITE_OPEN_READ_ONLY` connection (`db.diagnostic_conn()`) and drops it:

```rust
// database.rs:1047 / 1063
let conn = db.diagnostic_conn().await?;   // opens SQLITE_OPEN_READ_ONLY per call
rows::collect(&conn, &sql, bound).await
```

`rows.rs:19` documents this is deliberate (*"Each call opens its own `SQLITE_OPEN_READ_ONLY` connection and drops it"*), and `§14` defends it as *"the R15-safe shape: 500 sequential opens measured clean"* — but **sequential**, not concurrent.

**Why it matters.** The project's own R15 record (README "Known Risks", `s11-s12`, D-111) measures that **concurrent** opens of the same file fault — `48 concurrent opens from 48 threads fault 2/12` through Python, matching the Rust control. The typed surface (`traverse`, `load_subgraph`, `reconstruct`, search) is fine because it reads through the long-lived `read_conn()`. But an application that calls `db.explain(sql)` or `db.diagnostic_query(sql, params)` from a thread pool, asyncio executor, or any concurrent context is performing concurrent `open()` on the libSQL engine — the exact shape R15 faults on.

The risk is not theoretical: the binding is advertised as `frozen`/concurrent-readable, and `diagnostic_query` is the *only* general-purpose SQL escape hatch a Python user has, so a multi-threaded caller hitting it for logging/diagnostics is a plausible usage pattern.

**Mitigation options (in order of cost):**
1. **Document the constraint.** State in the `diagnostic_query`/`explain` docstrings and §14 that these methods must not be called concurrently (they open per call; R15 is a concurrent-open fault). Cheapest; relies on caller discipline.
2. **Serialise diagnostic opens with a dedicated `Mutex`** around the diagnostic path inside `with_db`, so at most one fresh `SQLITE_OPEN_READ_ONLY` is outstanding at a time. Preserves the per-call-open semantic; bounds concurrency to 1 for this path only; does not touch the typed read path. Low risk.
3. **Pool a single long-lived diagnostic connection** on `PyDatabase` (open once, reuse). This removes the per-call open entirely and eliminates the exposure — but a pooled connection that outlives a `VACUUM`/schema change is a new lifecycle to manage, and `diagnostic_conn()`'s OS-level read-only boundary was chosen for a reason. Higher review cost.

**Recommendation:** Option 2 — a `Mutex` (or `tokio::sync::Mutex`) guarding the diagnostic path — is the smallest change that closes the exposure without revisiting the per-call-open design.

### 5.2 [Low — scaling characteristic, undocumented] `reject_overlapping_interval` is O(version-count), not O(1)

`OVERLAP_CANDIDATES` returns **every** recorded interval for one `(source_id, target_id, edge_type)` (3-equality point lookup plus `valid_from <> ?4`), and `Interval::overlaps` is evaluated in Rust for each. For an edge that has been asserted, retired, and re-asserted N times, the guard reads N rows on every `assert_edge` against that key. This is correct and generally small, but:
- It is **not** bounded by out-degree (it is bounded by one edge's version count).
- It is undocumented where the README's "single assertion is O(out-degree)" prose would more accurately have been "O(version-count)" — the wrong complexity was claimed for the wrong reason.
- For a high-churn edge (retroactive corrections in a loop), version count grows without bound and the guard grows linearly with it.

**Mitigation options:**
1. **Document** that the overlap guard is O(version count per edge) and that high-churn edges accumulate; note that the archive removes superseded `links` rows (and their `links_current` mirror), which caps version count for archived edges.
2. **If it ever bites:** the guard only needs to detect *overlap*, which requires comparing the proposed interval against each existing one — but the existing intervals for one edge are typically few. A worst-case-bound guarantee could cap the candidate read with `LIMIT` after a provable threshold, refusing with a typed error above it (analogous to `SubgraphTooLarge`). Probably unnecessary; just document.

**Severity: Low** — version counts are small in practice and the archive reclaims them; raise it only if a workload churns a single edge thousands of times without archiving.

### 5.3 [Low — invariant shape] `concepts` INSERT has no `recorded_at` monotonicity guard

`trg_concepts_monotonic_ra` is `BEFORE UPDATE` only. Concept **inserts** are not guarded at the engine for `recorded_at` monotonicity — the property is maintained by the injectable `Clock` (which floors to `MAX(recorded_at)`) on the supported path, and left open for a raw writer (the `raw()` hole, §4.7 row 2). This is consistent and documented under §4.7, but it is an asymmetry worth a sentence in `§4.3` so a future maintainer does not add a `BEFORE INSERT` guard expecting it to close a hole — it would not, because raw writers can also flip PRAGMAs.

**Action:** Optional one-line note. No code change.

---

## 6. Confirmed Genuine Architectural Items

These are real, were already documented, and I confirm them after re-checking the code.

| # | Item | Status | Evidence |
|---|---|---|---|
| 6.1 | **R15 — concurrent open → `STATUS_ACCESS_VIOLATION`** (libSQL 0.9.30) | Upstream fault; maturely mitigated | `RUST_TEST_THREADS=1` in `.cargo/config.toml`; `scripts/run_rust_suite.py` classifier; `examples/r15_soak.rs`; gated `property-tests` feature. Reproduces through the Python boundary at the same rate. |
| 6.2 | `links_current` (and pre-v7 cold files) carry no `weight` CHECK | Documented; loader guard retained | `ddl.rs` `CREATE_LINKS_CURRENT_TABLE` has no `weight` CHECK; `D-083` notes the loader guard stays for this and for pre-v7 cold files. `links_current` is rebuilt from `links` (which has the CHECK), so it inherits validity on the supported path — the residual gap is raw writes and old cold files. |
| 6.3 | Rehydrate superlinearity > ~1,000 concepts (FTS5 index maintenance ~53% at 10K; 1.1 s write-lock hold) | Documented (D-132); windowing declined to preserve atomicity | `archive.rs` rehydrate path; `RehydrateReport`; control arm in benches. A `rehydrate_windowed` analog to `archive_windowed` is the named escape if it ever matters. |
| 6.4 | `Database::raw()` is `#[doc(hidden)]` public | Intentional (D-068/D-091); Python binding deliberately does not expose `raw()`/`read_conn()`/free `register_model`/`upsert_embedding` | `connection.rs`; `bindings/python/src/lib.rs` surface. The contract is "a convention in perpetuity" — worth a `// SAFETY/convention:` doc sentinel so a future binding contributor cannot quietly add it. |
| 6.5 | `Subgraph::is_closed` is `pub` but unasserted at algorithm entry | Minor hardening opportunity | `graph/subgraph.rs`; `drop_dangling_adjacency` establishes closure; `is_closed()` exists. A `debug_assert!(g.is_closed())` at the entry of `dijkstra`/`astar`/`scc`/`k_core`/`louvain` would make the invariant auditable in debug builds at no release cost. |
| 6.6 | `SystemClock` uses `std::sync::Mutex<SystemTime>` for the monotonicity floor | Micro-contention only | `util/clock.rs`. The Write Actor is the primary caller and is single-threaded; the mutex is uncontended. An `AtomicU64` (micros-since-epoch, CAS loop) is a possible future micro-opt; not needed now. |
| 6.7 | Snapshot retention: pre-v2 (v1) snapshots require decompression for day-bucketing | Cosmetic; v1 spanned one release cycle (0.5.5c→0.5.5e) | `snapshot.rs` v2 header carries the instant; v1 files are decompressed. No realistic population of v1 files exists. |
| 6.8 | `links_current_shadow` may survive a mid-rebuild crash | Handled (`ShadowStep::Begin` drops-and-recreates) | `integrity/shadow.rs`. An orphan is harmless and self-cleaning; documented in §4.2. |

---

## 7. Strengths Worth Stating (so the bar is clear)

- **Doctrine is real and enforced at the right layer.** Eight invariants map to concrete enforcement points; the §4.7 table is honest about which are engine-enforced vs. API-enforced, and `tests/storage_boundary_tests.rs` asserts *both directions* (the gap is open where claimed, closed where claimed) — a tripwire that fails if a migration silently closes one.
- **Plan-pinning is a registry keyed by index** (`tests/index_plan_tests.rs`), which is how the D-059/D-064 covering-index trap is now caught by construction rather than by re-measurement.
- **Error enum size is pinned** (`the_error_enum_stays_small_enough_to_return_by_value`, ≤128 bytes) — the kind of thing that prevents `OverlappingInterval`'s 168 bytes from silently bloatging every `Result`.
- **The clock is injectable** (`Clock` trait, `FakeClock`) with a `Send + Sync` interior, making temporal tests deterministic across the actor boundary.
- **The async→sync Python boundary is correctly shaped**: `Python::detach` around `Runtime::block_on`, single `OnceLock` runtime (avoids `Runtime::drop` cross-runtime panic), `RwLock` acquired inside the GIL-released closure (avoids `close()` deadlock), `fork()` guard on Linux multiprocessing children.
- **Rehydrate re-points the FTS index on `rowid_pk` reassignment** (`archive.rs:645–672`, `RehydrateReport::rowids_reassigned`) — the exact failure `rowid_pk` was introduced to prevent, handled where it could recur.
- **Self-correcting documentation practice**: stale claims are struck, corrections are dated and explained. (This review exists because a few corrections did not propagate everywhere — see §4.)

---

## 8. Recommendations Before 1.0

Ranked by value, **only the documentation items are open**; no new code work is required for 1.0 readiness.

1. **Reconcile the D-059 / "O(out-degree)" prose** (§4.A) across README, quickref §6.2, `connection.rs` `chunk_rows::EDGES` comment, and D-118. Decide: correct it to "fixed in v6, point lookup," or measure a residual and locate it. This is the one item that has already caused a wrong review.
2. **Update the §4.3 restoration note** (§4.B) to reflect payload v2 / `embedding_model` shipped in 0.5.6.
3. **Re-sync quickref §8 test counts** (§4.C) to 2026-08-07, or replace hard counts with a pointer to the runner.
4. **Guard the Python diagnostic path against concurrent opens** (§5.1) — option 2 (a `Mutex` around `diagnostic_conn()` use) closes the R15 exposure the binding itself documents. This is the only *new* code change recommended, and it is small.
5. *Optional hardening:* `debug_assert!(g.is_closed())` at algorithm entry points (§6.5); a one-line convention comment on `raw()` (§6.4); a note on the concept-INSERT monotonicity asymmetry (§5.3).

---

## 9. Summary Table

| # | Severity | Finding | Type | Action |
|---|---|---|---|---|
| 3.1 | — | Prior review: "D-059 fix not applied" | **Retracted** — fix shipped in v6 (baseline + rung + plan-pin + migration test) | Drop from blocker list |
| 3.2 | — | Prior review: "payload missing `embedding_model`" | **Retracted** — payload v2 carries it since 0.5.6 | Drop from blocker list |
| 3.3 | — | Prior review: "`nul.pdb` committed" | **Retracted** — not tracked; `*.pdb` gitignored | No action |
| 4.A | **High** | "single-open guard still O(out-degree) / not applied" prose contradicts the applied, pinned fix | Doc | Reconcile README/quickref/connection.rs/D-118; measure a residual if one is believed to exist |
| 4.B | Medium | §4.3 restoration note says concept payloads omit `embedding_model` | Doc | Update note for payload v2 (0.5.6) |
| 4.C | Low | quickref §8 test counts stale (2026-08-02 vs 2026-08-07) | Doc | Re-sync or replace with pointer |
| 4.D | Low | §3 crate diagram vs. quickref module map | Doc | Reconcile if still divergent |
| 5.1 | **Medium** | Python `diagnostic_query`/`explain` open a fresh RO connection per call under a read-lock → concurrent opens = R15 shape | Binding risk | Serialise the diagnostic path with a `Mutex` (option 2) |
| 5.2 | Low | `reject_overlapping_interval` is O(version-count per edge), undocumented | Scaling note | Document; optionally cap with `LIMIT` + typed error if it ever bites |
| 5.3 | Low | No `recorded_at` monotonicity guard on concept INSERT (asymmetry with UPDATE) | Invariant shape | Optional one-line note |
| 6.1 | Info | R15 upstream concurrent-open fault | Confirmed | Already maturely mitigated |
| 6.2 | Info | `links_current`/cold files carry no `weight` CHECK | Confirmed | Loader guard retained; documented |
| 6.3 | Info | Rehydrate superlinear >1K concepts | Confirmed | D-132; `rehydrate_windowed` if needed |
| 6.4 | Low | `raw()` `#[doc(hidden)]` public | Confirmed | Optional convention comment |
| 6.5 | Low | `Subgraph::is_closed` unasserted at algorithm entry | Hardening | `debug_assert!` |
| 6.6 | Info | `SystemClock` `Mutex<SystemTime>` | Micro-opt | `AtomicU64` if ever needed |
| 6.7 | Low | v1 snapshot decompression for day-bucketing | Cosmetic | No realistic population |
| 6.8 | Info | `links_current_shadow` orphan after crash | Handled | `Begin` drops-and-recreates |

---

## 10. Overall Assessment

On current evidence, **Macrame 0.9.0 has no open code-level 1.0 blockers.** The two items the prior review elevated to "fix before 1.0" are already shipped and tested; the blocker list was an artifact of stale prose, not stale code.

The work that remains is:

- **Prose reconciliation** (§4.A and §4.B) — the same self-correcting discipline that produced the v2 payload and the v6 index failed to propagate into the performance table, the `chunk_rows` comment, the decision register's D-118 entry, and the §4.3 restoration note. The fix is editorial; the cost of leaving it is that the next reviewer repeats this one.
- **One small binding hardening** (§5.1) to close the Python concurrent-diagnostic-open exposure to R15.

After those, the crate is 1.0-ready on the terms its own doctrine sets: the boundary is sacred, the two clocks are unmixed, assertions are immutable, the ledger is a table, deletion is by archive, derivative state is disposable, embeddings are out of the ledger, and fidelity is a parameter. The implementation enforces all eight; the documentation should be made to say so consistently.