# Macrame bottleneck diagnostics — test & fix plan

**Status:** executed (F2 landed as [D-274](architecture/s13-decision-register.md#d-274), 0.16.1) · **Date:** 2026-09-10 · **Branch:** `dev/0.17.0`
**Trigger:** spikeladders (same box, seeded RNG, `.venv` with `macrame-db==0.16.0`):

| Observation | Numbers |
|---|---|
| Edge `bulk_import` superlinear | 2k→0.5s, 4k→2.3s, 8k→7.9s, 16k→29s (~4× per 2×; 40k never finished in budget) |
| Concept writes fast | 20k in ~1.0–1.2s |
| Vector build superlinear in dim (N=5k) | 64→11.5s, 256→82s, 512→did not finish |
| Reads fine at all scales | traverse 0.1ms, vector top-10 ≤9ms, keyword ≤21ms @20k |
| Footprint | 260MB + 11MB snapshots vs 79MB ladybug @20k docs/40k edges/64-dim |
| Reference (ladybug) | writes linear ~6k rows/s; vector build ~linear in bytes (3.5/7.5/12.3s for 64/256/512 @5k) |

Goal: attribute each bottleneck to **crate** (triggers, materialization, chunking,
binding) vs **engine** (libSQL FTS5/DiskANN/WAL), then fix at the right layer.

---

## 0a. Findings — what the execution of this plan attributed (2026-09-10)

The plan's §1–§2 were run in the order prescribed, and the edge ladder attributed
in two moves. Full detail in [D-274](architecture/s13-decision-register.md#d-274);
this section is the plan's record of its own execution, and the numbers below are
**this box** (Windows, NVMe, medians of ≥3 sessions, fresh DB per run, control
queries flat) — D-055 applies to every one of them.

**F2 confirmed and fixed — the prime suspect was the right one, by a different
arm of it.** The edge ladder's superlinearity is the single-open probe
(`trg_links_single_open`'s `EXISTS`) being served by `idx_lc_lineage_cut` with
only `branch_id` bound — a whole-lineage scan **per trigger firing** — on any
database that has rows and no statistics, which is what a fresh-file bulk import
is for its whole length (`close()`'s `PRAGMA optimize` (D-149) is what heals the
plan, for the *next* process). The existing plan pin was green throughout
because its fixture was empty; D-273's partial-index lesson, arrived at from the
other side. Fix: unary `+` on the predicate (the D-250 idiom), migration rung
v18 → v19, pin strengthened to a populated no-statistics arm. Measured on this
box, same method as the trigger table below:

| edges | before | after | per-row before | per-row after |
|---|---|---|---|---|
| 2,000 | 0.35 s | 0.25 s | 0.177 ms | 0.124 ms |
| 4,000 | 0.93 s | 0.52 s | 0.232 ms | 0.130 ms |
| 8,000 | 2.73 s | 1.09 s | 0.342 ms | 0.136 ms |
| 16,000 | 8.68 s | 2.22 s | 0.540 ms | 0.139 ms |

Per-row flat after the rung (0.124 → 0.139 ms across the ladder): what remains
is D-179's log₂ term and D-142's maintained materialization. Holds equal wall at
every size, so **F3 (WAL recipe) and F4 (snapshot cadence) are out by
measurement**: cadence arms measure the same ladders, and no checkpoint gap
ever appears between wall and holds. The call-size sweep (§3a) is flat after the
rung — 16k edges measure 2.18–2.20 s from one call to sixteen — so **F7's
"~2k/call" recipe is retired as load-bearing guidance**; call size now governs
latency, which is what `CHUNK_BUDGET` says it governs.

**F6 confirmed engine-side — deferred as written.** 5,000 vectors: 10.5 s at
dim 64, 76.4 s at dim 256, holds ≈ wall at both. The crate side of that path is
one prepared statement per chunk (one tx, one prepare, per-row execute), so the
chunking and binding contribute a fixed overhead and the growth is DiskANN
index maintenance. Route-around stands: per-model tables make a wider model
additive; the dimension stays enforced by the index (D-037).

**F1 remains the named successor** for callers who want the concept-write rate
from the edge path (20k concepts: 0.90 s beside 16k edges: 2.22 s): the residual
~135 µs/row is the maintained materialization (D-142), and the sanctioned
bulk-without-materialization + `rebuild_current_chunked` recipe is opt-in by
design — the guard is a read a bulk can skip; the materialization is a cache a
read cannot.

Harness: `benchmarks/diagnostics/spike_ladder.py`, results JSON in
`benchmarks/results/diagnostics/` (preliminary hardware, per the benchmarks
README's labeling rule). Engine-dissection figures quoted in D-274 came from a
plain-SQLite replica of the four trigger bodies and three indexes on file
copies; the macrame-side figures above come from the shipped API.

---

## 0b. The findings in detail — the execution record (2026-09-10)

Everything §0a claims, with the numbers and the reasoning that produced it, in
the order the plan prescribed. The decision itself is D-274; this section is
the measurement record it was decided against, written down while the sessions
that produced it are still open in `benchmarks/results/diagnostics/`.

### 0b.1 What ran, and how

`benchmarks/diagnostics/spike_ladder.py`, built for this plan to the discipline
§0 states:

- **Fresh DB per run, one open per DB** (R15), explicit temp dirs with tolerant
  cleanup — the `TemporaryDirectory` finalizer races the engine's file handle on
  Windows and was the first defect the harness itself hit.
- **Medians of ≥3 sessions** per point; single-run figures only where they are
  labeled as probes.
- **Control query per session** (`SELECT 1`, median of 5): flat at 0.041–0.066 ms
  across every arm below — the machine did not move under any of these runs.
- **Storage state with every timing**: `PRAGMA page_count`, `freelist_count`, WAL
  bytes, snapshot-dir bytes. Reported where they carry a finding (§0b.2); the
  headline is that they were unremarkable — freelist 0 everywhere, WAL 4.3–5.2 MB
  at 16k edges, snapshot bytes 0 in every ladder arm.
- **Per-chunk holds** via `bulk_import(edges, progress=cb)` — the primary
  signal, per §0.
- Arms: edge ladder 2k/4k/8k/16k (default snapshot cadence and `None`), call-size
  sweep 1×16k…16×1k, concept ladder 20k, vector ladder 5k @ dim 64/256, and an
  EXPLAIN pass against a populated 16k database (before and after `ANALYZE`).
- Data shape: unique random edge pairs over an n/2 concept pool, one edge type,
  all-open intervals, one stamp per chunk (the shipped bulk behavior).

Two engine-side dissections ran **outside** the crate, on a plain-SQLite
replica of the schema and on file copies of a real macrame database, because the
question they answer — *which statement costs what* — cannot be answered through
the actor without dropping the thing being measured. §0b.4.

### 0b.2 The edge ladder, before

One `bulk_import` per session, fresh DB, medians of 3 (first line = default
snapshot cadence, second = cadence `None`):

| edges | wall (cadence on) | wall (cadence off) | per-row | chunks | last-five holds |
|---|---|---|---|---|---|
| 2,000 | 0.354 s | 0.351 s | 0.177 ms | 56 | 7.5 ms × ~35 rows |
| 4,000 | 0.926 s | 0.942 s | 0.232 ms | 113 | 11–12 ms × 25 rows |
| 8,000 | 2.733 s | 2.722 s | 0.342 ms | 227 | 17–18 ms × 35 rows |
| 16,000 | 8.641 s | 12.054 s | 0.540 ms | 456 | 33–84 ms × 20–35 rows |

Four observations, each eliminating a candidate:

1. **The two cadence arms measure the same ladder** — the default-vs-`None`
   delta at 16k (8.64 vs 12.05) is session noise on this box, not a cadence
   effect; §1.4's earlier single-run delta (12.8 vs 24.7) was the same noise,
   as it suspected. **F4 out.**
2. **Holds ≈ wall** at every size (e.g. 16k: holds 8,622 ms against wall 8,682;
   holds 12,035 against 12,054). The cost is inside the actor's transaction
   holds. Nothing in the gaps — WAL checkpoints (F3), snapshots — is worth
   chasing. **F3 out.**
3. **The chunk controller is working and cannot win.** `holds_first5` show the
   ceiling start (8–8.7 ms × 90 rows) and the controller floor out within a
   chunk or two (~20–35 rows, `CHUNK_FLOOR = 35`'s clamp). The last-five holds
   *grow with the table* — 7.5 → 11 → 18 → 33–84 ms for the same chunk size.
   Per-chunk overhead is flat; per-row cost is what grows, and no row count
   bounds a duration on a path whose per-row cost grows (D-143's finding,
   restated by the data).
4. **Every turn is over budget**: `metrics().violations()` names
   `bulk_import_chunk over_budget=456/456` at 16k — the budget miss is total,
   not a tail. D-142 documented a known ~3× miss on a populated table; this is
   5–28× and *growing*, which is the signature of a plan defect, not of the
   documented index-maintenance cost.

A single-session in-process probe (build 16k edges, keep the handle open, re-run
the same ladder against the same connection — the state every fresh-file import
lives in) read **21.79 s**, with per-row holds climbing 127 µs → 3.6 ms across
the run. This is the number the released 0.16.0 wheel produced on the original
spikeladder too (the trigger table's "16k → 29 s"), and the difference between
21.8 s and 29 s is box noise at that shape.

### 0b.3 The EXPLAIN evidence

`EXPLAIN QUERY PLAN` on the four statements the write path executes per row,
against a populated 16k database, before and after `ANALYZE` (the plan's §1.3,
§3d): the guard is right in both states, the insert has no plan to read, and
**the single-open probe is the defect**:

| statement | populated, no statistics | same rows, after `ANALYZE` |
|---|---|---|
| overlap guard (trunk) | `SEARCH … idx_lc_open_interval (source_id=? AND target_id=? AND edge_type=?)` | same |
| overlap guard (forked) | same index, `valid_to` filter | same |
| single-open probe | **`SEARCH … idx_lc_lineage_cut (branch_id=?)`** | `SEARCH … idx_lc_open_interval (…edge_type=? AND valid_to=?)` |
| current-sync upsert | (no plan text; conflict-seek on the PK) | same |

The probe plans with **one column bound** out of the five the query offers, and
`idx_lc_lineage_cut` leads on `branch_id` — so the "seek" enters at the lineage
prefix and scans one lineage's whole projection per firing. At a 16,000-row
projection that is ~16,000 candidate rows per inserted edge, and it is the
*only* statement on the path whose plan is wrong. `ANALYZE` fixes the plan —
which is why it fixes nothing: the wrong plan exists exactly when statistics
do not, and `close()`'s `PRAGMA optimize` (D-149) writes them for the *next*
process. Every fresh-file bulk import spends its whole life in the bad state.

The replication set that isolates it (plain-SQLite replica, 4,000-row
projection, lookup timed directly): no stats → `idx_lc_lineage_cut`; after
`ANALYZE` → `idx_lc_open_interval`; with the unary `+` on the branch predicate →
`idx_lc_open_interval` **without stats**. The plan choice is the stats' absence,
not the row count — the wrong plan appears at 210 rows just as at 16,000.

### 0b.4 The engine dissection, trigger by trigger

A plain-SQLite replica of the four write-path objects — the three trigger
bodies verbatim, the three `links_current` indexes, the FKs to `concepts` —
populated to a 16,000-row projection, with one arrangement per run: each copy
drops one trigger (or carries the `+`), then the same 2,000-edge fill is timed
through single inserts. The numbers are the engine's, with no actor, binding
or crate around them:

| arrangement | per-row (replica) | what it attributes |
|---|---|---|
| all three triggers, branch predicate seekable | 142.9 µs | the shipped state |
| − `trg_links_single_open` | 81.9 µs | the probe's scan: **61 µs/row** |
| − `trg_links_current_sync` | 44.2 µs | the materialization + its three indexes: 98.7 µs/row (D-142's attribution, confirmed at this scale) |
| − `trg_links_log_insert` | 112.7 µs | the log write + its three indexes: 30 µs/row |
| all three, `+branch_id` in the probe | 77.4 µs | the same three triggers with the probe on its own index — **the whole of the scan, removed, for one character** |

A `DROP INDEX idx_lc_lineage_cut` variant was also attempted against file
copies of a real macrame database and was refused by the open-time schema
verification (the v18 stamp names the index) — the right behavior for the
crate, and the reason the dissection moved to a replica: the verification
exists so nobody can remove an index by accident, and the fix must not be an
index removal either.

### 0b.5 The pin that was green while the plan was wrong

`the_single_open_probe_seeks_rather_than_scans` has asserted the probe's plan
since D-059, and asserts it correctly — for an **empty** fixture. On an empty
table `idx_lc_lineage_cut` is empty, and an empty index is attractive to
nobody: the planner picks the right index there whether or not the SQL helps
it. The wrong plan appears at any populated size with statistics absent, which
is why a defect that changes 16k-edge imports by 4× coexisted with a green
suite for four releases (0.14.14 introduced `idx_lc_lineage_cut`; the planner's
no-stats choice flipped with it, and nothing held it).

D-273 wrote this lesson for partial indexes — *a partial index on a ledger with
no branches is empty, which is attractive to any planner and would make the pin
pass whatever the query said* — and this finding is the same lesson from the
other side: not the index was empty, the **statistics** were, and absence is
what the planner was reasoning about. The populated no-statistics fixture has
existed since D-198, which is why the repair is one fixture swap and not a new
instrument.

### 0b.6 The fix, the rung, and the after

The trigger's `branch_id` predicate now carries a unary `+` (D-250's idiom, the
same mechanism the `TrunkOnForked` guard arm has shipped since 0.15.8), the
body is restored by migration rung **v18 → v19** (`DROP TRIGGER IF EXISTS` +
today's `CREATE_LINKS_SINGLE_OPEN`), and the pin runs a populated
no-statistics arm that names the wrong index. Full decision in D-274; the
after-ladder, same method as §0b.2:

| edges | wall (cadence on) | wall (cadence off) | per-row | last-five holds |
|---|---|---|---|---|
| 2,000 | 0.249 s | 0.247 s | 0.124 ms | 4.1 ms × 35 rows |
| 4,000 | 0.521 s | 0.521 s | 0.130 ms | 4.2 ms × 35 rows |
| 8,000 | 1.088 s | 1.083 s | 0.136 ms | 4.2–4.4 ms × 35 rows |
| 16,000 | 2.224 s | 2.191 s | 0.139 ms | 4.2–4.5 ms, outliers 12.4 ms |

Per-row is flat: 0.124 → 0.139 ms across the ladder, 11% of growth where 310%
grew before. What remains is D-179's log₂ term plus D-142's materialization
cost — and the budget tells the same story honestly: `violations()` now reads
455/456 over budget at 16k with holds at 4.2–4.5 ms against the 3 ms bound,
which is D-142's documented ~3× miss at the floor chunk size, seen and not
enforced (D-055). The occasional 12 ms outlier in the last five holds is a
checkpoint landing inside a hold — bounded, and the reason the exemption table
names `bulk_import_chunk` as bounded-by-contract rather than met.

The call-size sweep, after (16,000 edges, medians): **2.20 / 2.19 / 2.20 /
2.19 / 2.18 s** for 1×16k / 2×8k / 4×4k / 8×2k / 16×1k — flat end to end. The
holds differ by call size because the controller re-derives chunk sizes per
call; total time does not care. The "~2k per call" recipe the pre-fix ladder
suggested is retired: nothing about the caller's batch size is load-bearing
for throughput anymore.

### 0b.7 The vector ladder (F6, engine-side)

5,000 vectors, one `upsert_embeddings` call, fresh DB with the concept pool
seeded (vectors carry an FK into concepts), medians of 2 sessions:

| dim | wall | hold sum | chunks | per-chunk shape |
|---|---|---|---|---|
| 64 | 10.53 s | 10.52 s | 167 | first chunks ~103–220 µs/row, last ~3.5 ms/row |
| 256 | 76.41 s | 76.39 s | 167 | same shape, steeper |

Holds ≈ wall at both dims: the chunk boundaries, the binding and the actor are
fixed overhead and the growth is **inside the chunk's own transaction** —
DiskANN index maintenance on the model's table (§5.9). The crate's loop is one
prepare per chunk and one execute per row (`vector::search::upsert_embedding_chunk`);
there is nothing superlinear left on the crate side to remove. This is the
plan's F6 confirmed with the layer-isolation evidence it asked for, and the
deferral is a measurement, not a judgment call.

### 0b.8 The concept contrast

20,000 concepts, one `write_concepts` call: **0.901 s** (holds 868 ms, 286
chunks, ~45 µs/row). The concept path is what the edge path would cost without
the materialization and the overlap guard — the number F1 competes against.

### 0b.9 What was run and not chased, on purpose

- **Raw-libSQL floor (§2a)**: not measured separately this session — the
  replica dissection (§0b.4) answers the same attribution question at the
  statement level, which is finer than the §2a row floor would have been.
- **FTS insert tax (§2b)**: untouched; no keyword-path ladder was in the
  trigger table and nothing this session implicated it.
- **512-dim vectors**: not re-measured (the original trigger read "did not
  finish"; 256 already prices the shape at 76 s).
- **Footprint (260 MB / snapshot bytes)**: snapshot-dir bytes were recorded
  (zero in every ladder arm) but the *size* question was never re-measured —
  see §7.

---

## 0. Measurement discipline (read first)

- **Medians of ≥3 sessions.** Session-to-session spread is ~29% on this project's
  own hardware (D-070). Single runs prove nothing.
- **Control query per session.** Time a `SELECT 1` round-trip (or equivalent)
  alongside every measurement; if the control moved, the machine moved, not the code.
- **Fresh DB per run, one open per DB.** R15 (cumulative-`connect()` access
  violation, libSQL 0.9.30): never reuse a path across runs in one process, and
  never open the same file twice concurrently from the harness.
- **Record storage state with every timing:** `PRAGMA page_count`,
  `freelist_count`, WAL bytes, snapshot-dir bytes. Growth explains superlinearity.
- **Per-chunk holds are the primary signal.** Pass `progress=` to
  `write_concepts` / `bulk_import` / `upsert_embeddings` and log
  `(chunk_index, rows, held_ms)`:
  - holds **grow** over the run → cost scales with n (index depth, trigger fan-out,
    transient-index rebuilds, degrading query plans);
  - holds **flat** → fixed per-chunk overhead × chunk count (Write Actor round-trips,
    transaction open/commit, binding/GIL).

---

## 1. Instrument — no code changes

1. **Per-chunk hold curves** for: 16k edges in one `bulk_import`; same 16k as
   8×2k calls; 5k vectors at dim 64/256. Plot hold vs chunk index.
2. **`metrics()` before/after** each run: kind counters + `budget_violations()`.
   (`metrics` ships on in the wheel; no feature flag needed from Python.)
3. **EXPLAIN QUERY PLAN on the hot path.** Find the statements the Write Actor
   executes per chunk (see `graph.rs` bulk paths) and run them under
   `EXPLAIN QUERY PLAN` via `diagnostic_query`/`diagnostic_conn` on a populated
   file. Red flags, in order:
   - `AUTOMATIC COVERING INDEX` — SQLite building a throwaway index per statement,
     O(n) each, O(n²) over a bulk. This is the prime suspect for the edge ladder:
     it is the exact pathology D-151 fixed on the archive path (`SCAN links` →
     per-statement auto-index → added `idx_links_target`).
   - `SCAN links` / `SCAN links_current` anywhere in a per-row/per-chunk statement.
   - `USING INDEX` with the *wrong* index after 10× table growth mid-bulk
     (planner staleness; cf. §3d).
4. **Snapshot accounting.** Snapshot-dir bytes before/after bulk with default
   cadence vs `snapshot_every_entries=None` (one prior run showed
   12.8s vs 24.7s — repeat 3× before concluding; that delta smells like noise).

---

## 2. Layer isolation — crate vs engine

Run these against **raw libSQL** (plain client, no macrame) on identical data:

- (a) Plain `INSERT` 40k rows into a `links`-shaped table, no triggers/indexes → engine row-write floor.
- (b) Same + FTS5 external-content table + triggers → FTS insert tax.
- (c) `F32_BLOB` table + `USING diskann`, insert 5k vectors at dim 64/256/512,
  time inserts and a top-10 query → DiskANN build/query cost in isolation.

Then compare with the macrame path. The deltas attribute cleanly:

| Delta (macrame − raw) | Attributed to |
|---|---|
| (bulk edges) − (a) | triggers + `transaction_log` + `links_current` + actor/chunking |
| (bulk edges) − (b)-ish | materialization + guard checks specifically |
| (upsert_embeddings) − (c) | chunking/binding overhead vs pure index build |

- (d) **Binding cost:** `bulk_import(2000×1 call)` vs 2000× single `assert_edge`
  vs 1× `write_bulk_atomic` → per-call (GIL detach, `block_on`, channel) vs per-row.

---

## 3. Caller-visible knob sweeps (no fork changes)

For a fixed total (16k edges / 5k vectors), sweep one variable at a time:

- (a) **Call size:** 1×16k, 2×8k, 4×4k, 8×2k, 16×1k. The ladder already hints the
  optimum is ~2k/call; confirm and document it as the supported bulk recipe.
  **Run post-D-274 (2026-09-10): flat — 2.18–2.20 s for 16k from one call to
  sixteen. The recipe is retired: call size governs latency, not throughput
  (§0b.6, §7).**
- (b) **WAL:** default autocheckpoint vs `"disabled"` + explicit `checkpoint()`
  after bulk. Read back `CheckpointReport` (`busy`, frames moved). Bulk inserts
  through log triggers generate far more WAL pages than the row count suggests;
  repeated autocheckpoints during a bulk are O(WAL) each.
  **Run post-D-274 (2026-09-10), on the random-pair shape the chain ladder was
  not: 16k edges, medians of 3 — default 5.0 s / WAL ~6 MB; disabled + one
  `checkpoint()` 3.6 s / WAL ~554 MB / checkpoint ≈ 240 ms;
  `wal_autocheckpoint=10_000` 3.7 s / WAL ~43 MB; 50,000 pages 3.8 s / WAL
  ~207 MB. The knob is the dial: ~26% off the wall at a bounded WAL. On the
  chain-shaped ladder the sweep is a wash (2.25/2.55 → 2.40/2.41 s) — the
  recipe is for the random-order shape a real importer produces, and §7's
  refutation of F3 is revised to shape-dependent. Documented in
  `Database::bulk_import`'s and the binding's docstrings.**
- (c) **Snapshot cadence:** default vs `None`, 3 sessions each.
- (d) **`analyze()` before bulk** (planner stats on empty tables go stale as the
  ledger grows 10–100× mid-run; a mid-bulk plan flip looks exactly like
  superlinearity).
- (e) **Edge kinds:** chain edges (disjoint keys, low degree) vs random edges
  (hub formation) — separates overlap-guard cost (O(versions per key)) from
  degree effects.

---

## 4. Candidate fixes, mapped to findings

**Status after execution (2026-09-10): F2 landed as D-274 — but as an index-avoidance `+` on the trigger's predicate, not a new index: the plan the planner wanted already existed (`idx_lc_open_interval`); the wrong plan chose a branch-led index whose first column the query binds. F3/F4/F7 out by measurement (holds ≈ wall; cadence arms equal). F6 confirmed engine-side and deferred. F1 stands as the named opt-in successor. F5 stays moot for the same reason F6 does — the build inside the chunk is the cost.**
| If you find | Fix | Where | Precedent/notes |
|---|---|---|---|
| F1. Per-row `links_current` maintenance dominates (§2b) | Bulk mode that writes ledger rows **without** maintaining the materialization, then `rebuild_current_chunked()` once at the end | crate (new bulk flag or documented recipe) | Sanctioned by Doctrine VI (derivative state is disposable); rebuild + audit paths already exist |
| F2. Transient auto-indexes / SCANs in EXPLAIN (§1.3) | Add the permanent covering index the planner wants | schema rung + migration | Pre-1.0 window per D-036/D-032; precedents D-151 (`idx_links_target`), D-273 (lineage index); add EXPLAIN assertions per project convention |
| F3. WAL checkpoint churn (§3b) | Documented bulk recipe: disable autocheckpoint, explicit `checkpoint()` after | docs + caller code | No crate change; verify via `CheckpointReport` — **measured post-D-274: shape-dependent. Random-order bulk: default 5.0 s → 3.7 s at a 10,000-page threshold (WAL 43 MB) → 3.6 s disabled (WAL 554 MB); chain-shaped: a wash. Recipe documented in the `bulk_import` docstrings (§3b, §7)** |
| F4. Snapshot anchoring mid-bulk (§1.4) | Defer cadence across bulk (pause/resume or open flag) | crate if knob insufficient | Measure first — prior single run says cadence is *not* the cost |
| F5. Embedding chunk ceiling row-based (`CHUNK_ROWS_EMBEDDINGS`=30) oblivious to width | Byte-budgeted chunking (`rows × dim × 4 B`) instead of row-counted | crate (`connection::chunk_rows` + adaptive loop) | If §2c shows the *index build itself* dominates, this only helps partially — then see F6 — **confirmed: it does (holds ≈ wall), so this stays moot until the build changes** |
| F6. DiskANN build itself is superlinear in dim (engine-side) | Accept + route around: default dim 256, wide models as offline backfill; per-model tables make this additive, not a migration | app-side (OKFgraph) + upstream libSQL tuning | Staging alternative unsupported today (index mandatory per D-037 — dropping it disarms dim enforcement) |
| F7. Per-call binding overhead (§2d) | Keep batches at the §3a optimum; document call-size guidance | docs | No code change |

Suggested order: §1 (a day, mostly scripts) → §2c/§3a (same day, biggest lever:
if 8×2k ≈ 10s for 16k edges, the OKFgraph importer just uses that and the urgency
drops) → §2a–c → §4 as findings dictate.

---

## 5. Lock it in — regression gates

- Commit the 2k/20k bench + edge ladder + dim sweep as a **non-gating** perf
  script (absolute budgets are not CI gates per D-055 — shared runners lie;
  use criterion baselines, machine-against-itself, per project convention).
  **Done for the ladder portion: `benchmarks/diagnostics/spike_ladder.py`, JSON
  in `benchmarks/results/diagnostics/`.**
- Gate instead on structure: EXPLAIN assertions for every index-sensitive
  statement (existing convention). **Done for the defect D-274 fixed: the pin
  now runs a populated no-statistics arm and names the wrong index — the empty
  fixture alone is what let the defect ship (D-273's lesson from the other
  side). A budget-violation test on the bulk path is deliberately NOT added:
  D-055's reasons hold here exactly as they hold everywhere else, and the
  ladder showed the violations were the symptom, not the cause.**
- Re-run the ladder after each fix; publish the new table in the release note
  (the README performance table is a per-release measured series — extend it,
  don't overwrite history). **Done in D-274.**

---

## 6. What the outcomes mean for the OKFgraph rework

- **F1/F2 land** (bulk ≈ linear, near concept-write rate) → macrame carries the
  full rework including vectors at 256 now, 512 as additive model later.
- **Only F3/F7 land** (bulk workable via recipe: ~2k calls, checkpoint discipline,
  dim ≤256) → proceed on macrame with the importer written to the recipe;
  512-dim as offline backfill (F6).
- **Nothing moves, engine-bound (F6)** → split backend honestly: macrame for
  graph + temporal + FTS, vectors stay on ladybug (or deferred). More ops
  complexity, but each engine does what it's good at.

**Outcome as measured (2026-09-10): the second branch, without the recipe.** F2
landed and the edge bulk is linear at 2.22 s for 16k — above the concept-write
rate (0.90 s for 20k) but no longer superlinear; call size is no longer a
discipline to follow. Vectors are F6-engine-bound, so the rework proceeds with
dim ≤ 256 in the importer and wider models as offline backfills, exactly the
second branch's plan minus the recipe constraints.

---

## 7. What has been deferred, and why

Deferred is not "forgotten": each row below names the measurement that holds it
open, the condition that would reopen it, and the layer the fix would land on.
None of these is a judgment call carried in someone's head — D-274 and §0b are
the records each one answers to.

| Deferred | Why | What would reopen it |
|---|---|---|
| **F6 — the DiskANN vector build** (5k vectors: 10.53 s @ dim 64, 76.41 s @ dim 256, holds ≈ wall at both) | The growth is index maintenance **inside the engine** — the crate's chunk loop is one prepare per chunk and one execute per row, fixed overhead, and there is nothing superlinear left on the crate side to remove (§0b.7). The route-around is already structural: per-model tables make a wider model *additive* rather than a migration, the importer defaults to dim ≤ 256, and 512+ arrives as an offline backfill. Dropping the index to stage rows is not on the table: the index carries dimension enforcement (D-037). | Upstream libSQL DiskANN work, or the OKFgraph rework's decision that inline dims stay ≤ 256 |
| **F1 — a bulk mode that skips the materialization** (write ledger rows without `links_current` maintenance; `rebuild_current_chunked()` once at the end) | The residual ~135 µs/row is D-142's maintained materialization — `trg_links_current_sync` keeping three indexes on `links_current` true on every insert (§0b.4: 98.7 µs/row of the 142.9). F1 would close most of the gap to the concept rate (0.90 s / 20k concepts against 2.22 s / 16k edges). It is deferred because it is an **API-surface decision and a semantic trade**: the overlap guard is a read a bulk can skip, but `links_current` is a cache a *read* cannot — skipping its maintenance means a window where reads are stale until the rebuild, which is a caller-facing contract that deserves its own measured entry rather than riding D-274's. Doctrine VI sanctions it; `rebuild_current_chunked` measured 104 ms at 16K rows plus a 46.8 ms swap turn (D-023, D-082), so the arithmetic is already on the table. | A caller whose importer needs concept-write rate: then measure rebuild end-to-end on their fixture and decide flag vs documented recipe |
| **F5 — byte-budgeted embedding chunks** | Moot by the same measurement that deferred F6: the build inside the chunk dominates (holds ≈ wall, §0b.7), so chunk shape changes nothing until the build does. | Only if F6's engine cost changes |
| **Footprint — 260 MB + snapshot bytes vs 79 MB reference @ 20k** | The plan's §1.4 measured snapshot cadence *cost* (time), not snapshot *size*; the trigger table's 260 MB was never re-measured this session and no optimization was attempted. Labeled unverified rather than promising. | If the OKFgraph rework cares about file size, snapshot format/compaction is its own measurement — start from the recorded `snapshot_bytes` field the harness already logs |
| **Reference-hardware rerun** | Every number in this document is **preliminary (non-reference hardware)** — this box, medians of 2–3 sessions. The claims are shapes (flat vs growing, which index, which layer); the decimals are not portable. | Reference iron (Windows 11, NVMe, 32 GB, release build), extending the benchmarks README's per-release measured series rather than overwriting it |

### Refuted, not deferred — recorded so they are not re-proposed

These were live candidates when the plan was written and are **out by
measurement**, with the arm that retired each:

- **F3 — WAL checkpoint churn**: holds ≈ wall at every size (§0b.2); no
  checkpoint gap ever appears between wall and holds. A bulk recipe (disable
  autocheckpoint, explicit `checkpoint()`) has nothing to buy. **Revised
  2026-09-10, post-D-274: the mechanism was right and the conclusion was
  shape-dependent.** The cost lands inside holds — true — but its *size*
  depends on the key order: on the chain-shaped ladder the sweep is a wash,
  and on the random-pair shape the in-hold checkpoint work is ~26% of the wall
  (5.0 → 3.7 s at a 10,000-page threshold, §3b). F3 moves out of this list and
  into the recipe column: the knob, the numbers and the shape caveat are
  documented in the `bulk_import` docstrings (§3b).
- **F4 — snapshot anchoring mid-bulk**: cadence arms (default 10,000 vs
  `None`) measure the same ladders within noise, at every size (§0b.2). §1.4's
  prior single-run delta (12.8 vs 24.7 s) was session noise, as it suspected.
- **F7 — per-call binding overhead / call-size recipe**: the post-fix sweep is
  flat — 16k edges take 2.18–2.20 s from one call to sixteen (§0b.6). Call size
  governs latency, which is what `CHUNK_BUDGET` says it governs; throughput is
  the caller's business and no longer the recipe's.
- **`ANALYZE` at open / at the first chunk boundary** (§3d): rejected in D-274
  on D-197's measurements — `optimize()` declines below a 25× growth ratio and
  costs 460 ms above it, an analysis on an empty table records nothing, and the
  wrong plan exists exactly when statistics are absent. The `+` makes the plan
  statistics-independent, which is strictly better than making the statistics
  arrive sooner.
- **A timing gate on the bulk path** (§5's budget-violation test): D-055's
  reasons hold — the ladder showed the violations were the symptom, not the
  cause, and the plan pins are structure instead.

---

## 8. What can be further optimized

Ranked by expected value; each names the measurement that would validate it
before anyone commits code, per this document's own §0 discipline.

1. **F1, the materialization-skipping bulk.** The biggest remaining lever on
   the edge path, and the only one with its arithmetic already measured: 16k
   edges at 2.22 s today; `rebuild_current_chunked` at 104 ms for 16K rows plus
   one 46.8 ms swap turn (D-023, D-082); the concept rate at 0.90 s / 20k as
   the target. Expected landing: a 16k bulk near ~1 s for a caller who opts
   in. Validation: the spike ladder against a build with the mode, plus the
   audit paths — the rebuild report and `metrics().violations()` — on the
   same run. Cost: one public knob and one honest doc entry about the
   stale-read window.

2. **Batch the per-row overlap guard.** ~~The next thing §0b.4's dissection
   should time before anyone assumes it is worth it.~~ **Measured
   2026-09-10 and closed: the per-row guard costs 1.4 µs/row on a 16,000-row
   projection (a 3-column seek on `idx_lc_open_interval`, engine-only), the
   batched form (one `VALUES`-CTE join per 90-key chunk) costs 0.8 — a saving
   of 0.6 µs/row, ~0.5% of the edge path's ~130. The same dissection closed
   the neighbouring idea too: a multi-row `INSERT` (90 `VALUES` tuples per
   chunk) measures 7% *slower* than per-row executes over one prepared
   statement, because the per-chunk statement text differs and recompiles
   where the single prepared statement is already compiled once. The per-row
   guard on the write path is not a bottleneck at any measured scale, and the
   statement it would be batched with is one that gets slower batched. The
   item is closed with its numbers rather than open with its estimate.**

3. **The same defect class elsewhere.** ~~Cheap insurance: run every pinned
   plan in `tests/index_plan_tests.rs` against `populated_without_statistics`
   too.~~ **Done 2026-09-10:**
   `every_justified_index_is_the_one_the_planner_picks_without_statistics`
   now sweeps the whole registry across D-198's fixture, with its own fixture
   guard (the table `sqlite_stat1` must not exist — it does not exist until
   the first `ANALYZE` creates it). The sweep passes clean — D-274 was the
   only instance in the current registry — and stands as the gate that holds
   every future index rung's first session, where the new index has no stat1
   row even on a database whose other indexes do.

4. **Vector route-around hygiene (F6).** Not an optimization so much as
   executing the deferral cleanly: importer defaults dim ≤ 256, wider models
   as offline backfills on their own per-model tables (additive, not a
   migration), and the dimension stays enforced by the index.

5. **Footprint (§7's unverified row).** The 260 MB figure predates this
   session and was never re-measured. If file size matters downstream, the
   measurement starts from what the harness already records (`snapshot_bytes`,
   `page_count`, WAL bytes per arm) and asks the snapshot format whether it is
   carrying anything a fold could regenerate — which is the same question
   Doctrine VI answers for `links_current`.

Deliberately closed, and not worth reopening: a statistics-refresh schedule
(§7's refuted row — the `+` is better than sooner statistics), and any timing
assertion in CI (D-055). The remaining flat ~135 µs/row on the edge path is
the maintained materialization doing its job; the lever for it is F1, which is
a decision a caller makes, not a defect.

---

## 9. The follow-on cycle — what the 0.16.1 comparison leaves open (2026-09-11)

The same battery run against ladybug 0.20.3 on the same box, with 0.16.1
plugged in, splits cleanly into what this crate has already won and what it
has not. **Won**: small writes (927 ms vs 1,010 ms for 2k + 4k), and every
traversal — 1-hop 0.13 ms vs 5.9 ms, 5-hop 0.23 ms vs 4.4 ms, 19–45× across
the ladder. **Still open**, ranked by gap size; each item names the
measurement that would validate it before anyone commits code, per §0's own
discipline. Per the release discipline adopted with 0.16.1: **each landed fix
ships as its own patch release** (0.16.2, 0.16.3, …) with its own register
entry and its own gates run; a measured refutation that changes no code lands
as a docs commit without a bump.

### 9.1 The vector build — 8.5–10× behind, and superlinear in dimension (F6, engine-side)

The largest gap in the table: 2k×256 is **29.8 s against ladybug's 3.5 s**, and
2k×512 is **58 s against 5.6 s**. Within macrame the scaling itself is the
tell: 64→256 is 4× the dimensions but **7.8× the time** (3.8 s→29.8 s),
~12× per-vector. A linear-in-bytes build (ladybug's shape, ~linear) would be
4×; the excess is the item.

Measurements before any code:

- **(a) Blob I/O vs DiskANN maintenance.** Same 2k×256 corpus, insert with the
  DiskANN index dropped vs present (fresh file per arm; recreate after). If
  blob inserts are flat while indexed inserts grow, the cost is per-row index
  maintenance, not I/O.
- **(b) Full build vs incremental.** Insert every row with the index absent,
  then `CREATE INDEX` once (one-pass DiskANN build), against the incremental
  path. If the one-pass build is materially faster, the candidate is a
  **drop → load → rebuild recipe**.
- **(c) The D-275 WAL recipe on the vector path** — heavy WAL traffic, never
  measured with the 10k-page threshold.
- **(d) The dim-scaling dissection** — per-chunk holds at 64/256/512 on the
  same N, to say whether the growth is in the encode, the statement, or the
  index.

The correctness twist §4 of the schema states plainly: the DiskANN index is
**load-bearing for correctness**, not only for speed — it is the storage-layer
dimension backstop (a wrong-length blob is *accepted* without it, `ddl.rs`
`create_embeddings_index`). So a drop-rebuild recipe cannot be a silent
default; it is a register decision with its own Rejected line for keeping the
index during loads, and it must lean on the API layer's check
(`EmbeddingCodec::encode` rejects a wrong dimension before any statement
runs). The storage backstop would hold for everything that goes through the
crate and be given up only for callers loading through the recipe.

### 9.2 The edge ladder residual — ~2.5× per 2×, down from ~4×

Post-D-274 the ladder is 0.4/1.3/3.5/8.0 s at 2k/4k/8k/16k — per-row cost
still climbs (~200→500 µs) where the fix made it *flat at a fixed size*. Two
hypotheses, in order:

- **Checkpoint drain.** The ladder ran at the default WAL threshold; D-275
  already measured 16k at 5.0→3.7 s with the 10k-page recipe. Rerun the whole
  ladder with the recipe: if scaling moves toward ~2×, the residual is WAL
turnover and the recipe (or an opt-in `bulk_import` knob) closes it.
- **Page-cache misses.** If the growth survives the recipe, dissect cache_size
  against DB size — a file that outgrows the page cache turns every probe into
  a miss, and that shape would also explain the concept-per-row gap.

### 9.3 The vector query — 16.4 ms vs 4.5 ms top-10

The path is `vector_top_k` → distance recompute on the k rows it selects, and
the escalation loop only when a first pass comes up short — which a clean
corpus should never trigger. Dissect raw `vector_top_k` against the full
`search()` path to locate the 16.4 ms; then price the DiskANN search-width
knob against recall, with a Rejected line for anything that silently lowers
it.

### 9.4 The footprint — 260 + 11 MB vs 79 MB, never re-measured

§7's unverified row (§8.5). First re-measure @20k with the D-275 recipe —
transient WAL is part of that number — then decompose what remains:
page_size, the content envelope, the FTS shadow, the log table. The question
is the same one Doctrine VI answers for `links_current`: is anything being
stored that a fold could regenerate.

### 9.5 The keyword cell — empty for honesty's sake

0.16.0's "21 ms top-10" and ladybug's "47 ms, 18k rows" are different
workloads; the cell as it stands proves nothing in either direction. Run
ladybug's shape — the full 18k-row match — on 0.16.1's FTS5 and fill the cell
with the same workload on both sides. Likely outcome: macrame is already
faster, and the row closes with a number instead of an absence.

---

