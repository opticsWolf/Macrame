# Macrame Codebase Review — v0.12.0

**Reviewed:** 2026-08-09 · commit `2c11f5f` · branch `main` (clean)
**Scope:** `src/` (13,374 lines), `bindings/python/src/` (4,438 lines), `tests/`, `benches/`, `.github/workflows/`, schema DDL and migration ladder.
**Method:** full read of the write path, actor loop, schema, temporal and graph modules; targeted reads elsewhere; `cargo clippy --all-targets --features "metrics property-tests"`; `cargo test --features metrics --no-fail-fast` run twice.

---

## 0. What was verified, not assumed

| Check | Result |
|---|---|
| `cargo clippy --all-targets --features "metrics property-tests"` | clean, exit 0 |
| `cargo test --features metrics` — run 1 | exit 0, no crash |
| `cargo test --features metrics --no-fail-fast` — run 2 | **exit 101**, 28 binaries, 320 passed, 0 failed — `wave1_regression_tests` died `0xC0000005` |
| `TODO` / `FIXME` / `XXX` / `HACK` in `src/` and `bindings/` | **zero** |
| `unsafe` blocks | **zero** |
| Panic sites outside `#[cfg(test)]` | 9, all reachable only via broken invariants (§3.5) |

Two things worth stating before the findings.

**This is an unusually disciplined codebase.** The decision register runs to 2,314 lines, every constant carries its derivation and the measurement session it came from, and several comments explicitly record a hypothesis that was *refuted* rather than quietly replaced. Clippy is clean under `-D warnings` including rustdoc. The findings below are mostly about things the design chose not to do yet, not about carelessness.

**Run 2 is a live reproduction of R15 in the main suite.** `.cargo/config.toml` documents the reporting hazard precisely — "a pass-count sum comes back SMALLER with no failures rather than red" — and that is exactly what happened: 320 passed, 0 failed, exit 101. The crash was in `wave1_regression_tests`, which is *not* one of the quarantined property binaries. One in two runs, on this machine, in the non-quarantined suite. See §5.1.

---

## 1. Findings at a glance

| # | Finding | Class | Severity |
|---|---|---|---|
| 2.1 | `links` has no explicit index; five hot queries full-scan it | Performance | **High** |
| 2.2 | `CONCEPTS_ARCHIVABLE` is O(concepts × links) | Performance | **High** |
| 2.3 | Per-transaction overhead is the write floor, and no API states it | Performance | Medium |
| 2.4 | Snapshot serialize/compress/IO runs on a tokio worker | Performance | Medium |
| 2.5 | `Subgraph` adjacency is `BTreeMap<String, _>` in every algorithm's inner loop | Performance | Medium |
| 2.6 | `reject_overlaps_within` is O(n²) on an uncapped batch | Performance | Low (documented) |
| 3.1 | `TraversalBuilder::as_of(ts)` applies one timestamp to two different time axes | Correctness | **High** |
| 3.2 | `AttributeMode::AtTime` silently degrades after an archive | Correctness | Medium |
| 3.3 | Snapshot loader has no integrity check and no decompression bound | Robustness | Medium |
| 3.4 | A future `recorded_at` poisons the clock permanently | Robustness | Medium |
| 3.5 | `run_writer_actor` cannot return `Err`; `close()`'s propagation is half-dead | Robustness | Low |
| 3.6 | `write_annotations_atomic` bypasses `classify` | Consistency | Low |
| 3.7 | Snapshot rename is atomic but not durable | Robustness | Low |
| 4.1 | Strict preemption has no anti-starvation floor | Operational | Medium |
| 4.2 | No cancellation or progress on bulk paths | API | Medium |
| 4.3 | `metrics` off by default ⇒ the latency bound is unobservable in the shipped build | Observability | Medium |
| 4.4 | `metrics`' public surface is frozen by accident — no `#[non_exhaustive]` anywhere | API stability | **High** (pre-1.0 window) |
| 4.5 | No WAL/checkpoint surface | API gap | Low |
| 4.6 | Python has no clock injection, no model introspection, no chunk constants | Binding gap | Medium |
| 4.7 | `Database` is not `Clone` | API note | Low |
| 5.1 | R15 reaches the main suite, not just the quarantine | Test infra | **High** |
| 5.2 | The index registry is one-directional | Test infra | Medium |
| 5.3 | No performance regression detection | Test infra | Low (deliberate) |
| 5.4 | No fuzzing on the snapshot loader | Test infra | Low |
| 6.1 | `docs/architecture/README.md` release table stops at 0.9.0 | Docs | Low |
| 6.2 | `Cargo.toml`'s `metrics` cost model describes 0.11.0 and is now false | Docs drift | Medium |
| 6.3 | Comment-to-code ratio: navigation and drift surface | Docs note | Low |
| F-28 | `ANALYZE`/`PRAGMA optimize` run nowhere; the planner has never had statistics | Performance | **High** |
| F-29 | `index_plan_tests` pins plans against a fixture with no rows and no statistics | Test infra | **High** (once F-28 lands) |
| F-30 | Autocheckpoint is an unbudgeted hold inside 0.12.0's adaptive controller | Performance | Medium |

Every numbered subsection below has a row here — the table is the complete list,
not a selection.

**F-28 through F-30 were raised after this review was written** and have no
subsection here. They are stated in full in
[Macrame Road to 1.0 §0.3](Macrame%20Road%20to%201.0.md), which also carries the
plan that closes them and every row above. F-28 is listed High because it is the
generator of the D-042/D-059/D-064 bug class this codebase has already paid for
three times: with no `sqlite_stat1`, SQLite costs plans by counting bound
columns, which is the literal definition of *"captures a query because it
contains the columns, not because it discriminates"*.

---

## 2. Performance

### 2.1 `links` carries no explicit index — five hot queries scan it · **High**

`ddl::CREATE_INDICES` ([ddl.rs:479](../src/schema/ddl.rs:479)) declares exactly four indexes: two on `links_current`, two on `transaction_log`. `migrations.rs` adds none. So `links` — the ledger's largest table, the one that only grows — has one usable index: the implicit PK autoindex over `(source_id, target_id, edge_type, valid_from, recorded_at)`.

That autoindex serves seeks led by `source_id`. It serves nothing else. These queries have no index and scan the whole table:

| Site | Query | When it runs |
|---|---|---|
| [clock.rs:42](../src/util/clock.rs:42) | `SELECT MAX(recorded_at) FROM links` | **every `Database::open()`** |
| [shadow.rs:155](../src/integrity/shadow.rs:155) | `SELECT MAX(recorded_at) FROM links` | every `rebuild_current_chunked` |
| [shadow.rs:262](../src/integrity/shadow.rs:262), [:274](../src/integrity/shadow.rs:274) | `FROM links WHERE recorded_at >= ?1` | shadow catch-up, twice per swap |
| [archive.rs:128](../src/temporal/archive.rs:128) | `recorded_at < :cutoff AND (…)` | every archive session |
| [archive.rs:204](../src/temporal/archive.rs:204) | `SELECT 1 FROM links WHERE … OR target_id = …` | see 2.2 |

The `open()` one is the most consequential because it is unconditional and on the startup path. Every process that opens the database pays a full scan of `links` before the actor starts, and the cost grows with the ledger forever. On a 10M-row `links` this is seconds of startup, with no way for a caller to opt out — `SystemClock::new` calls `recorded_at_floor` internally.

**Fix.** One index closes four of the five rows:

```sql
CREATE INDEX idx_links_recorded_at ON links (recorded_at);
```

