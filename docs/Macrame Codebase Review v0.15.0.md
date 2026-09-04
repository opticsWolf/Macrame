# Macrame Codebase Review — v0.15.0

**Reviewed at:** `dev/0.15.0`, commit `deeec15` (2026-09-04). Crate `macrame-db` 0.15.0,
schema v15, snapshot format v4, libSQL 0.9.30, MSRV 1.88. 22,566 lines of Rust under
`src/`, about 6,000 more in `bindings/python/src/`.

**Predecessor:** [Macrame Codebase Review v0.12.0](Macrame%20Codebase%20Review%20v0.12.0.md).
Every finding in that document has since shipped or been rejected in writing (its
"Superseded" section, then D-148 … D-242), so nothing here restates it. This review is
against the code as it stands after W12, the branching wave, and most of what it finds is
the residue of that wave: places where lineage reached the storage layer but not yet the
caches, the maintenance paths, or the second and third spellings of a query.

---

## 0. What was verified, not assumed

Everything below was found by reading the source, the DDL, the tests' names, and the
release notes. **No measurement was taken.** Where a finding rests on a cost, the cost is
argued from the query shape and the indices declared in `src/schema/ddl.rs`, and it is
marked as an estimate. Where the crate has already measured the thing, the decision entry
is cited and its number is quoted rather than re-derived.

Read in full: `connection.rs`, `error.rs`, `branch.rs`, `schema/{ddl,migrations}.rs`,
`graph/*`, `vector/*`, `integrity/*`, `temporal/*`, `metrics.rs`, `util/*`,
`bindings/python/src/{lib,runtime}.rs`, `Cargo.toml`, `.cargo/config.toml`, the four
workflows, `docs/releases/v0.15.0.md`, `docs/architecture/{s11-s12,appendices}.md`, and
the relevant sections of the road map and the Jacquard plan. Skimmed by grep:
`bindings/python/src/database.rs`, `benches/budgets.rs`, the test tree.

Not run: the test suite, the property suite, the benches, the Python suite. Not audited:
the migration ladder rung by rung below v12, the fuzz targets, the road map's §15.3
option analysis.

**Severity** is an estimate of what a finding costs a user today. **High**: wrong answers,
or an operation that does not scale with the ledger. **Medium**: a cost or a gap a real
deployment will hit. **Low**: hygiene, drift, or a cost bounded by something small.

---

## 1. Findings at a glance

| ID | Area | Severity | One line |
|---|---|---|---|
| C-1 | temporal/archive | **High** | Every archive session that deletes a link rebuilds `links_current` from scratch under the write lock: O(links), not O(archived) |
| C-2 | temporal/replay | **Medium** | An archived database refuses every recorded-time hydrate, including instants the hot log still covers; `hot_log_answers_for` ignores its timestamp |
| C-3 | temporal/archive | **Medium** | `rehydrate` of a concept minted by an archived branch fails on the `branches` foreign key, with an error that names neither the branch nor the remedy |
| C-4 | schema/ddl | **Medium** | No composite index on `transaction_log (entity_id, branch_id, seq_id)`; the archive predicate probes it per candidate row |
| C-5 | temporal/replay | **Medium** | `hot_log_is_intact` runs `COUNT(*)` over the whole log on every recorded-time read |
| C-6 | connection | **Medium** | The single-edge write pays two extra round trips per call: a `branches` count and a per-call prepare of the overlap guard |
| C-7 | graph/lineage | **Medium** | On a forked database the trunk itself takes the resolved path; `main` has no ancestors and needs a filter, not the CTE stack |
| C-8 | graph/vector_filter | **Medium** | `probe_cap` truncates after the traversal has run: it bounds memory, not work |
| C-9 | connection | **Medium** | `diagnostic_conn` opens a fresh database handle per call, which is the shape R15 counts |
| C-10 | temporal/replay | **Medium** | `MaterializedState::edges` returns every lineage's belief unresolved, and the resolution rule lives only in SQL |
| C-11 | api | **Medium** | `Tuning`, `TraversalBuilder` and `SnapshotCadence` expose public fields without `#[non_exhaustive]`; a new knob after 1.0 is a major version |
| C-12 | integrity/shadow | Low | The swap recreates indices and triggers by substring match on DDL constants |
| C-13 | temporal/archive | Low | `COLD_SCHEMA` runs against the cold file before the session transaction begins |
| C-14 | temporal/snapshot | Low | `save_and_prune` still maps a `JoinError` to `ReplayCorrupt`, the subject D-240 corrected everywhere else |
| C-15 | vector/registry | Low | `registered_models` uses `LIKE` with an unescaped `_`; a model named `…shadow` is hidden |
| C-16 | vector/hybrid | Low | `rank_of` is a linear scan per hit, quadratic in the rerank depth |
| C-17 | temporal/replay | Low | Two rustdoc comments contradict each other on whether a rollback leaves a `seq_id` gap |
| C-18 | temporal/replay | Low | `verify_snapshot_chain` is two full reconstructions and cannot check one link of the chain |
| C-19 | temporal/snapshot | Low | The cadence polls an aggregate every 5 s on an idle database; the actor already knows when it wrote |
| C-20 | error | Low | `abort_kind` classifies on libSQL's message text |
| C-21 | temporal/archive | Low | The closed-interval archive arm is conservative across lineages, and its rustdoc does not say what that costs |
| C-22 | temporal/archive | Low | `rehydrate` is a per-id loop with two to three round trips per id |
| C-23 | ci | Low | Neither workflow triggers on `dev/**`, so a release branch is unreplicated until it merges |
| C-24 | connection | Low | `check_lineages` runs one `branches` query per distinct lineage and keeps only the last answer |
| A-1 | architecture | — | Three spellings of one read: the builder, `query_as_of_edges_on` and `diff` each assemble their own lineage SQL |
| A-2 | architecture | — | Ancestry is recomputed inside SQL per query; `branches` is tiny and append-only and could be resolved once in Rust |
| A-3 | architecture | — | The write actor holds no per-connection state between turns |
| A-4 | architecture | — | `temporal/archive.rs` is 1,385 lines mixing predicates, sessions, lineage upgrade and rehydration |
| A-5 | tests/ci | — | No lineage property generator; no cost visibility on the budget-exempt kinds |
| A-6 | python | — | `close()` can be held off indefinitely by a hot read loop |

