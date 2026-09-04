# Jacquard Plan — v0.2.0

**Supersedes** [Jacquard Plan v0.1.0](Jacquard%20Plan%20v0.1.0.md), which was written
against Macrame 0.12.0, schema v10, and D-147. This revision is rebased on Macrame
**0.15.0**, schema **v15**, snapshot format v4, and **D-242**, and on the findings of the
[v0.15.0 codebase review](Macrame%20Codebase%20Review%20v0.15.0.md). Turso is still taken
at **0.7.0** and every capability claim is still read from documentation; nothing in
Phase 0 has run. Sections that v0.1.0 got right are kept in their shape and marked
*unchanged*; sections it could not have written are marked *new*.

The one-sentence delta: **v0.1.0 planned a server for a ledger with one lineage.
Macrame now has a tree of them, lineage is in the primary key, and every part of the
plan that touched a read, a guard, or an archive predicate has to be re-derived.**

---

## 0. What changed between the plans

| Macrame since 0.12.0 | Consequence for Jacquard | § |
|---|---|---|
| **Branching** (W12, D-213 … D-238): `branches` register, `branch_id` on all four ledger tables and last in the `links` key, nearest-lineage resolution under a running-minimum fork cutoff, shadow retirement, `diff`, `archive_branch` | The largest new section. Ancestry is a recursive CTE and Turso has no `WITH RECURSIVE`; the overlap guard is now a read over what a lineage can see, which MVCC cannot make atomic | §5 |
| **D-229**: the archive predicates were lineage-blind and deleted rows the ledger still believed | The predicates are Bin A design input and must port verbatim | §5.6 |
| **`ErrorKind`** (D-242): twelve categories, one exhaustive match inside the crate | Jacquard's wire-level error taxonomy exists already | §9 |
| **`Tuning`, `WalCheckpointPolicy`, `FutureStampPolicy`** (W5, W7) | The knobs a server exposes per tenant, and the ones it must not | §10 |
| **`hot_log_reach`** trichotomy, snapshot container v4 with CRC and bounded decode, `SnapshotWriteFailed` | Replay and snapshot design transfers engine-independently | §8 |
| **R15 resolved** as a per-`connect()` probability on distinct files (D-148) | Confirms v0.1.0's "no R15" row and sharpens what a server must not do: churn handles | §4 |
| Review findings C-1, C-5, C-7, C-8, A-1, A-2, A-3 | Costs Jacquard should design out rather than inherit | §16 |

---

## 1. What Jacquard is, and what it is not — *unchanged*

A server-side bitemporal graph ledger on Turso, multi-tenant, one database per tenant,
carrying Macrame's eight doctrines and the branching clause D-213 added to Doctrine II. It
is not Macrame recompiled: the engine is different, the concurrency model is different,
and the code that transfers is deliberately small (§2). It is not a merge engine: a
branch is forked, read, compared and abandoned, and merge is refused with a reason (road
map §19), in Jacquard as in Macrame.

---

## 2. What transfers: the register, not the code — *amended*

### Bin A — engine-independent, transfer as design input

v0.1.0's list stands (the doctrines, the two clocks, the fold, the projection rule, the
canonical timestamp, the payload version discipline, D-070's measurement methodology).
Added since:

- **The lineage model, whole.** One shared ledger, logical versions, `branch_id` as
  provenance on concepts and as identity on links (D-214, D-232). Nearest-ancestor wins,
  not `IN (ancestry)` (D-220). Fork points are visibility cutoffs that narrow under a
  running minimum and never widen (D-223). Retirement across lineages is a shadow row,
  never a write to the parent (D-225). Divergence is one statement over two resolutions,
  never row provenance (D-228). Abandonment moves links, concepts, log **and** the
  `branches` row, or the fold lies (D-230).
- **The archive predicates as corrected by D-229**: a "later assertion" is later on the
  same lineage; a closed interval is archivable only if no other lineage holds the key.
- **`ErrorKind`** and the rule that the exhaustive match lives inside the crate (D-242).
- **The stability contract's shape** (appendix D): what freezes, what does not, and that
  derivative tables have no schema guarantee at all.
- **The snapshot container** (format v4): magic, versioned header, CRC-32, bounded zstd,
  bounded bincode. Bytes, not SQL; it transfers as a file format.
