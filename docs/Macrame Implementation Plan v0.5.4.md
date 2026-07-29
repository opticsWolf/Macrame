# Macrame — Implementation Plan

| | |
|---|---|
| Plan version | 1.17 |
| Against document | Macrame v0.5.4 — [docs/architecture/](architecture/README.md) |
| Date | 2026-07-28 |
| Test baseline | **190 passing, 0 failing** (`--features property-tests`, `--no-fail-fast`) — green for the first time in several cycles. The last red, `a_high_priority_write_completes_while_the_backlog_is_still_queued`, turned out to be asserting something §5.1.5 does not promise rather than catching a defect; see §8.1c. **`--no-fail-fast` is not optional for reading this number**: without it cargo stops at the first failing binary and everything alphabetically behind it never runs, which is how the archive defect below sat unnoticed. Separately, `doctrine_property_tests` faults under R15 on roughly 15% of runs and takes the suite with it when it does. |
| Status | Phases 0–3 and the native-graph work delivered, and Phase 3 is now reachable from the public API (D-048). Phase 4 is complete: §5.2–§5.9, §6 and Appendix A restored, and the rest of the architecture document de-corrupted. Snapshot composition landed (D-049). **Phase 5 is complete** — Doctrine VIII divergence, archive crash safety, the Doctrine VII property suite, and empirical cost estimates, the last arriving with D-050. **Filtered vector search is implemented** and `TwoPhaseTempTable` is removed as unimplementable on libSQL 0.9.30. **Hybrid search is implemented** (D-051), closing the last capability gap Appendix A.2 recorded. **The archive read path is sound** (D-052) — it had been dropping entities from pre-cutoff reconstructions, and closing it also closed D-049's composition carve-out. **The snapshot cadence is implemented** (D-053), closing D-049's second carve-out and with it both. **Retention gains its daily tier** (D-054), which the cadence had just made load-bearing. **§9 is measured for the first time** (D-055): a criterion harness over twelve rows, eleven inside budget and one — the chunk-commit calibration §5.1.5's whole latency argument rests on — **missing by 20×**. **That fix landed** (D-056): the per-row statement preparation was real and worth 41% (≈62 → ≈37 ms), and isolating the rest showed the two ledger triggers are ~92% of what remains — the same commit without them takes **2.96 ms, which is the ≤ 3 ms budget itself**, so the budget was set without the amplification its own preamble claims. The consequence lands on §5.1.5's golden rule (chunks of ~40 rows, not 500, for a 3 ms bound) and is left open with the numbers rather than settled by editing a constant. Open: that §5.1.5 re-derivation, the same statement hoist in the other three bulk paths, the `Subgraph` integer-index rewrite, the `write_annotations` rename, and the R15 upstream report. |

---

## 0. Where the code actually is

| Area | State | Notes |
|---|---|---|
| Schema, triggers, guards | **Done** | §4 fully realised, incl. D-008/D-029 |
| `util::timestamp` | **Done** | canonical form, parser, formatter |
| `audit_current` / `rebuild_current` | **Done** | symmetric difference (D-030) |
| `archive()` | **Done, ratified** | predicates ratified §1.1 and now lifted into §5.7 |
| `reconstruct()` | **Done (Phase 2)** | ATTACH bracketed and released on all paths, self-healing on the way in (D-044) |
| Migration ladder | **Done (Phase 0)** | legacy-free baseline at v2 (D-032); rungs v2 → v3 (D-041) and v3 → v4 (D-042) |
| External review round | **Done** | 3 accepted (D-042 index, D-043 snapshot header, D-044 self-healing ATTACH), 2 declined with reasons (D-045) |
| Write Actor | **Done (Phase 1)** | exhaustive match, no wildcard |
| Public write API | **Done (Phase 1)** | assert / retire / upsert / bulk-atomic / bulk-import / annotations / rebuild / archive |
| Snapshots (write side) | **Done (Phase 2)** | atomic temp+fsync+rename; retention by parsed `seq_id`; versioned container (D-043) |
| Embedding tables | **Done (Phase 3)** | `register_model()` creates table + DiskANN index in one tx (D-037) |
| Vector search | **Done (Phase 3)** | `vector_top_k` + `vector_distance_cos`; `vector_distance` never existed |
| Graph analytics | **Done (D-039)** | native `Subgraph`; petgraph dropped; five algorithms with brute-force oracles |
| Traversal builder | **Done (D-039)** | edge types bound, not interpolated; `attribute_mode` now read |
| Vector write path on `Database` | **Done (D-048)** | `register_model` + `upsert_embeddings` through the actor; Phase 3 reachable from the public API for the first time |
| **`VectorFilterStrategy` implementations** | **Done (D-050)** | `FilteredVectorSearch`; two strategies with bodies, `TwoPhaseTempTable` removed as unimplementable on this engine; `byte_budget` read; estimates returned |
| §5.2–§5.8, §6, Appendix A | **Restored** | recovered from a v0.5.1 copy and forward-ported; Appendix A rewritten against the crate (D-040) |
| Architecture document | **De-corrupted** | headings, fences, identifiers and eaten `<…>` spans repaired throughout; §4.3 trigger DDL recovered from `schema::ddl` |
| **Hybrid search** | **Done (D-051)** | `concepts_fts` FTS5 external-content index on a v4 → v5 rung; `HybridSearch` builder; `rebuild_fts()` for D-036 |
| Snapshot composition | **Done (D-049, D-052)** | anchored fold + tombstone merge; composes by default, **and now across the archive boundary**. Cadence still open — §5.4 |
| Archive read path | **Done (D-052)** | `hot_log_covers` replaced by a real completeness test; it had been losing entities from pre-cutoff reconstructions |
| Subgraph loader | **Done, now linear** | per-row byte check made loading O(E²); fixed with incremental accounting (D-047) |
| `Subgraph` internals | **Deferred** | integer-index rewrite waits on a benchmark — §5.5 below (D-047) |

---

## 1. Ratification record (closed)

### 1.1 The archive predicates

- **`LINKS_ARCHIVABLE`** — archivable when `recorded_at < cutoff` **and** either superseded by a later assertion for the same interval key, or the current belief for an interval that closed before the cutoff.
- **`LOG_ARCHIVABLE`** — archivable when `recorded_at < cutoff` and a later entry exists for the same `entity_id`. The newest entry per entity always stays hot, so `reconstruct(now)` never touches the cold file.

