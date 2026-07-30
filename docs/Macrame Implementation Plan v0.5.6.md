# Macrame — Implementation Plan

| | |
|---|---|
| Plan version | 1.27 |
| Against document | Macrame v0.5.6 — [docs/architecture/](architecture/README.md) |
| Date | 2026-07-30 |
| Crate version | **0.5.6** — `Cargo.toml` had said 0.5.2 for three releases before this cycle corrected it (§8.7); nothing reads `CARGO_PKG_VERSION`, which is why nothing noticed |
| Schema version | **6** — the v5 → v6 rung ships D-059's index (Wave 2.1) |
| Test baseline | **221 passing, 0 failing** on plain `cargo test`; **240 with `--features property-tests`**. Fifty new tests across Waves 1–5. Zero clippy warnings on `--all-targets --all-features` (D-075). **`--no-fail-fast` is not optional**: without it cargo stops at the first failing binary and everything alphabetically behind it never runs, which is how the archive defect sat unnoticed. R15 remains — an intermittent `STATUS_ACCESS_VIOLATION` that took a *different* binary on each of two consecutive full runs during Wave 1, passed in isolation both times, and did not fire at all during Wave 2; it is a libSQL fault under concurrent local opens, not a test failure, and the only honest way to read a red is to re-run the named binary alone. **§8.5 is still the right lens on this green**, but nine of its ten defects are now closed by tests inside it rather than sitting behind it. |
| Status | Phases 0–3 and the native-graph work delivered, and Phase 3 is now reachable from the public API (D-048). Phase 4 is complete: §5.2–§5.9, §6 and Appendix A restored, and the rest of the architecture document de-corrupted. Snapshot composition landed (D-049). **Phase 5 is complete** — Doctrine VIII divergence, archive crash safety, the Doctrine VII property suite, and empirical cost estimates, the last arriving with D-050. **Filtered vector search is implemented** and `TwoPhaseTempTable` is removed as unimplementable on libSQL 0.9.30. **Hybrid search is implemented** (D-051), closing the last capability gap Appendix A.2 recorded. **The archive read path is sound** (D-052) — it had been dropping entities from pre-cutoff reconstructions, and closing it also closed D-049's composition carve-out. **The snapshot cadence is implemented** (D-053), closing D-049's second carve-out and with it both. **Retention gains its daily tier** (D-054), which the cadence had just made load-bearing. **§9 is measured for the first time** (D-055): a criterion harness over twelve rows, eleven inside budget and one — the chunk-commit calibration §5.1.5's whole latency argument rests on — **missing by 20×**. **That fix landed** (D-056): the per-row statement preparation was real and worth 41% (≈62 → ≈37 ms), and isolating the rest showed the two ledger triggers are ~92% of what remains — the same commit without them takes **2.96 ms, which is the ≤ 3 ms budget itself**, so the budget was set without the amplification its own preamble claims. The consequence lands on §5.1.5's golden rule (chunks of ~40 rows, not 500, for a 3 ms bound) and is left open with the numbers rather than settled by editing a constant. **The other three bulk paths now have the same hoist** (D-057), measured rather than assumed: concepts 34.1 → 11.9 ms, annotations 4.60 → 2.13 ms, embeddings 73.4 → 67.4 ms at 500 rows. The spread contradicts D-056's explanation of its own 41% — the untriggered table saved the most-but-one and the DiskANN tables the least, so preparation is a near-constant per row rather than a function of trigger weight. It also produces the control this project lacked (2.13 ms for 500 untriggered upserts, corroborating D-056's 2.96 ms) and identifies the embedding chunk at ≈135 µs per vector as the worst bulk path in the system, which makes the §5.1.5 question sharper there than for edges. **§5.1.5's golden rule is re-derived** (D-058), which was the last thing D-056 left open: it is a bound on *duration*, so `CHUNK_ROWS = 1000` becomes `CHUNK_BUDGET` = 3 ms plus four measured row counts (edges 90, concepts 70, annotations 600, embeddings 30, measuring 2.39/2.35/2.36/2.06 ms against the old constant's 89/24/3.5/143 ms). Three of §5.1.5's numbers were wrong: one constant cannot express one duration across paths whose per-row costs span 60×; per-transaction overhead is ~0.8 ms rather than "noise"; and two of the four paths are superlinear in chunk size, so their old chunks were the worst latency *and* the worst throughput simultaneously — cutting the edge chunk is 3.3× faster in total, not a trade. That also corrects D-056's own "~40 rows" and its "unreachable by construction". **The superlinearity is now diagnosed (D-059), and it corrects D-058 twice.** The sweep measured every chunk size into a fresh database, so chunk size and table size were one variable; separated, a fixed 90-row chunk into a hub of 0/2,000/8,000 edges costs 4.4/18.4/47.7 ms. So D-058's "3.3× faster as eleven chunks" is wrong — end to end it is 85.5 ms as one transaction against 94.7 ms as eleven, ~11% *slower* — and smaller chunks are a genuine latency-for-throughput trade on every path after all. The bound and the four constants stand. The edge path's growth is a **defect**: `trg_links_single_open`'s `EXISTS` is served by `idx_lc_traversal_cover` with only `source_id` bound (it wins as a covering index over the PK autoindex, which lacks `valid_to`), so every insert scans its source's out-degree — 90 rows into a 90,000-edge hub take 1.06 s, and this slows interactive `assert_edge` too, not just chunks. A proven fix exists (`idx_lc_open_interval`, 47.7 → 8.0 ms and flat) and is **not shipped**: it is a D-036 schema change wanting its own migration rung. The embedding path is the same shape and inherent to DiskANN. Open: that migration rung; the chunk constants are empty-database figures and need a realistic fixture, which requires deciding what "realistic" means; the residual `links_current` upsert growth (not the commit, cache, index count, or table size); the `Subgraph` integer-index rewrite; the `write_annotations` rename; and the R15 upstream report. **A full read of the crate against the document on 2026-07-30 (§8.5) changes what comes next.** It found ten defects the suite does not see, six of which return a wrong answer rather than an error: the concept log payload omits `embedding_model` so every temporal read loses it; the fold partitions on `entity_id` alone so a concept can be conflated with an edge and vanish; overlapping *closed* valid-time intervals are unguarded; `AttributeMode::AtTime` disagrees with the other two readers about retirement; and `Subgraph` lets its adjacency reference nodes absent from `nodes`, which makes `louvain` **panic**, `scc` emit phantom components, and `k_core` inflate degrees — four handlings of one violated invariant, none of them chosen. It also found that defect H was marked **Fixed** and is not: `classify_archive_violation` is still called from nowhere, so `DbError::ArchiveViolation` remains unconstructible. §9 is re-sequenced into four waves accordingly, and correctness now precedes the D-059 migration rung rather than following it. **Wave 1 is delivered (2026-07-30):** all six wrong-answer defects that needed no schema change are fixed and closed by thirteen new tests — 184 passing, up from 171. Three decisions were taken along the way rather than deferred: `Subgraph` drops adjacency to invisible nodes rather than admitting flagged tombstones (which left all five algorithms untouched); retirement means *not returned as of the instant asked about*, uniformly; and `classify_archive_violation` was **deleted** rather than wired up, because `error::classify` already did the same job and D-033 requires one classifier, not two. Doctrine VII's static guard had to be narrowed to permit `embedding_model` — it banned the substring `embedding`, which is coarser than the doctrine, since what Doctrine VII excludes is the *vector* (**AH**). The register's own duplicate lettering was found and corrected. **Wave 2 is also delivered (2026-07-30).** All four deferred decisions taken, three of them recorded as **D-060**, **D-061** and **D-062**; `SCHEMA_VERSION` moves to 6 with D-059's index on its own rung, `Cargo.toml` is corrected to 0.5.5, and the suite stands at 202. Two of the plan's own recommendations did not survive contact with the code and are corrected in place rather than quietly re-decided: the overlap guard cannot live in `EdgeAssertion::normalized`, which is a pure function with no connection, and *both* offered identity options were wrong — requiring ULIDs would invalidate every id the crate has ever been used with, and declaring ids fully opaque leaves `transaction_log.entity_id` ambiguous between different links. **Wave 3 is delivered (2026-07-30)** — six new bench groups and three more decisions (**D-063**, **D-064**, **D-065**). It found a defect rather than only confirming numbers, and the defect was introduced by Wave 2: the overlap guard's narrowing predicate made a covering index win over the selective one, so the guard scanned the source's out-degree — **D-059's defect, reproduced by D-060's fix, one wave later, worth +9.8 ms per chunk with every correctness test passing throughout** (**AI**, D-064). It also *retired* two proposed optimisations rather than performing them: D-047's `Subgraph` rewrite, whose deferral condition had gone unanswered for two releases (the load dominates any single analysis at every size measured), and **AF**'s planner cache, whose stated justification — "neither input can change without DDL" — is false of `corpus_size`, which changes on every embedding write, and which measures at under 1% of a filtered search either way. The four `chunk_rows` constants were re-derived against the v6 index and **stand unchanged**. **Wave 4 is delivered (2026-07-30)** — five more decisions (**D-066** … **D-070**), and the cycle's four waves are complete. Two of Wave 4's own instructions were carried out and then reduced on the evidence they produced: the `Drop` `debug_assert` fired on ~30 tests and became a `warn!` (what dropping costs is one snapshot, which Doctrine VI makes disposable — slower, not wrong), and the migration re-anchor had to be gated twice after breaking two contracts the suite already pinned. The half of 4.2 nobody had argued about is the one that mattered: `close()` was discarding the writer's `Result`, so a `Database` whose write actor had panicked closed "successfully". Wave 3's unexplained superlinearity is explained — an O(E log E) `DISTINCT` sort, load-bearing, with two attempted fixes measured and rejected (**D-070**) — and that investigation also established that this cycle's *absolute* timings carry ~29% session noise, so only the within-run ratios should be relied on. **Wave 5 is under way (2026-07-30)** and its first two items both ended by *removing* things. **D-072**: `reconstruct`'s `'D'` branch handled an operation no trigger writes — a claim about the ledger that is not true — and now raises `ReplayCorrupt`; `Delta::edges_gone` went with it, since that branch was its only writer and closing one unreachable path by opening another is not a fix. **D-071**: the FTS5 `VACUUM` hazard was investigated and **nothing was built**, twice. It is not reachable — concepts can never be deleted (D-022) and upserts preserve rowids, so `concepts.rowid` is dense and `VACUUM`'s renumbering is the identity, measured — which means **the delete guard is load-bearing for the search index as well as the ledger**, a fact nothing had recorded and one that Appendix C's deferred concept archival would break. And the `verify_fts()` that would have proved it was not shipped, because FTS5's `'integrity-check'` verifies the index's internal consistency and not its agreement with content: after `'delete-all'` it reports healthy on an empty index. **D-073** closed `load_subgraph`'s missing `edge_types`/`min_weight` filter — a reachability limit, since the byte budget bounds the *unfiltered* neighbourhood — and found two defects older than itself doing it: a one-byte-per-edge drift between the running byte total and `estimated_bytes()`, and `TraversalBuilder`'s `min_weight` default of `0.0` silently disarming the negative-weight guard on the delegating path. **D-074** merged the three "storage permits what the API refuses" statements into a single §4.7 property, which showed they are not the same shape: `assert_edge` accepts a negative weight, so that one is a gap in the *write API* and not only in the schema — a file this crate wrote alone can hold a row it declines to read back. **D-075** renamed `write_annotations` to `write_concepts` — it wrote concepts, not annotations, and had done since D-041 split the two — and cleared the clippy backlog by hand, which is how it found that `OverlappingInterval`'s seven `String`s had made `DbError` 168 bytes: a Wave 2 fix of ours that silently doubled the return type of every fallible function in the crate, boxed here and pinned by a `size_of` test. **Still open:** the R15 upstream report; the periodic full-fold cross-check against composed snapshots; whether `links.weight` should carry a `CHECK`. |