---

## 2. Correctness and robustness

### C-2 · `hot_log_answers_for` ignores its timestamp · Medium

[replay.rs:1024](../src/temporal/replay.rs:1024):

```rust
pub(crate) async fn hot_log_answers_for(conn: &libsql::Connection, _ts: &str) -> Result<bool> {
```

It returns `hot_log_is_intact`, which is false the moment one archive session has run.
Every caller (`hydrate_at_time`, the traversal's recorded-reach check, the filtered
vector search) then refuses with `ArchiveRequired` for **every** recorded instant,
including instants after the horizon that the hot log answers exactly. D-189 documents
the refusal as deliberate, and it was the right call when the alternative was a silently
short answer. But `hot_log_reach` ([replay.rs:813](../src/temporal/replay.rs:813))
already computes the precise verdict per timestamp: `Covers` when the requested instant is
at or after the horizon and the log is contiguous from there. The hydrate path should
consult it and refuse only on `NeedsArchive` or `PredatesRecordedHistory`.

**Effect today:** an application that archives monthly loses `AttributeMode::AtTime` and
every `recorded()` traversal for its entire history, not for the archived part.

### C-3 · `rehydrate` after `archive_branch` · Medium

`archive_branch` (D-230) moves the branch's `branches` row to `cold.branches`.
`rehydrate_session` ([archive.rs:1225](../src/temporal/archive.rs:1225)) reinstates
concept rows from `cold.concepts` by id and does not touch `branches`. A rehydrated
concept carries `branch_id` of the archived branch, `concepts.branch_id` is
`REFERENCES branches(branch_id)`, and `foreign_keys` is on, so the insert fails. The
error comes out of `classify` for a concept insert, which does not know that a branch is
the missing parent. The message names neither the lineage nor the remedy.

Two acceptable answers. Refuse before writing with a typed error (`BranchArchived {
branch, concept }`) saying the lineage must be reinstated first. Or reinstate the
`branches` row inside the same session, accepting that the branch is then "known" again
with none of its links. The first is smaller and matches D-230's stance that an archived
branch is forgotten. What is not acceptable is the present shape, where the refusal is a
foreign-key error attributed to the wrong table.

### C-10 · `reconstruct` cannot answer for one lineage · Medium