`archive()` also deletes `links_current` rows for the intervals it archives, so `audit_current()` does not report drift after an archive.

> **RATIFIED as written. CLOSED in plan 1.2** — lifted into §5.7 of the normative document, together with the third table the archive touches (`links_current`, re-derived by `rebuild_within` inside the transaction rather than deleted by a compensating predicate, D-035).

### 1.2 `SingleOpenViolation` field names

> **RATIFIED:** `source_id` / `target_id`. **Outstanding:** amend §7. Still outstanding as of plan 1.1.
>
> Note: `DbError::NegativeEdgeWeight` (D-039) was added using the same field names for the same reason — `source` is claimed by `thiserror` as the error cause and demands `std::error::Error`. The §7 amendment now covers two variants, not one.

### 1.3 Existing-database policy

> **RESOLVED as (a) — legacy-free.** Recorded as **D-032**.

---

## 2–4. Phases 0, 1, 2 — delivered

Migration ladder (D-032), write path, replay/snapshot correctness. See the 0.5.4 version-history row and D-030 through D-036 for detail. Nothing outstanding in these phases except the two prose amendments in §1 above.

---

## 5. Phase 3 (vector) and D-039 (graph) — delivered, with one gap

Both are complete in the library and covered by tests. Four live defects surfaced during Phase 3 and two more during D-039; all are fixed and recorded (D-037, D-038, D-039). One item was deliberately **not** closed:

### 5.1 There was no wired-up path from `Database` to an embedding *(FIXED — D-048)*

`vector::search::upsert_embedding` takes a `&libsql::Connection`. `Database::read_conn()` is `PRAGMA query_only = ON`, and the write connection is owned by the actor and never exposed. **So an application could not store a vector at all through the public API.** The 15 passing vector tests all reached around the actor to a raw connection, which is why this was green and unreachable at the same time.

Closing it meant:

1. A `LowPriCommand::UpsertEmbeddingChunk { model: ModelName, rows: Vec<(String, Vec<f32>)>, responder }` variant. Low priority, not high: embedding is bulk derived work and must not preempt interactive writes (§5.1.5, D-011).
2. A `Database::upsert_embeddings(model, rows)` chunked at `CHUNK_ROWS`, matching `write_annotations`.
3. The dimension resolved **once per chunk**, not once per row — `declared_dimension` is a `PRAGMA table_info` round trip and doing it per row makes a bulk embed O(n) round trips.
4. A decision, below in §7.2, on whether `register_model` should also move behind the actor. It currently executes DDL on a caller-supplied connection.

**Why it was left open rather than guessed:** the chunk shape determines the public API and the failure semantics (per-chunk atomic vs. all-or-nothing, as `bulk_import` vs. `write_bulk_atomic` already distinguish). That is an Appendix A surface decision, and Appendix A had no readable body to check it against.

**Shipped, all four steps.** `LowPriCommand::UpsertEmbeddingChunk` and `Database::upsert_embeddings(model, rows)`, chunked at `CHUNK_ROWS`, atomic per chunk, dimension resolved once per chunk. The chunk shape followed the precedent Appendix A established: `bulk_import` and `write_annotations` are both per-chunk atomic on the low tier, and Doctrine VII makes the trade safer here than for either — a partially written embedding batch is recoverable by re-embedding.

**Step 4 was §7.2, and I took it rather than deferring it.** `register_model` moved behind the actor as `HighPriCommand::RegisterModel`. The deferral no longer had a defensible position: with the write connection actor-owned, "a caller-supplied write connection" is not something an application has, so leaving `register_model` outside would have delivered a write path that was reachable in principle and not in fact. High tier, not low, because every embedding write for a model blocks on it. The DDL exception is bounded — one table plus its index, one transaction, created once by explicit call (D-037 requires both together or dimension enforcement does not exist).

**Five tests.** The first is the regression test and its constraint is what it *uses*: it touches `Database` and nothing else on the write side, so if the only route to a stored vector becomes a caller-built connection again, it stops compiling. That mattered because the pre-existing 15 vector tests all opened their own connection — which is exactly why this shipped broken and green. The rest pin: re-embedding replaces and never reaches `transaction_log`; a bad vector rolls back its whole chunk rather than leaving a prefix; an unregistered model gives typed `ModelNotRegistered`; a backfill larger than one chunk lands completely.

### 5.2 `VectorFilterStrategy` has no implementations *(FIXED — D-050)*

**Shipped.** `FilteredVectorSearch` is the public surface — a builder mirroring `TraversalBuilder` — with both strategies given execution bodies, `CostEstimator` reading the `byte_budget` it used to carry unused, and the estimate logged at `debug` *and* returned as a `CostEstimate` so a test asserts on the plan rather than scraping log output. That last part also closes Phase 5's fourth item: D-007's empirical-tuning requirement is met by a value, not by a log line.

**Measuring the three premises removed a strategy.** All three are now settled, and premise 2 turned out worse than "not known to exist": `vector_top_k` refuses a fourth argument at runtime, and `vectorIndexSearch` in the bundled amalgamation rejects `argc != 3` outright. Together with premise 1 — `CREATE TEMP TABLE` on `read_conn` returning `SQLITE_READONLY (8)`, re-measured rather than taken on trust — `TwoPhaseTempTable` had *neither* of its two mechanisms. It is removed, on D-039's precedent. Premise 3 is answered by a bounded counting probe that doubles as the candidate set, so the traversal is paid for once; `CandidateCount::Exact` vs `::AtLeast` keeps "measured" and "capped" apart in the type.

**The design decision worth carrying forward.** `PostFilter`'s failure is silence — a top-ten returning four rows and reporting success. So a short result from a *saturated* index scan escalates to the exact strategy, and the acceptance gate is that the two strategies agree across filter tightness and k. Strategy is a performance decision and nothing else, which is the only form in which a planner is safe.

**One of the new tests was wrong before it was trusted.** The chunk-merge test ran 60 candidates against a 500-id statement chunk — one chunk, so the merge it claimed to test was never exercised. Worse, sizing the corpus up would still not have caught it: candidate ids arrive in id order and the fixture made distance monotone in id, so the nearest rows land in the first chunk and a concatenating merge returns the right answer anyway. The fixture now reverses the embedding order. Three mutations were then applied together — merge sort dropped, escalation disabled, old candidate-count heuristic restored — and each failed its own test.