---

## 0. Where the code actually is

Read with §8.5. A row saying **Done** below means the mechanism exists and its tests pass; several such rows still carry a defect that those tests do not reach, and the **Defect** column names it. "Done" has never meant "audited", and the 2026-07-30 review is the first time most of this surface was read end to end against the document.

**Wave 1 closed six of the ten (V, W, Z, AB, AE, AC).** Rows below carry the letter with its state, so a closed defect stays visible where it happened rather than disappearing from the row it was found in.

| Area | State | Notes | Defect |
|---|---|---|---|
| Schema, triggers, guards | **Done** | §4 fully realised, incl. D-008/D-029; concept log payload at v2; `idx_lc_open_interval` on a v6 rung (D-059) | ~~V~~, ~~AA~~ fixed — AA in the write actor, not the trigger (D-060), so raw SQL can still write an overlap and §4.2 says so |
| `util::timestamp` | **Done** | canonical form, parser, formatter | — |
| `util::ids` | **Live** | `validate_id` enforces the two reserved delimiters at both write boundaries (D-061); `generate_id` is offered, not required | ~~AD~~, ~~J~~ fixed |
| `temporal::Interval` | **Live** | `overlaps()` is the overlap guard's decision procedure (D-060) | ~~AG~~ fixed |
| `audit_current` / `rebuild_current` | **Done** | symmetric difference (D-030) | — |
| `archive()` | **Done, ratified** | predicates ratified §1.1 and now lifted into §5.7; its two deletes now classified | ~~AC~~ fixed — duplicate classifier deleted |
| `reconstruct()` | **Done** | ATTACH bracketed and released on all paths, self-healing on the way in (D-044); folds partition on `(table_name, entity_id)` | ~~V~~, ~~W~~ fixed |
| Migration ladder | **Done (Phase 0)** | legacy-free baseline at v2 (D-032); rungs v2 → v3 (D-041), v3 → v4 (D-042), v4 → v5 (D-051), v5 → v6 (D-059) | — |
| External review round | **Done** | 3 accepted (D-042 index, D-043 snapshot header, D-044 self-healing ATTACH), 2 declined with reasons (D-045) | — |
| Write Actor | **Done (Phase 1)** | exhaustive match, no wildcard | — |
| Public write API | **Done (Phase 1)** | assert / retire / upsert / bulk-atomic / bulk-import / annotations / rebuild / archive | see §8.6 — three paths sit outside `CHUNK_BUDGET` |
| Snapshots (write side) | **Done (Phase 2)** | atomic temp+fsync+rename; retention by parsed `seq_id`; versioned container (D-043) | — |
| Embedding tables | **Done (Phase 3)** | `register_model()` creates table + DiskANN index in one tx (D-037) | — |
| Vector search | **Done (Phase 3)** | `vector_top_k` + `vector_distance_cos`; `vector_distance` never existed | — |
| Graph analytics | **Done** | native `Subgraph`; petgraph dropped; five algorithms with brute-force oracles. `Subgraph` now carries a stated closure invariant the algorithms may rely on | ~~Z~~ fixed — none of the five needed changing once the invariant holds |
| Traversal builder | **Done (D-039)** | edge types bound, not interpolated; `attribute_mode` now read | filters `retired` only on the final projection, so paths route *through* retired concepts |
| Attribute hydration | **Done, one semantic** | `Current` / `AtTime` / `Omit` all reachable (D-039); one retirement rule across all three readers; batched | ~~V~~, ~~AB~~, ~~AE~~ fixed |
| Vector write path on `Database` | **Done (D-048)** | `register_model` + `upsert_embeddings` through the actor; Phase 3 reachable from the public API for the first time | — |
| **`VectorFilterStrategy` implementations** | **Done (D-050)** | `FilteredVectorSearch`; two strategies with bodies, `TwoPhaseTempTable` removed as unimplementable on this engine; `byte_budget` read; estimates returned | **AF** — the planner's own input is O(corpus) |
| §5.2–§5.8, §6, Appendix A | **Restored** | recovered from a v0.5.1 copy and forward-ported; Appendix A rewritten against the crate (D-040) | — |
| Architecture document | **De-corrupted** | headings, fences, identifiers and eaten `<…>` spans repaired throughout; §4.3 trigger DDL recovered from `schema::ddl` | — |
| **Hybrid search** | **Done (D-051)** | `concepts_fts` FTS5 external-content index on a v4 → v5 rung; `HybridSearch` builder; `rebuild_fts()` for D-036 | no graph filter; cannot be combined with `FilteredVectorSearch` |
| Snapshot composition | **Done (D-049, D-052, D-053, D-054)** | anchored fold + tombstone merge; composes across the archive boundary; cadence and daily retention landed | — |
| Archive read path | **Done (D-052)** | `hot_log_covers` replaced by a real completeness test; it had been losing entities from pre-cutoff reconstructions | — |
| Subgraph loader | **Done, batched, measured** | per-row byte check made loading O(E²); fixed with incremental accounting (D-047). `hydrate` issues one query per 400 ids: 400 nodes in 0.82 ms. Still linear — a constant-factor win. `load_subgraph` itself is mildly superlinear (12.5× for 10× nodes) and **unexplained** | ~~AE~~ fixed |
| `Subgraph` internals | **Retired (D-063)** | the integer-index rewrite optimises the algorithms, and the load dominates any single analysis at every size measured — 62.2 ms against Louvain's 28.3 ms at 10K nodes. Available with a stated trigger, not an open item | — |
| §9 benchmarks | **Done (D-055, Wave 3)** | twenty rows, plus six groups covering `load_subgraph`, the five algorithms, `archive()`, `FilteredVectorSearch`, the batched hydrate, the overlap guard and the index's write cost | — |

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

> **Update (2026-07-30, §8.6).** The condition has never been met because the benchmark does not exist: `benches/budgets.rs` measures twenty rows and none of them is an algorithm or a subgraph load. It is scheduled as **Wave 3.1**, and defect **AE** suggests what it will find — `hydrate` issues one query per node, 400 nodes measured at 400 round trips, so the dominant cost of getting a graph into memory is round trips rather than the in-memory representation this rewrite would change. Expect Wave 3.1 to retire the rewrite rather than schedule it. Fixing **AE** first (Wave 1.5) is also what makes the measurement meaningful, since otherwise the loader's N+1 buries whatever the algorithms cost.

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

> **Update (2026-07-30).** Still open, now scheduled as **Wave 4.2**, and the review adds two facts to decide it with. `close()` discards the writer's `Result` *and* the shutdown response, so a panicked actor closes "successfully" — whatever is decided about `Drop`, that is a separate one-line fix. And `Shutdown` arrives on the **high-priority** channel, so `close()` preempts every queued low-priority command and their callers see `WriterDroppedResponder`. That is defensible — D-028 already says a queued command is not cancellable — but it is undocumented, and it means `close()` is not "drain then stop" as this entry assumes.

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

