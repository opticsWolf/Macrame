# Macrame Update Plan — v0.16.0

**From:** 0.15.0 (schema v15, snapshot payload v4, public surface 1,624 items)
**To:** 0.16.0 (**schema v16 — one rung**, and it is item 6 of the review, not the read plan)
**Source:** [Macrame Codebase Review v0.15.0](Macrame%20Codebase%20Review%20v0.15.0.md) §5, read against [road map §16](Macrame%20Road%20to%201.0.md) (W13) and [D-223](architecture/s13-decision-register.md#d-223)'s named escalation
**Shape:** one wave that is *placed* first because everything after it is cheaper once it exists, then the review's order as written, renumbered into releases.

---

## 0. What this release is, and what it deliberately is not

**It is:** the read path learns to spell its lineage SQL once. Today the branched read — ancestry, the churned set, `links_cut`, the recorded-time fold, the nearest-lineage window — is assembled in three readers (`TraversalBuilder::walk_cte`, `query_as_of_edges_on`, `diff_sql`) and once more, per key, in the overlap guard. The four spellings agree today because [D-227](architecture/s13-decision-register.md#d-227) made them agree after four releases in which one of them did not. This cycle moves the assembly into one crate-private lowering, gives the lowering a name, and then makes the changes the review asked for *against the lowering* rather than against each reader.

**It is W13, brought forward.** The road map wrote W13 as the first item to cut if 0.15.0 grew, and 0.15.0 grew, so it was cut. The review argues the cut was the right call for 0.15.0 and the wrong call for what comes after: C-7 (the trunk pays for the branches), C-8 (the probe cap bounds memory, not work) and C-10 (`reconstruct` cannot answer for one lineage) are each a fourth spelling waiting to be written, and the next `query_as_of` defect is the same defect D-227 repaired.

**It is not a query language.** Road map §16 said *a query AST, not a macro* and that holds: no `macrame_query!`, no text syntax, no parser. The typed plan is a struct the builders lower into, and the first three releases of the wave do not even make it public — they make the SQL byte-identical and move it, so the plan-pinning tests and golden strings are the proof that nothing changed.

**It is not a performance release in W13's first releases and it is one in W13.2 and W14.** The lowering itself buys no speed. What it buys is that the one shape D-223 named as the escalation — *the naive filter emitted when no ancestor holds a post-cutoff row* — lands in one place and the three readers get it on the same day.

**One rung, late.** Schema v16 exists for C-4's composite index on the fold's partition and nothing else, and it is scheduled after the plan lowering so that the plan pin it needs is pinned once.

---

## 1. The structural fact everything follows from

**The lineage prelude is one sequence, and only three things vary.** Every branched read the crate emits is:

```
WITH RECURSIVE
  lineage{tag}(branch_id, dist, cutoff)                       — ancestry from ?{branch_slot}
  [links_at_tx(…)]                                            — the recorded-time fold, if a recorded instant is set
  [churned{tag}(…), links_cut{tag}(…)]                        — the hybrid cut, if no recorded instant is set
  visible{tag}(…)                                             — the nearest-lineage window over the source
… the reader's own query, joined to the last relation …
```

What differs between `walk_cte`, `query_as_of_edges_on` and `diff_sql` is:

1. **which placeholder holds the branch** — `?5` in the traversal ([`BRANCH_SLOT`](../src/graph/builder.rs)), `?2` in the as-of read, `?1` and `?2` in `diff`;
2. **which placeholder holds the recorded instant**, when there is one — the traversal's `recorded_slot(shape)`, and the other two readers do not take one at all;
3. **the tag** — empty everywhere except `diff`, which lowers twice as `_a` and `_b` and joins the two `visible` relations.

The reader's *own* query then names one relation: `visible` under `Resolved`, and under `Trunk` either `links_current` or `links_at_tx`. That choice is the only other thing the readers compute for themselves, and they compute it three different ways (`link_source`, a `match` on the shape, and a hard-coded `visible_a`/`visible_b`).

So the lowering is a function of exactly `(shape, branch_slot, recorded_slot, tag)`, and its output is `(prelude CTEs, source relation)`. That is small enough to write in an afternoon and it is the whole of W13.1. Everything else in the wave is a *consumer* of that function gaining an argument.

**The write path is not a fourth reader, yet.** `overlap_candidates_resolved` and `retire_from_resolved` use a per-key spelling (`key_rows`, `churned_key`, `visible_key`, `resolved_key`) because the guard has the key in hand and a full `links_cut` would be a table scan under a write lock. The lowering gains a key-narrowed form in W13.3; until then the guard keeps its own text and its own tests.

---

## 2. W13 — the read plan, five releases

### W13.1 · 0.15.1 — the lowering, crate-private, SQL byte-identical

A new `src/graph/plan.rs` holding:

```rust
pub(crate) struct Resolution<'a> {
    pub shape: LineageShape,
    pub branch_slot: usize,
    pub recorded_slot: Option<usize>,
    pub tag: &'a str,
}

pub(crate) struct Lowered {
    pub ctes: Vec<String>,   // in prelude order; empty under Trunk without a recorded instant
    pub source: String,      // the relation the reader joins: visible{tag} | links_at_tx | links_current
}

pub(crate) fn lower(r: &Resolution<'_>) -> Lowered;
```

`links_at_tx_cte` moves from `builder.rs` into the lowering, parameterised by slot and tag; the five CTE generators in `lineage.rs` stay where they are and the lowering calls them in the fixed order. The three readers call `lower` and stop assembling. `query_as_of_edges_on`'s two arms become one `format!` with an optional prelude.

**The acceptance is that nothing observable moves.** `tests/index_plan_tests.rs`, `tests/graph_tests.rs`, `tests/bitemporal_plan_tests.rs`, `tests/branch_read_tests.rs`, `tests/branch_diff_tests.rs` and every golden string in `builder.rs`'s unit tests pass unchanged — not re-pinned, *unchanged*. The public surface stays at 1,624 items because every new item is `pub(crate)`. New unit tests in `plan.rs` assert that the three readers' preludes for the same resolution are one string.

### W13.2 · 0.15.2 — the third shape (C-7, D-223's escalation)

`LineageShape` gains `TrunkOnForked`: chosen when `branches` has more than one row **and** no ancestor of the requested lineage holds a `links_current` row recorded after that ancestor's cutoff — which for `main` is always, because the trunk has no ancestors. The lowering emits the `Trunk` prelude and the reader's edge filter gains `AND l.branch_id = ?{branch_slot}`, covered by `idx_lc_lineage_cut`.

The probe that chooses it is one query against `branches` and `links_current`, run where `lineage_shape` already runs. `examples/branch_traversal_probe.rs` measures the three shapes on the same database and the numbers go into the register entry, next to D-219's 1.1–1.3× and D-223's 1.45×, which are the costs this shape removes for the trunk. The three readers gain the shape in one release because the lowering is where it lives; that is the review's A-1 argument made concrete, and D-227's warning is the reason the release is not allowed to land the shape in fewer than all three.

**Shipped as 0.15.2, [D-244](architecture/s13-decision-register.md#d-244), with three departures from the paragraph above.** The condition is the *root* — `parent_id IS NULL` on a forked ledger — and not the general post-cutoff probe: the root's answer is structural and free, the general probe is a query per branched read that no workload yet justifies, and it stays D-223's escalation on record. The predicate is `+l.branch_id`, not `l.branch_id`: served as an equality it takes the walk off its covering index and onto `idx_lc_lineage_cut` for a scan of the whole trunk per hop, which is D-231's prediction arriving; the plan is pinned. And the measurement found something the plan did not ask about: the transaction-time fold ran as a co-routine inside the recursive step on every shape since 0.13.2 — 10.6 s against 59 ms — because the only reads the probes ever timed were the branched ones, whose window materialises on its own. The numbers were taken by a temporary unit test rather than the example, since the example writes its traversal SQL longhand and the question here is what the *builder* emits.

### W13.3 · 0.15.3 — the overlap guard lowers too

`Resolution` gains an optional `key: Option<KeySlots>` that narrows `links_cut` and the fold to one `(source, target, type)` before the window runs. `overlap_candidates_resolved` and `retire_from_resolved` become `lower(&Resolution { key: Some(…), … })` plus their own tail. The per-key CTE text in `lineage.rs` is deleted once the guard's tests pass against the lowered form, and `examples/branch_write_probe.rs` (or the existing write-cost bench group) confirms the guard's plan did not lose its seek.

After this release the crate has one lineage spelling and D-227's failure mode — a reader that agrees with the others by accident — has no place left to happen.

### W13.4 · 0.15.4 — `ReadPlan` becomes public

```rust
#[non_exhaustive]
pub struct ReadPlan { pub branch: Option<BranchId>, pub valid: Option<String>, pub recorded: Option<String>, pub limit: Option<usize> }
impl ReadPlan { pub fn new() -> Self; pub fn on(self, BranchId) -> Self; pub fn valid_at(self, &str) -> Self; pub fn recorded_at(self, &str) -> Self; }
impl TraversalBuilder { pub fn plan(self, ReadPlan) -> Self; }
impl Database { pub async fn edges(&self, ReadPlan) -> Result<Vec<Edge>>; }
```

This is the release F-34 was about: the three qualifiers stated once and composed, with the builders lowering into the same struct the crate uses internally. `as_of_valid`, `as_of_recorded` and `branch` on `TraversalBuilder` stay (C-11 decides their fate in W15.3) and `plan()` sets all three. Appendix A gains the items, Appendix D.1's count moves, `public-api.txt` is regenerated, and the Python binding gets `ReadPlan` in the same release so that W6's finding — a Rust-only layer opened in the release that created it — is not repeated.

### W13.5 · 0.15.5 — `limit` pushed into the walk (C-8)

`ReadPlan::limit` becomes a `LIMIT ?n` on the walk CTE's outer `SELECT`, and `vector_filter.rs` stops truncating after the fact. `CostEstimator` then receives the count that was paid for. The plan-pinning test for the walk gains the limited form. Public surface: `ReadPlan::limit(self, usize)` and `TraversalBuilder::limit(self, usize)`.

---

## 3. W14 — what the crate costs at scale

The review's items 1, 2 and 3, in that order, because they are what a deployment hits first.

### W14.1 · 0.15.6 — keyed projection repair in `archive_session` (C-1)

The archive arm rebuilds `links_current` in full after deleting archived rows. Replace it with a repair keyed on the rows the arm deleted: for each `(source, target, type, valid_from, branch)` touched, re-derive that key's current row from surviving `links`. A bench group `archive_session` measures the rebuild before and after at three populations, and the register entry carries the numbers.

### W14.2 · 0.15.7 — `hot_log_reach(ts)` (C-2)

`hot_log_answers_for` takes a timestamp and ignores it. `hydrate_at_time` and the two reach checks consult `hot_log_reach(ts)` — the earliest recorded instant the hot log still answers for — instead of the boolean. Small, and it closes a false "cannot answer" on databases whose archive horizon is behind the asked instant.

### W14.3 · 0.15.8 — `ActorState` (C-5, C-6, C-24, A-3)

The write actor is stateless between turns and pays for it three times: `hot_log_is_intact` counts the log on every recorded read, the single-edge path makes two round trips it could prepare once, and `check_lineages` recomputes an answer that changes only under `Fork` and `ArchiveBranch`. One `ActorState { prepared, lineage_generation, intact }` owned by `run_writer_actor`, invalidated by the two operations that can change it. The lineage cache here is the one A-2 later reads from.

---

## 4. W15 — correctness and the API before 1.0

### W15.1 · 0.15.9 — typed refusal for a rehydrate that needs an archived lineage (C-3)

`rehydrate` after `archive_branch` currently fails with whatever the cold file's `branches` absence produces. It refuses with a `DbError` variant naming the lineage, classified under `ErrorKind::Branch`, so the gate from [D-242](architecture/s13-decision-register.md#d-242) fails to compile until the classification exists.

### W15.2 · 0.15.10 — schema v16: `idx_txlog_entity_lineage` (C-4)

The fold partitions on `(entity_id, branch_id)` and orders by `seq_id`; no index covers that. One rung adding the composite index, the fold's plan pinned in `index_plan_tests.rs` through the lowering (so the pin is written once for every reader), and the write cost measured by the existing bulk-import bench before and after. The ladder's rung tests from [D-231](architecture/s13-decision-register.md#d-231) cover the climb.

### W15.3 · 0.15.11 — builders and `#[non_exhaustive]` (C-11)

`Tuning`, `TraversalBuilder` and `SnapshotCadence` have public fields, so any field added after 1.0 is a major version. Each gains a builder, the struct gains `#[non_exhaustive]`, and the fields stay readable. Breaking, so it lands in this cycle or not before 1.0. The surface count moves and Appendix D.1 with it. `api-review-0.16.0.md` is written against `api-review-0.14.0.md`'s method ([D-212](architecture/s13-decision-register.md#d-212)).

### W15.4 · 0.15.12 — a lazy read-only handle behind `diagnostic_conn` (C-9)

One `connect()` per call becomes one `OnceCell<Connection>` per `Database`, opened on first use and dropped with the database.

---

## 5. W16 — ancestry in Rust, and the hygiene batch

### W16.1 · 0.15.13 — resolve ancestry once, in Rust (C-10, A-2)

The `lineage` CTE becomes a bound `VALUES` table produced from W14.3's cache: `(branch_id, dist, cutoff)` per ancestor. The lowering emits it instead of the recursive CTE, differentially tested against the CTE it replaces on the branch fixture generator. `reconstruct_on(branch)` and a pure `resolve(&[Branch], id) -> Vec<Ancestor>` come with it. This is the form Turso can run, which is the Jacquard argument for doing it here first.

### W16.2 · 0.15.14 — hygiene (C-12 … C-22)

The DDL substring match in the shadow swap, cold DDL outside the session transaction, `save_and_prune`'s `JoinError`, `registered_models` and `LIKE`, the two contradicting comments, `abort_kind` on message text, the conservative closed-interval arm, the quadratic hybrid rank lookup, `verify_snapshot_chain` on one link, the polling cadence, `rehydrate` per id. One release, one register entry with a row per item.

### Ongoing, not a release (A-4, A-5, A-6)

`temporal/archive.rs` split into its four modules; a lineage fixture generator for the tests; `dev/**` in CI's branch filter; the four fuzz targets; the closing flag on the Python side. Each lands when it is convenient and none of them gate 0.16.0.

---

## 6. The 0.16.0 release itself

Merge to `main` after W16.2, tagged. `docs/releases/v0.16.0.md` written before the merge, as [D-212](architecture/s13-decision-register.md#d-212)'s habit and 0.14.18 did: schema v15 → v16, `DbError` 41 → 42, surface 1,624 → *n*, decisions D-243 … D-2xx, and the acceptance list below read as evidence.

---

## 7. Work items

| # | Release | Wave | Item | Files | Gates |
|---|---|---|---|---|---|
| 1 | 0.15.1 | W13.1 | `plan.rs` lowering; three readers consume it; SQL byte-identical | `src/graph/{plan,builder,lineage,mod}.rs`, `src/temporal/as_of.rs`, `src/branch.rs` | every plan pin and golden string unchanged; surface 1,624 |
| 2 | 0.15.2 | W13.2 | `TrunkOnForked`; three readers; the fold materialised — **done** | `lineage.rs`, `plan.rs`, `builder.rs`, `subgraph.rs`, `as_of.rs` | numbers in D-244; two plan pins |
| 3 | 0.15.3 | W13.3 | key-narrowed lowering for the guard | `plan.rs`, `lineage.rs`, `connection.rs` | guard plan keeps its seek |
| 4 | 0.15.4 | W13.4 | public `ReadPlan`; Python parity | `src/plan.rs` (public), `bindings/python` | Appendix A/D.1, `public-api.txt` |
| 5 | 0.15.5 | W13.5 | `limit` in the walk | `plan.rs`, `builder.rs`, `vector_filter.rs` | walk pin gains the limited form |
| 6 | 0.15.6 | W14.1 | keyed archive repair | `temporal/archive.rs`, `benches` | bench group `archive_session` |
| 7 | 0.15.7 | W14.2 | `hot_log_reach(ts)` | `temporal/replay.rs`, `as_of.rs` | reach tests |
| 8 | 0.15.8 | W14.3 | `ActorState` | `connection.rs` | single-edge path round trips counted |
| 9 | 0.15.9 | W15.1 | typed rehydrate refusal | `errors.rs`, `branch.rs` | kind gate |
| 10 | 0.15.10 | W15.2 | schema v16, composite index | `schema.rs`, `migrations`, `index_plan_tests.rs` | rung tests; fold plan pinned |
| 11 | 0.15.11 | W15.3 | builders + `#[non_exhaustive]` | `tuning.rs`, `builder.rs`, `snapshot.rs` | `api-review-0.16.0.md` |
| 12 | 0.15.12 | W15.4 | lazy diagnostic handle | `connection.rs` | — |
| 13 | 0.15.13 | W16.1 | ancestry in Rust | `plan.rs`, `branch.rs` | differential test against the CTE |
| 14 | 0.15.14 | W16.2 | hygiene batch | various | one register row per item |
| 15 | 0.16.0 | — | release note before merge; merge; tag | `docs/releases/v0.16.0.md` | §8 |

---

## 8. What must be true before this is called done

1. One function in the crate emits lineage SQL, and `grep -c "ROW_NUMBER() OVER (PARTITION BY l.source_id"` over `src/` returns 1.
2. `TrunkOnForked` is chosen for `main` on every database with more than one branch, and the traversal probe shows the trunk's read within noise of a single-branch database.
3. The overlap guard's resolved form is produced by the same lowering as the readers.
4. `ReadPlan` is public in both languages and `public-api.txt` and Appendix D.1 agree on the count.
5. A `limit` stops the walk rather than truncating its result, and the plan pin says so.
6. `archive_session` on a database with 10⁶ current edges and one archived branch does not rebuild `links_current`.
7. Schema v16 climbs from every fixture on the ladder, and the fold's plan uses the composite index in all three readers.
8. `Tuning`, `TraversalBuilder` and `SnapshotCadence` are `#[non_exhaustive]` with builders, and the surface review is written.
9. The ancestry `VALUES` form answers identically to the CTE on the fixture generator's lineages, including the churned ones.
10. The suite passes under `python scripts/run_rust_suite.py --features metrics --attempts 3` on all three platforms and the feature-off run passes on Ubuntu.

---

## 9. Rejected before starting

* **A `macrame_query!` macro or any text syntax.** Road map §16's reason stands: the algebra is the deliverable, a syntax is additive later, and a proc-macro emitting SQL strings is the least testable artefact the crate could produce.
* **Making `ReadPlan` public in W13.1.** The first release's whole proof is that the SQL did not move; adding surface at the same time means the public-API gate and the plan pins fail together and neither failure is diagnostic.
* **Landing `TrunkOnForked` before the lowering.** It is a small change in `edge_filter_sql` and `link_source`, and it is also a change in `query_as_of_edges_on` and the guard, and that is D-227's failure mode again with a fourth spelling. The lowering is one release and it removes the mode.
* **Resolving ancestry in Rust in W13.** A-2 depends on the actor's lineage cache (W14.3), and doing it before the cache exists means a second cache on the read side that the write side does not invalidate.
* **Cutting the v16 rung by adding the index to v15.** Schema versions are a ladder and a live database at v15 has no index; the rung is how it gets one.
* **Folding C-11's builders into W13.4.** `plan()` is additive and C-11 is breaking. They share a struct and not a release, so a caller pinned to `0.15` gets the plan and is not broken by it.
* **A performance figure in this document.** Nothing in the review was measured, and [D-070](architecture/s13-decision-register.md#d-070)'s rule applies: every cost claim above is a hypothesis with a named experiment, and the register entry that closes each release carries the number.