### 5.2b Original write-up *(superseded, kept for the inventory)*

`vector_filter.rs` defines `CostEstimator` and three strategy variants; the estimator selects among strategies that do not exist. The estimator's tests are pure-function tests over the cost model, so they pass without any strategy being implemented. This is the same shape as the pre-D-039 Louvain: a named thing that is not the thing.

D-007 additionally requires the cost estimates be **empirical** — measured bytes touched, logged estimated-vs-actual — and nothing currently logs either.

**Sharpened while writing §5.3 of the architecture document (§6).** It is worse than "the strategies have no bodies." `CostEstimator` holds a `byte_budget` field and never reads it; `select_strategy` branches on `candidate_count` against two hard-coded thresholds (500, 5000). So the selector is a candidate-count heuristic carrying the name of a byte-budget cost model — D-007's interface exists and D-007's mechanism does not.

**The 0.4.5 §5.3 supplies the missing mechanism, and three of its premises do not hold.** The cost formulas are now in the architecture document. But: (a) **measured false** — `PRAGMA query_only = ON` rejects `CREATE TEMP TABLE` with `SQLITE_READONLY (8)`, so `TwoPhaseTempTable` cannot run on `read_conn` at all. D-019 is not negotiable, so the strategy needs a CTE/bound-`VALUES` reformulation or a `temp`-only writable read connection, which is a §5.1 connection-topology change. (b) `vector_top_k(index, vec, k)` accepts no candidate allow-list, so the third strategy currently degrades to `PostFilter` with an inflated k′ and the cost table prices an operation the engine does not offer. (c) Selectivity has no source — SQLite has no histograms, and `sqlite_stat1` gives average rows-per-key, which does not estimate multi-hop reachability. Any work here starts with (a) and (c), not with writing strategy bodies.

### 5.2b Analytics write-back overwrote concept content *(FIXED — D-041)*

Surfaced by recovering §5.4 from the 0.4.5 document. `Subgraph::write_back_annotations` built a `ConceptUpsert` per node with the annotation value in `content`, so **a Louvain write-back replaced every annotated concept's document text with a community label** — and, since the write went through the ledger, each rerun versioned every concept again.

Shipped: `analytics_annotations` (§4.5 of the architecture document) with its index and no log trigger; `Annotation`; `Database::write_analytics_annotations`, chunked on the low-priority tier behind `LowPriCommand::WriteAnalyticsChunk`; `write_back_annotations` repointed and given the `label` parameter it never had; and a `v2 → v3` migration rung — the first time the D-032 ladder has had a step beyond the baseline, so the loop in `run()` is now exercised rather than merely present.

Four regression tests in `write_path_tests.rs` pin content-untouched, log-unchanged, rerun-replaces-without-versioning, and the same three through `write_back_annotations`. Nothing is backfilled: a label that landed in `content` cannot be told from the text it replaced, and recomputation is the recovery.

**Two follow-ups this left behind.**

- `Database::write_annotations` still takes `Vec<ConceptUpsert>` and is the bulk *concept* path — an on-ledger write with a name that now means the opposite of what it does. Renaming it (`bulk_upsert_concepts`) is the right call and was deferred pending the `concurrency_tests.rs` rewrite. **That rewrite has landed, so the rename is unblocked** — it is now five test call sites and the method itself, and it is the last entry in Appendix A.2 that is a naming problem rather than a missing capability.
- `verification_counts_every_declared_object` in `migration_tests.rs` asserted `4 tables, 9 triggers, 4 indices` as literals and failed the moment a fifth table arrived — **D-038's exact mistake, in the test that guards D-038.** Rewritten to check tables by name and derive trigger/index counts from `CREATE_TRIGGERS` / `CREATE_INDICES`, and renamed to `the_baseline_leaves_every_declared_object_behind`. Worth a sweep for other count-based assertions on `sqlite_master`.

### 5.4 Snapshots were written and never read *(FIXED — D-049; two carve-outs open)*

**Shipped.** `reconstruct` selects the newest snapshot at or before `ts`, folds the hot log above its anchor (`seq_id > :anchor`), and merges last-writer-wins. `Database::reconstruct(ts)` supplies both paths from the handle, so composition is the default and not something a caller opts into with two extra arguments — which also closes another Appendix A.2 divergence.

**Tombstones were the design problem.** The old fold treated a winning `'D'` row as `continue` and a retired concept as "do not insert". Folding from nothing that is right; composed onto a snapshot it leaves the entity standing. The delta now carries tombstones and the full fold applies them to an empty base, so one code path serves both.

**The acceptance gate found itself wanting.** The property test — composed equals full-fold, over generated histories, at every instant in the delta — **passed under mutation on its first run**, because the generator had no operation that produced a tombstone. `Op::RetireConcept` was added; the mutation then failed and shrank to a two-op history ending in the retirement. Third time this cycle that a test needed mutation to reveal it was asserting nothing.

**A factual correction to D-024.** It attributes `seq_id` gaps to rolled-back transactions. Measured: `INSERT`, `BEGIN…INSERT…ROLLBACK`, `INSERT` yields ids `1, 2` — `sqlite_sequence` is transactional and rolls back too. Gaps are real anyway, from the archive deleting superseded log rows scattered through the sequence. The rule stands, the reason did not, and the gap-tolerance test now builds the state the real mechanism produces (and fails under `seq_id = :anchor + 1`).

**Carve-out 1 — FIXED (D-052), and the "related and unfixed" half was a live defect.** Composition now folds hot and cold together above the anchor, so the reason for the refusal is gone rather than worked around.

The `hot_log_covers` half turned out to be worse than this entry recorded. It was not merely an unsound completeness test — it was **returning wrong answers on shipped code**. `MIN(recorded_at) <= ts` asks how far back the hot log reaches, not whether it is complete; one entity archived beside one entity never superseded separates the two, and the unarchived one keeps `MIN` pointing before the cutoff while the archived one's winning row sits in cold. Reproduced through the public API: a concept **vanished entirely** from a pre-cutoff reconstruction, with no error. Recording it as a caveat rather than a defect is what let it sit — an unsound test named as a limitation reads as a known edge, not as a bug.

