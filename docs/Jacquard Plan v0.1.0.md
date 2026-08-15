# Jacquard Plan — v0.1.0

**From:** Macrame 0.12.0 (libSQL 0.9.30, schema v10) as prior art
**To:** Jacquard 0.1.0 — a bitemporal graph ledger **server** on Turso
**Engine:** [Turso](https://github.com/tursodatabase/turso) 0.7.0 (July 2026), pre-1.0
**Status:** Draft for approval. Nothing here is implemented. Every Turso capability claim is sourced from `COMPAT.md`/`CHANGELOG.md` on `main` and is **unverified against a running build** — §10 is the list of things that must be measured before this plan is committed to.

---

## 0. What Jacquard is, and what it is not

**It is** a server-side bitemporal graph ledger on Turso, distributed through Turso's sync engine.

**It is not a port of Macrame.** The priority order is set: *performance and clean implementation first; concept and code transfer between the two is a bonus, not a constraint*. Nothing in Jacquard is shaped to keep a Macrame source file compiling, and no abstraction is introduced whose only justification is sharing.

**There is no shared engine trait, and that is a deliberate reversal.** The first sketch of this plan proposed `macrame-core` + a port trait over `Connection`/`query`/`execute` with per-engine capability impls. It is dropped. Measured: ~55% of `src/` is *mechanically* portable — a `libsql::` → `turso::` swap and a compile-error walk — which is exactly the portion a connection trait would abstract, so the trait buys nothing that find-and-replace does not. The remaining 29% diverges at the **SQL dialect**, which no trait over a connection reaches. An abstraction that costs a refactor of `connection.rs` (3,113 LOC) and the migration ladder, to deduplicate the part that was never the problem, is the tax this plan exists to avoid.

**Macrame is not deprecated and does not change.** It stays on libSQL 0.9.30 at schema v10 and keeps shipping to crates.io and PyPI. §9 is the short list of things it may take *back*, all of them free.

**Jacquard opens its own decision register**, numbered `J-001…`, and cites Macrame's `D-…` as prior art. Shared numbering would be the tightest coupling in the whole design for the least benefit.

---

## 1. What actually transfers: the register, not the code

Macrame's transferable asset is **`docs/architecture/s13-decision-register.md` — 147 decisions**, most of them findings that cost a measurement to establish. Those are worth more than the code, and they sort into three bins.

### Bin A — engine-independent, transfer as design input

| Decision | The finding |
|---|---|
| Doctrine I–VIII | The whole invariant set survives. Only Doctrine I's wording ("everything above libSQL") changes engine name. |
| [D-029](architecture/s13-decision-register.md#d-029) | Fixed-width canonical timestamps, because lexicographic order must equal chronological order. `GLOB` is supported on Turso, so the CHECK ports verbatim. |
| [D-030](architecture/s13-decision-register.md#d-030), [D-035](architecture/s13-decision-register.md#d-035) | Two copies of a thing that must agree is a defect class. Applies to SQL, not just Rust. |
| [D-039](architecture/s13-decision-register.md#d-039) | Shortest-path analytics are unsound over negative weights; refuse at load time. Pure algorithm. |
| [D-055](architecture/s13-decision-register.md#d-055) | Absolute latency budgets are not CI gates. Methodology, engine-blind. |
| [D-070](architecture/s13-decision-register.md#d-070), [D-145](architecture/s13-decision-register.md#d-145) | Session-to-session spread is ~29%; a tight spread *within* a session says nothing about the spread *between* them. This is the single most valuable methodological finding in the register and Jacquard must adopt it before it publishes one number. |
| [D-131](architecture/s13-decision-register.md#d-131) | Rehydration is a physical move and mints no transaction-time facts. |
| [D-134](architecture/s13-decision-register.md#d-134) | The edge write path is O(version count per edge key), **not** O(out-degree). Four releases of a wrong caveat because nothing measured it. |
| [D-116](architecture/s13-decision-register.md#d-116), [D-123](architecture/s13-decision-register.md#d-123) | Absent content crosses as `None`; `""` cannot mark *not loaded* because it is a valid value of the type. API design, applies to the wire format. |

### Bin B — libSQL-specific, dies with the engine

| Decision | Why it does not survive |
|---|---|
| [D-071](architecture/s13-decision-register.md#d-071) | FTS5 `'integrity-check'` cannot see disagreement with the content table. There is no FTS5 on Turso and no external-content index — see §6.1. |
| [D-119](architecture/s13-decision-register.md#d-119) | `rowid_pk` exists because external-content FTS5 keys on the rowid and `VACUUM` renumbers implicit ones. With no external-content index, the hazard has no mechanism. |
| [D-147](architecture/s13-decision-register.md#d-147), R15 | Concurrent-open access violation in libSQL 0.9.30 — 93 crashes in 100 attempts under load. An engine bug Jacquard simply does not inherit. |
| [D-140](architecture/s13-decision-register.md#d-140) | `--all-features` quarantine exists to contain R15. No R15, no quarantine. |

### Bin C — re-open under Turso, do not assume

| Decision | Why it must be re-derived |
|---|---|
| [D-058](architecture/s13-decision-register.md#d-058), [D-143](architecture/s13-decision-register.md#d-143), [D-146](architecture/s13-decision-register.md#d-146) | The entire cooperative-chunking apparatus. See §3 — its founding constraint does not exist on Turso. |
| [D-010](architecture/s13-decision-register.md#d-010) | Two-tier priority channels exist to schedule access to one write lock. |
| [D-059](architecture/s13-decision-register.md#d-059), [D-089](architecture/s13-decision-register.md#d-089), [D-118](architecture/s13-decision-register.md#d-118) | Every index decision was measured against libSQL's planner with `EXPLAIN QUERY PLAN`. Turso's planner is a different implementation and will not reproduce those plans. |
| [D-008](architecture/s13-decision-register.md#d-008) | The archive-session marker probes `sqlite_master` from inside a trigger `WHEN` clause. Unverified on Turso — spike **S2**. |
| [D-042](architecture/s13-decision-register.md#d-042) | `idx_lc_traversal_cover`'s column order was fitted to the recursive CTE. §5 replaces the CTE, so the index is re-fitted to a different query shape. |

---

## 2. The engine delta, measured against Macrame's actual usage

### What Turso gives

| Capability | Consequence for Jacquard |
|---|---|
| **MVCC via `BEGIN CONCURRENT`** — snapshot isolation, no locks, row-level conflict detection at commit, `SQLITE_BUSY` on write-write conflict | §3. This is the headline. |
| Rust API is a near-clone of libSQL's — `Builder::new_local().build().await`, `db.connect()`, `conn.query(sql, params)`, `params!`/`named_params!`, `Value`, `Rows`/`Row` | The mechanical cost of writing against Turso is near zero for anyone who knows libSQL's API. |
| Triggers out of experimental in 0.7.0; `RAISE(ABORT, …)` supported, incl. outside triggers | Guard triggers port. §7.2 argues the *log* triggers should not. |
| `RETURNING` supported | Genuinely useful for a server write path — one round trip instead of insert-then-select. |
| Row-level CDC into a real table (`turso_cdc`: monotonic `change_id`, before/after images, COMMIT records) | Structurally the object Macrame hand-builds with log triggers. Doctrine IV rejects "WAL or CDC *frames*"; it does not reject a CDC **table**. Not a day-one move, but a named future simplification (**J-open-1**). |
| `ATTACH DATABASE`, `PRAGMA query_only`, `PRAGMA foreign_keys`, `json_object`, `GLOB`, `ON CONFLICT DO UPDATE`, `ROW_NUMBER() OVER (…)` | The archive path, the read guard, the CHECK constraints, the upsert and the fold all port. |
| No R15 | The property-test quarantine and its six-attempt retry budget do not exist in Jacquard. |

### What Turso takes

| Gap | Macrame's dependency | §  |
|---|---|---|
| **`WITH RECURSIVE` not implemented** | `walk_cte()` — one 13-line string, but §5.2's entire traversal and everything `load_subgraph` feeds | §5 |
| **FTS5 not supported** (Tantivy-backed `CREATE INDEX … USING fts`, `fts_match()`, `fts_score()` instead) | `concepts_fts` external-content table, 3 sync triggers, `bm25()`, `escape_fts5_query`, D-071 | §6.1 |
| **No `F32_BLOB(n)`, no `libsql_vector_idx`, no `vector_top_k`, no DiskANN** — exact search only | `declared_dimension()` parses the dimension out of the column type; the DiskANN index is documented as the only storage-layer dimension check | §6.2 |
| **`PRAGMA synchronous` partial — only `OFF` and `FULL`** | `configure()` sets `NORMAL` | S8 |
| **`PRAGMA recursive_triggers` not supported** | `configure()` sets it `OFF` explicitly | S9 |
| **MVCC gap: all statements on a connection share one MVCC transaction** | Macrame holds one write connection and one read connection | §3.1 |
| `VACUUM` in-place experimental (`VACUUM INTO` fine) | D-119's reasoning, already Bin B | — |

---

## 3. Jacquard does not inherit the Write Actor

**This is the central design decision and it follows directly from "performance is king".**

The Write Actor exists for one reason, stated in §5.1.5: *SQLite's write lock is not preemptible*. One task holds the sole write-capable connection, all writes serialise through it, and because a transaction in flight cannot be shortened without rolling it back, latency is protected by keeping each transaction small. Everything downstream of that — the `CHUNK_BUDGET`, the four per-path ceilings, the 35-row floor, the adaptive time-based sizing loop of 0.12.0, the two-tier priority channels — is scaffolding around a constraint.

Turso's MVCC removes the constraint. `BEGIN CONCURRENT` takes no locks; conflicts are detected per row at commit. A server that serialised every write through one task on top of that would be choosing the largest available performance ceiling on purpose.

**So Jacquard writes concurrently, and the following do not port:**

- the Write Actor and its command enum
- two-tier priority channels ([D-010](architecture/s13-decision-register.md#d-010))
- `CHUNK_BUDGET`, `chunk_rows::{EDGES, CONCEPTS, ANNOTATIONS, EMBEDDINGS}`, the 35-row floor
- the adaptive chunk loop and its `Instant::now()` feedback ([D-146](architecture/s13-decision-register.md#d-146))
- `low_chunked`, the high/low priority split, `ActorMetrics`' queue-depth sampling

That is roughly 1,200–1,500 LOC of `connection.rs` that has no successor, plus the §5.1.5/§5.1.6 documentation and the `perf_claim_tests` that pin it. **Deleting it is the single largest cleanliness win available in this project.**

`write_bulk_atomic`'s contract survives ([D-014](architecture/s13-decision-register.md#d-014)) — caller-sized, one transaction, one stamp — because it was never a workaround; it is a fidelity guarantee.

### 3.1 The connection model changes shape

COMPAT records an MVCC gap: *"all statements on a connection share one MVCC transaction, so a write statement that finishes while a sibling statement is still active defers its commit."*

So Jacquard cannot hold one write connection. It needs **one connection per in-flight transaction** — a pool, sized to expected write concurrency, with `PRAGMA query_only = ON` on the read half exactly as Macrame does today (that pragma is supported and the engine-level read guard ports unchanged).

### 3.2 What concurrency costs: three invariants lose their guard

Serialisation was silently enforcing three things. §4 is the replacement design, and it is the real work of Phase 1.

---

## 4. The three invariants, and their replacements

### 4.1 `trg_links_single_open` → a partial unique index

**The break.** The trigger runs `EXISTS (SELECT 1 FROM links_current WHERE source_id=… AND target_id=… AND edge_type=… AND valid_from<>… AND valid_to=sentinel)` before every edge insert. Under MVCC, two concurrent transactions each insert a *different* row for the same edge key; there is no row-level conflict to detect, both snapshots see no open interval, both commit. **Two open intervals, no error.** This is the invariant `assert_edge` is named after.

**The replacement:**

```sql
CREATE UNIQUE INDEX idx_lc_single_open
    ON links_current (source_id, target_id, edge_type)
 WHERE valid_to = '9999-12-31T23:59:59.999999Z';
```

Better on three independent counts, which is why this is the plan's preferred shape rather than a fallback:

1. **Correct under MVCC.** A unique-index violation *is* a conflict the engine detects. The trigger's `EXISTS` is not.
2. **Faster on the hot write path.** It removes a correlated subquery from every edge insert. [D-059](architecture/s13-decision-register.md#d-059) measured that probe at 4.4 ms into an empty table and **1.06 s into a 90,000-edge hub**, and needed a dedicated five-column index (`idx_lc_open_interval`) to flatten it to 8.0 ms. The partial unique index replaces **both the trigger and that index** — one index write instead of one index write plus a probe.
3. **Cannot drift.** Today the invariant lives in trigger text that must stay byte-identical to `ABORT_SINGLE_OPEN`, which `error::abort_kind` string-matches to produce the typed error. A schema constraint needs no classifier agreement.

**Cost:** the typed error derives from a unique-constraint violation rather than a message match — strictly more robust. **Blocker:** partial indexes are not documented in COMPAT. Spike **S3**, and it gates this section.

### 4.2 `transaction_log.seq_id` — the append-only log is the contention point

**Two distinct breaks, and the second is the one nobody expects.**

*Under sync:* `seq_id INTEGER PRIMARY KEY AUTOINCREMENT` is database-local. The fold at [as_of.rs:172](src/temporal/as_of.rs:172) does `ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) = 1` — last-writer-wins **by sequence**. Two nodes allocating independently makes sequence order stop being causal order, and `reconstruct()` returns wrong answers. This is [D-131](architecture/s13-decision-register.md#d-131)'s failure arriving through a different door.

*Under single-node MVCC:* the fold itself is **safe**, and this is worth stating because it looks unsafe. Concurrent transactions get seq_ids in allocation order, which may differ from commit order — but the fold partitions by `entity_id`, and two concurrent writes to the *same* entity conflict at the row level so only one commits. Order between different entities never mattered. **The fold needs no change for single-node concurrency.**

What *does* break under single-node MVCC is the allocator. `AUTOINCREMENT` maintains a counter in `sqlite_sequence` — **a single shared row that every log insert updates**. Under row-level conflict detection, that makes every concurrent write to the ledger conflict with every other one, serialising exactly the path this design set out to parallelise. Plain `INTEGER PRIMARY KEY` is no better: it reads a shared max.

**The replacement: `seq_id` becomes a ULID.** Lexicographically sortable by time, allocated client-side with no shared state, no counter row, no conflict. `ulid` is already a Macrame dependency. The fold's SQL shape is unchanged — `ORDER BY seq_id DESC` still means what it meant, on TEXT instead of INTEGER.

**Topology decision (J-001):** *single writer per tenant database; replicas are read-only.* Under that rule ULID ordering is exact, because one node stamps every row. Multi-master is deferred, not refused — but it needs a specified conflict semantics for "what did we believe at time T", which nobody has written, and inventing one under time pressure is how a ledger stops being trustworthy. Recorded as **J-open-2**.

Note this still leaves Jacquard writing concurrently *within* a tenant and *across* tenants. Single-writer-per-tenant is a routing rule, not a serialisation point.

### 4.3 `trg_concepts_monotonic_ra` — survives unchanged

`BEFORE UPDATE ON concepts WHEN NEW.recorded_at <= OLD.recorded_at`. This is **row-scoped**: two concurrent updates to the same concept conflict at the row level and one aborts. It is correct under single-node MVCC with no change. It breaks only under multi-master, which §4.2's topology decision rules out.

One of three needed a schema change, one needed a topology decision, one was already fine. Worth stating plainly because "MVCC breaks your invariants" is the kind of claim that gets over-applied.

---

## 5. Traversal: the hop-by-hop driver, chosen on performance

`WITH RECURSIVE` is not implemented on Turso, so `walk_cte()` has no direct port. **The replacement is chosen on its merits, not as a workaround.**

**The design.** A frontier loop in Rust: one prepared, parameterised query per depth level against a covering index on `links_current`, results deduped in a `HashSet`, terminating on depth bound or empty frontier.

**Why it is the better implementation regardless of engine:**

- **It deletes the dedupe b-tree.** The shipped CTE uses `UNION`, which maintains a b-tree over every row entering the queue. `builder.rs` records the cost, measured: on the star-of-stars fixture at depth 3, **8–10% slower** than the old `UNION ALL` form on trees, where nothing ever dedupes. A Rust `HashSet` is a hash lookup against pages the engine never writes.
- **It keeps the 2,000× win.** The `UNION` form exists because the original simple-path form produced 299,593 walk rows on a 328-edge graph at depth 6 (428 ms vs 0.1 ms). The driver dedupes on `(node_id)` per level, so it inherits that bound — `V × (depth+1)` — without paying b-tree maintenance for it.
- **Round trips are function calls.** Turso is in-process. N hops is N prepared-statement executions, not N network round trips.
- **It expresses things a CTE cannot.** Early termination (stop when k nodes found), per-hop budget checks, streaming results to a client as they are discovered, cancellation on client disconnect. All four matter for a server and none is available inside a recursive CTE.
- **Bounded, inspectable memory.** The frontier is a Rust collection whose size is known per hop, rather than a queue inside the VDBE.

**The risk** is per-hop parameter binding on a wide frontier — an `IN (?, ?, …)` list that grows with the frontier. Mitigation is a per-hop temp table or a carried-frontier join; **S13** measures which, on a fixture that produces a wide frontier at depth 2.

**Validation strategy, and it is the plan's best free lunch.** Build the driver **in Macrame first, against libSQL**, where `integrity_property_tests` already contains a 512-case harness that compares two traversal forms and requires identical node *and* edge sets across cycles, self-loops, diamonds and expired edges. That harness was written to retire the simple-path form; it validates the driver at zero cost. If the driver also measures faster on libSQL, Macrame keeps it — see §9. Jacquard then takes a form that has already been differentially tested against a recursive CTE, which is a stronger starting position than writing it clean-room on an engine that cannot express the reference implementation.

`idx_lc_traversal_cover`'s column order ([D-042](architecture/s13-decision-register.md#d-042)) was fitted to the CTE's access pattern and must be re-fitted to the driver's single-hop query. Same reasoning, different query.

---

## 6. Search

### 6.1 Full-text — the triggers die and that is the win

FTS5 is not supported. Turso ships a Tantivy-backed index method: `CREATE INDEX … USING fts`, `fts_match()`, `fts_score()` (BM25).

**What dies:** the `concepts_fts` external-content virtual table, `content_rowid='rowid_pk'`, all three sync triggers (`trg_concepts_fts_insert`/`_update`/`_delete`), `escape_fts5_query`'s FTS5 quoting dialect, `bm25()`'s negative-magnitude convention, `VERIFY_CONCEPTS_FTS`, `REBUILD_CONCEPTS_FTS`.

**Why that is an improvement rather than a loss.** Turso's FTS is an **index**, so the engine maintains it. The entire hazard class documented at length in `ddl.rs` — that an external-content index must be told which terms to *retract* using the old column values, and that getting `trg_concepts_fts_update` wrong leaves an index still matching words no concept contains, silently — has no mechanism. So does [D-071](architecture/s13-decision-register.md#d-071)'s finding that the integrity check cannot see content drift: there is no second copy to drift.

**Schema consequence.** `rowid_pk` (v8, [D-119](architecture/s13-decision-register.md#d-119)) exists solely because an external-content index keys on the rowid and `VACUUM` renumbers implicit ones. With no such index, Jacquard's `concepts` table takes a clean `id TEXT PRIMARY KEY`. *Macrame's is shipped and is not re-litigated.*

**Spikes:** can the FTS index be filtered (`retired = 0`) or must filtering happen after? What is the rebuild story? Is `fts_score`'s sign convention BM25-negative like `bm25()` or inverted? All **S6** — they decide whether `keyword_search`'s `ORDER BY rank ASC` flips.

### 6.2 Vector — the one place Turso is materially behind, and where being a server helps

Turso has `vector32`/`vector64`/`vector_distance_cos`/`vector_distance_l2` and **exact search only**. No `F32_BLOB(n)`, no `libsql_vector_idx`, no `vector_top_k`, no DiskANN. Issue [#3778](https://github.com/tursodatabase/turso/issues/3778) suggests upstream is starting from SIMD brute force, so an ANN index is not imminent.

**Two things break, and the second is not about speed.**

**Correctness.** `ddl.rs` documents, from measurement, that the DiskANN index is *load-bearing for correctness*: a blob of the wrong length inserted into an `F32_BLOB(4)` column is **accepted** while no vector index exists and rejected once one does. Turso has neither the typed column nor the index, so both storage-layer checks are gone. `declared_dimension()` — which deliberately keeps no registry of its own, reading `F32_BLOB(n)` back out of the column type — has nothing to parse.

*Replacement, and it is stricter than what it replaces:* the dimension moves into an explicit `vector_models(model, dim)` registry row, enforced by `CHECK (length(embedding) = :dim * 4)` on the per-model table and validated in Rust at the write boundary. It no longer depends on an index existing, which was always an uncomfortable place for a correctness guarantee to live.

**Performance.** §9's budget is *top-10 over 100K concepts in ≤ 20 ms*, measured at 246–264 µs with DiskANN. Exact search is linear in the corpus: 100K × 768 dims × 4 B ≈ **307 MB scanned per query**. It will not meet 20 ms and no amount of SQL tuning changes that.

*Three options:*

| Option | Assessment |
|---|---|
| Wait for upstream ANN | No committed date. Not acceptable on the critical path. |
| **Jacquard owns an in-process ANN index (HNSW), built from the embeddings table** | **Recommended.** |
| SIMD brute force over a memory-mapped f32 slab | Viable to ~10K concepts; a stopgap, and cheap enough to build first. |

**Why option 2 is the right shape for this product specifically:** Jacquard is a *process with a lifetime*, which is precisely what makes a memory-resident index viable — and precisely what Macrame, a library living in someone else's process, could never justify. It also lands exactly inside Doctrine VI: derivative state is disposable, rebuildable from the ledger, and drift is recoverable by rebuild. The ANN index is `links_current`'s argument applied to vectors.

**S12** measures exact search at 10K and 100K to size the decision and to set the threshold at which the slab stops being enough.

---

## 7. Sync, and where the ledger gets written

### 7.1 The one unknown that could invalidate the design

**S1: do triggers fire when the sync engine applies replicated rows?**

- If **yes** → every replica re-runs `trg_links_log_insert` on rows that already carry their log entries, double-logging the ledger and corrupting `reconstruct()` on every replica.
- If **no** → replicas are consistent, because every table replicates including `transaction_log` and `links_current`.

This is the highest-priority spike in the plan. Everything in §7.2 is written assuming a specific answer, and §7.2 is also the design that makes the answer stop mattering.

### 7.2 Jacquard writes the ledger explicitly, not through triggers

**Recommendation, on cleanliness and performance grounds both.**

Macrame's log triggers exist because a library cannot guarantee it is the only writer to the file — a trigger is the only place an invariant can live that an unknown third-party writer cannot bypass. **Jacquard is the only writer.** That changes the calculus completely:

1. **It removes the sync hazard** in §7.1 regardless of how S1 resolves. Explicit inserts from the write path do not fire on replicated rows because there is no write path on a replica.
2. **It is faster.** A trigger fires per row and builds its payload with `json_object(...)` inside the VDBE. An explicit insert binds a `serde` — serialised payload as a parameter. One statement, no dispatch, no in-engine JSON construction on the hot write path.
3. **It is dramatically cleaner.** The payload format today is a JSON schema maintained as **string literals inside trigger DDL**, with a hand-rolled `'v', 2` version field, read back by two Rust call sites that must independently agree with it. `ddl.rs` documents the consequence — the v1→v2 `embedding_model` defect, where the field "was written by nobody and read by two" and `AttributeMode::AtTime` silently returned *less* than `Current` for four releases. In Rust, the payload is a versioned enum, the writer and reader share one type, and that defect class stops being expressible.
4. **It sidesteps `PRAGMA recursive_triggers` not existing** (S9) for the log path entirely.

**The guard triggers stay.** `trg_links_guard_delete`, `trg_txlog_guard_delete`, `trg_concepts_guard_delete` defend Doctrine V against a bug in Jacquard's own write path, which is exactly the threat a trigger is the right tool for. `RAISE(ABORT, …)` is supported. Their `sqlite_master`-probing `WHEN` clause is spike **S2** and, if it fails, the archive-session marker becomes a table column or a session flag rather than a schema object.

### 7.3 Topology

Single writer per tenant database (**J-001**, §4.2); read replicas via the sync engine; `PRAGMA query_only = ON` on every replica connection. Sync's push/pull semantics, conflict handling and Turso Cloud coupling are **not documented in `docs/manual.md`** — spike **S14** is reading the sync engine source or asking upstream, and it must complete before Phase 5 is scoped.

---

## 8. What code transfers for free — deliberately small

Take only what drops in with **no adaptation**. Everything below is already engine-free and product-neutral:

| Source | LOC | Why it ports untouched |
|---|---|---|
| `graph/algorithms.rs` | 501 | Dijkstra, A\*, SCC, k-core, Louvain over a `Subgraph`. Zero engine contact. |
| `util/timestamp.rs` | 308 | Canonical form, the `GLOB` pattern, comparison helpers. `GLOB` is supported. |
| `util/ids.rs` | 170 | Identifier validation. |
| `vector/model.rs` | 129 | `ModelName` validation and table/index naming. |
| `vector/embedding.rs` | 38 | Little-endian f32 codec — same wire format on both engines. |
| `temporal/interval.rs` | 34 | Interval algebra. |
| `reciprocal_rank_fusion()` (in `vector/search.rs`) | ~25 | Pure function over two rank lists. |
| **Total** | **≈1,205** | |

**Copy it. Do not extract a crate.** A shared crate for 1,200 LOC of stable pure functions costs more in release coordination, version skew and cross-repo CI than it saves. Revisit only if the shared surface passes ~3,000 LOC *and* starts changing in both places — which would itself be evidence the two products are not as different as assumed.

`error.rs`'s taxonomy is a **concept** transfer, not a code one: the shape is right — typed errors with intermediate groups for catching sets — but `DbError::Engine(#[from] libsql::Error)` and the whole of `abort_kind`/`classify` are libSQL-specific.

That is the entire code-transfer story, and its smallness is the point.

---

## 9. Macrame's own path

**Unchanged.** libSQL 0.9.30, schema v10, the existing release cadence, the existing risk register. Jacquard gets no vote on Macrame's schema and no seat in its decision register.

Two things may flow **back**, both free, both on measured grounds only:

1. **The traversal driver (§5).** It is built in Macrame first and validated by an existing 512-case harness. If it measures faster there — and the `UNION` dedupe overhead says it should on tree-shaped graphs — Macrame keeps it and `walk_cte()` retires. This is the transfer the owner's priority order actually favours: it makes Macrame faster, and the fact that Jacquard needed it is incidental.
2. **The partial unique index (§4.1),** if libSQL supports it. It would retire `trg_links_single_open`, `ABORT_SINGLE_OPEN`, its `abort_kind` arm, and possibly `idx_lc_open_interval` — on a path [D-059](architecture/s13-decision-register.md#d-059) already measured. That is a real reduction in Macrame's hot write path, arrived at from Jacquard's constraints.

Nothing else. In particular Macrame does **not** adopt the explicit ledger writes of §7.2 — its trigger-based log exists for a reason that still holds for a library.

---

## 10. Phase 0 — spikes

Each is one small program answering one question. **None of Phase 1 starts until S1–S5 are answered**, because each of them can invalidate a section above.

| ID | Question | What it gates |
|---|---|---|
| **S1** | Do triggers fire on sync-applied rows? | §7.1 — the whole replication design |
| **S2** | Can a trigger `WHEN` clause query `sqlite_master`? Does uncommitted DDL stay connection-local under MVCC? | [D-008](architecture/s13-decision-register.md#d-008), the archive session, all three delete guards |
| **S3** | Partial indexes (`CREATE INDEX … WHERE`)? Expression indexes? `IF NOT EXISTS`? | §4.1 — undocumented in COMPAT, and §4.1 is the preferred design |
| **S4** | `BEGIN IMMEDIATE` availability; `BEGIN CONCURRENT` semantics; does connection-per-transaction actually give parallel commits? | §3 — 8 call sites use `transaction_with_behavior(Immediate)` |
| **S5** | Does `AUTOINCREMENT`/`sqlite_sequence` serialise concurrent inserts under MVCC? Does the ULID scheme avoid it? | §4.2 — the ledger is the hottest write path |
| **S6** | Tantivy FTS: filtering, rebuild, `fts_score` sign convention, index-on-column syntax | §6.1 |
| **S7** | `ATTACH` + cross-database `DELETE` inside one transaction | The archive path (`archive.rs`, 722 LOC) |
| **S8** | `PRAGMA synchronous` has no `NORMAL` — what is the WAL durability/throughput curve between `OFF` and `FULL`? | `configure()`; a server's durability posture |
| **S9** | `PRAGMA recursive_triggers` unsupported — what is the default behaviour? | Guard triggers; §7.2 removes the log-trigger exposure |
| **S10** | Windows I/O path and stability under load (`io_uring` is Linux-only; development is on Windows 11) | Whether Jacquard is developed on the platform it is written on |
| **S11** | `ROW_NUMBER() OVER (PARTITION BY … ORDER BY …)` parity — COMPAT says "default frame" only | The fold, `integrity/shadow.rs`, `integrity/mod.rs` |
| **S12** | Exact vector search timing at 10K and 100K × 768 dims | §6.2 — sizes the ANN decision |
| **S13** | Wide-frontier binding: `IN (?…)` vs temp table vs carried join | §5's one real risk |
| **S14** | Sync engine: push/pull granularity, conflict handling, Turso Cloud coupling | Phase 5; undocumented in `manual.md` |

---

## 11. Phasing

| Phase | Content | Exit condition |
|---|---|---|
| **0** | S1–S14 | Every section above either confirmed or amended in writing |
| **1** | Schema, connection pool, explicit ledger writes, guard triggers, the fold, `as_of`/`reconstruct` | A database that records and reconstructs belief correctly under concurrent writers. No search, no traversal. |
| **2** | Traversal driver — **built in Macrame first**, differentially tested against `walk_cte()`, then taken | Identical node and edge sets across the existing 512-case harness; a measurement on both engines |
| **3** | Search: Tantivy FTS, vector registry + dimension CHECK, slab brute force, then HNSW | §9's budgets re-derived on named reference hardware under [D-070](architecture/s13-decision-register.md#d-070)'s methodology |
| **4** | Archive, rehydrate, snapshots | Point-in-time reconstruction composes from snapshot + fold |
| **5** | Server surface, sync topology, multi-tenancy | Scoped only after S14 |

---

## 12. Open decisions

| ID | Decision |
|---|---|
| **J-001** | Single writer per tenant database; replicas read-only. Proposed in §4.2, needs sign-off. |
| **J-open-1** | Whether to replace the hand-written ledger with Turso's CDC table once §7.2's explicit writes are proven. Deferred; not a Phase 1 question. |
| **J-open-2** | Multi-master conflict semantics for a bitemporal ledger. Deferred and *named*, because deferring it silently is how it gets decided by accident. |
| **J-open-3** | Whether Jacquard's wire format is its own protocol or leans on Turso Cloud's client surface. Depends on S14. |

---

## 13. What this plan is most likely to be wrong about

Stated because [D-070](architecture/s13-decision-register.md#d-070) and [D-134](architecture/s13-decision-register.md#d-134) are both records of confident claims that went unmeasured for releases.

1. **Every Turso capability here is read from documentation, not measured.** COMPAT.md on `main` may lag the code in either direction. Phase 0 exists because of this and its findings outrank this document.
2. **§3's premise — that MVCC makes concurrent writes actually faster for this workload — is unmeasured.** Optimistic concurrency with row-level conflict detection can lose badly to serialisation when contention is high, and a graph ledger with hub nodes is a high-contention shape. If S4 shows conflict-retry storms on hub writes, the Write Actor comes back as a *choice* rather than a workaround, and §3 is rewritten.
3. **§5's driver is argued from a measurement of a different thing** — the 8–10% `UNION` overhead was measured against `UNION ALL` inside the engine, not against a Rust-side frontier. Phase 2 measures the actual comparison before Macrame adopts anything.
4. **§6.2's HNSW is the largest unbudgeted piece of work in the plan** and is described in one paragraph. It deserves its own document before Phase 3 starts.