`MaterializedState.edges` is `Vec<EdgeBelief>` carrying `branch` (D-222), and the
rustdoc says resolution is the caller's job. But the crate is the only thing that knows
the resolution rule, nearest lineage under a running-minimum cutoff (D-220, D-223), and
the rule exists only as SQL in `graph/lineage.rs`. There is no function a caller can
apply to a `Vec<EdgeBelief>`. The public fold answers a question no caller can finish.

Add `reconstruct_on(ts, branch)` that resolves before returning, backed by a pure
`resolve(beliefs, ancestry) -> Vec<EdgeBelief>`. The pure function is also a second
oracle for the lineage SQL: a property test can fold, resolve in Rust, and compare with
the resolved read.

### C-11 · Public fields without `#[non_exhaustive]` · Medium (pre-1.0 window)

`Tuning` ([connection.rs:1151](../src/connection.rs:1151)) has four public fields and
`Default`. Every knob W5 added arrived as a field, and the next one (a snapshot retention
policy, a starvation floor, a cold-file path) will too. After 1.0 that is a major version.
`#[non_exhaustive]` on a struct with public fields also forbids
`Tuning { cadence, ..Default::default() }` outside the crate, so the attribute alone
would break every caller. It needs builder methods (`Tuning::default().cadence(..)`)
in the same release, with the fields kept readable. The same applies to
`TraversalBuilder` and `SnapshotCadence`. D-207 did this for the enums and stopped at the
structs.

### C-12 · The shadow swap recreates DDL by substring match · Low

[shadow.rs:307](../src/integrity/shadow.rs:307):

```rust
for stmt in ddl::CREATE_INDICES {
    if stmt.contains("links_current") { ... }
}
for trigger in ddl::CREATE_TRIGGERS {
    if trigger.contains("trg_links_current_sync") || trigger.contains("trg_links_single_open") { ... }
}
```

D-231 replaced this pattern in the migration rungs with `create_indices(&[names])`, which
panics on a name no declaration matches. The swap should use the same registry. The
substring test also has a false positive waiting: any future index on `links` whose text
mentions `links_current` would be recreated against the renamed table.

### C-13 · Cold DDL outside the session transaction · Low

[archive.rs:544](../src/temporal/archive.rs:544) runs `COLD_SCHEMA` on the attached
file before `BEGIN IMMEDIATE`. A session that fails after that leaves a cold file with
schema and no horizon row. Nothing downstream is wrong, since an empty cold log folds to
nothing, but `hot_log_reach` now sees "an archive exists" for a file that has never
received a row. Either move the DDL inside the transaction or treat "no `archive_horizon`
row" as "no archive".

### C-14 · `save_and_prune` and `JoinError` · Low

[snapshot.rs:479](../src/temporal/snapshot.rs:479). D-240 added `SnapshotWriteFailed`
because a snapshot failure said the ledger was damaged. The `spawn_blocking` join arm in
the same file still says `ReplayCorrupt`. A panic in the writer closure is a defect to
chase, but it is not a replay defect; it should carry `SnapshotWriteFailed` with the panic
payload as its reason.

### C-15 · `registered_models` and `LIKE` · Low

[registry.rs:140](../src/vector/registry.rs:140): `name LIKE 'embeddings_%' AND name NOT
LIKE '%_shadow'`. `_` is a single-character wildcard, so `embeddings_ashadow`, a model
legally named `ashadow`, is excluded. Use `GLOB` or filter in Rust after a prefix match.
Better: keep a `models` registry table so the registry is data rather than a
`sqlite_master` pattern.

### C-17 · Two comments, one contradiction · Low

[replay.rs:234](../src/temporal/replay.rs:234): "`AUTOINCREMENT` leaves gaps whenever a
transaction rolls back." [replay.rs:961](../src/temporal/replay.rs:961): "A rolled-back
transaction leaves no gap — `sqlite_sequence` rolls back with it." D-049 and the R13 row
say the second is the measured one. The first should name the real source of gaps, the
archive's scattered deletions.

### C-20 · `abort_kind` on message text · Low

[error.rs:956](../src/error.rs:956). Turning a trigger abort into a typed error keys on
the `RAISE(ABORT, …)` message, which the crate controls, and on libSQL's own prefix,
which it does not. This is pinned only by tests against 0.9.30. Where the extended result
code plus the crate's own message text identify the guard, prefer that and keep the
free-text match as the fallback.

### C-21 · The closed-interval arm is conservative across lineages · Low