The sound test is `ts >= MAX(recorded_at)` of the hot log, which rests on the one guarantee the archive makes: the newest row per entity is never archivable. That preserves the `reconstruct(now)` fast path and nothing else; everything earlier pays an ATTACH. Two candidate widenings were rejected as unsound — `ts > MAX(cold.recorded_at)` and `ts >= cutoff` both fail when an entity's rows straddle the mark with the winner below it.

**Carve-out 2 — FIXED (D-053).** The read-side task §5.5 specifies now exists, and the lifecycle is settled: `Database` owns it, a `watch` channel stops it (a dropped sender counts, so a handle dropped rather than closed does not leave it running), and `close()` stops and joins it before stopping the actor and taking the final snapshot — both it and `write_final` end by running retention, which deletes files.

The trigger is a **distance** in log entries, not a schedule, so an idle database is never anchored; a time-based cadence would rewrite an identical snapshot forever and call it maintenance. It anchors at `MAX(recorded_at)` rather than the clock's `now()`, so the file does not claim an instant later than anything it reflects, and it shares `read_conn` rather than opening a third connection.

**The stop test asserted nothing until mutation said so.** Close the handle, wait, check no new snapshot — that passes whether or not the task is alive, because nothing is writing after `close()`. It now keeps the log growing afterwards through a raw connection (not a second `Database`, which would be exactly the churn R15 punishes) and fails under a leaked stop signal with two snapshots where one was expected.

**What this made newly load-bearing: retention — FIXED immediately after (D-054).** §5.5 specifies "last five plus one daily for thirty days"; the implementation was newest-five-flat. That cost nothing when snapshots were written once per shutdown, and with a cadence it defeated the feature that had just been added: five anchors can span minutes under load, so every older instant folded the whole log.

The date source the filename does not carry is now in the container header (format v1 → v2), so bucketing reads eighteen bytes per file instead of decompressing each one. It is deliberately a second copy of `MaterializedState.timestamp` — the exception this codebase normally refuses — and D-054 records why it earns it and why it cannot drift. "Today" is the newest snapshot's own day, not the wall clock, so retention depends on the directory alone.

**A fixture bug the mutation pass caught, and the third of its kind this cycle.** The retention tests built dates with `format!("2026-01-{:02}", day + 1)`, which yields `2026-01-41` past day 39 — shape-valid, calendar-invalid, correctly refused by `parse`. Those snapshots had *no* instant in their header, so two tests exercised the dateless path while claiming to test the daily one, and one passed for the wrong reason.

### 5.4b Original write-up *(superseded, kept for the inventory)*

Surfaced while declining single-flight coalescing (D-045), whose whole argument was "snapshots are the sanctioned mechanism for this." They are — and they do not run.

`load_snapshot` has no caller in `src/` outside its own module. `reconstruct` folds the hot log, or hot and cold together (D-026), and never consults a snapshot at any `ts`. So `reconstruct` is correct and **unbounded**: it costs what the entire log costs, at every `ts`, forever. The §9 budget of ≤ 200 ms at 1M entries "with snapshot" is unreachable, and the row above it — "snapshot composition expected at this scale" — describes an expectation nothing meets.

Six specific claims depended on it. Each is now marked in the architecture document rather than left standing:

| Claim | Reality |
|---|---|
| §5.5 "`reconstruct(ts)` locates the newest snapshot `S`…" | Never loads one |
| §4.3 "the anchored fold (`seq_id > :anchor`)" | No fold carries an anchor; both filter on `recorded_at` alone |
| §5.5 "three paths, one rule, verified to agree by the property suite" | Two paths exist, and no test compares them |
| §5.5 "written every 10,000 log entries… a lightweight maintenance task watches `seq_id`" | No such task; `close()` → `write_final` is the only writer |
| §5.5 "retention of the last five plus one daily for thirty days" | Newest five, flat |
| §9 "with snapshot ≤ 200 ms" | Unreachable |

**The subtlest of these is the anchored fold.** D-024 guarantees that all replay logic uses inequality comparisons (`seq_id > :anchor`) and never successor arithmetic. That guarantee is currently *vacuous* — there is no anchor to compare against — so the discipline it describes is untested at exactly the moment it starts to matter, which is the first line of the first anchored fold anyone writes. The gap-tolerance test §8 specifies has never had anything to test.

**Closing it:**

1. Anchor selection: newest snapshot with `S.seq_anchor` reachable at-or-before `ts`. Must treat `DbError::SnapshotIncompatible` (D-043) as *discard and fall back to the full fold*, not as an error — that is the variant's entire reason for existing.
2. An anchored fold variant carrying `seq_id > :anchor AND recorded_at <= :ts`. `idx_tx_log_entity` already serves the per-entity partition; §4.3's index rationale was written for this query.
3. Merge under last-writer-wins by `seq_id` — the same rule the `links_current` upsert and the cold fold already apply.
4. A property test asserting `anchored(snapshot, ts) == full_fold(ts)` for arbitrary `ts` over generated histories. This is the "three paths, one rule" claim made executable, and it is the acceptance gate: without it the merge is a second description of state, which is the D-030/D-035 failure class.
5. Plus the `seq_id` gap-tolerance test, which becomes writable for the first time.

**One decision inside it.** The cadence. §5.5 specifies a read-side maintenance task watching `seq_id` through `read_conn`, which keeps snapshotting off the write path — consistent with §5.1.5's latency bound. The alternative is writing an anchor from the actor at a chunk boundary, which cannot miss but taxes the write path. The specified design should be honoured absent a reason not to; flagging it because "a lightweight maintenance task" is a spawn nobody has yet decided the lifecycle of (who owns it, what stops it, what `close()` does with it).

**Why this ranks where it does.** It is not a correctness bug — no wrong answers, no data loss — so it sits below §5.1, which makes a delivered phase unreachable. But it is above the remaining items: it is the only open gap where the document describes a working cache that is write-only, and unbounded reconstruction is the kind of cost that is invisible in tests and obvious in production.

### 5.5 `Subgraph` integer-index rewrite *(deferred pending measurement — D-047)*

A second external review proposed carrying five `petgraph` design choices into `Subgraph`. Outcome: two already implemented (`out_adj`/`in_adj` since D-039; `out_edges` already returns a borrowed slice, which beats the proposed iterator because `degree()` stays O(1)), two real but unmeasured (integer indices, topology/payload separation), one a correct worry about determinism.