> **Update (2026-07-30).** This entry assumed `AtTime` is the faithful mode and `Current` the fast-but-wrong one. Defects **V** and **AB** say otherwise as things stand: `AtTime` loses `embedding_model` entirely, because the log payload never carried it, and it ignores `retired` while both other readers honour it. So today the "historical" mode is the *less* faithful of the two, and the split recommended above would route callers toward it. Waves 1.1 and 1.4 have to land before this recommendation is safe to act on.

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
- **`FakeClock` is constructed in `harness.rs` and never injected.** §5 claims "every test uses `FakeClock`"; no test does. The compiler warns about the dead field on every build, which is how long this has been true. *(Defect K. Scheduled as **Wave 2.4**, with the reason it has stalled written down: `Database::open` hardcodes `SystemClock`, and a fake starting at the epoch trips `trg_concepts_monotonic_ra`, so the fake needs the same `MAX(recorded_at)` floor the real clock already has.)*
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
- ~~`reconstruct` handles `operation == "D"`, but no trigger writes a `'D'` row (Doctrine V). Document as forward-compatible or remove; an unreachable branch in replay logic is a claim about the ledger that is not true.~~ **Fixed 2026-07-30 (Wave 5.1, D-072)** — it now raises `ReplayCorrupt`, naming the seq, the table and the rule. The fix was larger than the branch: `Delta::edges_gone` was populated *only* from that branch, so refusing `'D'` would have left it unreachable — closing one dead path by opening another, in the change whose whole subject is that pattern. It went too. `concepts_gone` stays, because retirement genuinely populates it, and the asymmetry (a concept can vanish from a composed state, an edge cannot) is Doctrine III at the fold rather than an oversight.
- Three pre-existing clippy warnings: an empty line after a doc comment in `ddl.rs`, an unnecessary deref in `archive.rs`, a manual `is_multiple_of` in `embedding.rs`.
- ~~`Cargo.toml` still declares `version = "0.5.2"` while the code, this plan and the register are all at 0.5.5.~~ **Fixed 2026-07-30 — bumped to 0.5.5.** Nothing in the crate reads `CARGO_PKG_VERSION`, which is exactly why it drifted three releases without anything noticing; the snapshot container carries its own format version (D-043) and the schema its own `user_version`, so the package version is documentation and was wrong.

---

## 8.5 The 2026-07-30 review — defects the suite reports green on

The crate was read end to end against the document for the first time. The suite was green before the review and is green after it: **171 passing on plain `cargo test`, 190 with the property features**. Every defect below is inside that green.

> **Wave 1 has since closed six of these — V, W, Z, AB, AE and AC.** The findings are kept as written rather than edited into the past tense, because what they describe is what the code did and because §8.7's point is that a register entry moved to *Fixed* should stay legible enough to check. Each carries a resolution line; §9's Wave 1 table names the closing test for each. The baseline is now **184 passing** on plain `cargo test`.

Findings were verified rather than inferred wherever a probe could settle them. Four of five throwaway probes reproduced; the fifth is recorded as unproven below rather than promoted. Severity follows this document's standing rule — **a wrong answer outranks a crash** — which is why the panic is fifth on this list and not first.

### The six that answer wrongly

**V — every temporal read loses `embedding_model`.** `trg_concepts_log_insert` and `trg_concepts_log_update` build their payload from `title, content, valid_from, valid_to, retired`. `embedding_model` is not among them. Both `replay::fold_delta` and `as_of::hydrate_attributes` read `payload["embedding_model"]` anyway, so both always see `null`. Measured on a concept written with `.embedding_model("nomic_v1")`:

```text
live column          = Some("nomic_v1")
reconstruct(now)     = None
AttributeMode::AtTime = None
```

The consequence is worse than a dropped field. `AtTime` is the mode Doctrine VIII exists to offer, and it returns a *less* faithful record than `Current`, the mode §5.2 documents as wrong for historical text. This is defect O one layer down — a field written by nobody and read by two.

> **Fixed, Wave 1.1.** Payload v2. The interesting part was not the fix but that Doctrine VII's static guard refused it — see the note under Wave 1 and defect **AH**. Databases created before the change keep their v1 triggers, because `CREATE TRIGGER IF NOT EXISTS` does not replace a body and `verify` checks presence by name; they lose nothing they were not already losing, and gain the field on the next rung that moves `user_version`. That is recorded on `CREATE_TRIGGERS` rather than here, where the code is.

**W — the fold partitions on `entity_id` alone.** Every fold in `replay.rs` uses `ROW_NUMBER() OVER (PARTITION BY entity_id …)`, not `PARTITION BY table_name, entity_id`. Link keys are `source|target|type|valid_from`; concept keys are whatever the caller passed, and **nothing validates them** (see **AD**). A concept whose id equals a link's entity key is conflated with that link, and the higher `seq_id` wins. Measured: the concept was **silently absent** from the reconstruction while present in `concepts` and in `transaction_log`. The one-line fix is the partition; the durable fix is **AD**.

> **Fixed, Wave 1.2** — the one-line fix, in all four folds. **AD stays open**, and the distinction matters: the collision is now *harmless*, not *unreachable*. Identifiers are still unvalidated and the CTE cycle check still assumes a fixed width. The `AtTime` query in `as_of.rs` turned out not to need the change at all — it already filtered `table_name = 'concepts'`.

**AA — overlapping closed valid-time intervals are unguarded.** `trg_links_single_open` fires `WHEN NEW.valid_to = '9999-…'`. Two *closed* intervals for one `(source, target, edge_type)` that overlap in valid time are accepted without complaint, and `query_as_of_edges` at an instant inside both returns the relationship twice:

```text
assert [2026-01-01, 2026-06-01)  -> Ok
assert [2026-03-01, 2026-09-01)  -> Ok
query_as_of_edges(2026-04-01)    -> 2 rows, one relationship
```

Every weighted algorithm then double-counts that edge. The property suite's invariant is stated over *open* intervals only, so nothing tests the stronger claim — and `Interval::overlaps`, which is exactly the arithmetic that decides it, is dead code (**AG**).

> **Fixed, Wave 2.2 (D-060), in the write actor rather than in `EdgeAssertion::normalized`** as recommended — `normalized` is pure and has no connection, and doing the read at the API boundary would leave a check-then-write race against the actor's insert. **AG** closes with it: `Interval::overlaps` is the decision procedure, which is why the SQL narrows to a provable superset instead of restating the comparison.
>
> The case the recommendation missed: **two open intervals overlap**, so the general check shadowed `SingleOpenViolation` and three existing tests failed. Left in, that would have made a typed error constructible by nothing — defect Q's shape, reintroduced by a fix. The two guards now partition the space on exactly `trg_links_single_open`'s `WHEN` clause.

**AB — three readers, three retirement semantics.** `hydrate_attributes(Current)` filters `retired = 0`; `fold_delta` treats a retired concept as a tombstone and removes it; `hydrate_attributes(AtTime)` reads the payload and never looks at `retired` at all, so it returns concepts retired long before `ts`. One of the three is right and the document does not say which.

> **Fixed, Wave 1.4.** Two of the three were already right and agreed; `AtTime` was the outlier. The rule is now stated on `hydrate_attributes`: *a concept retired as of the instant asked about is not returned*. Note the clause that keeps this from collapsing into "filter the live column" — `Current` asks whether it is retired **now**, `AtTime` whether it was retired **then**. Those differ, and the difference is Doctrine II rather than a leftover of this defect, which is why there is a second test asserting `AtTime` before a retirement still sees the concept.

**Z — `Subgraph` breaks its own invariant and five algorithms disagree about it.** `hydrate` filters `retired = 0`, but the adjacency lists were built from `links_current`, which still carries edges to retired concepts. So `out_adj` references nodes absent from `nodes`. On a three-node graph with one retired neighbour:

| | behaviour |
|---|---|
| `louvain` | **panics** — `comm[&edge.node]`, "no entry found for key" |
| `scc` | returns the retired node as its own phantom component |
| `k_core` | counts the surviving node's degree as 2 when one edge is in the graph |
| `dijkstra` | returns a distance to a node the caller cannot look up |

Four handlings of one violated invariant, none of them chosen. The panic is the *least* damaging of the four; `k_core`'s inflated degree is the one that will be believed. Retirement is the supported path — concepts are never deleted (D-022) — so this is reachable in ordinary use, not a corner.

> **Fixed, Wave 1.3, by choosing to drop dangling entries** rather than admit retired nodes behind a flag. The deciding argument was the shape of the change, not taste: dropping made the invariant true and **none of the five algorithms needed touching**, while the flag would have required a correct three-state handling in all five and in every algorithm added later. The invariant is stated on the `Subgraph` type rather than in the loader's doc comment, because "the loader happens to produce this" and "callers may rely on this" are different claims and the algorithms rely on the second.

**AC — defect H was marked Fixed and is not.** `classify_archive_violation` and `WriteOp::Delete` are defined and called from nowhere in `src/` or `tests/`, so `DbError::ArchiveViolation` cannot be produced by any code path. The Phase 1 fix made the function *delegate* to `abort_kind` instead of making it *called*, so the defect as stated — "never called" — survived its own repair. Structurally identical to defect Q, which was fixed properly. **H is reopened as AC**, and the lesson is about the register rather than about the archive: a defect line should be closed by the test that would have caught it, not by a commit that touched the file.