`LINKS_ARCHIVABLE`'s second arm (a closed row older than the cutoff) is guarded by
`NOT EXISTS (… other.branch_id <> links.branch_id)`. D-229 explains why: archiving a
shadow retirement un-retires the edge. The predicate is correct, and its cost should be
in its rustdoc: a key any live branch has written stays in the hot file on **every**
lineage until that branch is archived, at which point the trunk's row at the key becomes
archivable at the next session because the `NOT EXISTS` runs against `links`, not `cold`.
Bounded by live branches, and it disappears with them.

### C-24 · `check_lineages` keeps the last answer · Low

[connection.rs:3646](../src/connection.rs:3646):

```rust
for name in names {
    shape = crate::graph::lineage::lineage_shape(conn, Some(name)).await?;
}
```

Every name is validated, but the shape returned is the last iteration's. That is correct,
because the shape depends only on the `branches` row count, and it reads like a bug. One
count and one `IN (…)` membership check would say what it means.

### Documented gaps that are still gaps

Listed so a reader does not rediscover them. `write_final` composes without ever
re-folding from genesis, and nothing schedules `verify_snapshot_chain` (rustdoc, D-092).
Windows directory sync is a no-op (`sync_directory`). `links_current` has no foreign keys
(Doctrine VI). The trunk's `branches.created_at` is a wall-clock stamp from migration
(D-224). Identical concept upserts write a new version and a log row. `load_subgraph`'s
`DISTINCT` is superlinear in frontier width (rustdoc). None is wrong. All are costs a
deployment should know.

---

## 3. Performance

### C-1 · Archive rebuilds the projection in full · **High**

`archive_session` ([archive.rs:539](../src/temporal/archive.rs:539)) calls
`rebuild_within(Verify::No)` whenever `links_deleted > 0`. That is `DELETE FROM
links_current` plus `INSERT … LATEST_BELIEF_PROJECTION` over the whole `links` table,
inside the archive's `BEGIN IMMEDIATE`, on the write connection. D-077 measured 318 ms at
40K rows. `Archive` is on the budget-exemption table so no counter flags it, and the hold
grows with the ledger while the work that justified it, the archived rows, is a small and
shrinking fraction.

The projection is keyed per `(source_id, target_id, edge_type, valid_from, branch_id)`.
The only projection rows a session can invalidate are at keys whose rows it deleted, and
the first arm of `LINKS_ARCHIVABLE` deletes only rows that have a **newer** row at the
same key and lineage, which the projection already prefers. So the affected set is the
second arm's keys, closed rows that were the latest belief. The repair is keyed:

```sql
-- keys collected into a temp table before the DELETE, same transaction
DELETE FROM links_current
 WHERE (source_id, target_id, edge_type, valid_from, branch_id) IN (SELECT * FROM archived_keys);
INSERT INTO links_current
 SELECT … FROM (LATEST_BELIEF_PROJECTION) WHERE key IN (SELECT * FROM archived_keys);
```