`MAX(recorded_at)` then becomes a single index-tail seek, and the archive and shadow range filters become range scans. Cost is one more index write per edge insert on a table that already takes three — measurable, and against a scan that grows without bound it is clearly the right trade.

This should be a `v10 → v11` rung, and it needs a `Justification::Query` entry in `index_plan_tests.rs` naming `recorded_at_floor` as the reader.

**Note on the existing gate.** `index_plan_tests.rs` is a genuinely good idea — it fails when an index has no reader. But it is one-directional: it cannot notice a *reader with no index*, because there is no index to hang the registry entry on. That is why these five have gone unnoticed. See §5.2.

### 2.2 `CONCEPTS_ARCHIVABLE` is quadratic · **High**

[archive.rs:199](../src/temporal/archive.rs:199):

```sql
AND NOT EXISTS (
    SELECT 1 FROM links
    WHERE links.source_id = concepts.id
       OR links.target_id = concepts.id
)
```

`links.source_id` is the leading PK column, so that half seeks. `links.target_id` has **no index at all**, so the `OR` arm is a full scan of `links` — and this is a correlated subquery evaluated per candidate concept.

`archivable_concepts` ([archive.rs:222](../src/temporal/archive.rs:222)) runs it over every row in `concepts`, and it is public API a caller is explicitly invited to call before planning a session ("the predicate that decides it is observable on its own"). The same predicate runs again inside `archive_concepts` during the session, under the write lock, inside the archive's single transaction. On a ledger with 100K concepts and 1M links that is a scan budget in the region of 10¹¹ row visits.

SQLite's OR-optimization can split a two-index `OR` into a union of seeks, but only when *both* sides have an index. Here one does not, so the whole disjunction degrades to a scan.

**Fix.**

```sql
CREATE INDEX idx_links_target ON links (target_id);
```

That makes both arms seekable and lets the planner use the OR-by-union transform. Cheap and mechanical. Worth confirming with `EXPLAIN QUERY PLAN` in `index_plan_tests.rs` that the plan actually splits rather than assuming it — this is precisely the class of mistake D-042, D-059 and D-064 were each an instance of.

### 2.3 Per-transaction overhead is the write path's real floor · Medium

`quickref.md` publishes it — ~0.8 ms per BEGIN/COMMIT/fsync — and every chunk constant is derived against it. But the API surface does not communicate it, and the consequence is that the single-item write methods look like reasonable primitives for bulk work when they are not.

`upsert_concept` ([connection.rs:1201](../src/connection.rs:1201)) is one transaction per concept. A caller looping it over 2,000 entities spends ~1.6 s in fixed transaction overhead alone, against ~75 ms for the same rows through `write_concepts`. That is a 20× penalty available by choosing the obvious-looking method, and nothing in either rustdoc points at the other.

This is not hypothetical — it is exactly the CodeRadar bottleneck analysed earlier in this session, where the conclusion drawn was that Macrame *needed* a bulk concept API rather than that it already had one since 0.5.6.

**Fix — documentation, not code.** Add to `upsert_concept`, `assert_edge` and `retire_edge` rustdoc a "for more than a handful of rows, use `write_concepts` / `bulk_import`" line with the order-of-magnitude. The crate already does this well elsewhere (`archive` → `archive_windowed`, `rebuild_current` → `rebuild_current_chunked`); the single-item write paths are the ones missing the pointer. `docs/quickref.md` gaining a "choosing a write method" table would close it for the reader who never opens rustdoc.

### 2.4 Snapshot work runs synchronously on a tokio worker · Medium

`save_snapshot` ([snapshot.rs:107](../src/temporal/snapshot.rs:107)) does `bincode::serialize` → `zstd::encode_all` → `File::create` / `write_all` / `sync_all` / `rename`. All synchronous, all inside an `async fn`, no `spawn_blocking`. `load_snapshot` ([snapshot.rs:163](../src/temporal/snapshot.rs:163)) is the same in reverse.

Three callers:

- `run_cadence` ([snapshot.rs:465](../src/temporal/snapshot.rs:465)) — every 10,000 log entries, on a shared runtime worker.
- `Database::close` ([connection.rs:1922](../src/connection.rs:1922)).
- `open_inner`'s post-migration re-anchor ([connection.rs:833](../src/connection.rs:833)).

For a large `MaterializedState` this is hundreds of milliseconds of CPU-bound zstd plus an `fsync`, blocking a worker thread that other tasks are queued on. The Python binding makes this worse in a specific way: `runtime()` is process-wide and shared, so a cadence tick stalls a worker that a `block_on` from the interpreter may be waiting on.

There is also a peak-memory shape worth noting: `save` holds the serialized `Vec` and the compressed `Vec` simultaneously; `load` holds the raw file bytes, the decompressed buffer, and the deserialized state. Roughly 2–3× the state size at peak, on both sides.

**Fix.** Wrap the serialize/compress/write body in `tokio::task::spawn_blocking`, and the read/decompress/deserialize body likewise. The state is `Send`; the change is mechanical. Streaming through `zstd::Encoder` into the file would additionally halve peak memory, at the cost of losing the "one buffer, one write" simplicity — worth doing only if snapshots get large enough to matter.

### 2.5 `Subgraph` adjacency is string-keyed in every algorithm's inner loop · Medium

[subgraph.rs:67](../src/graph/subgraph.rs:67):

```rust
pub struct Subgraph {
    nodes: BTreeMap<String, NodeData>,
    out_adj: BTreeMap<String, Vec<EdgeRef>>,
    in_adj: BTreeMap<String, Vec<EdgeRef>>,
    …
}
```

The design already half-solved this: `EdgeRef` is `Copy` and holds `u32` interner indices, and there is an `Interner` for strings. But the adjacency maps themselves stayed keyed by `String`, so every traversal of an edge costs a `BTreeMap<String, _>` lookup — O(log n) tree descent with a full string comparison at each level.

Louvain ([algorithms.rs:414](../src/graph/algorithms.rs:414)) is the worst case:

```rust
for edge in graph.out_edges(node).iter().chain(graph.in_edges(node)) {
    if edge.node(graph) == node { continue; }
    *k_i_c.entry(comm[edge.node(graph)]).or_insert(0.0) += edge.weight();
}
```

Per edge, per node, per sweep: two `out_edges`/`in_edges` string lookups, two `edge.node(graph)` resolutions, one `comm[…]` string-keyed index, and a string comparison. Plus `comm.insert(node.to_string(), best_comm)` — a heap allocation per community move. `dijkstra`, `scc` and `k_core` have the same shape.

The rustdoc says the `BTreeMap` and the `String` keys are "part of the public API", so this is a knowing choice with a compatibility cost attached. That is fair, but it is worth separating the *interface* from the *representation*: the algorithms could build a dense `Vec`-indexed view once (node id → `u32`, CSR adjacency), run on integers, and translate back at the boundary. The public `BTreeMap<String, usize>` return type is unchanged; only the interior changes. On a 10K-node subgraph that is plausibly a 5–20× improvement in the analytics module, and it is entirely additive.

Order-of-magnitude only — this was read, not benchmarked. It is worth a `benches/` group before it is worth implementing.

### 2.6 `reject_overlaps_within` is O(n²) on an uncapped batch · Low

[connection.rs:2653](../src/connection.rs:2653), called from `write_edges_atomic` before the transaction opens. `estimated_bulk_hold` models it honestly — 5.5 ns per mismatched pair, 86 ns per matching pair — and documents the 20,000-edge worst case at 18.6 s. `BULK_ATOMIC_WARN_HOLD` warns at 250 ms.