> **Fixed, Wave 1.6, by deleting the function.** `error::classify` with `WriteOp::Delete` already produced exactly the same `ArchiveViolation` from exactly the same `abort_kind`, so this was never one classifier missing a caller — it was two classifiers where D-033 requires one, and the redundant one was the one nobody called. `archive()`'s two `DELETE`s now go through the survivor, which makes the typed error reachable and gives it the only site where a guard firing is genuinely diagnostic: the marker table missing means the session's own invariant broke from inside.
>
> Note what the closing test can and cannot do. "This function is called from nowhere" is a property of the call graph, and a test that calls the function cannot see it — which is precisely how the Phase 1 commit closed H without changing anything. The test therefore pins the *behaviour*, and the deletion is what makes the defect unrepeatable.

### Four that are slow or dead rather than wrong

**AD — nothing validates an identifier.** `generate_id` and `validate_id` are exported from `util` and called nowhere. Three modules nevertheless assume ULIDs: **W** above, the `entity_id` concatenation whose safety argument in `edge.rs` rests on `|` never occurring in a component, and the traversal CTE's cycle check, which is `INSTR(w.path, CAST(l.target_id AS BLOB)) = 0` and is only correct because every id is the same fixed width. With variable-length ids a short id that is a substring of the path prunes a live branch. Assumed by three, enforced by none.

> **Fixed, Wave 2.3 (D-061) — and neither of the two options §9 offered was the answer.** Requiring ULIDs is a *different* contract, not a tighter one: every id the suite uses (`a`, `SRC`, `n000`) would become invalid, which is the evidence that the assumption was simply wrong. Declaring ids fully opaque understates its own cost — the width-independent cycle check is the smaller half, and the larger is that `|` inside a component makes `transaction_log.entity_id` ambiguous between *different links*, which W's partition cannot help with because both rows are links. Shipped: ids are opaque except that `|` and `/` are reserved. Every existing id stays valid, the cycle check becomes `INSTR(path, '/' || id || '/')` over a doubly-delimited path, and **W's collision becomes unreachable** rather than merely harmless. **J** closes in the same change — `validate_id` returned `NotFound`, which tells a caller the thing is missing and invites them to create it again with the same id.

**AE — two N+1 loops on the read path.** `subgraph::hydrate` and `as_of::hydrate_attributes` both issue **one query per node**. Measured: `load_subgraph` over 400 nodes and 399 edges takes 13.2 ms, of which 400 are round trips. Linear in node count, so roughly 330 ms at 10 000 nodes, essentially all of it round-trip overhead. Both collapse to one `WHERE id IN (…)` or a join against the walk. Neither is benched (§8.6).

> **Fixed, Wave 1.5 — and deliberately not claimed as a speedup.** Both are batched at 400 ids per statement, which is a bind-variable ceiling rather than a latency budget: `CHUNK_BUDGET` bounds how long the *writer* holds the lock and these are reads. The round-trip count is now `ceil(n/400)` instead of `n`, but **the improvement is unmeasured**, because the benchmark that would measure it is Wave 3.1 and still does not exist. That is the same gap §8.6 records against D-047's deferral, and it is left stated rather than closed by assertion.
>
> One incidental find: batching changed the result order, because the per-node loop had been supplying `node_ids` order for free. Restored explicitly and tested — a read that permuted its own output between runs would have failed the property suite for a reason unrelated to any property under test.
>
> **Measured in Wave 3.1b**, which is what the paragraph above was owed: 100 / 400 / 1,000 nodes hydrate in **0.18 / 0.82 / 2.03 ms**, against 13.2 ms for the whole pre-fix 400-node load. Still linear in node count — the round-trip count fell from *n* to *ceil(n/400)*, but within a chunk the per-row cost is unchanged, so this was a constant-factor win and not an asymptotic one. Worth stating plainly because "N+1" invites the assumption that removing it changes the growth rate, and here it does not.

**AF — the planner's input costs more than the plan.** `FilteredVectorSearch::corpus_size` runs `SELECT COUNT(*)` over the whole model table on **every query** to feed `CostEstimator`. D-007's argument is that strategy choice should be arithmetic rather than a rule of thumb; the arithmetic is currently O(corpus) per query and the thing it selects is not.

**AG — `Interval` is decorative.** `overlaps()` is used by one unit test and no production path. It is the missing half of **AA**.

> **Fixed, Wave 2.2 (D-060)** — see AA above. It is now the guard's decision procedure, and the SQL beside it is deliberately only a narrowing filter so that the definition of "overlap" lives in one place.

### One hazard, recorded as unproven

`run_cadence` is given `read_conn.clone()`, so the cadence and a caller's `reconstruct` can both enter the unsynchronised `ATTACH cold … DETACH cold` region on one connection, and `detach_stale_cold` would tear down a handle another in-flight fold is using. **This did not reproduce**: 200 concurrent reconstructions against a 1 ms / 1-entry cadence with an archive present produced zero errors, because the cadence anchors at `MAX(recorded_at)` and therefore almost always takes the hot path. The window is real — a write landing between `log_head` and the fold — and narrow. Recorded as a hazard to close cheaply in Wave 4, not as a defect.

> **Closed, Wave 4.1 (D-066), by giving the cadence its own connection** rather than by synchronising the region. That removes the shared state instead of ordering access to it: there is nothing left to race on and nothing a future call site has to remember to hold. The R15 objection that produced the sharing — every extra local connection is a cost — does not apply, because R15 is about *concurrent* opens and this is one more sequential open inside `open()`.

### Three more, by inspection only

- ~~**FTS5 external content is keyed on `concepts.rowid`**, which is implicit and not stable across `VACUUM`.~~ **Investigated and closed without a fix, Wave 5.2 (D-071).** The hazard is real in general and **not reachable here**: concepts can never be deleted (D-022, unconditional guard) and upserts preserve rowids, so they are dense `1..n` and `VACUUM`'s renumbering is the identity. Measured. The delete guard is therefore load-bearing for the search index too, which nothing had recorded — and Appendix C's deferred concept archival and GDPR erasure would each break it.
- **Snapshot chains compound.** `write_final` composes onto the previous snapshot, so an error propagates forward indefinitely with no periodic full-fold cross-check. **Still open** — the only item of the three Wave 4 did not close, and the reason it is harder than it looks is that a full fold from genesis is exactly the cost snapshots exist to avoid, so the check has to be scheduled rather than merely written.
- ~~**A `SCHEMA_VERSION` bump invalidates every snapshot on disk**~~ — correct per D-043, but nothing re-anchored after a migration, so the first reconstruct after an upgrade folded from genesis. **Fixed, Wave 4.4 (D-067)**, gated twice: a *fresh* file is not an upgrade, and a handle opened with no cadence still writes nothing before `close()`.

---

## 8.6 Bounds that are stated and not bounded

Three things carry a number in the document and nothing in the code holds them to it.

> **All three items in this section are resolved by Wave 3.** Kept as written; each carries its outcome. Two of the three predictions made here were right, and the third was right about the direction and wrong about the margin.

**`CHUNK_BUDGET` has three exemptions and none is declared as one.** D-058 derives a 3 ms bound with care and four measured constants, and then three operations hold the actor for unbounded time: `write_bulk_atomic` takes an uncapped `Vec` on the **high-priority** channel, so a large batch stalls the tier that exists for interactive work; `archive()` is one transaction that also runs a full `rebuild_within` inside itself; and `rebuild_current` is high-priority and documented at ~50 s per 10M edges. Each is individually justified — D-012 for the archive, D-023 for the rebuild, D-014 for the atomic batch — and the exemption is recorded in three separate rustdoc notes rather than in §5.1.5, which is where a reader looks for the bound's scope.

> **Resolved, Wave 3.3.** All three are atomic *by contract*, so neither of the two active options worked: capping the batch breaks the guarantee `write_bulk_atomic` exists to provide, and a third priority tier changes which caller waits without changing how long the lock is held. The exemption is now stated in §5.1.5 with `archive()` measured at **26.8 ms** for 2,000 archivable edges, and `CHUNK_BUDGET`'s rustdoc carries the same table. The finding was that the exemptions were correct and their *location* was the defect.

**The `Subgraph` rewrite is deferred on a benchmark nobody has written.** D-047 defers the integer-index rewrite until Louvain and Dijkstra are measured on a budget-sized graph. `benches/budgets.rs` measures twenty rows and none of them is an algorithm, a subgraph load, an archive, or a filtered vector search. So the deferral condition cannot be met, and **AE** suggests the measurement would retire the rewrite rather than schedule it: the dominant cost of getting a graph in memory is per-node round trips, which an integer-index representation does not touch.

> **Resolved, Wave 3.1 — the prediction held, with one qualification it missed** (D-063). The load does dominate: 62.2 ms against Louvain's 28.3 ms at 10K nodes, and the margin does not close with size. But the reasoning above credits the round trips, and those were already batched in Wave 1.5 — the load is still 2.2× the most expensive algorithm *after* that fix, so the cause is the walk and row decoding rather than the round trips the prediction named. Right answer, partly wrong reason.
>
> The qualification: the *sum* of all five algorithms approaches the load (≈58 ms against 62 ms at 10K), so a caller who loads once and runs the whole battery is a case where the rewrite would pay. Retired with that stated as its trigger, rather than closed as though no such case existed.