- **`hot_log_reach`**: `Covers` / `PredatesRecordedHistory` / `NeedsArchive` as the only
  three answers a recorded-time read can get.
- **`MaterializedState` with `EdgeBelief`** per lineage (D-222), and the review's C-10:
  a fold that returns unresolved beliefs needs a pure `resolve` beside it.

### Bin B — libSQL-specific, dies with the engine

v0.1.0's list stands (the Write Actor as a workaround for one write connection, the
`links_current` sync trigger, FTS5 and its three triggers, DiskANN and `vector_top_k`,
`ATTACH`-based cold storage, `PRAGMA synchronous = NORMAL`, R15). Added:

- **The recursive ancestry CTE** (`ancestry_cte`) and the `WITH RECURSIVE` walk.
- **The five guard triggers whose `WHEN` reads `sqlite_master`** for the archive-session
  marker. A server that is the only writer does not need a trigger to know it is in an
  archive session (§5.6).
- **`lineage_shape` chosen by a `branches` row count per query.** A server holds the
  register in memory (§5.2).
- **`diagnostic_conn` opening a handle per call**: a server has a pool.

### Bin C — re-open under Turso, do not assume

- `ROW_NUMBER() OVER (PARTITION BY …)`: four folds, the projection, `diff`, and now the
  nearest-lineage `links_cut` all use it (**S11**, sharpened as **S17**).
- Partial unique index with `branch_id` in the key and the sentinel in the `WHERE`
  (**S18**).
- Multi-row `VALUES` as a derived table, bound from the driver (**S15**).
- The host parameter limit and `IN (…)` width (**S16**).
- `INSERT OR IGNORE` / `ON CONFLICT DO NOTHING` with `RETURNING` (**S19**).
- `json_extract` over the payload column, used by every fold.

---

## 3. The engine delta, re-measured against 0.15.0's usage — *amended*

v0.1.0's two tables stand. What 0.15.0 added to Macrame's dependency on the engine:

| Macrame 0.15.0 uses | Where | Turso 0.7.0 | Jacquard's answer |
|---|---|---|---|
| `WITH RECURSIVE` for ancestry (a second use beyond the walk) | `graph/lineage.rs` | Not implemented | Resolve in the driver, bind as a list (§5.2) |
| `ROW_NUMBER()` partitioned by `(key, branch)` for `links_cut` and `diff` | `lineage.rs`, `branch.rs` | "default frame" only | S17; fallback is a correlated `NOT EXISTS` (§5.3) |
| Trigger `WHEN` clauses reading `sqlite_master` (five triggers) | `ddl.rs` | S2 | Not needed: guards move into the server's write path (§5.6) |
| `REFERENCES branches(branch_id)` on four tables | `ddl.rs` | `PRAGMA foreign_keys` supported | Ports; but a two-phase cold move (§5.6) means the FK is checked per phase |
| `idx_lc_lineage_cut` and the v15 key with `branch_id` last | `ddl.rs` | Ordinary indexes | Ports verbatim |
| Cross-database `DELETE … WHERE id IN (SELECT id FROM cold.concepts)` | `archive.rs` | S7 | Two-phase move with an idempotent marker (§5.6) |

---

## 4. Concurrency: the sequencer, not the actor — *rewritten*

v0.1.0 §3 said "Jacquard does not inherit the Write Actor" and argued from MVCC. Two
things have moved since.

**First, the review's A-3 says what the actor is actually worth**, and it is not the
serialisation. The actor is the one place that can hold prepared statements, the lineage
shape, the hot-log verdict, and the last committed sequence number, and invalidate each
on a named command. A pool of stateless connections gives all of that up.

**Second, D-225 made the overlap guard a read over what a lineage can see**, and that
read cannot be a unique index. The trunk-only guard could be (v0.1.0 §4.1's partial
unique index handles single-open). The cross-lineage rule, *a lineage may not overlap
what it can see*, compares a proposed interval against rows on other lineages that the
writer does not touch, so two concurrent `BEGIN CONCURRENT` writers on different branches
have no row conflict to detect and can both commit an overlap. MVCC's conflict detection
is row-level; this invariant is not.

So the model is:

- **One ledger sequencer per tenant database.** A task that owns the writes to `links`,
  `concepts`, `transaction_log` and `branches`, holds the state A-3 names, and runs the
  guards in code before each write. It is the Write Actor kept for what it is good at
  and freed from what it was compensating for.
- **`BEGIN CONCURRENT` for everything else.** Embedding upserts, analytics annotations,
  FTS maintenance, the derived projection's shadow rebuild, and all reads. These conflict
  only on their own rows and the sequencer's commits never wait on them.
- **The chunk budget becomes a fairness budget.** `CHUNK_BUDGET` bounds one process's
  write-lock hold. A server's equivalent is p99 write latency per tenant under mixed
  load, and the sequencer's turn metrics (`CommandKind`, the histogram, `over_budget`)
  transfer as the instrument for it, per tenant.
- **Hub contention** (v0.1.0 §13.2) is settled by construction: the sequencer serialises
  the ledger, so there is no conflict-retry storm to measure. **S22** measures the
  opposite risk, that one sequencer per tenant is a throughput ceiling, against the
  ledger's own transaction floor.

**J-001 is confirmed with sharper wording**: one writer per tenant for the four ledger
tables; MVCC for derived state and reads; replicas read-only.

---

## 5. Branching under Turso — *new*

### 5.1 The storage model transfers verbatim

Schema v15's shape is what Jacquard's Phase 1 schema is. `branches (branch_id, parent_id,
forked_at, created_at)` with the root `CHECK` (`parent_id IS NULL` iff `forked_at IS
NULL`); `branch_id NOT NULL DEFAULT 'main' REFERENCES branches` on `concepts`, `links`,
`transaction_log`; `links` keyed `(source_id, target_id, edge_type, valid_from,
recorded_at, branch_id)` with `branch_id` **last** (D-232's plan argument holds on any
b-tree engine). Concept identity global, `branch_id` provenance (D-214). A fork writes
one row.

Jacquard starts with this key. D-036 forbids a key change after 1.0 and v0.1.0's schema
would have needed one; that is the strongest reason to rebase now rather than add
branching in a later phase.

### 5.2 Ancestry without `WITH RECURSIVE`

The `branches` table has one row per fork ever made and is written only by the sequencer.
The sequencer holds it in memory as `Vec<Branch>` with a generation counter, refreshed on
`Fork` and `ArchiveBranch`. Ancestry for a reading branch is computed in Rust:

```text
walk parent pointers from the reader to the root;
dist = 0, 1, 2, …;
cutoff(reader) = open;
cutoff(ancestor) = min(cutoff(child), child.forked_at)   -- running minimum, D-223
```

and bound into the query as a derived table:

```sql
WITH ancestry(branch_id, cutoff, dist) AS (VALUES (?, ?, 0), (?, ?, 1), …)
```

**S15** confirms multi-row `VALUES` as a CTE body. If it fails, the fallback is
`IN (?, …)` for membership plus a `CASE branch_id WHEN ? THEN ? …` for `dist` and
`cutoff`; uglier, same plan. Chain depth is bounded by the parameter limit (**S16**),
which at SQLite's 999 default still allows a chain of over 300, far beyond D-219's
measured 100.

This is the review's A-2 and it is built in **Macrame first**, differentially tested
against the CTE on the existing branching fixtures, then taken. Macrame keeps the CTE as
the oracle in tests and ships the bound list if it measures no worse.

**Reads may see a `branches` table newer than the cached one, never older.** A newer
table with a name the cache lacks is `UnknownBranch`, which the read handles today. An
extra ancestor is impossible because ancestry is fixed at fork time. So a stale cache is
sound for reads and the sequencer refreshes it before writes.

### 5.3 `links_cut` without the window function

D-223's hybrid: the projection for keys the ancestors have not touched since the cutoff,
unioned with a per-lineage log fold for the keys they have (`churned`). Both arms and the
nearest-lineage pick use `ROW_NUMBER() OVER (PARTITION BY key, branch ORDER BY …)`.

If **S17** confirms partitioned window functions, port as is with the ancestry bound per
§5.2. If not, the same result without a window:

```sql
-- latest belief per (key, lineage) without ROW_NUMBER
SELECT l.* FROM links l JOIN ancestry a ON a.branch_id = l.branch_id
 WHERE l.recorded_at <= a.cutoff
   AND NOT EXISTS (SELECT 1 FROM links n
                    WHERE n.source_id = l.source_id AND n.target_id = l.target_id
                      AND n.edge_type = l.edge_type AND n.valid_from = l.valid_from
                      AND n.branch_id = l.branch_id
                      AND n.recorded_at > l.recorded_at AND n.recorded_at <= a.cutoff)
```

and the nearest-lineage pick (`MIN(dist)` per key) is done **in the driver**, which
already holds the frontier for the hop traversal (§6). The correlated `NOT EXISTS` is a
seek on the v15 key. This is the baseline choice, because it also removes the trunk's
cost on a forked database (review C-7): the trunk's ancestry is one row with no cutoff
and the query degenerates to a filtered projection read with no fold arm.

### 5.4 The guards under MVCC

| Macrame guard | Mechanism | Jacquard |
|---|---|---|
| `trg_links_single_open` | Trigger, per lineage | Partial unique index `(source_id, target_id, edge_type, branch_id) WHERE valid_to = sentinel` (**S18**). The `branch_id` is what v0.1.0 §4.1 lacked |
| `reject_overlapping_interval` (D-060, D-225) | Actor read over what the lineage sees | Sequencer read, same SQL as the resolved read narrowed to one key (§5.3). **Cannot be an index**; this is §4's argument |
| `reject_overlaps_within` (D-179) | Sort and sweep in Rust | Transfers unchanged |
| `trg_concepts_monotonic_ra` | Trigger | Trigger, unchanged (v0.1.0 §4.3), plus the sequencer's clock floor |
| `trg_concepts_cross_lineage`, `trg_concepts_branch_immutable` | Triggers | Sequencer checks in code; the trigger stays as belt-and-braces if S1 shows triggers fire on synced rows |
| `trg_branches_frozen_*` | Triggers reading `sqlite_master` | Sequencer refuses; no trigger (§5.6) |
| `trg_*_guard_delete` | Triggers reading `sqlite_master` | The ledger role has no `DELETE` on hot tables; the archive session uses a separate role (§5.6) |

### 5.5 Fork

One row. The fork point is a **ledger-clock** stamp issued by the sequencer, never the
wall clock (D-224 found the trunk's `created_at` stamped from the wrong clock and the
invariant uncheckable). The rule is `forked_at >= parent.forked_at`, same clock by
construction, which keeps fork points non-decreasing down a root path, the property §5.2's
running minimum assumes. `BranchExists`, `ForkPrecedesParent`, `UnknownBranch` transfer.

### 5.6 Archive, cold storage, and the session

v0.1.0 §S7 asked whether `ATTACH` plus cross-database `DELETE` works in one transaction.
Jacquard should not depend on the answer. Cold storage is a **second Turso database per
tenant**, and a move is **two phases with an idempotent marker**:

1. In the hot database: select the archivable set under D-229's predicates, write its
   keys to `archive_pending (session_id, table, key)`, commit.
2. In the cold database: insert the rows (`INSERT OR IGNORE` on the same key, so a retry
   is harmless), commit.
3. In the hot database: verify the cold count for `session_id`, delete the rows named in
   `archive_pending`, write the `archive_horizon` row, delete the pending rows, commit.

A crash between 2 and 3 leaves rows in both places; the next session finds
`archive_pending` non-empty and resumes at 3. A crash between 1 and 2 leaves pending
rows and no cold rows; resume at 2. Nothing is lost at any point, and `hot_log_reach`
reads the horizon row, which is written last.

**The projection repair is keyed** (review C-1): the session collects the archived keys
anyway, so `links_current` is repaired for those keys only.

**The delete guards become a role.** Macrame's guards are triggers because raw SQL can
reach the file. In a server the only writer is the server, so "no physical deletion in
hot tables outside an archive session" (Doctrine V) is a connection role that lacks
`DELETE`, and the archive session is the one code path that uses the other role. If Turso
has no per-connection authorizer, the sequencer enforces it and **S21** is kept as a
belt-and-braces trigger without the `sqlite_master` probe.