This is documented, measured, and deliberate: `write_bulk_atomic` is exempt from `CHUNK_BUDGET` by contract. Listed here only so the review is complete. The warning is the mitigation and it is the right one — capping would break the guarantee the method exists to provide.

---

## 3. Correctness and robustness

### 3.1 `TraversalBuilder::as_of(ts)` applies one timestamp to two different axes · **High**

This is the finding I would act on first.

`TraversalBuilder::as_of(ts)` reaches two places with the same `ts`:

**Topology** — [builder.rs:265](../src/graph/builder.rs:265), the `walk` CTE:
```sql
AND l.valid_from <= ?3 AND ?3 < l.valid_to
```
That is **valid time**, evaluated against `links_current` — i.e. *current belief* about what was true at `ts`.

**Attributes** — [as_of.rs:175](../src/temporal/as_of.rs:175), `hydrate_at_time`:
```sql
WHERE table_name = 'concepts' AND recorded_at <= ?1
```
That is **transaction time** — what was *believed* at `ts`.

So `as_of("2026-01-01") + AttributeMode::AtTime` returns: the graph as we *now believe* it was on 1 Jan, wearing the titles we *believed on* 1 Jan. Two different bitemporal questions answered with one parameter.

Doctrine II is "two clocks, never mixed", and the codebase enforces it rigorously everywhere else — `archive.rs:333` has a long comment about exactly this class of bug in the `links_current` compensation, and calls it "permanent drift no later audit could explain". The traversal path does the same mixing at the API surface.

`hydrate_attributes`' rustdoc gets close: *"Note what 'as of ts' means for each: `Current` asks whether the concept is retired **now** and `AtTime` asks whether it was retired **then**."* But that sentence is about `retired`, and it frames the difference as `Current` vs `AtTime` — not as valid-time vs transaction-time within `AtTime` itself. A reader who selects `AtTime` to be maximally faithful gets the mixed answer and no signal.

Note this is a *narrower* form of the problem D-085 already solved once. D-085 made the unstated-mode case a typed error (`AttributeModeUnstated`) precisely because "the past's graph wearing the present's titles" is "a legitimate thing to want and a terrible thing to get by accident". The same sentence applies to the axis mix, and it is currently the *default* thing `AtTime` gives you.

**Fix, in increasing order of cost:**

1. **Document it.** Add to `as_of`'s rustdoc that the instant is interpreted on the valid-time axis for topology and the transaction-time axis for attributes, and that these coincide only when nothing has been retroactively corrected. Cheapest, and it removes the silence.
2. **Split the parameter.** `as_of(valid_ts)` and `as_believed_at(recorded_ts)`, with `AtTime` reading the second. Truthful, and it makes both questions expressible — including "current belief about the past", which is the common one.
3. **Extend `AttributeModeUnstated`'s reasoning.** Refuse `as_of` + `AtTime` without an explicit transaction-time instant, the same way an unstated mode is refused.

Option 1 is the minimum. Option 2 is what the doctrine implies.

### 3.2 `AttributeMode::AtTime` silently degrades after an archive · Medium

`hydrate_at_time` reads `transaction_log` on the **main** database only. It never ATTACHes cold. But `archive()` physically moves superseded log rows to `cold.transaction_log` ([archive.rs:364](../src/temporal/archive.rs:364)–[:383](../src/temporal/archive.rs:383)).

`LOG_ARCHIVABLE` keeps the newest entry per entity hot, so `reconstruct(now)` is safe by construction — that is stated and correct. But `hydrate_at_time(ts)` for a `ts` before the archive cutoff asks for a *superseded* version, which is exactly what went cold. The row is not found, the concept is dropped from the result, and the return type is a `Vec` where "absent" is indistinguishable from "retired" and from "no such concept".

`reconstruct` handles this properly — it has `COLD_FOLD` and `ANCHORED_COLD_FOLD` and takes an `archive_path`. `hydrate_at_time` has neither, and `TraversalBuilder::execute` passes it `db.read_conn()` with no archive path available.

**Fix.** Either thread the archive path into `hydrate_at_time` and use the union form the folds already use, or — cheaper and more honest — detect that `ts` precedes the archive horizon (it is recorded in `cold.archive_horizon`) and return a typed error rather than a quietly short list. The horizon exists precisely so "archived" can be told from "never existed"; this reader does not consult it.

### 3.3 Snapshot loader has no integrity check and no decompression bound · Medium

`load_snapshot` ([snapshot.rs:163](../src/temporal/snapshot.rs:163)) validates an 18-byte header — magic, format version, schema version — then:

```rust
let decompressed = zstd::decode_all(compressed)?;
let state: MaterializedState = bincode::deserialize(&decompressed)?;
```

`save_snapshot`'s own rustdoc concedes the gap: *"a snapshot is read back with no integrity check beyond what zstd and bincode happen to notice"*. Two concrete consequences:

- **No decompressed-size bound.** `zstd::decode_all` expands whatever ratio the frame declares, into a `Vec` with no ceiling. A corrupted or hostile `.snap.zst` in the snapshots directory is an unbounded allocation. This is the sharper of the two.
- **`bincode::deserialize` runs with an infinite byte limit.** bincode 1.3's `deserialize` uses `DefaultOptions`, whose limit is `Infinite`. Serde's cautious-capacity behaviour blunts the worst case — a corrupted length prefix does not become one huge `Vec::with_capacity` — but the deserializer will still work through a corrupt stream to exhaustion rather than refusing it on the declared size.

The threat model here is mild: the directory is derived from the database path and written by the crate itself. But it is a plain filesystem path, snapshots are the first thing a restart reaches for, and the failure mode is process death rather than a typed error — in a crate whose entire posture is that a snapshot is disposable and its loss should cost time and nothing else.

**Fix.**
- Put a CRC32 or xxhash of the compressed payload in the header (there are 18 bytes of structure there already; a v3 format bump is cheap and the version-refusal path is already built and tested).
- Bound both stages: `zstd::Decoder` with a ceiling, and `bincode::DefaultOptions::new().with_limit(n)` for deserialization.
- Both failures should be `SnapshotIncompatible` or `ReplayCorrupt` — never a panic — so the existing "discard and fold from genesis" recovery handles them.

### 3.4 A future `recorded_at` poisons the clock permanently · Medium

`recorded_at_floor` ([clock.rs:36](../src/util/clock.rs:36)) returns `MAX(recorded_at)` across `concepts` and `links`, unbounded above. `SystemClock::new` ([clock.rs:73](../src/util/clock.rs:73)) uses it directly as the floor, and `now()` ([clock.rs:87](../src/util/clock.rs:87)) issues `floor + 1µs` for as long as wall time is behind it.