**Deferred, with a condition.** Nobody has benchmarked the algorithms. Until this cycle the load path dominated everything (see below), and §9's budgets are not implemented as gates. The trigger for revisiting is a measurement of Louvain and Dijkstra on a budget-sized loaded graph — not the plausibility of the argument.

**The cost the proposal does not price.** Determinism is structural today (`BTreeMap` in, `BTreeMap`/`BTreeSet` out, explicit tie-breaks). Integer indices in insertion order make it procedural — dependent on the loader's `ORDER BY` and `ids.sort()`. Both are correct now, but that substitution is the project's recurring defect, and here it fails silently: a reordered `ORDER BY` gives a different Louvain partition, and §8's oracle is an *upper bound*, so a different-but-valid answer passes. Any implementation must ship with a test loading one graph under two SQL orderings and asserting identical index assignment.

**Blueprint defects to not inherit:** it drops `embedding_model` from `NodeData`; `ulid_to_idx` stores every id twice, partly cancelling the memory win; and `Vec<Vec<EdgeId>>` → `Vec<EdgeData>` does not deliver its own cache argument, since reading a `weight` still pulls three `String`s in. The separation that pays puts weight beside topology.

### 5.6 Byte-budget enforcement was quadratic *(FIXED — D-047)*

Found while reviewing the above. `load_subgraph` called `estimated_bytes()` — O(V + E) — once per row, so loading was O(E²): **500 edges 26 ms, 1,000 76 ms, 2,000 231 ms**. The budget exists to bound what a dense neighbourhood does to a load, and the budget check was the part that did not scale. Untested because the one large-graph test builds its `Subgraph` in memory and never touches the loader.

Fixed with a running total threaded through the load and into `hydrate`, which now also refuses inside its loop rather than after it — checking at the end allocates the whole oversized result before declining to return it. `node_bytes`/`edge_bytes` are the single definition, so the running total and `estimated_bytes()` are the same arithmetic rather than two accounts of it.

Two tests, both mutation-verified. The agreement test sets the budget one byte under the derived total, so an undercount fails. The growth test asserts a ratio, not a duration, at **8× sizes** — deliberately, because at 4× the quadratic term does not dominate and no CI-safe bound catches it. Measured: fixed **8.0×**, mutated **21.3×**, bound **16×**.

### 5.3 Hybrid search does not exist *(FIXED — D-051)*

**Shipped, with the go/defer call taken as go.** `concepts_fts` is an FTS5 external-content index over `concepts(title, content)` on a `v4 → v5` rung, with two sync triggers; `HybridSearch` is the public builder; `Database::rebuild_fts()` satisfies D-036's rebuildability using FTS5's own `'rebuild'` command rather than a reimplementation of the triggers. The rung backfills, which D-041's could not — the index is a pure function of text the ledger already holds.

**Three things worth carrying forward.** External content means the update trigger must *retract* old terms using the OLD values, and omitting that half leaves an index matching words the concept no longer contains — silent, and detectable only by searching for something that should be gone. Arbitrary text is escaped before reaching MATCH, because FTS5's syntax is a language in which a malformed query is an exception and `NOT` is a *wrong answer*. And there is no delete trigger because D-022's guard is unconditional — a dependency that runs the wrong way round to notice later, so it is recorded in §4.6: a change to the archive would break the search index.

**One defect fixed in passing.** `reciprocal_rank_fusion` sorted on score alone, leaving ties in `HashMap` order — and ties are the common case, since two documents at the same pair of ranks score identically by construction. The same query could answer in a different order twice. Now broken by id.

Mutation-verified: dropping the FTS retraction, dropping the `retired` filter, and bypassing the escaping each failed exactly its own test.

### 5.3b Original write-up *(superseded, kept for the inventory)*

Surfaced by the Appendix A rewrite. `reciprocal_rank_fusion` is a pure function over two rank lists. Nothing produces the keyword half, nothing fuses them, and **there is no FTS5 table in the schema** — `grep -ci fts5 src/schema/ddl.rs` returns 0. §9 budgets hybrid search at ≤ 50 ms for top-10 over 100K concepts. It is not reachable.

**The 0.4.5 §5.6 answers the open schema question:** a `concepts_fts` shadow table maintained by trigger, with RRF at k = 60 over it and the vector top-k, both reads served from `read_conn`. That text is now restored as **§5.9** of the architecture document — the vector module had had no section at all since the 0.5.x renumbering moved §5.6 to `as_of` without relocating its contents, which is also why `search.rs`'s doc comments cited a section about attribute hydration. Under D-036 an FTS index over `concepts` is derivative and rebuildable, so it is periphery and the contract permits adding it. The decision left is whether to take the design as specified or defer hybrid search entirely.

---

## 6. Phase 4 — Restore the document *(complete)*

**Done.** A v0.5.1 copy of the architecture document surfaced with the destroyed sections intact, and §5.2–§5.8, §6 and Appendix A are restored from it and forward-ported.

The diagnosis in the previous revision of this plan was partly wrong and is corrected here. The corruption is not "code fences consumed the bodies." It is an HTML-ish sanitisation pass that ate everything between each `<` and the next `>`, stripped `_` from identifiers as markdown emphasis, and removed every `#` heading marker in the file. That is why `recorded_at <= :ts` reads as `recordedat `, why `links_current` reads as `linkscurrent`, and why §5.1.8's `# Latency` rustdoc swallowed the whole of §5.2's opening. **Appendix A was not empty** — it had a body with a `<`-eaten hole through its middle, which is a worse failure than absence because it reads as complete.

Forward-porting applied to the restored text: the canonical timestamp sentinel (D-029), the `main.sqlite_master` archive marker replacing the unimplementable TEMP-table probe (D-008 revised, 0.5.3), the cold-database ATTACH read path (D-026), `rebuild_within` inside the archive transaction (D-035), the symmetric-difference audit (D-030), bound edge types and read `attribute_mode` (D-039), and native analytics in place of the petgraph bridge (D-039).

Two sections v0.5.1 carried only as *"unchanged from 0.4.0"* stubs were written from the implementation instead: **§5.6** (`as_of`, hydration) and **§5.3** — see below.