**`archive_branch`** is the same two-phase move over a whole lineage, `branches` row
last, and its four refusals (trunk, unknown, has descendants, concept named by another
lineage's hot link) run in the sequencer before phase 1.

**`rehydrate`** takes the reverse path. A concept whose lineage has been archived is
refused with a typed error naming the branch (review C-3), never a foreign-key failure.

### 5.7 Surface parity

Everything Macrame's `branch` module exposes, on the wire: `fork`, `branches`, `diff`,
`archive_branch`, `Branch`, `Divergence`, `EdgeBelief`; `branch=` on every read;
`on_branch` on every write; `reconstruct_on(ts, branch)` (review C-10) from the start.
`BranchView` is a client-side convenience, as it is in Python.

### 5.8 What Jacquard does not add

Merge (road map §19). Cross-branch edges (D-238: not representable). A ninth doctrine
(D-213: lineage is a clause of Doctrine II). CDC-derived lineage (J-open-1 stays
deferred).

---

## 6. Traversal: the hop driver — *amended*

v0.1.0 §5 stands: hop-by-hop in Rust, frontier bound per hop, differentially tested
against `walk_cte()` on the 512-case harness before anything is taken. Three amendments:

- **The frontier carries lineage.** Each hop's edge read is §5.3's query with the bound
  ancestry, and nearest-lineage resolution happens in the driver as the rows arrive.
- **The cap is a real limit** (review C-8). The driver stops expanding when the node
  budget is met, so `probe_cap` and the byte budget bound work, not only memory.
- **Frontier binding** (**S13**) now interacts with **S16**: the per-hop `IN (…)` width
  plus the ancestry list must fit the parameter limit together; chunk the frontier if
  not.

The same driver serves `load_subgraph`, `FilteredVectorSearch::probe`, and `diff`'s
reach, which is the review's A-1 achieved by having one implementation rather than one
plan.

---

## 7. Search — *amended*

**Full text** (v0.1.0 §6.1): Tantivy-backed index, no sync triggers, `fts_score`. **S6**
unchanged. `escape_fts5_query` is replaced by Tantivy's query grammar; D-071's
escaping decision does not transfer.

**Vector** (v0.1.0 §6.2, still the largest unbudgeted piece). What 0.15.0 adds to the
brief:

- **A `models` registry table**, not a `sqlite_master` pattern (review C-15): `(model,
  dimension, table_name, created_at)`. Dimension check is a `CHECK (length(embedding) =
  4 * dim)` on the per-model table, since Turso has no `F32_BLOB(n)`.
- **The escalation loop transfers.** `search_vector`'s `k'` doubling until the visible
  set is satisfied or the index is exhausted, `rerank_depth = max(5k, 50)`, half-life
  decay, and `visible_concept` as a post-filter are engine-independent and are the shape
  any ANN index sits behind.
- **HNSW design gate.** Before Phase 3, one document: in-process HNSW (usearch or
  `hnsw_rs`) per model per tenant, built from the table on open, updated on upsert by the
  sequencer, persisted as a sidecar the table can always regenerate (Doctrine VI). **S12**
  sizes whether exact search is enough below 100K.
- **Hybrid RRF** transfers; `rank_of` is a map (review C-16).

---

## 8. Replay, snapshots, archive reach — *amended*

- **The container format v4 transfers as bytes.** Snapshots live in object storage per
  tenant, keyed by `seq_anchor`, with the same retention (5 plus one per day for 30).
  `sync_directory` is replaced by the store's durability.
- **`hot_log_reach` transfers.** "Intact" is *no `archive_horizon` row*, never a
  `COUNT(*)` (review C-5), and `hot_log_answers_for` takes the timestamp seriously
  (review C-2): a reach of `Covers` is answered from the hot log after any number of
  archives.
- **`seq_id`.** v0.1.0 §4.2's ULID scheme stands if **S5** shows `AUTOINCREMENT`
  serialises under MVCC; with the sequencer owning the ledger it may not matter, and a
  server-issued monotonic counter is simpler. Either way replay uses inequalities (R13)
  and gap tolerance is already tested.
- **Chain verification is incremental by default** (review C-18): each save folds from
  the previous anchor and compares before it writes. The genesis fold stays on the
  operator's schedule.
- **`verify_snapshot_chain` and `reconstruct` are per lineage** (`reconstruct_on`).

---

## 9. Errors and the wire — *new*

`ErrorKind`'s twelve categories are the wire taxonomy; `DbError`'s variants are the
detail field. A first mapping, to be argued in Phase 5:

| `ErrorKind` | Transport status |
|---|---|
| Validation, Branch (`InvalidBranchId`, `BranchMismatch`) | 400 |
| NotFound, `UnknownBranch` | 404 |
| Conflict (overlap, single-open, `BranchExists`, `CrossLineage`) | 409 |
| Temporal refusals (`ArchiveRequired`, `FutureRecordedAt`, `RecordedAtRegression`) | 422 |
| Containment (`WriterStopped`, sequencer dead) | 503 |
| Storage and replay corruption | 500, with the tenant flagged |

The rule D-242 established transfers: the exhaustive match over `DbError` lives in the
crate that defines it, and a variant added without a wire mapping does not compile.

---

## 10. Server surface, tenancy, tuning — *new*

- **Tenant = database.** One hot database, one cold database, one snapshot prefix, one
  sequencer, one in-memory `branches`. Cross-tenant queries do not exist.
- **Handles are held, never churned.** D-148's finding is a per-`connect()` fault on
  libSQL; Turso has no R15, but a pool that opens per request is wrong on any engine.
  One pool per tenant, bounded, opened on first use, closed on eviction.
- **Tuning per tenant** is `Tuning` minus what a server owns: the cadence and the
  checkpoint policy are exposed; the clock is not; `synchronous` is `FULL` and not a
  knob (v0.1.0 S8 measures the cost; road map §19 rejects the knob for the same reasons).
- **Observability**: `MetricsSnapshot` per tenant, per `CommandKind`, exported; plus the
  reach verdict counts, the archive session durations (exempt from the budget, visible
  in their own histogram: the review's A-5), and the branch count.
- **Wire protocol** (J-open-3): a thin, operation-level protocol over gRPC or HTTP+JSON.
  Not SQL over Turso's remote protocol: Jacquard's operations are ledger-level
  (`assert_edge`, `fork`, `reconstruct_on`) and exposing SQL would reintroduce `raw()`,
  which both codebases refuse.

---

## 11. Sync topology — *unchanged in shape, one decision added*

v0.1.0 §7 stands and **S1** and **S14** still gate it. One decision is made now rather
than after S14: **the ledger is written only at the primary, by the sequencer, and
embedded replicas are read-only.** Branching is a server-side operation. This follows
from §4 and it removes J-open-2 from the critical path: multi-master conflict semantics
for a bitemporal ledger stay deferred and named, and nothing in Phases 1 to 4 depends on
them.

---

## 12. Phase 0 spikes — *amended*

S1 to S14 stand as written in v0.1.0. Added, with what each gates:

| ID | Question | Gates |
|---|---|---|
| **S15** | Multi-row `VALUES` as a CTE body; row count bound from the driver | §5.2, the ancestry list |
| **S16** | Host parameter limit; `IN (…)` width; cost curve of a wide `IN` | §5.2, §6 |
| **S17** | `ROW_NUMBER() OVER (PARTITION BY a, b ORDER BY c DESC)` with a filter on `rn = 1`; sharpens S11 | §5.3, the folds, the projection |
| **S18** | Partial unique index whose key includes `branch_id` and whose `WHERE` names the sentinel; `IF NOT EXISTS` | §5.4 |
| **S19** | `INSERT OR IGNORE … RETURNING` and `ON CONFLICT DO NOTHING` | §5.6's idempotent phase 2 |
| **S20** | Crash-injection of the two-phase move at every boundary; resume correctness | §5.6 |
| **S21** | Trigger `WHEN` without `sqlite_master`; per-connection authorizer or role | §5.4, §5.6 |
| **S22** | One sequencer per tenant: sustained assertion throughput against the transaction floor; queue depth at which p99 doubles | §4, J-001 |
| **S23** | `json_extract` over a BLOB/TEXT payload at fold scale | §8 |

Phase 1 does not start until S1 to S5 and S15 to S19 are answered. S17 is the one most
likely to change the plan: if partitioned window functions are absent, §5.3's fallback
becomes the design rather than the fallback, and Macrame's four folds are re-spelled to
match before Jacquard borrows them.

---

## 13. Phasing — *amended*

| Phase | Content | Exit condition |
|---|---|---|
| **0** | S1 to S23 | Every section confirmed or amended in writing |
| **1** | Schema **v15-shaped**, sequencer with `ActorState`, explicit ledger writes, guards in code, the fold, `as_of`, `reconstruct_on`, **fork and branched reads** | A database that records and reconstructs belief on a tree of lineages under concurrent derived-state writers |
| **2** | Hop driver and Rust-side ancestry, **both built in Macrame first** and differentially tested | Identical node and edge sets on the branching fixtures across both engines; a measurement on both |
| **3** | Search: Tantivy, `models` registry, exact vector, then HNSW behind its own design document | §9 budgets re-derived on named hardware under D-070 |
| **4** | Two-phase archive, `archive_branch`, rehydrate, snapshots in object storage, incremental chain check | Point-in-time reconstruction composes; crash injection at every boundary passes |
| **5** | Wire protocol, tenancy, sync, observability | Scoped after S14 |

Branching moved from "later" to Phase 1 because it is in the primary key.

---

## 14. Open decisions — *amended*

| ID | Decision | State |
|---|---|---|
| **J-001** | One ledger sequencer per tenant; MVCC for derived state and reads; replicas read-only | Confirmed, §4 |
| **J-002** | Cold storage is a second database per tenant; moves are two-phase with an idempotent marker | Proposed, §5.6; S20 gates |
| **J-003** | Ancestry resolved in the driver and bound as a list; built in Macrame first | Proposed, §5.2; S15 gates |
| **J-004** | Snapshots in object storage keyed by anchor; retention as Macrame's | Proposed, §8 |
| **J-005** | `ErrorKind` is the wire taxonomy | Proposed, §9 |
| **J-006** | The hot-log verdict is "no horizon row", cached in the sequencer | Proposed, §8 |
| **J-open-1** | Replace explicit ledger writes with Turso's CDC table | Deferred, unchanged |
| **J-open-2** | Multi-master conflict semantics | Deferred and off the critical path, §11 |
| **J-open-3** | Wire format | Narrowed to "operation-level, not SQL", §10 |

---

## 15. What this plan is most likely to be wrong about — *refreshed*

1. **Every Turso capability is still read, not measured.** Phase 0's findings outrank
   this document, as v0.1.0 said.
2. **§4's sequencer may be a ceiling.** A single writer per tenant serialises the ledger
   at the transaction floor. S22 measures it; if a tenant needs more than the floor
   allows, the answer is sharding the tenant, not relaxing the guard.
3. **§5.3's `NOT EXISTS` fallback has not been planned.** It is a seek per row on the
   v15 key and should be linear in the visible set, but "should" is what D-070 warns
   about.
4. **The two-phase move (§5.6) trades one atomic transaction for a protocol**, and
   protocols have more states than transactions. S20's crash matrix is the deliverable.
5. **HNSW is still one paragraph.** §7 commits to a design document before Phase 3 and
   not to a design.
6. **Building §5.2 and §6 in Macrame first assumes Macrame wants them.** The review
   argues it does (A-1, A-2, C-7, C-8); if Macrame's measurements say the CTE is faster,
   Jacquard carries the driver alone and the differential harness is still the test.

---

## 16. Macrame's own path — *refreshed*

v0.1.0 §9 listed what Macrame ships that Jacquard needs first. The list now, in the
order the review recommends:

1. **`ActorState`** (review A-3): prepared statements, lineage cache, hot-log verdict.
   The sequencer is this struct with a pool behind it.
2. **`ReadPlan`** (review A-1, road map W13): one lowering for the three readers and the
   guard. Jacquard has one reader by construction, and Macrame reaching that shape first
   is what makes the differential test meaningful.
3. **Ancestry in Rust** (review A-2): the CTE as oracle, the bound list as
   implementation, `reconstruct_on` and a pure `resolve` beside it.
4. **Keyed projection repair in the archive** (review C-1) and the timestamp-aware reach
   check (review C-2): both are design corrections Jacquard should not inherit as bugs.
5. **A lineage property generator** (review A-5): the fixture set both engines are
   tested against.

Each is a Macrame release on its own merits. Jacquard's Phase 2 is the point at which
they are borrowed, and nothing in Jacquard's Phase 0 or 1 waits on them.