**~~D-059's fix is proven and unshipped.~~ Shipped, Wave 2.1.** `idx_lc_open_interval (source_id, target_id, edge_type, valid_to, valid_from)` on a v5 → v6 rung. It takes a 90-row chunk into an 8 000-edge hub from 47.7 ms to 8.0 ms and flat, and fixes interactive `assert_edge` on a high-degree node at the same time.

The acceptance test asserts the **plan**, not a duration, and that is the more useful assertion here: D-059's diagnosis was causal — the `EXISTS` was served by `idx_lc_traversal_cover` with only `source_id` bound — so what has to be pinned is which index is chosen and how much of it is bound. Verified by dropping the index and re-running: `SEARCH links_current USING COVERING INDEX idx_lc_traversal_cover (source_id=?)` before, all three equality columns bound after. A duration assertion would need a hub large enough to clear the noise and would fail for machine reasons.

**Still not re-measured:** the four `chunk_rows` constants were derived against the pre-index cost, and the edge chunk in particular was sized around a scan this index removes. That belongs to Wave 3 with the rest of the measurement, and is called out here rather than assumed away.

---

## 8.7 Documentation drift found by the review

Recorded here rather than silently corrected, because the pattern matters more than any one line: the docs have drifted in **both** directions, claiming fixes that did not land and defects that did.

| Location | Claim | Reality | Status |
|---|---|---|---|
| README, "Known test gaps" | `tests/concurrency_tests.rs` is `assert!(true)` | six real tests, all passing since §8.1c | **corrected** |
| README, defect **S** | `CostEstimator` selects among strategies "with no implementations" | both strategies have bodies (D-050) | **corrected**; absorbed into this register as S |
| README, defect **T** | hybrid search: "no FTS5 table, no keyword retrieval, nothing fuses them" | delivered (D-051) — and contradicted 38 lines lower in the same file | **corrected**; absorbed as T |
| This plan, defect **H** | `classify_archive_violation` never called — **Fixed** | still never called | **reopened as AC**; closed for real in Wave 1.6 by deleting the function |
| This plan, the register itself | absorbing the README's letters reused **V**, **W**, **T** and **U**, giving four letters two meanings each | found while closing Wave 1 | **corrected** — see the note at the end of the Appendix. Letters are not reused; the highest assigned is **AH** |
| `subgraph.rs`, `node_bytes` doc | names a test, `load_subgraph_totals_agree_with_the_derivation` | the test did not exist | **corrected** — written in Wave 1.5, since batching moved the accounting it describes |
| `Cargo.toml` | `version = "0.5.2"` | code, plan and register are at 0.5.5 | **corrected** — bumped to 0.5.5 |

The README carried its own informal defect letters, S and T, independent of this register. Both are now recorded here as fixed and the README defers to this table, so there is one numbering rather than two.

**The mechanism, since this is the second time.** §8.1b already recorded a mutation surviving in the tree because nothing distinguished it from a commit. Defect H is the same shape at the register level: a line was closed by a commit that touched the file rather than by a test that would have caught the defect. Every Wave 1 item below therefore names its closing test in the table, and a defect line should not move to **Fixed** without one.

---

## 9. Sequencing

Everything the previous plan sequenced is delivered:

```
Phase 4 restoration       — DONE (§5.2–§5.9, §6, Appendix A)
Phase 4b de-corruption    — DONE
Phase 5 (test matrix)     — DONE (all four cells)
5.1 vector write path     — DONE (D-048)
5.2 vector_filter         — DONE (D-050)
5.2b annotations          — DONE (D-041)
5.3 hybrid search         — DONE (D-051)
5.4 snapshot composition  — DONE (D-049, D-052), cadence D-053, retention D-054
7.2 register_model        — DONE (D-048)
8.1c concurrency test     — DONE; the write_annotations rename it blocked is free
§9 measurement            — DONE (D-055 … D-059)
```

**So the old sequence is exhausted, and §8.5 supplies the new one.** Note what changed in priority: before the review the obvious next move was D-059's migration rung, because it is the largest measured win. It is now second. Ten defects that the suite reports green on outrank a performance fix, and four of the six wrong-answer defects are single-file changes that need no schema movement at all — they can land while the migration rung is still being decided.

The four waves below are ordered by that rule. Within a wave the items are independent unless stated.

### Wave 1 — the six silent defects · no schema change · days — **DELIVERED 2026-07-30**

Everything here was a code fix inside one or two files, and every item ends in a test that would have caught it, all twelve in `tests/wave1_regression_tests.rs`. **Nothing in this wave touches `user_version`.**

| # | Defect | Work | Test that closes it | |
|---|---|---|---|---|
| 1.1 | **V** | Added `embedding_model` to both concept log triggers; payload bumped to `'v', 2`; `fold_delta` and `hydrate_attributes` accept v1 (field absent) and v2 against a shared `PAYLOAD_VERSION`. First real use of `DbError::PayloadVersion`, unexercised since 0.5.2. | `embedding_model_survives_every_temporal_read`, `a_v1_concept_payload_still_folds`, `the_trigger_payload_version_matches_the_reader_ceiling` | ✅ |
| 1.2 | **W** | Partitioned all four folds in `replay.rs` on `(table_name, entity_id)`. **The `AtTime` query in `as_of.rs` did not need it** — it already carries `table_name = 'concepts'` in its `WHERE`, so the discriminator was applied by the filter rather than the partition. Noted in the code so the asymmetry does not read as an oversight. | `a_concept_whose_id_looks_like_an_edge_key_survives_reconstruction` | ✅ |
| 1.3 | **Z** | **Chose: drop adjacency entries whose endpoint is not a hydrated node.** Stated as a *closure invariant* on `Subgraph` itself rather than as a step in the loader, with `is_closed()` to check it. The five algorithms needed no changes once it holds — which is the argument for this option over admitting flagged tombstone nodes, since that one would have changed all five. | `a_retired_neighbour_leaves_no_dangling_adjacency`, `retiring_the_start_node_yields_an_empty_graph` | ✅ |
| 1.4 | **AB** | **Chose: a concept retired as of the instant asked about is not returned.** That is what `Current` and `reconstruct` already did; `AtTime` now does too. The rule is per-clock, not per-mode — `Current` asks "retired now", `AtTime` asks "retired then" — which is the two clocks, not a residual disagreement. | `all_three_readers_agree_a_retired_concept_is_not_visible`, `at_time_before_the_retirement_still_sees_the_concept` | ✅ |
| 1.5 | **AE** | Both loops batched to one query per 400 ids. `hydrate_attributes` reorders results to `node_ids` order, because the per-node loop was incidentally providing that and the property suite compares results for equality. | `hydrate_spans_more_than_one_chunk`, `load_subgraph_totals_agree_with_the_derivation` | ✅ |
| 1.6 | **AC** | **Deleted, not wired up.** `classify_archive_violation` duplicated `error::classify` with `WriteOp::Delete`; the defect was one classifier too many. `archive()`'s two deletes now go through the surviving one. | `deleting_a_link_outside_an_archive_session_is_a_typed_violation`, plus `a_legal_archive_is_unaffected_by_the_classified_deletes` as the control | ✅ |
| 1.7 | doc drift | This section, §8.5, §8.7 and the register; README reconciled. The register's own duplicate-lettering drift was found and corrected in the Appendix. | — | ✅ |

**One thing Wave 1 did not anticipate.** `no_payload_carries_a_vector` — the Doctrine VII static guard — refused item 1.1, because its needle was the substring `embedding` and `embedding_model` contains it. The guard was right to fire and wrong in scope: Doctrine VII excludes the *vector*, a derived and reconstructible artifact, and `embedding_model` is a short scalar column of a ledger table naming which model produced one. It is narrowed to permit that identifier **by name**, so `'embedding'` or `'embedding_vector'` in a payload still fails, and a second test pins that the permitted identifier is still a `TEXT` column — because if it ever stops being one, the carve-out silently stops being sound. Recorded as **AH**.

**Verified by reverting.** Nine of the twelve new tests were confirmed to fail against the pre-Wave-1 tree by putting the defects back and re-running. The three that pass either way are named in the test file's header rather than left to look like coverage they are not — the AC test in particular exercises a classifier that was always correct, since "nothing calls it" is a property of the call graph and not observable from a test that calls it.

### Wave 2 — the decisions that have been deferred a full cycle · a week — **DELIVERED 2026-07-30**

Each of these was a fork the previous cycles declined to take unilaterally. All four are taken; three produced decision-register entries (**D-060**, **D-061**, **D-062**). `SCHEMA_VERSION` is now **6**.

**Two of the four recommendations did not survive contact with the code, and the plan was wrong in an instructive way both times.** Recorded here rather than quietly re-decided, because the pattern is the same in both: a recommendation reasoned about the code's *shape* without checking what the shape rests on.

| # | Defect | What was recommended | What shipped |
|---|---|---|---|
| 2.1 | D-059 | Ship the index as v5 → v6 | **As recommended.** `idx_lc_open_interval`, one rung, nothing dropped |
| 2.2 | **AA**, **AG** | Refuse in `EdgeAssertion::normalized` | **Same decision, wrong location** — moved to the write actor (D-060) |
| 2.3 | **AD**, **J** | Require ULIDs, *or* declare ids fully opaque | **Neither** — opaque with two reserved characters (D-061) |
| 2.4 | **K** | `open_with_clock`, floored like `SystemClock::new` | **As recommended**, with the floor lifted into the `Clock` trait (D-062) |