**§5.3 turned out to be a finding, not a transcription.** Writing it from the code confirmed §5.2 of this plan and sharpened it: `CostEstimator` carries a `byte_budget` field it never reads, and `select_strategy` branches on `candidate_count` against two hard-coded thresholds. So it is not merely that the strategies have no bodies — the *selector* is a candidate-count heuristic wearing the name of D-007's byte-budget cost model. The architecture document now says so in §5.3 rather than describing the design as though it were implemented.

**Appendix A was rewritten against the crate, not transcribed (D-040).** The 0.5.1 text was a pre-implementation sketch and diverges from the code in roughly half its entries. Three entries name operations that exist in no form: `db.set_embedding`, `db.hybrid_search`, and an atomic annotation write. Appendix A is now split — A.1 is the real surface, A.2 tabulates every unkept promise — so the gaps stay visible. Two are live and are the same two this plan already tracks (the vector write path, §5.1; hybrid search, new). A third is small: `macrame::prelude` does not re-export `Subgraph` or any of the five algorithms, so the documented analytics flow does not compile from the prelude alone.

**The rest of the file is now done too (plan 1.9).** Headings and fences restored, mangled identifiers corrected, and the `<`-eaten spans rebuilt. Four of those spans held real content: §4.1's `retired`-vs-`valid_to` note and its embedding DDL; **§4.3's entire trigger set**, which was gone; §8's 0.5.0/0.5.1 test list, where two bullets had merged into one sentence; and the generics throughout §5.1's code sketches (`Option>>`, `oneshot::Sender>`, `-> Result {`).

**Recovering §4.3 from `schema::ddl` rather than from the 0.5.1 prose exposed three divergences**, now recorded in §4.3 rather than reconciled away: the log triggers are named `trg_concepts_log_insert` / `_update` / `trg_links_log_insert`, not the `_i` / `_u` forms the prose uses; **there is no delete-logging trigger on either table**, so no `` 'D' `` row is ever written and D-049's deletion handling is correct but unreachable; and concept payloads do not carry `embedding_model`, which the prose claims they do. Two further corrections landed in the same pass: §9 now says its budgets are not CI gates and nothing measures them, and §5.1.3/§5.1.4's command sketches are marked as 0.4.5 vintage — the wildcard arm they show is the exact defect D-034 removed.

Fence all SQL/Rust blocks so this cannot recur.

---

## 7. Deferred decisions

Each of these is a real fork where I chose not to pick unilaterally. Listed with what I would recommend.

### 7.1 Should `links.weight` carry a `CHECK (weight >= 0)`? *(D-039)*

Dijkstra and A* settle a node permanently on first pop, which is sound only for non-negative weights. `links.weight` is a bare `REAL NOT NULL`. `load_subgraph` now refuses a negative weight with `DbError::NegativeEdgeWeight`, which moves the failure to the boundary — but the ledger can still *contain* one.

- **For:** the constraint makes the guarantee a property of the data, exactly as the D-029 timestamp CHECK does.
- **Against:** it is a schema change against the D-036 freeze on ledger tables. D-036 permits `ADD COLUMN` and new indexes only; adding a CHECK to `links` is neither, so this needs either an explicit carve-out or a major-version slot.
- **Also:** a negative weight is meaningful for some graph semantics (repulsion, dissimilarity). Forbidding it storage-wide to satisfy two of five algorithms may be the wrong trade.

> **Recommendation:** leave the schema alone; keep the load-time refusal. Revisit only if a caller wants shortest paths over a graph they cannot control.

### 7.2 Should `register_model` move behind the write actor? *(RESOLVED — moved, D-048)*

It ran `CREATE TABLE` + `CREATE INDEX` in one `BEGIN IMMEDIATE` on a caller-supplied connection, while everything else that writes went through the actor.

- **For:** "the actor is the only writer" is a §5.1 invariant, and a second writer executing DDL is exactly the case SQLite handles worst. It was also possible to pass `read_conn()` and get an opaque failure.
- **Against:** DDL during migration also runs outside the actor, so the invariant already has one sanctioned exception; adding a command for a once-per-model operation may be ceremony.

> **RESOLVED: moved**, as `HighPriCommand::RegisterModel`. Decided by §5.1 rather than on its own merits — a caller-supplied write connection is not something an application can obtain, so leaving `register_model` outside would have made the vector write path reachable in principle and not in fact. The "ceremony" objection evaporates once the alternative is *no route at all*. High tier because every embedding write for a model blocks on it. Recorded in D-048.

### 7.3 `Database::close()` — mandatory, or implement `Drop`? *(carried from the previous cycle)*

`Database` has no `Drop` impl, so dropping one detaches the write actor's `JoinHandle` without draining it. `close()` drains and writes the final snapshot. A caller who forgets loses the final snapshot silently.

- **Option A:** implement `Drop` to abort the actor and log at `warn!`. Cannot await, so cannot drain — `Drop` is not async.
- **Option B:** document `close()` as mandatory and add a `debug_assert` in `Drop` that fires if `close()` was not called.
- **Option C:** both.

> **Recommendation:** B, moving to C if a real caller trips it. A alone is the worst of the three: it looks like cleanup and isn't.

### 7.4 Louvain phase two *(D-039)*

The implementation is the local-moving phase only; it does not aggregate communities into super-nodes and recurse. Documented as such on the function.

- The aggregation phase finds coarser structure and matters on large graphs.
- The byte budget (D-007) bounds subgraphs well below the size where it pays.

> **Recommendation:** leave it. Revisit if the budget is ever raised substantially. The oracle test already bounds the result against the true optimum on small graphs, so a future phase two has a correctness harness waiting.

### 7.5 `AttributeMode::Current` on a historical query

`hydrate_attributes` emits a `tracing::warn!` when `Current` is requested for an `as_of` query, then returns live attributes. §5.2 documents this as "fast, WRONG for historical text."

- A warning is not a boundary. A caller who does not read logs gets wrong text with no signal.
- But `Current` is also the sensible default for present-time traversal, which is the common case, and it is the builder's default.

> **Recommendation:** keep the default, but consider splitting the *call* — `execute()` (present) versus `execute_as_of(ts)` (historical, rejecting `Current`) — so the mode cannot be wrong for the call being made. Not urgent; the warning is at least present.

---

## 8. Open items

### 8.1 Owed externally