D-027 handles the *unparseable* case — it logs and falls back to wall clock. It does not handle the *implausible* case. One write from a machine whose clock was a year fast (or one raw-SQL write through `raw()`, which the architecture spec's own §4.7 concedes exists) sets `MAX(recorded_at)` a year ahead. Every subsequent stamp in that process is `floor + kµs`. And because the floor is re-read from the database on every `open()`, the poisoning is **persistent across restarts** — there is no path back to wall-clock time short of editing the ledger, which the delete guards refuse.

The monotonic-`recorded_at` contract is doing exactly what it promises here. The gap is that nothing distinguishes "the ledger is ahead of me by microseconds because another process just wrote" from "the ledger is ahead of me by a year because something was wrong".

**Fix.** In `SystemClock::new`, compare the floor against wall clock and `tracing::warn!` when the gap exceeds some threshold — a minute is generous and would never fire in normal operation. Do not clamp: clamping would break monotonicity, which is the stronger guarantee. But a caller currently gets no signal at all, and a warning at open is the cheapest place to catch it.

### 3.5 `run_writer_actor` cannot return `Err` · Low

[connection.rs:2023](../src/connection.rs:2023) is `async fn run_writer_actor(…) -> Result<()>`, and its only return is `Ok(())` at [:2051](../src/connection.rs:2051). Every `execute` arm answers its responder with the error and returns `LoopCtl::Continue`; nothing propagates upward.

`close()` ([connection.rs:1906](../src/connection.rs:1906)) does:

```rust
match handle.await {
    Ok(res) => res?,                              // structurally always Ok(())
    Err(e) => return Err(DbError::WriterStopped(…)) // JoinError — a panic
}
```

The Wave 4.2 comment says *"the inner `Result` is whatever it returned"*. It is always `Ok`. The half of the change that works — catching a panic via `JoinError` — is real and valuable. The other half is vestigial, and the comment reads as though both are live.

Not a bug: nothing is lost, because per-command errors reach their caller through the responder. But it is a claim in a comment that the code does not support, in a codebase whose standard is that claims are checked. Either give the actor a genuine fatal path (a poisoned connection is the plausible one) or change the return type to `()` and say that the only failure `close()` can report is a panic.

### 3.6 `write_annotations_atomic` bypasses `classify` · Low

The three atomic chunk writers should be symmetric. Two are:

- `write_edges_atomic` ([connection.rs:2761](../src/connection.rs:2761)) → `classify(&tx, e, WriteOp::Edge {…})`
- `write_concepts_atomic` ([connection.rs:2862](../src/connection.rs:2862)) → `classify(&tx, e, WriteOp::Concept {…})`
- `write_annotations_atomic` ([connection.rs:2829](../src/connection.rs:2829)) → **`DbError::Engine(e)`**

`analytics_annotations` has `concept_id TEXT NOT NULL REFERENCES concepts(id)` ([ddl.rs:395](../src/schema/ddl.rs:395)) and `foreign_keys = ON`, so writing an annotation for a concept that does not exist is a real, reachable failure — and it comes back as an opaque `engine: FOREIGN KEY constraint failed` rather than anything naming the concept.

Defensible, since none of `classify`'s current `AbortKind`s apply to this table (no triggers, no guards). But the asymmetry is undocumented, and `classify` already falls through to `DbError::Engine(err)` for unmatched cases — so routing through it costs nothing and makes the three paths identical. If the asymmetry is intentional, a one-line comment saying "this table has no guards, so there is nothing to classify" would close it.

### 3.7 Snapshot rename is atomic but not durable · Low

`save_snapshot` does `sync_all()` on the temp file, then `fs::rename`. The file's *contents* are durable; the *directory entry* is not. On POSIX, a crash between the rename and the OS flushing the directory can lose the rename, leaving the old snapshot in place.

The rustdoc's claim — *"a crash leaves either the old snapshot or the new one, never a splice"* — is correct. Atomicity holds. Only durability of the newer name is at risk, and the consequence is that the next `reconstruct` folds from an older anchor: slower, not wrong, exactly as Doctrine VI says. So this is genuinely low severity.

Worth an `fsync` on the directory handle after the rename on Unix if it is ever cheap to add, and worth a half-sentence in the rustdoc distinguishing atomicity from durability so the claim is exactly true.

---

## 4. Operational and API surface

### 4.1 Strict preemption has no anti-starvation floor · Medium

[connection.rs:2035](../src/connection.rs:2035):

```rust
let ctl = tokio::select! {
    biased;
    Some(cmd) = highpri_rx.recv() => { … }
    Some(cmd) = lowpri_rx.recv()  => { … }
    else => LoopCtl::Break,
};
```

`biased` means the high-priority arm is polled first every iteration. Under sustained interactive load — anything arriving faster than one write per chunk duration — low-priority work makes **zero** progress, indefinitely. `bulk_import`, `write_concepts`, `upsert_embeddings`, `archive`, `rehydrate`, `rebuild_fts` and the chunked shadow rebuild are all on that channel.

This is the documented design ("strict preemption"), and for a UI-backed application it is the right default. Two things make it worth naming anyway:

- **`low_chunked` awaits each chunk before sending the next** ([connection.rs:1831](../src/connection.rs:1831)–[:1844](../src/connection.rs:1844)), so a starved bulk import is a caller blocked on a future that may never resolve, with no timeout and no diagnostic. From the caller's side it is indistinguishable from a hang.
- **The metrics that would reveal it are off by default** (§4.3). `record_turn` samples both queue depths, and `MetricsSnapshot` carries `low_depth_max` — which is exactly the signal — but only under the `metrics` feature.

**Fix.** The cheapest useful change is a starvation counter: track consecutive high-priority turns taken while `lowpri_rx` was non-empty, and `tracing::warn!` past a threshold. It costs one counter in the loop, needs no feature gate, and turns an invisible hang into a log line that names the cause. A full fix — serve one low-priority command every N high-priority turns — is a real policy change and should not be made without measuring what it costs the interactive bound.

### 4.2 No cancellation or progress on bulk paths · Medium

`bulk_import(Vec<EdgeAssertion>) -> Result<usize>` is the whole surface. A 1M-edge import is one future that resolves when it is done. There is no way to:

- observe progress (chunks sent, rows written, current adaptive chunk size),
- cancel cooperatively — dropping the future stops *sending*, but the chunk in flight commits, and the caller learns nothing about how far it got,
- discover the prefix that committed after an error. `low_chunked` returns `Err` and discards the running `written` count ([connection.rs:1841](../src/connection.rs:1841)), so D-011's guarantee — "earlier chunks committed" — is true but the caller is not told *how many*.

That last one is the sharpest. The recovery story D-011 promises is "retry from where it stopped", and the API does not say where it stopped.

**Fix.** Return `Err` carrying the partial count, or add a `bulk_import_with_progress(items, impl FnMut(usize))` variant. The chunk loop already has the number; it just drops it. This is a small change with a real effect on recoverability.

### 4.3 `metrics` is off by default, so the bound is unobservable in the shipped build · Medium

The reasoning in `Cargo.toml` is sound: *"the crate's contract is a latency bound and the instrumentation is not free of risk even where it is free of cost"*. And the Python binding turns it on unconditionally (D-093) with the right argument — a feature flag does not survive into a wheel.

The net effect is an inversion worth stating plainly: **a Python consumer has the complete metrics surface today with no opt-in; a Rust consumer building `macrame-db` normally has none of it.** The binding is the better-instrumented of the two. See §4.4 for the full Python surface and why it is also the safer-shaped of the two.

But the consequence for a Rust consumer is that the default build cannot answer the one question the design is organised around. `budget_violations()` — described in the rustdoc as "the intended first question" — does not exist unless the consumer knew to opt in. A caller hitting a latency problem has no per-`CommandKind` attribution, no hold histogram, no queue depths, and no way to tell "the actor is slow" from "I am queued behind a bulk import". They are left subtracting macro-benchmarks — which is exactly the diagnostic dead end the CodeRadar assessment reached.

D-093's own argument applies here more than it looks: it says an unobservable `CHUNK_BUDGET` is an aspiration. That is true of the default Rust build today. The escape clause it offers — *"a Rust consumer can turn it on"* — is a statement about **capability**, not about what happens. A consumer only goes looking for the feature after they have hit a latency problem and spent a while diagnosing it blind, which is the situation the feature exists to prevent. D-093 applied a universal premise to the one consumer for whom opting in was impossible, and left it unapplied to the default where opting in is merely unlikely.

#### What is actually gated is small

`lib.rs:5` is `pub mod metrics;` with **no `#[cfg]`**. So the following ship in every build today, feature off:

| Already public, unconditionally | Gated behind `metrics` |
|---|---|
| `CommandKind` — 14 variants, `#[repr(u8)]` | `KindSnapshot` ([metrics.rs:431](../src/metrics.rs:431)) |
| `CommandKind::{ALL, COUNT, index, as_str, exempt_from_budget}` | `MetricsSnapshot` ([metrics.rs:450](../src/metrics.rs:450)) |
| `BUCKET_BOUNDS_MICROS`, `BUCKET_COUNT` | `MetricsSnapshot::budget_violations()` |
| `HoldTimer::{start, elapsed}`, `ActorMetrics` (ZST form) | `Database::metrics()` ([connection.rs:1074](../src/connection.rs:1074)) |

The frightening half of the contract — the enum and the bucket boundaries — is **already committed**. Turning the feature on adds two structs and one method. That materially shrinks the objection, and it is why §4.4 rather than this section is where the real risk lives.

#### The cost, counted rather than asserted

From [metrics.rs:298](../src/metrics.rs:298) and [:314](../src/metrics.rs:314), per actor turn with the feature on:

| Call | Atomic operations |
|---|---|
| `record_turn` (per loop iteration) | 3 × `fetch_add`, 2 × `fetch_max` |
| `record_hold` (per turn) | 3 × `fetch_add`, 2 × `fetch_max`, +1 `fetch_add` only when over budget |
| `bucket_of` | ≤ 9 integer comparisons (linear scan, deliberate — [:168](../src/metrics.rs:168)) |

**10–11 relaxed atomic RMWs per turn**, on a fixed-size array, no allocation, no lock. Uncontended with the line hot in L1 that is roughly **50–150 ns**. Against the ~0.8 ms per-transaction floor `quickref.md` publishes:

```
   ~100 ns /   800,000 ns  ≈ 0.01%    (the cheapest turn there is)
   ~100 ns / 2,350,000 ns  ≈ 0.004%   (a 70-row concepts chunk)
```

Per **turn**, not per row — `record_hold` fires once per chunk of 70 concepts, not seventy times. Memory is ~1.6 KB fixed: 14 kinds × 14 `AtomicU64`, allocated once at `open()`.

The only contention path is a monitoring thread calling `db.metrics()`, whose relaxed loads pull those lines to Shared and cost the actor's next `fetch_add` a read-for-ownership. Bounded by sampling rate, and the design already refuses the sharp version — `snapshot()` accepts torn reads rather than locking, because *"locking the actor to produce a report would make the observer a source of the latency it is measuring"*. That is the right call. The residual hazard is a caller polling `metrics()` in a tight loop, which deserves a rustdoc line: ~200 relaxed loads plus a `Vec` allocation per call is fine at Hz and not at kHz.

**Fix — two options, and I would take the first, but only after §4.4.**
- Make `metrics` a **default** feature. Consumers who measured the overhead and want it gone use `default-features = false`.
- Or keep it off and add an always-present minimal counter set — turns, over-budget count per kind — leaving the histogram gated. Smaller permanent surface, more code.

At minimum, `README.md`'s performance section should say plainly: *the default build cannot observe the bound; build with `--features metrics`.*

**How to settle it, given this project's standards.** The above is static analysis. `benches/budgets.rs`'s `write_path` and `chunk_commit` groups already carry a `control/select_1` row by construction (`controlled_group`), and criterion writes baselines to `target/criterion`. Run both groups with and without the feature and compare **control-relative** deltas. Prediction: unresolvable — D-070 measured ~29% session-to-session noise, and a 0.01% effect is four orders of magnitude under that floor. Which is itself the answer worth having: if it is invisible to the best instrument in the repository, it is not what limits anything, and the decision belongs on the stability grounds below.

### 4.4 The `metrics` public surface is frozen by accident · **High** (pre-1.0 window)

Not one item in `src/metrics.rs` carries `#[non_exhaustive]`, and the whole module is `pub` unconditionally. Four break vectors, in descending order of how much they will hurt.

**1. `CommandKind` is an exhaustive public enum.** Adding a variant breaks every downstream `match`. This is **not hypothetical — it has already changed the code**. [connection.rs:2325](../src/connection.rs:2325):

```rust
// No counter of its own: rehydration is the archive path run
// backwards and shares its budget, and a `CommandKind` variant is a
// public enum addition (D-036 periphery, but still a break).
LowPriCommand::Rehydrate { .. } => K::Archive,
```

`Rehydrate` has no counter **because of this constraint**, and the consequence is that rehydrate holds are silently attributed to `Archive` in the histogram. The stability cost has already bought a worse observability story, inside the module whose job is observability. That is the clearest possible evidence the constraint is binding rather than theoretical.

**2. Declaration order is a data contract, and breaking it is silent.** `#[repr(u8)]` plus `pub const fn index(self) -> usize { self as usize }` ([metrics.rs:99](../src/metrics.rs:99)) means any consumer who persists `index()` — into a metrics table, a time-series store, a log — has stored a number whose meaning is the variant's *position*. Reorder the enum and every historical record silently re-labels: no compile error, no runtime error, just wrong dashboards. `CommandKind::ALL` and `COUNT` being public compounds it — a downstream `[T; CommandKind::COUNT]` breaks on any addition.

This is the sharpest of the four, because it is the only one the compiler cannot catch.

**3. `KindSnapshot.buckets: [u64; BUCKET_COUNT]`** — a fixed-size array whose length is a public const derived from a public slice. Changing a bucket boundary changes `BUCKET_COUNT` changes the field's *type*. Destructuring and length assertions break at compile time; anything storing histogram data keyed by bucket index breaks silently, same mechanism as #2.

The Python binding already got this right: [observe.rs:108](../bindings/python/src/observe.rs:108) is `fn buckets(&self) -> Vec<u64>`, an accessor, so the array length never reaches the Python signature. The Rust side is the exposed one.

**4. Both snapshot structs have all-`pub` fields and no `#[non_exhaustive]`.** Adding a metric — a p99, a wait-time histogram, the starvation counter §4.1 recommends — is a breaking change. This is the vector that most constrains future work: every observability improvement costs a minor version.

#### The Python binding already solved all four, which is the argument for doing it in Rust

The metrics exposure is **inverted** from what the feature gate suggests. The wheel is built with `--features metrics` unconditionally (D-093), so every Python consumer has the full surface today with no opt-in, while the default Rust build has none of it:

| Python surface (shipped now) | Backed by |
|---|---|
| `db.metrics() -> MetricsSnapshot` | [database.rs:1081](../bindings/python/src/database.rs:1081) |
| `.violations()`, `.kinds`, `.turns`, `.longest` | `MetricsSnapshot` |
| `.depth_samples`, `.high_depth_mean` / `_max`, `.low_depth_mean` / `_max` | queue-depth counters |
| `KindMetrics.{kind, turns, over_budget, mean, longest, buckets}` | `KindSnapshot` |
| `BUCKET_BOUNDS_MICROS`, `BULK_ATOMIC_WARN_HOLD` | module constants ([lib.rs:138](../bindings/python/src/lib.rs:138)) |
| `chunk_budget_ms()`, `estimate_bulk_hold()` | free functions |

All of it in `__all__`, with `.pyi` stubs and `py.typed`.

And it is wrapped in exactly the shape the Rust side lacks. `CommandKind` never crosses the boundary — [observe.rs:79](../bindings/python/src/observe.rs:79) is `fn kind(&self) -> &'static str`, returning `"assert_edge"`, `"shadow_rebuild"` — and `buckets()` flattens the array to a `Vec<u64>`. Every field is a `#[getter]` on a `frozen` pyclass rather than a public field.

The result is that **the binding is already immune to all four vectors above**:

| Vector | Python exposure | Consequence |
|---|---|---|
| 1 — exhaustive enum | strings | a new variant is a new string in `kinds`; nothing breaks |
| 2 — `index()` as persisted order | never exposed | immune |
| 3 — `[u64; BUCKET_COUNT]` type coupling | `list[int]` | the array length never enters a signature |
| 4 — all-`pub` fields | `#[getter]` properties | adding a metric is purely additive |

So the accessor design is not a hypothetical: it was already built once, on the harder side of an FFI boundary, and it costs nothing. The Rust surface simply never got the same treatment. That is the strongest available argument for the fix below.

**The one risk both sides share, which no attribute fixes.** `BUCKET_BOUNDS_MICROS` is public in Python too (`Final[list[int]]`). Moving a boundary breaks neither language's types and silently re-labels any stored histogram. [observe.rs:104](../bindings/python/src/observe.rs:104) already names it — *"a histogram whose buckets move between builds cannot be compared across them, and comparison across builds is the only reason to keep the numbers"* — so the answer is "do not move them", and that sentence belongs beside the const in `src/metrics.rs`, not only in the binding.

**Why now.** D-036 stabilises the API at 1.0. The crate is at 0.12.0, so all four are currently free to fix and none of them will be afterwards.

**Fix — about four lines.**

```rust
#[non_exhaustive] pub enum CommandKind { … }       // adding a variant → patch release
#[non_exhaustive] pub struct MetricsSnapshot { … } // adding a metric  → patch release
#[non_exhaustive] pub struct KindSnapshot { … }
```

plus making `buckets` private behind `pub fn buckets(&self) -> &[u64]`, matching what the binding already does.

`#[non_exhaustive]` costs downstream a `_` arm on matches and blocks struct-literal construction — which nothing legitimately needs, since only `ActorMetrics::snapshot()` builds these. Declaration order stays a contract regardless of attributes; that one wants a comment on `index()` saying so explicitly, so the next person to tidy the enum knows what they are touching.

**None of this touches the binding.** `#[non_exhaustive]` blocks struct-literal construction and exhaustive patterns in downstream crates; field *reads* like `self.inner.turns` are unaffected, and `macrame-py` never constructs a `MetricsSnapshot` or `KindSnapshot` — it receives them from `db.metrics()` and wraps them. Making `buckets` private is the only change needing a one-line edit in [observe.rs:108](../bindings/python/src/observe.rs:108), from `self.inner.buckets.to_vec()` to `self.inner.buckets().to_vec()`.

**Sequencing matters: this must land before any decision on §4.3.** Defaulting `metrics` on freezes the surface for a far larger population of consumers. Harden first, then default. The immediate payoff of hardening on its own is concrete and independent: `Rehydrate` can have its own counter — **and that fix is additive for Python**, since `kind` is a string, so a new `"rehydrate"` row simply appears. Python users are seeing the same mis-attribution today.

### 4.5 No WAL or checkpoint surface · Low

`configure` ([connection.rs:1974](../src/connection.rs:1974)) sets `journal_mode = WAL`, `busy_timeout = 5000`, `synchronous = NORMAL`, `foreign_keys = ON`, `recursive_triggers = OFF`. It sets no `page_size` and no `wal_autocheckpoint`, so both sit at SQLite defaults — 4096 bytes and 1000 pages, i.e. a ~4.1 MB WAL ceiling, which is the steady state an observer sees.

That is a defensible default. What is missing is any way to influence it. There is no `Database::checkpoint()`, no way to set `wal_autocheckpoint`, and `diagnostic_conn()` is `SQLITE_OPEN_READ_ONLY` below the pragma layer so it cannot run one either. `grep -rn checkpoint src/` returns exactly one hit, and it is a comment.

For a long-running embedded process this is fine — autocheckpoint keeps it bounded. It bites in two places: a caller who wants a compact file at a known moment (backup, container image, shipping the DB as an artifact), and a caller who wants to *tune* the ceiling for their write pattern.

**Fix.** A `Database::checkpoint(mode) -> Result<CheckpointReport>` as a `HighPriCommand`, running `PRAGMA wal_checkpoint(TRUNCATE)` on the write connection. It must go through the actor — it needs the write lock — which is also why a caller cannot build it themselves without `raw()`. Small, self-contained, and it closes a real gap.

### 4.6 What the Python binding does *not* expose · Medium

The binding is close to complete — 40 methods on `PyDatabase` covering essentially the whole `Database` surface, plus `.pyi` stubs, `py.typed`, and 27 mapped error variants. Two omissions are deliberate and correct: `raw()` ([lib.rs:122](../bindings/python/src/lib.rs:122) carries a sentinel comment forbidding it) and `read_conn()`, superseded by `diagnostic_query` / `explain`.

The rest are gaps, in descending order of consequence.

**1. Clock injection is entirely absent — `Clock`, `FakeClock`, `SystemClock`, `open_with_clock`.** Zero occurrences anywhere in `bindings/python/`. `PyDatabase::open` takes `snapshot_every_entries` and `snapshot_poll_seconds` — a good, idiomatic flattening of `SnapshotCadence` — and has no clock parameter at all.

This is the same shape as **defect K**, which D-062 spent three releases fixing on the Rust side, left unfixed on the Python side. The consequence is precisely what D-062's rustdoc says it was: *"every test that wanted to assert on one had to either avoid it or drive a raw connection"*. `tests_py` cannot write a bitemporal test that asserts on `recorded_at`, because it cannot control the transaction-time axis — and the Rust API has had the fix since 0.6.0.

Non-trivial to expose (`Arc<dyn Clock>` across an FFI boundary needs a `#[pyclass]` wrapping a `FakeClock` and an `advance()` method), but it is the one gap that costs test *capability* rather than convenience.

**2. Model introspection: `registered_models()` and `declared_dimension()`.** `register_model` and `upsert_embeddings` are both exposed. Neither reader is. So a Python caller can create an embedding table and write to it, and cannot ask which models exist or what dimension one was declared at — the only way to learn a model's dimension is to write a wrong-sized vector and read `DimMismatchError`. Write without read is an asymmetry worth closing; both are cheap, being plain queries.

**3. `MAX_ARCHIVE_SESSIONS` is not exposed, and `archive_windowed` is.** The method can raise `ArchiveWindowError`, which exists *because* of that limit, and the constant it is measured against is unreachable from Python. This is directly against the precedent set two constants over — [lib.rs:136](../bindings/python/src/lib.rs:136) exposes `BULK_ATOMIC_WARN_HOLD` with the reasoning *"so a caller can compare an estimate against it rather than hard-coding 250ms"*. The same sentence applies here and was not followed.

**4. `chunk_rows::{EDGES, CONCEPTS, ANNOTATIONS, EMBEDDINGS}`.** `chunk_budget_ms()` is exposed; the four ceilings are not. Since 0.12.0 these are the adaptive loop's starting point and upper bound, so they are the numbers a caller reasoning about a bulk write actually wants — and §5.1.6's warning that stamp counts are not reproducible run to run is unreadable from Python without them.

**5. `shadow_step(ShadowStep)`.** `rebuild_current_chunked` is exposed; the manual seam is not, and `ShadowStep` / `ShadowOutcome` have no Python types. Arguably correct to omit — its rustdoc attaches an obligation the looping version cannot get wrong ("`epoch` must be handed back to `Swap`, or the archive interlock is defeated"), and handing an obligation like that across an FFI boundary is a reasonable thing to decline. Worth recording as a decision rather than leaving it looking like an oversight.

**6. Minor.** `escape_fts5_query` is used internally at [database.rs:900](../bindings/python/src/database.rs:900) but not exported, so a caller composing their own FTS query through `diagnostic_query` cannot escape it. `TraversalBuilder::build_sql()` is absent, though `explain()` covers the diagnostic need. `save_snapshot` / `cleanup_expired_snapshots` are absent, so a Python caller cannot force an anchor outside the cadence or `close()`.

**Fix.** Items 2–4 are a handful of lines each and close real asymmetries. Item 5 wants a sentence in `lib.rs`'s convention block, beside the `raw()` sentinel — that block is already the place where "deliberately not exposed" is recorded. Item 1 is a small design task and should be scoped on its own; the argument for it is D-062's, unchanged.

### 4.7 `Database` is not `Clone` · Low

`Database` is `Send + Sync` (asserted at compile time in the binding, [runtime.rs:140](../bindings/python/src/runtime.rs:140)) but not `Clone`, so multi-threaded Rust callers must wrap it in `Arc` themselves. That is normal and fine. Worth one line in the README's concurrency section, since the type owns channel senders that are individually cheap to clone and a reader may reasonably wonder why the handle is not.

---

## 5. Tests and CI

The suite is 320 tests across 28 binaries plus doc-tests, and the *design* of the test suite is the strongest part of this repository. `index_plan_tests` inverting the direction of plan-pinning, `doc_sync_tests` checking prose against constants, `packaging_tests` asserting `cargo metadata`'s workspace members, and the const-block assertion in `util/limits.rs` are all above the norm. Four gaps.

### 5.1 R15 reaches the main suite · **High**

This session reproduced it. Run 2 of `cargo test --features metrics --no-fail-fast`:

```
error: test failed, to rerun pass `--test wave1_regression_tests`
  process didn't exit successfully: wave1_regression_tests.exe (exit code: 0xc0000005, STATUS_ACCESS_VIOLATION)
```

Summary line: **28 binaries, 320 passed, 0 failed, exit 101.** Exactly the reporting hazard `.cargo/config.toml` describes — the crash is invisible in the pass/fail counts and only the exit code and the missing `test result:` line reveal it.

The measurement discipline around R15 is exemplary — `.cargo/config.toml` is 150 lines of honest measurement including two explicitly refuted hypotheses, and D-147 correctly identifies that a flaky test biases the crash rate downward. What the published framing understates is the blast radius: the narrative is *quarantined property binaries are the problem, the main suite under `RUST_TEST_THREADS=1` is manageable*. Here the main suite went red on run 2 of 2, in `wave1_regression_tests`, which is not quarantined.

`.cargo/config.toml` already flags this as unexplained: *"every binary is 0/15 alone while the suite that runs those same binaries serially is 5/10, all under `RUST_TEST_THREADS = "1"`. If concurrent opens are required, the suite-level runs are finding concurrency that per-binary runs do not, and this data does not say where."*

The named suspect — `Database::open` taking three connections, four with the cadence, which can land on different tokio workers even with libtest serialised — is testable and has not been tested. **That is the single highest-value experiment available.** If it is right, the mitigation is inside this crate rather than upstream: open the three connections sequentially under a process-wide mutex in `open_inner`, or on a current-thread runtime. The existing `examples/r15_soak.rs` harness and the `--arm claim` / `--arm control` structure are already the right instrument for it.

Concretely: add an arm that opens N `Database` handles with the connections serialised behind a `static Mutex`, against a control that does not, at n in the hundreds. That is a day of machine time and it either closes R15 or eliminates the leading hypothesis.

### 5.2 The index registry is one-directional · Medium

`index_plan_tests.rs` asserts every entry in `CREATE_INDICES` has a query that seeks on it, and that the unread set is empty. Excellent, and it found two dead indexes.

It cannot find the inverse — a hot query with no index — because the registry is keyed *by index*. §2.1 and §2.2 are five queries and one `OR` arm that no test can currently notice.

**Fix.** Add a second registry keyed by *query*: a list of `(label, sql)` for the crate's hot reads, asserting each plan contains no `SCAN <table>` for a table above some row count in the fixture. That catches "this query lost its index" and "this query never had one" with the same assertion, and it is the same `EXPLAIN QUERY PLAN` machinery already in the file. The candidate list is short — `recorded_at_floor`, `LINKS_ARCHIVABLE`, `CONCEPTS_ARCHIVABLE`, `shadow::begin`, the shadow catch-up, `hydrate_at_time`.

### 5.3 No performance regression detection · Low, and deliberate

D-055's argument against gating on absolute durations is correct: arbitrary CI hardware makes them meaningless. But the consequence is that nothing catches a *relative* regression either. `benches/budgets.rs` is well built — `controlled_group` structurally guarantees a `control/select_1` row per group, which is exactly the right instrument — and criterion writes baselines to `target/criterion`, so the machinery for "compare against the last run on this machine" already exists and nothing uses it.

A non-gating CI job that runs the benches and posts the criterion delta as a PR comment would surface regressions without ever failing a build on hardware noise. The control row makes the comparison interpretable; that is the hard part and it is done.

### 5.4 No fuzzing on the snapshot loader · Low

`load_snapshot` parses an untrusted-shaped binary format (header + zstd + bincode) and is the first thing a restart touches. It is the one place in the crate where a malformed input reaches an allocator decision (§3.3). A `cargo-fuzz` target over the byte string, or even a proptest that mutates a valid snapshot, would be cheap. The `.proptest-regressions` replay discipline is already in place for the property binaries.

---

## 6. Documentation

The docs are the strongest asset here and I would change very little. Three notes.

### 6.1 The release-history table stops at 0.9.0 · Low

`docs/architecture/README.md` has no rows for 0.10.0, 0.11.0 or 0.12.0. Flagged during the 0.12.0 release and deliberately left; recorded here so it does not get lost.

### 6.2 `Cargo.toml`'s `metrics` cost model describes 0.11.0 · Medium

`Cargo.toml` justifies the feature default with a claim about cost:

> it is close to free of cost: **with the feature off `HoldTimer` reads no clock** and `ActorMetrics` is a ZST whose methods are empty, so the actor loop compiles to what it compiled to before.

`src/metrics.rs:179` says the opposite, under its own heading:

> **# This clock is no longer optional (0.12.0, W1)** — Until 0.11.0 the field was `#[cfg(feature = "metrics")]` and `elapsed()` returned `Duration::ZERO` in a default build… So the clock is unconditional and **only the histogram is still gated**.

W1 made it so deliberately and for a good reason: `next_chunk_size` needs the hold as a control signal in every build, or `bulk_import` would size chunks off `Duration::ZERO` — a value that reads as comfortably under budget — and grow every chunk to the ceiling, in exactly the builds nobody measures. [connection.rs:2144](../src/connection.rs:2144) computes `elapsed()` unconditionally and hands it to both `record_hold` (a no-op without the feature) and the caller's `ChunkOutcome`.

So the number is already measured and already crosses the channel in every build; **the feature flag now only decides whether anything remembers it.** Enabling `metrics` roughly doubles an overhead already sitting at ~0.01%.

This matters beyond tidiness: the stale paragraph is the stated justification for a decision (§4.3), and it argues against a cost that no longer exists. It is also D-144's exact shape, and `doc_sync_tests` does not reach `Cargo.toml` — worth extending the gate, since `Cargo.toml` comments are where feature-level decisions are recorded and nothing checks them.

### 6.3 On the comment-to-code ratio

`connection.rs` is 3,113 lines and a large majority is rustdoc. Individual comments run to 60+ lines with tables, measurement sessions and refuted hypotheses. This is not padding — it is genuinely the reasoning, and several comments record something that could not be recovered from the code (why 90 stays at 90 when the measurement says 20; why pipelining was implemented, measured and removed; why the floor is the operating point rather than a backstop).

Two costs worth naming, both mild:

- **Navigation.** Finding `write_concepts` means scrolling past ~200 lines of constant derivations. `chunk_rows`, `next_chunk_size`, `CHUNK_BUDGET`, `estimated_bulk_hold` and `CHUNK_FLOOR` are ~400 lines of tuning theory sitting in front of the `Database` impl. Splitting the sizing law into `src/connection/chunking.rs` would leave `connection.rs` as the actor and the API. Purely mechanical, no behaviour change.
- **Drift risk.** Prose that cites measurements is prose that can silently go stale. The project knows this — D-144 exists because of it, `doc_sync_tests` was built for it, and this session's own quickref:671 correction came out of exactly that failure. The mitigation is working; it just needs to keep pace with the prose, and the prose is growing.

---

## 7. Recommended order

**Do first — cheap, high return**

1. `CREATE INDEX idx_links_recorded_at ON links (recorded_at)` — closes four full scans including the one on every `open()`. §2.1
2. `CREATE INDEX idx_links_target ON links (target_id)` — takes concept archival from quadratic to seekable. §2.2
3. **Correct `Cargo.toml`'s `metrics` cost paragraph.** It states the clock is gated; W1 made it unconditional. It is the stated justification for the §4.3 decision and it is currently false. §6.2
4. Document the `as_of` axis mix on `TraversalBuilder::as_of`. §3.1
5. Return the partial count from `low_chunked` on error. §4.2
6. Point `upsert_concept` / `assert_edge` at their bulk equivalents. §2.3
7. **Export the three missing Python constants** — `MAX_ARCHIVE_SESSIONS`, `chunk_rows::{EDGES, CONCEPTS, ANNOTATIONS, EMBEDDINGS}` — following `BULK_ATOMIC_WARN_HOLD`'s own precedent at [lib.rs:136](../bindings/python/src/lib.rs:136). A few lines. §4.6

**Do next — a day or two each**

8. **Harden the `metrics` public surface — this is the pre-1.0 window and it closes.** §4.4
   - `#[non_exhaustive]` on `CommandKind`, `MetricsSnapshot`, `KindSnapshot`.
   - `buckets` private behind `pub fn buckets(&self) -> &[u64]`, matching [observe.rs:108](../bindings/python/src/observe.rs:108).
   - Comment on `index()` stating that declaration order is a persisted contract — the one break the compiler cannot catch, and it binds Python too, since `BUCKET_BOUNDS_MICROS` is a module constant there.
   - **Binding impact: none.** `#[non_exhaustive]` blocks construction and exhaustive patterns downstream, not field reads, and `macrame-py` never constructs these types. Private `buckets` needs one line changed in `observe.rs`.
   - **Immediate payoff, independent of anything else: give `Rehydrate` its own `CommandKind`.** It currently reports as `Archive` ([connection.rs:2325](../src/connection.rs:2325)) purely because the enum addition was a break, so rehydrate holds are attributed to the wrong command in **both** languages today — and the fix is additive for Python, since `kind` is a string.
9. Run the R15 sequential-open experiment. Highest-value single item in this report. §5.1
10. `spawn_blocking` around snapshot save/load. §2.4
11. Bound and checksum the snapshot format (v3 header). §3.3
12. Query-keyed index plan registry. §5.2
13. Starvation counter in the actor loop. §4.1 — a new `MetricsSnapshot` field, so item 8 is its prerequisite if it is to be exposed, plus one `#[getter]` in `observe.rs` to reach Python.
14. `registered_models()` and `declared_dimension()` on `PyDatabase` — closes the write-without-read asymmetry on the vector surface. §4.6

**Consider — needs a decision, not just work**

15. **Make `metrics` a default feature. Blocked on item 8** — defaulting it on freezes the surface for a much larger population, so harden first, then default. Optionally settle the cost question first with the `write_path` / `chunk_commit` bench groups against their `control/select_1` rows; expect it to be unresolvable under D-070's noise, which is itself the answer. §4.3, §5.3
16. **Clock injection for Python.** The one binding gap that costs test *capability* rather than convenience — `tests_py` cannot assert on `recorded_at` today, which is defect K's exact shape on the side that never got D-062's fix. Needs a `#[pyclass]` over `FakeClock` with `advance()`. §4.6
17. Split `as_of` into two parameters, or refuse the ambiguous pairing. §3.1
18. `Database::checkpoint()`. §4.5
19. Cold-log union in `hydrate_at_time`, or a horizon error. §3.2
20. Index-based interior for the graph algorithms — benchmark first. §2.5
21. Record `shadow_step`'s omission as a decision in `lib.rs`'s convention block, beside the `raw()` sentinel — or expose it. Either is fine; silence is what is not. §4.6

### The metrics thread, in one place

Five items above are one story and are easiest to read together. Note the asymmetry they start from: **Python already has the full metrics surface and the safer-shaped one; the default Rust build has neither.**

| Step | Item | Rust | Python | Why this order |
|---|---|---|---|---|
| 1 | Fix the `Cargo.toml` cost claim (§6.2) | doc | — | It is false, and it is the argument everything downstream leans on |
| 2 | `#[non_exhaustive]` + private `buckets` (§4.4) | 4 lines | 1 line in `observe.rs` | Free now, impossible after 1.0. Unblocks adding metrics at all |
| 3 | `Rehydrate` gets its own kind (§4.4) | 2 lines | free — new string | Falls out of step 2; fixes a live mis-attribution in both languages |
| 4 | Starvation counter (§4.1) | field + counter | one `#[getter]` | The first *new* metric, and the one §4.1 needs |
| 5 | Decide the default (§4.3) | judgement | unaffected | Only meaningful once the surface is safe to commit to |

Steps 1–3 are perhaps half a day together and carry no design risk. Step 5 is the only one needing a judgement call, and it gets easier once 1–3 are done. Python is unaffected by step 5 in either direction — the wheel already builds with the feature on.

### Superseded by the road map

The ordering above is this review's own recommendation, written before F-28
through F-30 existed. It is kept as the diagnosis's view of its own priorities,
but the scheduled version — all 30 findings across 0.13.0 and 0.14.0, with the
dependency arguments for why the order is what it is — is
[Macrame Road to 1.0](Macrame%20Road%20to%201.0.md). Where the two disagree,
the road map is current.

Nothing above is reversed there. The road map's rejections — `bulk_session()`,
raw pragma passthrough, a `Durability` knob — are all proposals raised *after*
this review, refused, and recorded with the reason in its §16. §4.1's own
position, that a forced-yield policy needs measurement before it needs an
implementation, is carried across unchanged.

---

## Appendix: what this review did not cover

- `migrations.rs`'s ladder was read for structure (10 rungs, `verify` at each) but not audited rung by rung. `migration_tests.rs` has 28 tests over it.
- `vector_filter.rs`'s cost model and `hybrid.rs`'s RRF were read for shape only.
- `tests_py/` and the Python-side suite were not run.
- No profiling was done. Every performance claim above is from reading query shapes against the declared indexes, or from measurements the codebase itself publishes. The two index findings are the ones I would expect to hold; §2.5's magnitude is a guess and is labelled as one.