**2.1 — the index.** `idx_lc_open_interval (source_id, target_id, edge_type, valid_to, valid_from)` on a v5 → v6 rung. The acceptance test asserts the *plan* rather than a duration: `EXPLAIN QUERY PLAN` on the probe gave `SEARCH links_current USING COVERING INDEX idx_lc_traversal_cover (source_id=?)` before — one column bound, which is the whole defect — and binds all three equality columns after. A timing assertion would need a hub large enough for the difference to clear the noise and would fail for machine reasons. A second test pins that the trigger still contains the predicate the plan test models, since a trigger body cannot be handed to `EXPLAIN` and the copy could otherwise outlive its original.

**2.2 — `normalized` cannot do it.** It is a pure function with no connection. Doing the read at the API boundary instead would leave a check-then-write race against the actor's insert, so the guard runs *in* the actor — one writer by construction, and inside the batch transaction for the batch paths, so the window does not exist rather than being narrow. The recommendation's cost argument was also wrong: "one read per assertion rather than one per insert on the hot index" treats those as different, and on this path an assertion *is* an insert. What actually differs is the race and the reach.

One thing the plan did not anticipate: **two open intervals overlap**, so the general check reported them and shadowed `SingleOpenViolation` — three existing tests caught it. Left unfixed, that would have made a typed error constructible by nothing, which is defect Q's shape reintroduced by a fix. The two guards now partition the space on exactly the trigger's `WHEN` clause.

**2.3 — both options were wrong, and their being wrong is the finding.** Requiring ULIDs is not a tightening of the contract but a different contract: every id in the suite (`a`, `SRC`, `n000`) would become invalid, so three modules *assumed* ULIDs while nothing ever *required* them. And "fully opaque" understates its own cost — a width-independent cycle check is the smaller half; the larger is that `|` inside a component makes `transaction_log.entity_id` ambiguous between different links, which W's partition fix cannot help because both rows are links. The shipped constraint is the two delimiters and nothing else, which leaves every existing id valid and makes W's collision **unreachable** rather than merely harmless.

**2.4 — the floor belongs to the trait.** `Clock::raise_floor` is required, not defaulted: the trait's contract (strictly increasing across restarts) is not a property a clock can hold alone, since it depends on what the database contains and the clock cannot see that. A defaulted no-op would make silently declining to be floored the easy path, and that is the exact failure this closes. The `field is never read` warning that has been on every test build since 0.5.2 is gone.

### Wave 3 — measure what the bounds claim · a week — **DELIVERED 2026-07-30**

Five new bench groups (`graph_analytics`, `hydrate_scaling`, `overlap_guard`, `archive`, `filtered_vector`, `chunk_index_cost`). Three decisions recorded: **D-063**, **D-064**, **D-065**.

**This wave found a defect rather than only confirming numbers, and it was one of mine.** It also *retired* two proposed optimisations instead of performing them, which is what a measurement wave is for and is easy to forget to count as output.

| # | What it owed a number for | Result |
|---|---|---|
| 3.1 | D-047's deferral condition, unanswered for two releases | **Rewrite retired** (D-063) — the load dominates any single analysis at every size measured |
| 3.1b | The batched hydrate (**AE**) | 400 nodes: **0.82 ms**, against 13.2 ms for the whole pre-fix load. Still linear — the win was constant-factor, not asymptotic |
| 3.1c | The four `chunk_rows` constants against the v6 index | **All four stand.** And the index is a win on an *empty* table too, which D-059 did not claim |
| 3.1d | The overlap guard (**D-060**) | **Found a defect** — the guard reproduced D-059's own trap (D-064) |
| 3.2 | **AF**, the planner's O(corpus) input | **Not fixed, and the plan's justification for fixing it was wrong** (D-065) |
| 3.3 | The three unbounded write paths | Exemption recorded in §5.1.5, where the bound is stated |

**3.1 — D-047 is answered and the rewrite is retired.** Three sizes, load separated from algorithms:

| | 1,001 nodes | 5,001 nodes | 10,001 nodes |
|---|---|---|---|
| `load_subgraph` (3 hops) | 4.97 ms | 27.7 ms | 62.2 ms |
| `louvain` | 1.99 ms | 13.9 ms | 28.3 ms |
| `scc` | 0.90 ms | 7.7 ms | 12.9 ms |
| `k_core` / `dijkstra` / `astar` | 0.71 / 0.41 / 0.11 ms | 6.7 / 2.5 / 0.84 ms | — |

At 10K the most expensive algorithm is 45% of the load, and an integer-index representation does not touch the load. §8.6 predicted this outcome; the qualification it did not anticipate is that the *sum* of all five approaches the load (≈58 ms against 62 ms at 10K), so a caller who loads once and runs the whole battery has a workload where the rewrite would matter. Recorded with that trigger rather than closed outright. The measurement also surfaced something larger than what it retired: `load_subgraph` is mildly superlinear, 12.5× for 10× the nodes, and unexplained.

**3.1d — the guard reproduced D-059's defect, one wave after D-059 was fixed.** `OVERLAP_CANDIDATES` carried `AND valid_from < :new_valid_to`, a provably safe narrowing added for efficiency. It is what let `idx_lc_traversal_cover` win as a covering index while binding **one** equality column, so the guard scanned the source's out-degree:

```text
with the range:     idx_lc_traversal_cover (source_id=? AND valid_from<?)
without it:         idx_lc_open_interval   (source_id=? AND target_id=? AND edge_type=?)
```

**+9.8 ms on a 90-edge chunk into a 2,000-edge hub — and identical with and without `idx_lc_open_interval`**, which is what identified it: an index that makes no difference is one that is not being used. Dropping the range brings the guard's cost to within noise of not running it (18.4 → 8.58 ms, against 8.65 ms with the guard disabled). A second, smaller instance of D-056 — the guard preparing per row inside the batch — was found in the same investigation and was worth ~0.2 ms, an order of magnitude less, which is why it was not the answer.

Note what this says about the suite: **no correctness test could have caught it.** The guard returned the right answer, in the right transaction, with the right error type, throughout. `the_overlap_guard_seeks_on_all_three_equality_columns` now pins the plan.

**3.2 — AF is real in mechanism and under 1% in consequence, and the plan's reason for fixing it was false.** Measured: `corpus_size` costs 5.2 µs at 2,000 vectors and 8.5 µs at 20,000 — 10× the corpus for 1.6× the time, because ~4.9 µs is round trip and preparation rather than counting. Extrapolated to §9's 100K corpus, ~22 µs against a 2.5 ms search. The instruction was to cache both "since neither can change without DDL"; that is true of `declared_dimension` and **false of `corpus_size`**, which changes on every `upsert_embeddings`. The proposed fix would have bought <1% at the price of a staleness bug.

**3.3 — the exemption is recorded, because all three paths are atomic by contract.** `write_bulk_atomic` (D-014), `archive()` (D-012, measured at 26.8 ms for 2,000 archivable edges) and `rebuild_current` (D-023) cannot be chunked without breaking the guarantee each exists to provide. Capping and a third tier were both considered: capping breaks the contract, and a third tier changes which caller waits without changing how long the lock is held. The defect was never the exemption — it was stating the bound as though it had none. §5.1.5 now carries the scope, and `CHUNK_BUDGET`'s rustdoc points at it.

### Wave 4 — hardening — **DELIVERED 2026-07-30**

Five decisions: **D-066** … **D-070**. Two of this section's own instructions were carried out and then *reduced* on the evidence they produced — recorded rather than quietly softened, because in both cases the evidence was a test failure that turned out to be right.

| # | Instruction | Outcome |
|---|---|---|
| 4.1 | Serialise the `ATTACH cold` region, or give the cadence its own connection | **Own connection** (D-066) — removes the shared state rather than ordering access to it |
| 4.2 | `Drop` with a `debug_assert`; propagate `close()`'s writer error | **Half taken.** The error propagates; the assert became a `warn!` (D-066) |
| 4.3 | Decide whether `raw()` stays public | **Stays**, with the convention documented as a convention (D-068) |
| 4.4 | Re-anchor snapshots after a migration | **Done** (D-067), gated twice — see below |
| 4.5 | Five small items | Three fixed, one documented, one recorded as a feature gap (D-069) |
| 4.6 | *(added by Wave 3)* Explain `load_subgraph`'s superlinearity | **Explained, not fixed** (D-070) |

**4.2 — the `debug_assert` fired on ~30 tests, and the right reading was not "30 tests are wrong".** What dropping a `Database` costs is one final snapshot: every public write method awaits its responder, so by the time a caller *can* drop the handle every write has committed; the cadence stops on its own; and a snapshot is derivative under Doctrine VI, so the loss is **slower, not wrong**. Spending a test-run abort on a performance loss — in a project whose own notes say a suite that fails for unrelated reasons trains people to ignore red — is the wrong trade. The half nobody had argued about is the half that mattered: `close()` was discarding the writer's `Result`, so an actor that had panicked closed "successfully". It now returns `DbError::WriterStopped`, *before* writing the final snapshot, because a snapshot taken after a failed writer records a state the caller has no reason to trust.

**4.4 — the first implementation broke two tests that were right.** Re-anchoring on any version change made `open()` write a snapshot on every first open, which contradicts two contracts the suite already pins: an idle database is never anchored, and `open_with_cadence(None)` writes nothing until `close()`. So a fresh file (`from == 0`) is deliberately not an upgrade — it has no snapshots to invalidate — and the re-anchor is additionally gated on the cadence being enabled. It is written at open rather than left to the cadence because the cadence fires on log *growth*: an upgraded database that is read but never written would never re-anchor at all.