- **Upstream libSQL report for R15**, against 0.9.30, with the raw open/migrate/drop reproduction. Long outstanding. The two mitigations (`RUST_TEST_THREADS = "1"`, the `property-tests` gate) are workarounds in our tree and do nothing for anyone else.

### 8.1b A mutation was left in the tree *(FIXED — defect U)*

`archive_session` created the `macrame_archive_session` marker **before** `BEGIN`, as committed state, and then created it again inside the transaction — so every archive died on `table macrame_archive_session already exists`. It is the mutation that verifies the D-012 guard test, left behind by the session that ran it.

It broke six binaries' worth of assertions (`temporal_tests`, `integrity_property_tests`, `replay_snapshot_tests`, `write_path_tests`), and none of that was visible, because `cargo test` without `--no-fail-fast` stops at `concurrency_tests` — the one binary the plan already records as failing — and never reaches them. **A known-failing binary early in the alphabet hides every binary after it.** Either fix `concurrency_tests` or make `--no-fail-fast` the documented invocation; the plan's test baseline now says so.

Two process notes, since the same shape has now occurred twice. Mutation testing is the practice this project relies on most and the only one with a destructive step, and nothing distinguishes "mutation in flight" from a commit. A `MUTATION` marker is a comment — `grep -rn MUTATION src/` is what found this one, days late. Worth considering a `#[cfg(test)]`-gated form, or simply a pre-commit grep.

### 8.1c The last failing test was wrong, not the code *(FIXED)*

`a_high_priority_write_completes_while_the_backlog_is_still_queued` failed deterministically — every run, 40 backlog chunks of 40 committed before the probe, which is far too many to explain as one chunk already in flight. It was nonetheless the test at fault, in two layers:

- It `.await`ed the probe directly. That yields, so the probe's command had not reached the channel when the actor woke; the actor saw only low-priority work and drained the lot. The probe must be *enqueued* before the actor runs, which is what `poll_once_each` exists for and what its passing sibling already did. The helper's own doc comment explains the mechanism — it simply was not applied here.
- Even enqueued, `COUNT(BACKLOG) == 0` after the probe resolves is a wall-clock race: the actor keeps draining while the assertion's own `SELECT` awaits. §8 says this invariant is "stated as an ordering property over committed `seq_id`s, not a wall-clock timing measurement, so it is deterministic" — a count is a timing measurement wearing an ordering's clothes.

Restated as ordering and renamed `a_lone_high_priority_write_is_still_serviced_before_a_saturated_backlog`. Its distinct contribution from the sibling is the shape: one probe against a saturated low queue, the worst case for a biased select. **Preempting already-accepted work is not something two-tier channels can do** — a queued command cannot be retracted — and §5.1.5's guarantee is about what the actor picks up next, which is what the test now says.

### 8.2 Coverage gaps where green means nothing

- ~~**`tests/concurrency_tests.rs` is one `assert!(true)`.**~~ **Closed** — the binary now holds six real tests, the last of which is fixed in §8.1c above. The `write_annotations` rename it was blocking is therefore unblocked. Original note: The binary reports `ok` in every run while testing nothing; clippy flags the assertion as always true. §5.1's priority guarantee, §9's WAL-reader claim, and the per-chunk atomicity of `bulk_import` are all uncovered. *(Spun off as a separate task.)*
- **`FakeClock` is constructed in `harness.rs` and never injected.** §5 claims "every test uses `FakeClock`"; no test does. The compiler warns about the dead field on every build, which is how long this has been true.
- **`RecordedAtRegression` is mapped by the classifier but unreachable through the public API.** `SystemClock` is strictly increasing by contract, so no test can provoke the trigger without raw SQL. Good news about the clock, real gap in coverage.
- **`seq_id` gap tolerance (D-024)** — §8 names this test and it does not exist. **And it currently cannot exist**, which is worse than it being unwritten: no fold in the crate carries a `seq_id > :anchor` term, so there is nothing for gap tolerance to be a property *of*. D-024's guarantee is vacuous rather than satisfied. This test becomes writable when §5.4 lands, and it should land with it — the first anchored fold is precisely the code the rule binds, and writing the rule's test afterwards is how the `audit_current` defect happened.

### 8.3 Phase 5 — the test matrix §8 specifies *(three of four delivered)*