O(archived). A test that runs `audit_current` after the session proves it equals the full
rebuild. The shadow rebuild's catch-up step already does keyed repair for `recorded_at >=
build_start`; the same shape applies here.

### C-4 · No composite index on the fold's partition · Medium

[ddl.rs:1052](../src/schema/ddl.rs:1052): `transaction_log` carries `idx_txlog_time
(recorded_at)` and `idx_txlog_entity (entity_id)`. Every fold partitions by
`(table_name, entity_id, branch_id)` ordered by `seq_id`, and `LOG_ARCHIVABLE` is a
correlated `EXISTS` on `entity_id AND branch_id AND seq_id >`. The full fold must scan
and sort the whole log regardless (a snapshot anchor is what bounds it), but the archive
predicate runs its probe once per candidate row, and `idx_txlog_entity` answers
`entity_id` only, leaving `branch_id` and `seq_id` as a filter over every version of the
entity. An index on `(entity_id, branch_id, seq_id)` makes the probe one seek.

Add it in a v16 rung and pin the plan in `tests/index_plan_tests.rs`. Cost: one more
index maintained by every log insert. D-231 measured +12.6% on assertion throughput for
`idx_lc_lineage_cut`; measure this one the same way before deciding.

### C-5 · `hot_log_is_intact` counts the log on every recorded read · Medium

[replay.rs:986](../src/temporal/replay.rs:986): `MIN(seq_id) = 1 AND COUNT(*) =
MAX(seq_id)`. `MIN` and `MAX` on the rowid are O(1). `COUNT(*)` walks the b-tree, and
this runs on every `hydrate_at_time`, every `recorded()` traversal, and every filtered
vector search with a recorded instant. On a log of a few million rows that is tens of
milliseconds per read for a verdict that changes only when an archive session commits.

The verdict is a function of the log, and the log's only writer is the actor. Cache it:
compute once at open, invalidate after `Archive`, `ArchiveBranch` and `Rehydrate`, share
through `ActorShared` as an atomic `{unknown, intact, gapped}`. A read on `read_conn`
sees committed state at least as new as the cache, and the cache only moves from intact
to gapped on the write side, so a stale read is a stale "intact" for one archive commit,
the same window every read already has. Simpler still: `archive_horizon` records every
session, and "no horizon row" is "intact" without a count.

### C-6 · Two extra round trips on the single-edge path · Medium

`AssertEdge` runs `check_lineages` (a `branches` count,
[lineage.rs:136](../src/graph/lineage.rs:136)) and `reject_overlapping_interval` prepares
the guard per call ([connection.rs:4235](../src/connection.rs:4235)), the prepare-per-row
cost §8.8 measured at 10.4 ms per 90 rows on the batch path and removed there.
`INSERT_LINK` is prepared per call too. Against the ~0.8 ms transaction floor (§18) two
prepares and one count are a visible fraction.

The actor owns its connection for the process lifetime, so it can own the statements.
`libsql::Statement` is bound to the connection and reusable with `reset()`. An
`ActorState { insert_link, upsert_concept, guard_trunk, guard_resolved, shape }` built
once after open and refreshed on `Fork` and `ArchiveBranch` removes all three round
trips. The lineage cache is sound because `branches` is written only by the actor.

### C-7 · The trunk pays for the branches · Medium

`lineage_shape` ([lineage.rs:129](../src/graph/lineage.rs:129)) returns `Resolved` for
**any** name once `branches` has two rows, `main` included. The trunk has no ancestors:
its ancestry is one row with no cutoff, its churned set is empty, and `links_cut` reduces
to `links_current WHERE branch_id = 'main'`. D-219 measured the resolved read at 1.1 to
1.3× and D-223 the cutoff read at 1.45× at zero churn. That is what the trunk pays on a
database with one abandoned experiment in it.

A third shape, `TrunkOnForked`, emitting the trunk SQL plus `AND l.branch_id = 'main'`
(covered by `idx_lc_lineage_cut`) is a small change in `edge_filter_sql` and
`link_source`, and `examples/branch_traversal_probe.rs` exists to measure it. D-227's
warning applies: it must land in the shared builder, in `query_as_of_edges_on`, and in
the overlap guard's resolved form together, or the readers disagree again. That is A-1.

### C-8 · `probe_cap` bounds memory, not work · Medium

[vector_filter.rs:377](../src/graph/vector_filter.rs:377):

```rust
let mut ids = self.traversal.execute_ids(conn, now_ts).await?;
if ids.len() > self.probe_cap { ids.truncate(self.probe_cap); }
```

The whole traversal runs, then the tail is dropped. `DEFAULT_PROBE_CAP = 10_000` reads
as a bound on cost and is not one. `TraversalBuilder` should take a `limit` pushed into
the walk CTE's outer `SELECT` (the frontier is already depth-ordered), so a hub-heavy
graph stops expanding when the cap is met. `CostEstimator` then receives a count that
reflects what was paid.

### C-9 · `diagnostic_conn` is one `connect()` per call · Medium

[connection.rs:1589](../src/connection.rs:1589) builds a new `libsql::Database` and
connects per call. D-148 established R15 as a per-`connect()` probability: roughly 1 in
20,000–25,000 against distinct files and 1 in 40,000 reopening one file. A monitoring loop
calling `diagnostic_query` once a second reaches the second figure in eleven hours. The
surface is meant for the moment the typed path is suspect, but nothing stops it being
polled, and the Python binding exposes it behind a mutex around exactly this pattern.

Hold one read-only `libsql::Database` lazily in `Database` and connect from it. D-091's
`SQLITE_OPEN_READ_ONLY` is a builder flag and survives. State the R15 exposure in the
rustdoc either way.

### C-16 · Hybrid rank lookup is quadratic · Low

[hybrid.rs:347](../src/vector/hybrid.rs:347): `position()` over the candidate list per
hit. At `rerank_depth = max(5k, 50)` and `top_k = 1000` that is 25M string comparisons.
A `HashMap<&str, usize>` per list is the fix.

### C-18 · `verify_snapshot_chain` cannot check one link · Low

Two full reconstructions per call ([replay.rs:538](../src/temporal/replay.rs:538)). The
cheap check that catches a composition defect **as it is introduced** is: fold from
snapshot n−1 to n's instant and compare with snapshot n. That is one anchored delta, not a
genesis fold, and it can run inside `write_final` on every save. Keep the genesis check
for the caller's schedule; add the incremental one as the default.

### C-19 · The cadence polls · Low

`run_cadence` in [snapshot.rs](../src/temporal/snapshot.rs) runs `SELECT MAX(seq_id),
MAX(recorded_at)` every 5 s. Indexed and cheap, but it is a query on an idle database for
a fact the actor has. A `tokio::sync::watch<u64>` of the last committed `seq_id`, set by
the actor, lets the cadence sleep until the threshold is crossed and removes the cadence's
own connection from the open-time cost.

### C-22 · `rehydrate` per id · Low

[archive.rs:1225](../src/temporal/archive.rs:1225): per id, a `SELECT` from cold, a
`COUNT(*)` on the rowid, an `INSERT`, a `DELETE`. Chunk by `HYDRATE_CHUNK` with `IN (…)`
for the select and the delete; the rowid reinstatement stays per row because it is
conditional. Rehydrate is rare, so this is about the hold, not the throughput.

### Things that looked like costs and are not

`corpus_size` and `declared_dimension` per filtered search: measured under 1% (rustdoc).
`lineage_shape` per read on `read_conn`: one indexed count on a table of tens of rows.
The delete guards' `sqlite_master` probe per row: deletes only happen inside archive
sessions. The biased `select!` with no floor: D-153 measured it and D-199 kept it.
`json_extract` per column in the folds: the payload is small, and changing it is a
payload version.

---

## 4. Architecture

### A-1 · One read, three spellings

D-227 named the pattern: `query_as_of_edges_on` missed the fork-point cutoff for four
releases because it spells its own SQL, and the repair's tests were written against the
builder. Today the lineage SQL lives in `graph/lineage.rs` and is assembled in three
places (`TraversalBuilder`, `query_as_of_edges_on`, `diff`) plus
`overlap_candidates_resolved` for the write guard. C-7 needs a fourth shape and would
have to land in all of them.

W13 (road map §16) is the answer, and the road map names it as the first thing to cut.
This review argues the opposite: it is the first thing to build, because every remaining
branching cost (C-7, C-10, and the next `query_as_of` defect) is a consequence of not
having it. The shape is small: a `ReadPlan { branch, valid, recorded, limit }` that
`lineage.rs` lowers to `{ctes, link_source, edge_filter, params}`, consumed by the three
readers and the guard. The builder's golden-string tests become the plan's tests.

### A-2 · Resolve ancestry once, in Rust

The ancestry CTE is recursive SQL over `branches`, materialised per query. `branches` is
append-only except under an archive session, has as many rows as there have been forks,
and is written only by the actor. Resolving it in Rust, a cached `Vec<Branch>` with a
generation counter bumped by `Fork` and `ArchiveBranch`, turns the CTE into a bound
`(branch_id, cutoff, dist)` list. The SQL loses `WITH RECURSIVE` and gains a `VALUES`
table. That is a modest win on libSQL (D-219 says the CTE is a constant) and it is the
**only** viable form on Turso, which has no `WITH RECURSIVE`. Building it here first,
differentially tested against the CTE, is Jacquard's Phase 2 argument applied to lineage.

The cache's invariant: a read on `read_conn` may see a `branches` table newer than the
cache, never older. A newer table with an unknown name is the `UnknownBranch` case the
read already handles. A newer table with an extra ancestor is impossible, because
ancestry is fixed at fork time. So a stale cache is safe for reads, and the actor
refreshes it for writes.

### A-3 · The actor is stateless between turns

C-5, C-6 and C-24 are one finding. `run_writer_actor`
([connection.rs:3400](../src/connection.rs:3400)) passes `&conn` to each command and
nothing survives a turn. Introduce `ActorState` owned by the loop: prepared statements,
the lineage shape, the hot-log verdict, the last committed `seq_id` for the cadence. Each
is invalidated by a named command. `ActorShared` already holds the counters; this is the
mutable sibling only the actor touches.

### A-4 · `temporal/archive.rs` is four modules

1,385 lines: the cold schema and its lineage upgrade, three archivability predicates, the
archive session, the branch session, concept archival, rehydration. The predicates are
the correctness-critical part (both D-229 defects were there) and the least tested in
isolation, since the tests drive whole sessions. Split into
`archive/{cold,predicates,session,branch,rehydrate}.rs` with the predicates `pub(crate)`
and property-tested directly: generate a two-lineage history, apply the predicate, assert
the trunk's reach is unchanged. That is the test D-229 says did not exist.

### A-5 · Tests and CI

The testing story is unusually strong: doc-currency gates, index-plan pins,
mutation-found guards, a four-state suite verdict. The findings above sit where the gates
do not look.

- **No lineage property generator.** `doctrine_property_tests` generates single-lineage
  histories. A generator that forks, writes on both sides, archives, and then checks the
  trunk's reach and `audit_current` would have found D-229 and would catch C-1's repair
  if it is wrong.
- **No cost visibility on exempt kinds.** The archive's `rebuild_within` is invisible
  because `Archive` is exempt. A criterion group `archive_session` scaled against `links`
  would show the O(links) term. Not gated, per D-055, but seen.
- **CI triggers** (C-23): `push: branches: [main]` in both workflows. D-234 recorded
  sixteen unreplicated releases. Adding `'dev/**'` costs runner minutes and nothing else.
- **Fuzz coverage** stops at the snapshot container. `timestamp::parse`,
  `escape_fts5_query`, `BranchId::new` and `validate_id` are each a parser of external
  input and each a few lines. Four targets at 30 s each.

### A-6 · The Python boundary

`runtime.rs` is right: one `OnceLock` runtime, GIL released in `block_on`, fork poisoned.
Two observations. `PyDatabase.inner: RwLock<Option<Database>>` means `close()` waits for
every in-flight read, and `std::sync::RwLock` on Windows (SRWLock) does not promise writer
preference, so a hot read loop from another thread can hold `close()` off indefinitely.
The symptom is a hang in `__exit__`; the fix is a closing flag that `with_db` checks
before taking the read lock. And `diagnostic_query` through the binding is C-9's shape,
one process-lifetime handle away from safe.

---

## 5. Recommended order

Ordered by what a deployment hits first, with a rough size. "Rung" means a schema bump.

| # | Finding | Change | Size |
|---|---|---|---|
| 1 | C-1 | Keyed projection repair in `archive_session`; bench group `archive_session` | 1 release |
| 2 | C-2 | `hydrate_at_time` and the reach checks consult `hot_log_reach(ts)` | small |
| 3 | C-5, C-6, C-24, A-3 | `ActorState`: prepared statements, lineage cache, intact verdict | 1 release |
| 4 | C-7, A-1 | `ReadPlan` lowering shared by the three readers and the guard; `TrunkOnForked` | 2 releases; W13 brought forward |
| 5 | C-3 | Typed refusal for a rehydrate that needs an archived lineage | small |
| 6 | C-4 | `idx_txlog_entity_lineage` in a v16 rung, plan pinned, write cost measured | 1 release, rung |
| 7 | C-11 | Builders for `Tuning`, `TraversalBuilder`, `SnapshotCadence`; `#[non_exhaustive]` | before 1.0, breaking |
| 8 | C-8 | `TraversalBuilder::limit` pushed into the CTE | small |
| 9 | C-9 | Lazy read-only handle behind `diagnostic_conn` | small |
| 10 | C-10, A-2 | Rust-side ancestry; `reconstruct_on`; pure `resolve` | 1 release, shared with Jacquard |
| 11 | C-12 … C-22 | Hygiene batch | 1 release |
| 12 | A-4, A-5, A-6 | Archive split; lineage generator; `dev/**` in CI; four fuzz targets; closing flag | ongoing |

Items 1, 3 and 4 change what the crate costs at scale. Item 7 has a deadline.

---

## 6. What this review did not cover

The migration ladder below v12 (audited in the 0.12.0 review and by D-231's rung tests
since). `walk_cte` beyond its lineage arms. The bench harness's control-row methodology.
`docs/libsql-issue.md` and R15's upstream status. The road map's §15.3 option analysis,
taken here as decided. The Python package's distribution beyond what the workflows say.

Nothing here was measured. Every cost claim is a query-shape argument and should be
treated the way D-070 treats an unmeasured figure: a hypothesis with a named experiment,
not a number.