**4.6 — explained from the plan, and two fixes were tried and rejected.** `EXPLAIN` reports `USE TEMP B-TREE FOR DISTINCT`, an O(E log E) sort whose n log n term predicts ~13.3× against the 12.5× measured. The `DISTINCT` is load-bearing — two branches can reach one node, so the join multiplies that node's edges. Deduplicating the walk first is **36% slower** (on a hub, `walk` has more rows than the join emits); aligning `ORDER BY` with the full `DISTINCT` key does remove the second temp b-tree, verified in the plan, and measures **85.95 ms against 86.76 ms** — nothing. Reverted rather than kept, since it widens the ordering contract for no gain.

> **A caution about this cycle's absolute numbers.** That A/B pair sat near 86 ms where Wave 3 recorded 62.2 ms for the same code, and two identical runs in one session differed by 29%. The ratios in §8.8 were taken within single runs and stand; the absolute milliseconds carry that much noise and should not be compared across sessions.

### What Wave 4 did not close

- **`load_subgraph` takes neither `edge_types` nor `min_weight`** while `TraversalBuilder` takes both. A feature gap, not a defect — but worth closing for a reason beyond convenience: filtering after the fact cannot help a caller whose *unfiltered* neighbourhood exceeds the byte budget, so `SubgraphTooLarge` refuses loads a filtered walk would have fitted.
- **The periodic full-fold cross-check** against composed snapshot state, listed under 4.4. Snapshot chains compound (§8.5), and nothing yet verifies a composed result against a fold from genesis.
- **The R15 upstream report**, still outstanding and still blocked by nothing.

### Wave 5 — the last silent-wrong-answer paths · **IN PROGRESS**

Sequenced from the post-Wave-4 review. Five items delivered; one remains — the R15 report, which is an issue filed against another project rather than a change to this one.

| # | Item | State |
|---|---|---|
| 5.1 | **AJ** — `reconstruct`'s unreachable `'D'` branch | ✅ **D-072** |
| 5.2 | The FTS5 `VACUUM` hazard | ✅ **D-071** — investigated, nothing built, and that is the result |
| 5.3 | R15 upstream report | open |
| 5.4 | `load_subgraph_with` — the missing `edge_types` / `min_weight` filter | ✅ **D-073** — and it found two defects on the way in |
| 5.5 | Consolidate the three "storage permits what the API refuses" statements into §4 | ✅ **D-074** — and the three are not the same shape |
| 5.6 | `write_annotations` rename; three clippy warnings | ✅ **D-075** — and one of the warnings was ours, from Wave 2 |

**5.1 was larger than the branch.** `Delta::edges_gone` was populated *only* from the `'D'` arm, so refusing `'D'` would have left it reachable by nothing — closing one dead path by opening another, in the change whose whole subject is that pattern. It went too. `concepts_gone` stays because retirement genuinely populates it, and the asymmetry is Doctrine III at the fold: a concept disappears by being retired, an edge never disappears because retiring it asserts a successor under the *same* `entity_id` and last-writer-wins replaces the tuple in place. Two of the four tests are controls proving the refusal did not break what actually removes things.

**5.2 ended with nothing built, twice over, and both are the right outcome.**

*The hazard is not reachable.* `concepts_fts` is external-content on an implicit rowid, and `VACUUM` renumbers implicit rowids — so a vacuum should silently repoint every index entry. It does not, because `concepts` can never be deleted (D-022, unconditional guard) and upserts preserve rowids, leaving them dense `1..n` where `VACUUM`'s renumbering is the identity. Measured: rowids identical across a vacuum, index still matching all 20 rows. **The delete guard is load-bearing for the search index and nothing had recorded it**; Appendix C's deferred concept archival and GDPR erasure would each break it, and the test now says so.

*The check that would have proved it cannot.* `verify_fts()` was to be built on FTS5's `'integrity-check'` — the engine's own operation, per D-036's preference over a hand-rolled equivalent. It verifies the index's internal consistency and **not** its agreement with the content table: after `'delete-all'` the index matches zero rows where it matched ten, and the check still passes. A `verify_fts()` on that footing reports a healthy index for an empty one, which is defect AC's shape, so none shipped. The limitation is pinned as a tripwire that fails if a later engine fixes it.