- **Doctrine VIII** — **DONE.** `as_of(t)` vs `reconstruct(t)` after a retroactive correction, asserting they *should* differ, in both directions: belief withdrawn (a retroactive retirement) and belief added (a retroactive assertion). Two tests rather than one because an implementation can lose one direction and keep the other, and every *agreement* test in the suite passes against such an implementation.
- **Crash safety** — **DONE.** A failed archive session, failed at both points where failure is dangerous: after the marker exists (guards disarmed) and before the commit. Hot tables unchanged, marker gone, an outside `DELETE` still aborting, and a later session still succeeding — the last of these being the `DETACH`-on-error path, which fails days away from its cause.
- **Doctrine VII property test** — **DONE.** Two properties in `doctrine_property_tests.rs`, driven through `Database` alone (D-048), over histories interleaving ledger operations with embedding writes for two models of *different* widths:
  - `an_embedding_write_never_reaches_the_ledger` — after every embedding write, `transaction_log` and `concepts` are byte-identical; a vector of the wrong width for its model is refused with typed `DimMismatch` and stores nothing; and at the end, every stored vector is the declared width and no `embeddings_*` table carries a trigger.
  - `striking_the_embeddings_out_of_a_history_leaves_the_ledger_identical` — the same history run twice, once with the embedding steps and once without, must produce an identical stamp-free ledger in `seq_id` order and an identical `reconstruct`. This needs two databases; "the derivative is not an input to the ledger" is not a claim one database can be asked.

  **Mutation-verified, and the first attempt was not a valid probe.** Planting an `UPDATE concepts SET embedding_model` inside the embedding chunk made the property fail — via the `recorded_at` monotonicity trigger refusing the write, which tests the schema rather than the assertion. Restamped one microsecond ahead so it actually lands, both new properties fail and **all six pre-existing doctrine properties still pass**, which is the hole they exist to close. Disabling the Rust-side width check separately: the engine still refuses (`dimensions are different: 2 != 4`, confirming D-037's note that the DiskANN index is the storage-layer enforcement), so what the property pins is the *typed* refusal, not merely that the row fails to land.

  **Cost, measured.** These are the most database-expensive cases in the crate, and R15 makes database churn the scarce resource. Over 12 runs of the binary at 12 cases: six properties 2/12 faulted, eight 4/12. Reduced to 8 cases and re-measured over 20 runs each: 3/20 and 5/20. The gap narrowed and did not close, and at n = 20 two runs is not a result — what it establishes is a 15% *baseline*, i.e. R15 in both arms.
- **Empirical cost estimates (D-007)** — **DONE, by way of §5.2 (D-050).** It had been blocked on there being no estimates to log. `FilteredVectorSearch` logs the plan at `debug` and returns it as a `CostEstimate`, which is the stronger form: a test asserts on the value rather than scraping `tracing` output, and `the_planner_follows_the_arithmetic_not_a_threshold` does exactly that. **Phase 5 is complete.**

### 8.4 Small and unscheduled

- `validate_id` returns `NotFound` for a malformed ULID — wrong semantics (defect J, still open).
- `reconstruct` handles `operation == "D"`, but no trigger writes a `'D'` row (Doctrine V). Document as forward-compatible or remove; an unreachable branch in replay logic is a claim about the ledger that is not true.
- Three pre-existing clippy warnings: an empty line after a doc comment in `ddl.rs`, an unnecessary deref in `archive.rs`, a manual `is_multiple_of` in `embedding.rs`.

---

## 9. Sequencing

```
Phase 4 restoration      — DONE (§5.2–§5.9, §6, Appendix A)
        │
        └── unblocks ──> §5.1 vector write path ──> 7.2 register_model

5.1 vector write path      — DONE (D-048)
5.2b annotations (D-041)  — DONE
5.4 snapshot composition  — DONE (D-049)
                           carries one decision (cadence) and unblocks the
                           seq_id gap-tolerance test Phase 5 wants
Phase 4b de-corruption   — DONE
Phase 5 (test matrix)    — mostly independent; §5.4 owes it one test
8.2 coverage gaps        — independent; concurrency_tests spun off
5.2 vector_filter        — DONE (D-050)
5.3 hybrid search        — DONE (D-051)
8.1c concurrency test    — DONE; suite green, and the rename it blocked is free
```

**Recommended order:** §5.1, 7.2, §5.4 and the de-corruption pass are done. Next is **Phase 5** — the test matrix §8 specifies, now that §8 itself is legible and its 0.5.4 additions are recorded. The two carve-outs §5.4 leaves — composing across the archive boundary, and the snapshot cadence — are both decisions rather than transcription, and belong with whoever settles the maintenance-task lifecycle. §5.2's strategy work waits on the two premises above; §5.3 waits on a go/defer call now that its design is no longer missing.

The R15 upstream report is independent of everything and has been outstanding longest.

---

## Appendix — Defect register

Severity is about silence: a defect that returns a wrong answer without erroring outranks one that crashes.

| # | Location | Defect | Status |
|---|---|---|---|
| A | `connection.rs` | commands dropped the responder; caller saw `RecvError` | **Fixed** (Phase 1) |
| B | `replay.rs` | DETACH skipped on error paths; poisoned the connection | **Fixed** (Phase 2) |
| C | migrations, `embeddings_*` | vector search targeted tables no migration created | **Fixed** (D-037) |
| D | `snapshot.rs` | non-atomic write; a torn newest snapshot is the one that loads | **Fixed** (Phase 2) |
| E | `snapshot.rs` | `{:08}` + lexicographic retention breaks past 1e8 | **Fixed** (Phase 2) |
| F | `search.rs` | dimension check compared a length to itself | **Fixed** (D-037) |
| G | `replay.rs` | path interpolated into SQL rather than bound | **Fixed** (Phase 2) |
| H | `archive.rs` | `classify_archive_violation` never called | **Fixed** (Phase 1) |
| I | `snapshot.rs`, `seed.rs` | no-op stubs presenting as implementations | **Fixed** (Phase 2) |
| J | `util/ids.rs` | `validate_id` returns `NotFound` for a malformed ULID | **Open** |
| K | `tests/harness.rs` | `FakeClock` constructed but never injected | **Open** |
| L | `search.rs` | called `vector_distance`, which does not exist in libSQL | **Fixed** (D-037) |
| M | `schema/migrations.rs` | `verify()` counted `sqlite_master`; any registered model broke reopen | **Fixed** (D-038) |
| N | `graph/builder.rs` | edge types interpolated as SQL literals on an unvalidated read path | **Fixed** (D-039) |
| O | `graph/builder.rs` | `attribute_mode` stored, exposed, never read; `AtTime` silently returned `Current` | **Fixed** (D-039) |
| P | `graph/algorithms.rs` | `louvain_communities` returned one community per node — not Louvain | **Fixed** (D-039) |
| Q | `error.rs` | `SubgraphTooLarge` constructed nowhere; D-007's byte budget unenforced | **Fixed** (D-039) |
| R | `tests/concurrency_tests.rs` | entire binary is `assert!(true)`; reports green, tests nothing | **Fixed** (rewritten; last red resolved §8.1c) |
| X | `vector/search.rs` | `reciprocal_rank_fusion` sorted on score alone, leaving ties to `HashMap` order — the same query could answer in a different order twice | **Fixed** (D-051) |
| Y | `temporal/replay.rs` | `hot_log_covers` tested reach, not completeness; after an archive, a pre-cutoff `reconstruct` could **drop an entity entirely**, silently | **Fixed** (D-052) |
| S | `vector/vector_filter.rs` | `CostEstimator` selects among strategies with no implementations | **Fixed** (D-050) |
| V | `vector/vector_filter.rs` | `CostEstimator` carried `byte_budget` unread; `select_strategy` was a candidate-count heuristic wearing a cost model's name | **Fixed** (D-050) |
| W | `graph/vector_filter.rs` | `PostFilter` under-returns silently when the filter is tight — a wrong answer shaped like a small result | **Fixed** (D-050, escalation) |
| T | `vector/search.rs` | no path from `Database` to an embedding; feature unreachable from the public API | **Open** |
| U | `temporal/archive.rs` | an un-reverted mutation left in the tree: the archive-session marker created *before* `BEGIN`, as committed state, and then created again inside the transaction | **Fixed** (Phase 5) |