**5.4 closed a reachability limit and found two defects doing it.** `TraversalBuilder` is reused rather than two parameters bolted on, so there is one filter vocabulary and one validation path (D-039 already made edge types bind parameters for this walk's benefit). The filters apply to **both** the recursive step and the final projection — filtering only the walk would hand a caller who asked for `CITES` a graph reached via `CITES` and populated with `KNOWS` edges too.

The two defects were not in the feature:

- **The byte accounting was one byte per edge wrong.** The loader added `2 * edge_bytes(&edge)`, but `add_edge` stores the `in_adj` copy with `node` rewritten to the *source*, so the running total drifted from `estimated_bytes()` by `target.len() - source.len()` per edge — enough to refuse a graph sized at exactly its own estimate. `load_subgraph_totals_agree_with_the_derivation` could not see it, because it asserts refusal at *half* the budget. It now also requires the graph to fit its own estimate.
- **Delegating with the builder's default disarmed the negative-weight guard.** `TraversalBuilder` defaults `min_weight` to `0.0`, and `0.0` *excludes* negative weights — exactly the input `NegativeEdgeWeight` exists to report (D-039 refuses at the boundary because Dijkstra and A* are unsound over them). The first delegation turned a typed refusal into a graph quietly missing edges. Caught by an existing test; fixed by passing `NEG_INFINITY` from `load_subgraph`, so an edge the caller has not filtered reaches the guard and one they have is theirs to exclude.

A plan-shape test guards the new SQL, on D-064's lesson: adding a predicate to an index-sensitive query can push the planner off its index while every returned row stays correct.

**5.5 was filed as a tidying job and was not one.** Three decisions had each ended with a version of *the storage layer permits what the API refuses* — §4.2 for overlapping closed intervals, D-068 for `raw()`, §5.4 for negative weights. Each correct where it stood; none of them saying how many there are. They are now one property with a table, in a new **§4.7**, with a *Permitted by* column — and that column is what made the finding visible:

**They are not the same shape.** For overlapping intervals and for `raw()`, the write API enforces the rule and the gap opens only for a writer who goes around it, so "a database written only through this API is clean" holds. For negative weights it does not. `links.weight` is a bare `REAL NOT NULL` with no `CHECK`, and `EdgeAssertion::normalized` validates ids, edge type and timestamps but *not* weight — so `assert_edge` commits −1.5 and `load_subgraph` refuses the graph afterwards. **A file this crate wrote by itself, with no other client involved, can hold a row the crate declines to read back.** Three paragraphs in three sections could each be true and leave that unsaid; one table cannot. That asymmetry had been sitting in plain sight since 0.5.4 and no one had had cause to put the three side by side.

The section is written to fail rather than to reassure. `tests/storage_boundary_tests.rs` asserts each gap *from the other side* — raw SQL writes the overlapping pair, `assert_edge` accepts the negative weight, `read_conn()` refuses a write where `raw()` does not — so closing any of the three with a trigger or a `CHECK` fails a test and forces §4.7 to be corrected rather than left describing a limit that no longer exists. Those three passing is not the suite approving of anything; it is the suite recording where the checks are. Left explicitly open, and it is the only one of the three that *could* be closed at the storage layer: whether `links.weight` should carry `CHECK (weight >= 0.0)`, a D-036 change to frozen core wanting its own rung. Adding a check on the write path as a side effect of a documentation pass was rejected for the same reason — that is the fix, and it should be decided by someone rather than by nobody.

Seven cross-references to §4.2 were repointed while in there: they used an anchor (`#42-links-and-links_current`) that has not matched the heading since the section was renamed, so every one of them landed at the top of the file.

**5.6 was two chores and one of them wasn't.** The rename is straightforward and overdue: `write_annotations` took `Vec<ConceptUpsert>` and wrote **concepts**, while `write_analytics_annotations` wrote the annotations — the two were one call until D-041 split them and the name stayed on the wrong half for three releases. It is `write_concepts` now, `LowPriCommand::WriteAnnotationsChunk` → `WriteConceptsChunk` behind it, which finally makes the private `write_concepts_atomic` / `write_annotations_atomic` pair mean what they say. Breaking, pre-1.0, no deprecated alias — an alias keeps the confusing name reachable and documented forever, for callers this crate does not have. Appendix A.2's one open entry is closed with it.

**The clippy backlog was three warnings when it was written down, five kinds by the time it was cleared, and one of them is a finding.** `result_large_err` fired on twenty-four functions because `DbError`'s largest variant is **168 bytes** — and every fallible function in the crate returns `Result<T, DbError>`, so that size rides on the `Ok` path too. The variant is `OverlappingInterval`, seven `String`s, **added by D-060 in Wave 2 of this cycle.** Every variant before it sat comfortably under the 128-byte threshold. A fix we made three waves ago silently doubled the crate's ubiquitous return type and the only signal was a lint class nobody had read.

Boxed — the one boxed variant — with the payload kept whole rather than trimmed, since both intervals are load-bearing and `matches!(err, OverlappingInterval { .. })` is unaffected, which is how all four call sites use it. `size_of::<DbError>()` is now asserted at ≤ 128 by a unit test, because the failure mode is a line in a build log rather than a red test. The other four were ordinary: a `///` module header documenting the macro below it instead of the module, `*ddl` where auto-deref suffices, `% 4 != 0` for `is_multiple_of(4)`, and a `Default` for `TestHarness`. Fixed by hand rather than with `clippy --fix` — which is how the 168 bytes were noticed at all.

> Worth naming: this wave *removed* an unreachable branch, *removed* a set that served it, *declined* to add two mechanisms, added one method that turned up two accounting bugs older than itself, turned a documentation merge into a finding, and found in a lint backlog that one of its own earlier fixes had doubled the size of every `Result` in the crate. Five of the six items produced something nobody had written down, and two of the six were defects this cycle introduced. The output is a smaller crate and a shorter list of things believed without evidence. That counts.

### Independent of all four

The **R15 upstream report** against libSQL 0.9.30 is unblocked by nothing and has been outstanding longest. The `write_annotations` rename has been free since §8.1c and is still not done.

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
| H | `archive.rs` | `classify_archive_violation` never called | **Reopened as AC** — the Phase 1 commit made it *delegate*, not *called* |
| I | `snapshot.rs`, `seed.rs` | no-op stubs presenting as implementations | **Fixed** (Phase 2) |
| J | `util/ids.rs` | `validate_id` returns `NotFound` for a malformed ULID — a caller matching on it is told to create the thing again, with the same id, forever | **Fixed** (Wave 2.3, D-061) — `DbError::InvalidId { id, reason }` |
| K | `tests/harness.rs` | `FakeClock` constructed but never injected | **Fixed** (Wave 2.4, D-062) — `Database::open_with_clock`; the standing build warning is gone |
| L | `search.rs` | called `vector_distance`, which does not exist in libSQL | **Fixed** (D-037) |
| M | `schema/migrations.rs` | `verify()` counted `sqlite_master`; any registered model broke reopen | **Fixed** (D-038) |
| N | `graph/builder.rs` | edge types interpolated as SQL literals on an unvalidated read path | **Fixed** (D-039) |
| O | `graph/builder.rs` | `attribute_mode` stored, exposed, never read; `AtTime` silently returned `Current` | **Fixed** (D-039) |
| P | `graph/algorithms.rs` | `louvain_communities` returned one community per node — not Louvain | **Fixed** (D-039) |
| Q | `error.rs` | `SubgraphTooLarge` constructed nowhere; D-007's byte budget unenforced | **Fixed** (D-039) |
| R | `tests/concurrency_tests.rs` | entire binary is `assert!(true)`; reports green, tests nothing | **Fixed** (rewritten; last red resolved §8.1c) |
| X | `vector/search.rs` | `reciprocal_rank_fusion` sorted on score alone, leaving ties to `HashMap` order — the same query could answer in a different order twice | **Fixed** (D-051) |
| S | `graph/vector_filter.rs` | `CostEstimator` chose between strategies that had no implementations; a candidate-count heuristic named after a byte-budget cost model | **Fixed** (D-050) — absorbed from the README's own lettering |
| T | `vector/search.rs` | `reciprocal_rank_fusion` was a pure function with no FTS5 table, no keyword arm, and nothing feeding it | **Fixed** (D-051) — absorbed from the README's own lettering |
| U | `temporal/archive.rs` | archive-session marker created before `BEGIN` as committed state; every archive died on "table already exists" | **Fixed** (§8.1b) |
| Y | `temporal/replay.rs` | `hot_log_covers` tested reach, not completeness; after an archive, a pre-cutoff `reconstruct` could **drop an entity entirely**, silently | **Fixed** (D-052) |

Found by the 2026-07-30 review (§8.5). All ten were inside a green suite. **Six are closed by Wave 1**, each by the named test in `tests/wave1_regression_tests.rs` rather than by a commit — see §8.7 for why that distinction is now enforced.

| # | Location | Defect | Status |
|---|---|---|---|
| V | `schema/ddl.rs`, `temporal/replay.rs`, `temporal/as_of.rs` | concept log payload omits `embedding_model`; two readers read it, so **every temporal read returns `None`** and `AtTime` is less faithful than `Current` | **Fixed** (Wave 1.1) — payload v2; `embedding_model_survives_every_temporal_read`, `a_v1_concept_payload_still_folds` |
| W | `temporal/replay.rs` | folds partition on `entity_id` alone, not `(table_name, entity_id)`; a concept whose id collides with a link key **vanishes from the reconstruction** | **Fixed** (Wave 1.2) — `a_concept_whose_id_looks_like_an_edge_key_survives_reconstruction` |
| Z | `graph/subgraph.rs`, `graph/algorithms.rs` | adjacency may reference nodes absent from `nodes`; `louvain` **panics**, `scc` emits phantom components, `k_core` inflates degree, `dijkstra` returns unlookupable nodes | **Fixed** (Wave 1.3) — closure invariant on `Subgraph`; `a_retired_neighbour_leaves_no_dangling_adjacency` |
| AA | `schema/ddl.rs`, `connection.rs` | `trg_links_single_open` guards only the open sentinel; **overlapping closed valid-time intervals are accepted** and read back as two edges for one relationship | **Fixed** (Wave 2.2, D-060) — refused in the write actor; `overlapping_closed_intervals_are_refused` |
| AB | `temporal/as_of.rs` | `AttributeMode::AtTime` ignores `retired` while the other two readers do not — three readers, three semantics | **Fixed** (Wave 1.4) — `all_three_readers_agree_a_retired_concept_is_not_visible` |
| AC | `temporal/archive.rs`, `error.rs` | `classify_archive_violation` and `WriteOp::Delete` called from nowhere; `DbError::ArchiveViolation` unconstructible (reopens **H**) | **Fixed** (Wave 1.6) — duplicate classifier deleted, `archive()`'s deletes routed through `classify` |
| AD | `util/ids.rs` | `generate_id` / `validate_id` called nowhere; nothing validates an identifier, while three modules assume ULIDs (root cause of **W**, and of the CTE cycle check's width assumption) | **Fixed** (Wave 2.3, D-061) — ids opaque with `\|` and `/` reserved; W's collision now unreachable, and the CTE cycle check is width-independent |
| AE | `graph/subgraph.rs`, `temporal/as_of.rs` | `hydrate` and `hydrate_attributes` issue one query per node — 400 nodes measured at 400 round trips | **Fixed** (Wave 1.5) — batched to one query per 400 ids; **unmeasured**, see Wave 3.1 |
| AF | `graph/vector_filter.rs` | `corpus_size` runs `COUNT(*)` over the corpus on every filtered query; the planner's input is O(corpus) | **Closed, not fixed** (Wave 3.2, D-065) — measured at <1% of a filtered search, and the proposed cache was unsound for `corpus_size` |
| AI | `connection.rs` | The overlap guard's narrowing predicate made `idx_lc_traversal_cover` win as a covering index, so the guard scanned the source's out-degree — **D-059's defect, reproduced by D-060's fix**. +9.8 ms per 90-edge chunk into a 2,000-edge hub, with every correctness test passing throughout | **Fixed** (Wave 3.1d, D-064) — `the_overlap_guard_seeks_on_all_three_equality_columns` |
| AJ | `temporal/replay.rs` | `fold_delta` handled `operation == 'D'` as a tombstone, and **nothing writes a `'D'` row** — Doctrine V forbids the delete and the archive logs none. A claim about the ledger that is not true. `Delta::edges_gone` existed only to serve it | **Fixed** (Wave 5.1, D-072) — raises `ReplayCorrupt`; `edges_gone` removed with it; four tests, two of them controls proving retirement still removes |
| AG | `temporal/interval.rs` | `Interval::overlaps` is dead code — the crate's only overlap arithmetic, and the missing half of **AA** | **Fixed** (Wave 2.2, D-060) — it is the overlap guard's decision procedure, which is why the SQL narrows to a superset rather than restating the comparison |
| AH | `tests/doctrine_static_tests.rs` | `no_payload_carries_a_vector` banned the substring `embedding` in any trigger, which is coarser than Doctrine VII — it excludes the *vector*, not the scalar naming the model. The guard refused Wave 1.1 | **Fixed** (Wave 1.1) — narrowed to permit `embedding_model` by name, with `the_permitted_exception_is_still_a_scalar_column` pinning the shape the carve-out rests on |
### A correction to this register, found while closing Wave 1

The absorption of the README's informal letters (§8.7) left five rows below the
table above that **reused letters this register had already assigned** — a second
`V`, `W`, `T` and `U` — so the register briefly had two meanings for four letters
and told the reader nothing about which one a cross-reference meant. That is the
same class of failure as defect H, one level up: the register is supposed to be
the thing that does not drift.

The duplicate block is removed. Its content is preserved where it was not already
present, and one row of it was stale on arrival:

- The two extra `D-050` findings — `CostEstimator` carrying `byte_budget` unread,
  and `PostFilter` under-returning silently on a tight filter — are part of **S**,
  which is where D-050's work is recorded. They are not separate defects.
- The old `T` ("no path from `Database` to an embedding") was marked **Open** and
  has been closed since **D-048**; `T` above is the hybrid-search row.
- The old `U` duplicated **U** above verbatim.

**Letters are not reused and not recycled.** A defect that returns keeps its
original letter with a pointer (as **H → AC**), and the next new defect takes the
next unused letter. The highest letter assigned is **AG**.
