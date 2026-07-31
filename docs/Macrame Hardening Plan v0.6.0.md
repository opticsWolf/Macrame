# Macrame — Hardening Plan toward a Future-Proof 1.0

**Status:** proposed, 2026-07-30. Supersedes nothing; sequenced after v0.5.6 Wave 5.
**Basis:** a read of the crate and docs against the v0.5.6 architecture set, plus direct
measurement of the SQLite semantics each item depends on.

**Measurement caveat, stated once and applying throughout.** The probes below were run on
**SQLite 3.50.4** (via `node:sqlite`), not on **libSQL 0.9.30**, and on synthetic fixtures
rather than through the crate. Every mechanism they exercise is core SQLite behaviour rather
than a libSQL extension, so the results are expected to carry — but this project's own
standard is that a number is not a number until it is measured on its own harness. **Each
item below names the bench or test that has to reproduce it before the change lands.**
Nothing here should be taken on the strength of the probe alone.

---

## 0. What this plan is reacting to

The v0.5.6 architecture is unusually well reasoned, and its weakest point is not any of the
things most readily read as weaknesses. Ranked by what actually threatens a 1.0:

| Rank | Risk | Why it ranks here |
|---|---|---|
| 1 | **The benchmark fixture is a tree.** | It has already produced one wrong architectural conclusion (D-070) and it is the reason the largest performance defect in the crate is invisible. Every performance claim about graph work in §8.8 is conditional on a graph shape that real knowledge graphs do not have. |
| 2 | **The traversal enumerates paths, not nodes.** | Consequence of rank 1. Measured at 1,600× on a 328-edge graph. This is the scaling wall, not `links_current` and not string keys. |
| 3 | **Nothing is observable in production.** | Every latency claim in the crate is a `cargo bench` figure. A `CHUNK_BUDGET` that cannot be checked in situ is an aspiration, and "future-proof" is not a property a benchmark can confer. |
| 4 | **Repair costs more than the damage.** | `rebuild_current` is three O(E) passes, and `archive()` runs one inside its own transaction. |
| 5 | Schema gaps (§4.7) | Real, small, well documented, and already argued. Cheapest tier to close. |

Notably **not** on this list: the single-writer actor (correct, and the bottleneck is
schedulable rather than structural), `TEXT` primary keys (retired on measurement by D-063 —
see T3.3 for the argument that was *not* made), and the FTS5/`VACUUM` hazard (closed by
D-071 with a test).

---

## Tier 0 — Measured defects · ✅ **COMPLETE (D-076, D-077, D-078)**

All three delivered 2026-07-30, each reproduced on libSQL 0.9.30 first. Worth recording what
that cost: **every one of the three contained a claim that did not survive being checked** —
T0.1's "within noise" on trees (it is 8–10% slower), T0.2's prescribed ordering (which would
have deleted a check that was doing real work) and its "the published cost does not include
this" (it does), and T0.3's scope (NaN is the opposite of a §4.7 gap, not a weaker one). The
diagnoses were right in all three cases and the *justifications* were where the errors were.
That is an argument for T4.1 rather than against this plan.


### T0.1 — The traversal enumerates paths, not nodes *(the headline item)* — ✅ **DELIVERED, D-076**

> **Delivered 2026-07-30.** Reproduced on libSQL 0.9.30 before anything was changed
> (`examples/traversal_diag.rs`): the `walk` row counts came out **identical** to the table
> below — 1,555 / 9,331 / 37,449 / 299,593 — on a different engine, which is the strongest
> available evidence that the fixture here was reconstructed correctly. Shipped timings on the
> layered fixture: 1.8 → 0.1, 11.1 → 0.1, 51.5 → 0.1, **402.9 → 0.2 ms**.
>
> **Two things in this item were wrong and are corrected in D-076.**
>
> 1. **"No regression on the existing fixture … within noise" is not what libSQL measures.**
>    At best-of-15, stable to a tenth of a millisecond across runs: 1,011 nodes 1.6 ms either
>    way, 5,051 nodes 8.9 → 9.5, 10,101 nodes 17.8 → 19.6 — **8–10% slower on trees at scale**.
>    `UNION` maintains a dedupe b-tree over every queued row and on a tree it never removes
>    anything. The trade (~2,000× on graphs against ~9% on trees) is overwhelming and it is
>    still a trade, so it is recorded as one. A first attempt measured 18% and that was harness
>    error — the baseline arm discarded rows while the shipped arm allocated 10,101 `String`s.
> 2. **The bonus is real and was taken.** `/` is free again; only `|` remains reserved.
>
> Everything else in this item held: the equivalence argument, the plan shape (verified against
> the *shipped* string, not a hand copy), and the extraction of the two duplicate CTEs into one
> `TraversalBuilder::walk_cte`. The property test asked for is in `integrity_property_tests`
> and passes at 512 cases. D-070 is corrected, not deleted.

**Where.** [`TraversalBuilder::build_sql`](../src/graph/builder.rs) and the duplicate CTE in
[`load_subgraph_with`](../src/graph/subgraph.rs).

**What it does now.** The recursive CTE uses `UNION ALL` and carries a `path` column, so
`walk` holds **one row per distinct simple path** to each node, not one row per node. The
trailing `SELECT DISTINCT` collapses the duplication *after* the work is done.

**Why nothing caught it.** The `star_of_stars` fixture is a **tree**. In a tree there is
exactly one path to each node, so path count equals node count and the pathological term is
identically 1. D-070 investigated `load_subgraph`'s superlinearity on that fixture, correctly
identified `USE TEMP B-TREE FOR DISTINCT` as an O(E log E) term, measured two candidate fixes,
and concluded the growth was *"explained, inherent to producing a deduplicated result, and
left alone."* **That conclusion is true of trees and false of graphs.** The fixture chose the
answer.

**Measured.** Layered graph, branching factor 8, `depth = 6`, comparing the current CTE
against a `UNION`-on-`(node_id, depth)` rewrite:

| Fixture | Edges | `walk` rows, current | `walk` rows, rewrite | ms current | ms rewrite |
|---|---|---|---|---|---|
| 4 layers × 6 | 114 | 1,555 | 25 | 2.0 | 0.1 |
| 5 layers × 6 | 150 | 9,331 | 31 | 8.9 | 0.2 |
| 5 layers × 8 | 264 | 37,449 | 41 | 49.6 | 0.2 |
| 6 layers × 8 | 328 | **299,593** | **49** | **479.2** | **0.3** |

Row growth is multiplicative in branching factor per hop. A 328-edge graph — smaller than any
fixture in `benches/` — takes **479 ms** for a depth-6 traversal. The byte budget does not
protect against this: it bounds the *result*, and the result here is 328 edges. The cost is in
producing it.

**The rewrite.**

```sql
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION                                  -- not UNION ALL: dedupes the queue
    SELECT l.target_id, w.depth + 1
    FROM walk w JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
      {edge_filter}
)
```

The `path` column and the `INSTR` cycle check both **disappear**. `UNION` dedupes rows as they
enter the queue, so `walk` is bounded by `V × (depth+1)` and termination comes from the depth
bound rather than from path inspection.

**Why it is equivalent, argued not just measured.** The current form restricts to *simple*
paths. The rewrite admits any walk. These give the same reachable set: if a walk of length
*k ≤ D* reaches *X*, excising its cycles yields a simple path of length *≤ k* that also
reaches *X*. So simple-path reachability within *D* equals walk reachability within *D*, and
the two forms differ only in how much redundant work they do to establish it.

**Verified.** Node sets and projected edge sets compared at depths 1/2/3/5 across: 3-cycle,
self-loop, 2-cycle-with-tail, diamond (two paths to one node), complete K5, an expired edge
that must be excluded, and a disconnected graph. **All match.** `EXPLAIN QUERY PLAN` is
byte-identical between the two forms — same `SEARCH l USING COVERING INDEX
idx_lc_traversal_cover (source_id=? AND valid_from<?)`, so D-042's covering index is
undisturbed.

**No regression on the existing fixture.** Star-of-stars, depth 3, best of 5:

| Nodes | Edges | current ms | rewrite ms |
|---|---|---|---|
| 1,011 | 1,010 | 2.4 | 2.3 |
| 5,051 | 5,050 | 12.6 | 12.0 |
| 10,101 | 10,100 | 23.7 | 23.9 |
| 20,101 | 20,100 | 55.0 | 54.8 |

Within noise. The rewrite is free where the current form is optimal and 1,600× where it is not.

**Bonus, worth its own note.** D-061 reserved `/` in identifiers *solely* so the path check
could match a delimited element after variable-length ids made `INSTR(path, id)` unsafe.
Deleting the path column **frees `/`** — only `|` remains reserved, for
`transaction_log.entity_id`. A read-path optimisation retires half of an identity constraint.

**Before it lands:**
- A `benches/` group over a **clustered/dense** fixture, reproducing the table above on
  libSQL 0.9.30.
- A property test asserting the two CTE forms return identical node and edge sets over
  generated graphs — this is exactly the class `integrity_property_tests.rs` already models.
- The two CTEs are near-duplicates in two files and must be rewritten together, or extracted.
  They have already drifted once (D-073's `edge_types`/`min_weight` gap).
- D-070's decision entry needs correcting, not deleting: its analysis was right and its
  fixture was wrong, and that distinction is the lesson.

---

### T0.2 — Repair costs more than the damage — ✅ **DELIVERED, D-077**

> **Delivered 2026-07-30.** The audit is **roughly half the whole repair**, measured at
> 4K/16K/40K rows in `links` (`examples/repair_diag.rs`): ≈15/61/190 ms of the rebuild.
> Removed from the archive path; `rebuild_current` still verifies. Quoted as "about half"
> deliberately — the audit's own cost is stable across runs (179–212 ms at 40K over five) but
> the rebuild total is not (318–428 ms for identical work), so the *ratio* reads 42–61%
> depending on the run. First draft of this entry said "56–61%" from a single run.
>
> **The prescribed order was wrong and is inverted in D-077.** This item justifies dropping the
> audit because "the archive already knows the projection is correct because it just derived it
> from the definition". There was no *the* definition — the projection existed **twice**,
> byte-identical, in `rebuild.rs` and `audit.rs` — so the post-rebuild audit was not tautological
> at all: it was a live check that the two copies still agreed. Dropping it first would have
> removed the only thing keeping them honest. The projection is extracted to one
> `LATEST_BELIEF_PROJECTION` **first**, and only then is skipping the audit a saving rather than
> a silent loss of coverage.
>
> **"The published archive cost estimate does not include this" is not correct.** Both figures —
> the 26.8 ms in §5.1's exemption table and §9's ≤ 30 s — are stated over `archive()` end to end,
> and `benches/budgets.rs` measures the public handle method. The re-derivation was always inside
> them. The real defect is that it was **unattributed**, which is what §5.7 now fixes.
>
> **A finding worth more than the optimisation.** `rebuild_within` reprojects *all* of `links`,
> so the archive's repair term scales with the **surviving** table, not the batch archived —
> §9's "per 100K closed intervals" is the wrong variable, and archiving a fixed volume costs
> more as the ledger grows. That is T1.1's problem and is recorded, not fixed, here.
>
> Coverage moved rather than shrank: an ungated test archives and then audits from outside,
> mutation-verified (removing the rebuild makes it fail). The equivalent property test existed
> but sits behind `property-tests`, so plain `cargo test` had been proving nothing here.

`rebuild_within` ([rebuild.rs](../src/integrity/rebuild.rs)) is not one O(E) pass. It is:

1. `DELETE FROM links_current` — O(E)
2. the window-function reprojection — O(E log E)
3. `audit_current` — **two more** `EXCEPT` passes over the full projection, O(E log E) each
   ([audit.rs](../src/integrity/audit.rs))

And `archive()` calls `rebuild_within` **inside the archive transaction**
([archive.rs:187](../src/temporal/archive.rs)). So every archive pays copy + delete + a full
re-derivation + two audit passes, all under one write lock. The published archive cost
estimate does not include this.

**Actions.**
- Make the post-rebuild `audit_current` **opt-in** (`rebuild_current` verifies;
  `rebuild_within` from the archive path does not — the archive already knows the projection
  is correct because it just derived it from the definition).
- Record the true archive cost in §5.7. The current figure understates it.
- This is the strongest argument for T1.1 and T1.2 and neither the review nor the
  hardening proposal made it.

---

### T0.3 — §4.7 invariant 3 is misstated — ✅ **DELIVERED, D-078**

> **Delivered 2026-07-30.** Confirmed on libSQL 0.9.30 rather than carried over from the
> SQLite 3.50.4 probe, and through **three** doors rather than one: `assert_edge(f64::NAN)`,
> a raw `INSERT` binding NaN, and a raw `INSERT` computing `0.0/0.0` in the engine — which
> never crosses the binding layer and so could have behaved differently. All three:
> `NOT NULL constraint failed: links.weight`. Nothing lands.
>
> Row 3 now reads "non-negative", with the correction stated beside it rather than quietly
> dropped — a reader who trusted the old row would have written a NaN guard they did not need.
> The loader's `is_nan()` arm stays, commented as unreachable.
>
> **One refinement to the framing.** This is not a weaker instance of §4.7's property, it is the
> opposite of it: §4.7 exists to record where the schema is *silent*, and here the schema is
> strict, so the section claimed a hole in its own subject. The new test therefore runs in the
> **failing direction** — the other three assert the storage layer accepts what the API refuses
> and break if a gap closes; this one asserts refusal and breaks if a gap ever *opens*. Which is
> the direction that matters: the alternative failure mode is a shortest path over NaN, where
> every comparison is false and the answer is silently arbitrary.

---

### T0.3 — original text

§4.7 lists *"edge weights are non-negative **and not NaN**"* as refused only by
`load_subgraph`. **NaN is already refused by the storage layer.** SQLite stores NaN as NULL,
so `weight REAL NOT NULL` rejects it on insert — probed: `INSERT INTO t(w REAL NOT NULL)
VALUES(NaN)` → `NOT NULL constraint failed`. The `weight.is_nan()` branch at
[subgraph.rs:474](../src/graph/subgraph.rs) is therefore unreachable through the crate's own
write path.

**Actions.** Correct §4.7 to claim only what is true (the gap is negative weights, not NaN);
add a `storage_boundary_tests.rs` case pinning that NaN is refused, so the claim fails loudly
if libSQL ever diverges; keep the loader's `is_nan()` branch as defence for cold-file and
pre-migration reads, with a comment saying it is unreachable from this crate's writes.

---

## Tier 1 — Bounded actor latency · ✅ **COMPLETE (D-079, D-080, D-081, D-082)**

The actor is the right design. Its problem is that three operations are exempt from the
latency bound by contract, and two of the three did not have to be.

> **All four delivered 2026-07-30**, in the order T1.4 → T1.1 → T1.3 → T1.2, because the item's
> own claim that T1.4 is "a precondition for the rest of the plan" turned out to be literally
> true: every measurement below is read from the counters T1.4 added, and the alternative —
> wall time on the caller's side of the channel — includes queueing and answers a different
> question.
>
> **The recurring finding, in three of the four items: the loop must live on the handle.** T1.1
> spells this out for `archive_windowed` and T1.2 names it for `CREATE TABLE … AS SELECT`, but
> it is one fact and it applies to every "chunk it" item in this tier. The actor is
> single-threaded, so N small transactions inside one command produce N small transactions
> inside **one hold** — smaller transactions, identical latency. Only sending N commands returns
> the actor to its `select!`, which is where a high-priority assertion gets to jump the queue.
> `actor_metrics_tests` pins it for both by asserting turn counts, which is the one fact about
> chunking invisible from outside the actor.
>
> **What Tier 1 bought, measured:** the archive's longest hold 3,326 → 768 ms (4.3×); the
> rebuild's 353 → 47 ms (7.6×), and 2.3× cheaper in total as well; `write_bulk_atomic` still
> uncapped but no longer unpredictable, with an 18-second case now visible in a log line before
> it happens rather than in a support ticket after.
>
> **What it did not buy.** None of the three exempt operations now fits `CHUNK_BUDGET` — the
> rebuild's swap is still 15× over it, and windowing the archive trades total throughput for
> latency at small sizes. The exemptions remain exemptions; what changed is that their cost is
> measured, bounded by something the caller chooses, and stated in numbers.

### T1.1 — Window the archive — ✅ **DELIVERED, D-080**

> **Delivered 2026-07-30.** `archive_windowed(cutoff, window)`, no schema change, `archive()`
> kept. Boundaries derive from the oldest `recorded_at` actually present rather than from a
> fixed epoch, and the last boundary is `cutoff` itself rather than `start + n·window`, which
> would overshoot and archive rows the caller excluded.
>
> **The item does not say where the loop goes, and that is the whole decision.** Putting it
> inside the actor's `Archive` arm would produce N small transactions inside **one** hold —
> smaller transactions, identical latency, since the actor is single-threaded and nothing else
> writes until its turn returns however many `COMMIT`s that turn contains. The loop is on the
> handle, one command per session, which is what returns the actor to its `select!` in between.
> `actor_metrics_tests` asserts `Archive` *turns* equals sessions reported — the one fact about
> windowing invisible from outside the actor. This is the same trap the item itself names for
> `CREATE TABLE … AS SELECT` in T1.2, met one item earlier.
>
> **A prerequisite the item does not mention.** D-077 established that `rebuild_within` costs
> O(*surviving* `links`) regardless of how much the session archived. Windowing therefore
> multiplies the repair term by the session count, and a naive implementation makes the archive
> *several times slower in total*. `archive_session` now skips the rebuild when its `DELETE`
> removed nothing — sound, because `links_current` is a function of `links`. Without it,
> windowing is a latency improvement paid for with an unacceptable throughput regression.
>
> **Measured, and the trade inverts with size.** Longest hold read from the actor's per-kind
> high-water mark (T1.4), not from the caller's side of the channel:
>
> | fixture | window | sessions | longest hold | total |
> |---|---|---|---|---|
> | 8,000 keys | one session | 1 | 3,326 ms | 3,326 ms |
> | 8,000 keys | 1 h | 9 | **768 ms** | 4,160 ms |
> | 2,000 keys | one session | 1 | 260 ms | 260 ms |
> | 2,000 keys | 1 h | 9 | **117 ms** | 671 ms |
>
> At 8,000 keys: 4.3× less hold, total flat within noise (two runs: 3,362 / 4,160 ms against
> 3,730 / 3,326 ms single-session), and the intermediate windows were repeatedly *faster* than
> one session — the dominant terms are superlinear in the surviving table, so each session
> leaves less for the next. At 2,000 keys the same change costs 2.6× the total work to halve
> the hold.
>
> So the item's "it should stop being an escape hatch and become the default" is **not taken**.
> Windowing pays when the backlog is large, which is when the unwindowed hold is a problem in
> the first place; making it the default would make small databases 2.6× slower to fix a
> latency they were not suffering. Both are public, both are measured, and the numbers are
> published beside each other so the choice is informed rather than defaulted.
>
> A window is refused, not clamped, when it is zero or would need more than
> `MAX_ARCHIVE_SESSIONS` (4,096) — clamping would archive over boundaries the caller did not
> choose, invisibly.

---

### T1.1 — original text

`archive(cutoff)` is one transaction whose size is set by how long since the last one.
Replace with `archive_windowed(cutoff, window)`: archive to *T₁*, then *T₂*, … each a complete
atomic session with its own marker, horizon row, and rebuild.

D-012's atomicity requirement is **per session** — copy-then-delete must not be split. N small
sessions satisfy it exactly as one large one does. The only new obligation is that a partial
run leaves a coherent intermediate state, which it does: each session commits a valid horizon.

This is scheduling, not schema, and it is what Appendix C already names as the escape hatch
(*"phased, per-table archiving"*). It should stop being an escape hatch and become the default.

### T1.2 — Chunked shadow rebuild — ✅ **DELIVERED, D-082**

> **Delivered 2026-07-30.** `rebuild_current_chunked`, with `rebuild_current` kept — the two
> differ in contract, not only in speed. All three mechanical facts re-probed on libSQL 0.9.30
> and all three hold, including the rename/trigger trap and transactional DDL.
>
> **A fourth the item does not name, and it is silent.** Step 2 says to build the shadow with
> `CREATE TABLE … AS SELECT` and reasons only about that statement's duration. It copies rows
> and **nothing else** — no primary key, no `CHECK`s. The swap succeeds, the row count is
> right, and the next `INSERT INTO links` dies inside `trg_links_current_sync` with
> "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint". Probed. The shadow
> is built from `CREATE_LINKS_CURRENT_TABLE` with the name substituted, and every swap test
> writes afterwards rather than only counting rows.
>
> **The item's step 3 asks the right question and the answer forces the design.** Index names
> are global and SQLite has no `ALTER INDEX … RENAME`, so the shadow cannot carry
> `idx_lc_traversal_cover` while the live table holds it — and temporary names would leave
> `links_current` indexed under names absent from `CREATE_INDICES`, so the next migration would
> create a second copy of each. `DROP TABLE` frees the names, reusable in the same transaction
> (probed), so the swap pays the index build. The chunking moves the **projection** off the
> lock, not everything. Step 5's "microseconds" is not achievable and was not achieved.
>
> | `links` | `rebuild_current` | turns | chunked, longest turn | chunked, total |
> |---|---|---|---|---|
> | 4,000 | 26.0 ms | 7 | **11.1 ms** | 20.4 ms |
> | 16,000 | 110.6 ms | 19 | **16.6 ms** | 65.6 ms |
> | 40,000 | 353.5 ms | 43 | **46.8 ms** | 152.6 ms |
>
> 7.6× less hold at the largest size, and the advantage grows with the table. The unexpected
> result is the last column — the chunked path is also **2.3× cheaper in total**, where T1.1's
> windowing cost more. Same cause as the index constraint: the shadow fills *unindexed* and
> pays one bulk index build instead of maintaining three indexes per row. Still not within
> budget: 46.8 ms is 15× `CHUNK_BUDGET`, so `ShadowRebuild` is deliberately not exempt from the
> violation count.
>
> **An interlock the item does not call for.** The catch-up finds work by `recorded_at`, which
> cannot see a *deletion* — so an `archive` interleaving with the build would let the shadow
> resurrect archived history. The actor counts archives; `Begin` reports the count, `Swap`
> compares, and a mismatch drops the shadow and returns a new `RebuildInterrupted` — distinct
> from `RebuildFailed`, because "the repair did not run, `links_current` is untouched, retry" is
> a different fact from "the repair ran and did not repair". Verifying instead would cost
> O(E log E) under the lock, which is what this exists to remove.
>
> The item's "keep the current in-place `rebuild_within` as well" is taken, and for its stated
> reason plus two more: `rebuild_current` is one act, audits itself, and works inside a caller's
> transaction. The chunked path does none of the three and cannot decline to.

---

### T1.2 — original text

The shadow-swap idea is right and the naive form does not work. Two things must be true that
are easy to miss:

- **`CREATE TABLE … AS SELECT` is a write.** It runs on the actor's connection and holds the
  write lock for its full O(E) duration. There is no "outside the write lock" in a
  single-writer design. The latency win comes *only* from chunking the build so the actor
  interleaves — not from the swap.
- **The swap must drop the triggers first.** Probed: after `DROP TABLE links_current`,
  `ALTER TABLE links_current_new RENAME TO links_current` fails with `error in trigger
  trg_sync: no such table: main.links_current`, because rename reparses the whole schema
  (SQLite ≥ 3.25) and both `links` triggers reference `links_current`. `DROP TRIGGER` →
  `DROP TABLE` → `RENAME` → recreate triggers works (probed). `PRAGMA legacy_alter_table=ON`
  also works but disables the reference fixups the modern rename exists to perform — do not
  use it.

**Shape.**
1. Record `build_start = MAX(recorded_at)` from `links`.
2. Build `links_current_shadow` in chunks keyed by `source_id` range, one chunk per actor
   turn, each sized to `CHUNK_BUDGET`. The live table stays live and trigger-maintained
   throughout, so readers and `trg_links_single_open` keep working.
3. Build the two indexes on the shadow (also chunkable only as whole-index operations —
   measure; this may be the largest single hold and may need its own concession).
4. **Catch-up pass**: reproject only keys with `links.recorded_at >= build_start`. Bounded by
   writes during the rebuild, not by E.
5. One transaction: drop the two triggers, drop `links_current`, rename shadow, recreate
   triggers, final catch-up. Microseconds. DDL is transactional in SQLite (probed).

**Keep the current in-place `rebuild_within` as well** — `archive()` calls it inside an
already-open transaction and cannot interleave. Two paths, one for repair-while-live and one
for repair-inside-a-transaction, is the honest answer.

### T1.3 — `write_bulk_atomic` stays uncapped — ✅ **DELIVERED, D-081**

> **Delivered 2026-07-30.** Uncapped, warned, and predictable: a `tracing::warn!` above
> `BULK_ATOMIC_WARN_HOLD` (250 ms — fifteen frames, deliberately far above `CHUNK_BUDGET`,
> since this path is exempt by contract and a warning that fires on correct behaviour gets
> filtered out), plus a public `estimated_bulk_hold` so a caller can ask *before* committing
> to a size. The warning fires on the caller's task, before the send, so it carries their span
> and names the call site that chose the batch.
>
> **The prescribed model is wrong twice, and measuring it is what showed both.** "Rows ×
> measured per-row cost" assumes linearity. `write_edges_atomic` opens with
> `reject_overlaps_within`, which compares every **pair** in the batch — and the quadratic
> term's constant depends on the batch's *shape*, because the loop's early `continue` on
> mismatched `(source, target, type)` is 16× cheaper than the pairs that fall through to
> `Interval::overlaps`.
>
> | rows | fan-out to distinct targets | corrections to one relationship |
> |---|---|---|
> | 5,000 | 414 ms | 1,386 ms |
> | 20,000 | **2,618 ms** | **18,057 ms** |
>
> Two batches of one length, 7× apart — and a size-only model errs toward *under*-prediction,
> the only direction that matters for a warning. So the estimate counts matching pairs with one
> `HashMap` pass: `73 µs · rows + 5.5 ns · mismatched + 86 ns · matching`, within 1.03–1.11× of
> measurement over 500–20,000 rows in both shapes. A unit test pins both measured figures, so a
> coefficient edited without re-measuring fails.
>
> **This item's diagnostic found a defect in T1.4's instrumentation.** The hold was timed around
> the whole `execute` call, so it was recorded *after* each arm answered its `oneshot` — two
> tasks, so a caller reading `metrics()` right after its own write could be scheduled first and
> miss it. `bulk_atomic_diag` reported a 20,000-row batch as a **0 ms** hold, every time.
> Invisible to a dashboard, which is worse: the instrumentation would have been believed while
> being wrong precisely when someone tried to verify it. Timing now lives in a `Turn` type whose
> `answer` records and *then* sends.

---

### T1.3 — original text

D-014 is correct: the batch is one act under one stamp, and capping breaks the guarantee the
method exists to provide. But "uncapped and undocumented at the call site" is not the same as
"uncapped by decision." Add an estimated-hold warning above a threshold
(`tracing::warn!` with rows × measured per-row cost), and state the ceiling in rustdoc in
milliseconds rather than in prose. A caller who stalls the UI for 8 seconds should have been
able to predict it from the signature.

### T1.4 — Make the actor observable — ✅ **DELIVERED, D-079**

> **Delivered 2026-07-30.** `cargo test --features metrics`. All four counters shipped:
> queue depth per channel (mean and high-water, sampled *before* the turn), a per-kind hold
> histogram whose `3_000 µs` bucket boundary **is** `CHUNK_BUDGET` so "fraction within budget"
> is a prefix sum, an over-budget count per kind, and the longest hold since open with the
> command that caused it. Reached through `Database::metrics()`.
>
> **Taken further than the item asked in one place, and it turned out to matter.** The three
> contractual exemptions (`write_bulk_atomic`, `archive`, `rebuild_current`) are excluded from
> the *violation* count but not from measurement. Counting them as violations would have made
> the violation count noise on any database that archives — and their durations are exactly
> what T1.1 and T1.2 exist to shrink, so they must still be recorded.
>
> **Two defects of my own, both caught by tests rather than by review, and the second is the
> interesting one.** `turns` was incremented where the depth sample is taken — at the top of
> the loop, which is right for depth and wrong for turns, since an idle actor had already
> counted a command that had not arrived. The total disagreed with the sum of its own
> breakdown by one, permanently. Now two fields: `depth_samples` and `turns`, both exposed.
>
> Then: the longest hold and its kind are packed into one `AtomicU64` so a reader cannot see a
> duration from one turn beside a kind from another. The first packing put the kind in the
> **high** bits — and the update is a `fetch_max` on the packed word, so the "longest hold"
> became the hold with the largest *enum index*. A 3 ms `write_concepts_chunk` outranked a
> 10 ms `rebuild_current` because its variant is declared later. The unit test passed: its
> slowest hold was also its highest-indexed kind. Only the integration test, running real
> commands whose cost and declaration order disagree, exposed it. There is now a unit test
> whose two orderings deliberately conflict.
>
> **The loop has one shape with the feature on or off** — `HoldTimer` reads no clock and
> `ActorMetrics` is a ZST when `metrics` is absent — arranged as two impls of one type rather
> than `#[cfg]` in the loop body. That is not about the nanoseconds; a conditional in the loop
> is how the instrumented and uninstrumented paths drift until only one of them is real.

---

### T1.4 — original text

Everything the crate knows about its own latency lives in `benches/`. In production there is
no way to answer "is the 3 ms bound holding?" — and D-059 already established that it does
**not** hold on a large database, by a factor of 15.

Minimum viable set, behind a `metrics` feature:
- high/low queue depth, sampled at each actor turn;
- per-command-kind hold duration histogram;
- count of holds exceeding `CHUNK_BUDGET`, by kind;
- longest hold since open, with the command that caused it.

This is a precondition for the rest of the plan, not a nice-to-have: T1.1 and T1.2 are both
"make the tail bounded," and neither can be validated in the field without it.

---

## Tier 2 — Schema · ✅ **COMPLETE (D-083, D-084)**

### T2.1 — `CHECK (weight >= 0.0)` on `links` — ✅ **DELIVERED, D-083**

> **Delivered 2026-07-31.** Schema **v7**. Shipped as
> `CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')` on `links`, and the
> same on `COLD_SCHEMA`. All three of the item's corrections were taken. Two further clauses
> were needed that the item does not mention, and both are worse than the negative weight it
> was written for — found by probing, not by reading.
>
> **`typeof(weight) = 'real'`.** `REAL` is an *affinity*, not a type. `'abc'` cannot convert,
> so it is stored as TEXT — and every text value sorts above every numeric one, so
> `'abc' >= 0.0` is **true** and the plain CHECK passes it. Reading such a row back as `f64`
> does not error: it hits `unreachable!("invalid value type")` inside libsql 0.9.30 and
> **panics**, in whatever unrelated query first touches the row.
>
> **`weight < 9e999`, and two wrong predictions preceded it.** The item predicted the CHECK
> would admit `+∞` — correct — and concluded the loader guard must therefore stay. But the
> guard is `weight < 0.0 || weight.is_nan()`, so it does not catch `+∞` either; nothing did.
> My correction to that was that it is harmless, since IEEE infinity stays totally ordered and
> Dijkstra still terminates. Also wrong, and the suite caught it: **an infinite weight makes
> the transaction log unreplayable.** The log trigger serialises to JSON, JSON has no infinity,
> and every later `reconstruct()` — including the one `close()` runs — fails with
> `ReplayCorrupt`. Under Doctrine III that is corruption, not eccentricity.
>
> **The guard stays, for neither of those reasons.** A `CHECK` on `links` does not reach
> `links_current`, which is derivative and deliberately unconstrained, nor pre-v7 cold files.
> Those are where `NegativeEdgeWeight` is still reachable; `graph_tests` now plants its fixture
> in `links_current`, its old `links` fixture having stopped working the moment the constraint
> landed — the tripwire working.
>
> **Migration cost named, and one case refused.** Full rebuild of `links`: O(rows), ~2× peak
> disk, the only rung on the ladder that rewrites a ledger table. Rows are copied **verbatim**,
> so a database already holding a rejected weight cannot be migrated — the rung refuses before
> touching anything, with a count and an example row, because clamping or dropping would be an
> edit to an assertion. The v7 table shape is pinned as literal text rather than reading
> `ddl::CREATE_LINKS_TABLE`, so the rung stays a statement about the past.

---

### T2.1 — original text

Closes the one §4.7 gap the register calls genuinely open. Three corrections to the obvious
form:

- Write `CHECK (weight >= 0.0)`. **Not** `AND weight IS NOT NULL` — `REAL NOT NULL` already
  covers that, including NaN (T0.3).
- **Do not** delete the loader guard. `CHECK (weight >= 0.0)` accepts `+∞` (probed), and more
  importantly the guard also covers reads of files the CHECK never applied to — the cold
  schema at [archive.rs:30](../src/temporal/archive.rs) is a bare `weight REAL NOT NULL`.
  Either add the CHECK to `COLD_SCHEMA` too, or keep the guard. Preferably both.
- **Name the migration cost.** SQLite has no `ADD CONSTRAINT`, so the rung is a full rebuild
  of `links` — the largest table — and its rename hits the same trigger-reparse trap as T1.2.
  Pre-1.0 with D-032 this is a baseline re-issue, which is cheap; it will not be cheap later.
  **This is an argument for doing it now rather than after 1.0.**

### T2.2 — `rowid_pk INTEGER PRIMARY KEY` on `concepts` — ✅ **DELIVERED as a recorded deferral, D-084**

> **Delivered 2026-07-31.** The item's own decision is to defer, so the deliverable is the
> decision, not code. Recorded with its **trigger condition** — the first rung of whichever
> release implements concept archival or erasure, carrying `rowid_pk`, the FTS delete trigger
> and a `rebuild_fts()` together — because an omission with no trigger is indistinguishable
> from an oversight.
>
> **One tension worth stating, which the item does not.** T2.1 argues a ledger-table change
> should be taken *now*, pre-1.0, while D-032 makes it a cheap baseline re-issue and before
> D-036 freezes it. That argument applies here too. The difference is that T2.1's constraint is
> independently correct — it closes a live panic and a live corruption path whatever ships
> next — whereas `rowid_pk` is inert until its companions exist, and adding an inert column to
> beat a deadline is how a schema accumulates fields nobody can explain. If 1.0 approaches with
> erasure still unscheduled, D-084 says to revisit on the deadline argument alone.

---

### T2.2 — original text

Technically sound: an `INTEGER PRIMARY KEY` *is* the rowid, so `VACUUM` cannot renumber it,
and `content_rowid='rowid_pk'` makes D-071's dense-rowid argument unnecessary rather than
merely currently-true. Lookup cost by `id` is unchanged (a `TEXT PRIMARY KEY` on a rowid table
is already a unique index plus rowid indirection).

But it does **not** unblock erasure on its own — deletion also needs the FTS delete trigger
issuing FTS5's `'delete'` with OLD values, which §4.6 already names as the missing third
trigger — and `AUTOINCREMENT` costs a `sqlite_sequence` write per concept insert to buy
non-reuse that only matters once deletion exists.

**Decision: schedule it as the first rung of whichever release implements concept archival or
erasure, together with the delete trigger and a `rebuild_fts()` call.** Taking it earlier pays
a hot-path cost for a hazard D-071 has already measured as unreachable.

### T2.3 — The overlap guard stays in the actor — ✅ **CONFIRMED, folded into D-083**

> **Delivered 2026-07-31.** Confirmed, not reopened. §4.7 now reads "settled" where it read
> "open", and the confirmation is recorded inside D-083 rather than as its own decision,
> because it is the same question T2.1 answers the other way — and the contrast is the useful
> part. A `CHECK` was affordable for row 3 precisely because the engine enforces it against the
> raw connection too, so it does **not** share row 1's "constrains only the people already
> behaving" problem, and it costs a comparison rather than a second index probe on the path
> D-059 had just made fast.

---

### T2.3 — original text

Revisit and **confirm** D-060 rather than reopen it. The actor now performs exactly the probe
a trigger would, on an index built for it, once per row with the statement prepared once
(D-064). Moving it into `trg_links_single_open` adds a second probe to every insert on the
path D-059 just finished making fast, to constrain writers who bypassed the actor — and those
writers can bypass `PRAGMA`s too. The §4.7 statement is the right resolution. What is missing
is only that it reads as an open question; close it.

---

## Tier 3 — Read path beyond the traversal · ✅ **COMPLETE (D-085, D-086, D-087)**

> Two of the four items shipped as proposed, one was **refuted by its own measurement and
> removed**, and one **passed its gate and was deferred anyway** on a cost the item does not
> price. Details under each.

### T3.1 — One `HYDRATE_CHUNK` — ✅ **DELIVERED, folded into D-085**

> **Delivered 2026-07-31.** Now in `util::limits`, beside the `SQLITE_MAX_VARIABLE_NUMBER` it
> exists to stay under. The `// See as_of::HYDRATE_CHUNK` comment was itself the evidence: the
> duplication was known and being managed by convention, and a shared constant is what replaces
> a convention.
>
> Two things beyond the item. The margin check moved from a `#[test]` to a `const` block on
> clippy's prompting — both sides are constants, and the failure it guards is someone tuning
> the value upward, plausibly in a release build without running the suite, so it should fail
> the *build*. And `wave1_regression_tests` now derives its straddling fixture size from the
> constant instead of hardcoding `450`, which would have silently stopped straddling a chunk
> boundary the moment the constant moved — still passing, testing nothing.

---

### T3.1 — original text

The constant is defined twice, at [subgraph.rs:538](../src/graph/subgraph.rs) and
[as_of.rs:24](../src/temporal/as_of.rs), both `400`, with the reasoning written out once.
Two copies of a tuned constant is how they stop being equal. Move to `util`, keep one
rationale.

### T3.2 — `AttributeMode::Current` + `as_of` should not be a `warn!` — ✅ **DELIVERED, D-085**

> **Delivered 2026-07-31.** The item's first option, taken: `DbError::AttributeModeUnstated`
> when `as_of` is set and the mode was never stated. Both answers stay reachable and neither is
> silent.
>
> **The item is missing the step that makes it possible.** A typed error could not simply
> replace the `warn!`, because `hydrate_attributes` receives a mode and a `ts`, and a `ts` is
> just an instant — nothing in that call distinguishes "as of last Tuesday" from "as of now".
> The warning therefore fired on **every** `Current` hydrate, which is overwhelmingly the
> ordinary live case where it is exactly right: loud where it did not matter and, being a log
> line, silent where it did.
>
> So the fix is a representation change first. `TraversalBuilder::as_of(ts)` now exists, making
> "a query about the past" expressible; and `attribute_mode` became `Option<AttributeMode>`,
> where `None` means **defaulted** rather than `Current`. Those two carry exactly the
> information the decision needs, and the error names both resolutions — "you must choose"
> without saying between what is a worse boundary than a warning.
>
> **No existing caller changes.** A traversal with no `as_of` is a query about now, where the
> two modes agree about which text to return, so the default stands; the whole suite compiled
> and passed unaltered. That is what keeps this from breaking everyone to fix one combination.
>
> The test renames a concept *after* the instant asked about, so the modes genuinely disagree —
> without that, every mode returns the same string and the question is invisible, which is
> precisely how this survived to 0.6.0. Writing it surfaced Doctrine II inside the fixture: the
> topology filter is **valid** time while `AtTime` hydration is **transaction** time, and one
> `as_of` feeds both, so the first version failed by keying the rename on the wrong axis.

---

### T3.2 — original text

Today, an `as_of(Tuesday)` traversal with the default attribute mode returns Tuesday's
topology wearing today's titles, and says so via `tracing::warn!`. A log line is not a
boundary — it is invisible in any application that has not configured a subscriber, which is
most of them at first run.

For 1.0 this should be one of: a typed error unless the caller explicitly opts in
(`.attribute_mode(Current)` stated rather than defaulted when `as_of` is set), or a default
that flips to `AtTime` when `as_of` is present. The first is cheaper and more honest. This is
the last silent-wrong-answer path the v0.5.6 cycle documented but did not close.

### T3.3 — `Subgraph` interning — ⏸️ **GATE PASSED, DEFERRED TO 0.7.0, D-087**

> **Measured 2026-07-31.** The item's gate — edges per budget, not milliseconds — is **passed**,
> and comfortably. An edge costs 342 bytes at 8-byte ids (378 at ULID length), because
> `size_of::<EdgeRef>()` is 104 before any payload and every edge is stored twice. Interned to
> `{u32, u32, f64, u32, u32}` it is 48 bytes for the pair. That is **3,066 → 21,845 edges per
> MiB**, a 7.1× reachability gain, rising to 9.5× at 64-byte ids.
>
> **Deferred on a cost the item does not price.** `Subgraph`, `EdgeRef` and `NodeData` are
> public types with public fields, read directly by every algorithm and constructed directly by
> callers and tests. This is not an internal representation change; it is a breaking change to
> the crate's main read-side structure, in a hardening release whose other items are
> deliberately additive or invisible. Scheduled for 0.7.0 with the break named.
>
> **The sweep found a cheaper adjacent win, in the opposite regime.** Asking where the budget
> actually goes: edges are 97% of it at zero content per concept, 76% at 2 KB, and **25% at
> 20 KB**. Meanwhile `load_subgraph_with` *always* hydrates `content` — its own rustdoc says
> "`attribute_mode` is ignored" — and **no graph algorithm reads it**. A caller running Louvain
> over 20 KB documents spends three quarters of the budget on bodies nothing will look at.
> Smaller than interning, complementary to it, and recorded in D-087 rather than acted on here:
> `Subgraph` carrying live concept text is a documented property, so the fix is a load option,
> not a silent change of what the type contains.

---

### T3.3 — original text

D-063 retired the integer-index rewrite on **CPU** grounds and the measurement is sound —
load dominates every single algorithm at every size tested. The **memory** argument was never
made, and it is the stronger one:

`add_edge` stores a full `EdgeRef` in `out_adj` and a **clone** in `in_adj`, each carrying an
owned `String` id plus two owned timestamp `String`s. Every edge therefore holds roughly two
ids and four 27-byte timestamps on the heap, twice. The byte budget — the thing that actually
bounds what a caller can load — is spent on this.

Interning ids to dense `u32` at load time and storing timestamps as fixed-width would raise
the number of edges that fit in a given budget substantially, which is a **reachability**
improvement, not a speed one — the same category D-073 used to justify the filter work. It
also needs **no schema change**, which is where the earlier proposal to key this on
`rowid_pk` went wrong: `rowid_pk` is sparse and unbounded, so it would still need a map rather
than a vector.

**Do this only after T0.1**, and gate it on a measurement of edges-per-budget rather than of
milliseconds. If the budget is not the binding constraint for real callers, skip it.

### T3.4 — Pipeline the bulk paths — ❌ **IMPLEMENTED, MEASURED, REMOVED, D-086**

> **Measured 2026-07-31.** Built it, swept depths 1/2/4/8/16 over 20K and 100K edges, and took
> it out again.
>
> | edges | depth 1 | depth 2 | depth 4 | depth 8 | depth 16 |
> |---|---|---|---|---|---|
> | 20,000 | 1,557 ms | 1,573 ms | 1,548 ms | 1,578 ms | 1,578 ms |
> | 100,000 | 8,030 ms | 8,037 ms | 8,017 ms | 8,007 ms | 8,041 ms |
>
> Ten cells, all within 1% of sequential, in both directions. The longest hold held at 13.8 ms
> and 21.0 ms throughout — the control the sweep exists to check, since pipelining must change
> *when* chunks are sent and never how big they are.
>
> **Every observation in the item is true and the conclusion does not follow.** The ~11,000 idle
> gaps are real; they are also four orders of magnitude smaller than the work they interrupt. A
> tokio mpsc hop is sub-microsecond against a 13–21 ms chunk. The item sizes the win as though
> the round trip were comparable to the chunk cost.
>
> **And it was not free to keep.** With chunks in flight, a failure at chunk *i* no longer
> leaves a **prefix** committed — *i+1 … i+k−1* were already sent and commit anyway. D-011
> promises "earlier chunks committed"; trading that for nothing measurable is the wrong way
> round.
>
> **What stayed is the part worth doing regardless**: the four bulk paths had four copies of
> the same loop and now share `low_chunked`, sequential, stopping at the first error.
> `examples/pipeline_diag.rs` is kept on purpose — the item's reasoning is entirely plausible
> and will be re-proposed by the next reader of that loop, and the sweep is cheaper to re-run
> than the argument is to re-have.
>
> **One condition under which this flips.** The measurement is against an embedded, *local*
> libSQL, where channel and storage are both in-process. A network-backed or replicated
> deployment has a round trip that is not sub-microsecond, and there the item's reasoning holds.

---

### T3.4 — original text

`bulk_import` and friends `await` each chunk before sending the next
([connection.rs:673](../src/connection.rs)). A 1M-edge import is ~11,000 sequential
round trips through the channel, so the actor idles for one channel hop per chunk and the
64-slot low-priority queue is never more than one deep. Sending *k* chunks ahead and
collecting responses keeps the actor fed without changing chunk size, priority, or the
per-chunk atomicity contract. Measure *k*; 2–4 is likely to capture most of it.

---

## Tier 4 — Measurement discipline *(the systemic fix)* · ✅ **COMPLETE (D-088, D-089, D-090)**

### T4.1 — The fixture matrix — ✅ **DELIVERED, D-088**

> **Delivered as specified, plus one correction the item does not contain.** Four shapes live
> in [`tests/fixtures.rs`](../tests/fixtures.rs), each named for the cost it is the worst case
> for, pinned by [`tests/fixture_matrix_tests.rs`](../tests/fixture_matrix_tests.rs) and run
> against the database by [`examples/fixture_matrix_diag.rs`](../examples/fixture_matrix_diag.rs)
> and the new `fixture_matrix` bench group. `benches/budgets.rs` no longer holds its own copy of
> the star: `seed_edges` delegates to `Shape::StarOfStars`, so the shape every pre-0.6.0 §9
> figure was taken on is a named member of the matrix rather than an anonymous local function.
>
> **The number that makes the case.** `load_subgraph` at comparable coverage — each shape at
> the depth it needs to reach 90% of itself:
>
> | shape | depth | nodes | load |
> |---|---|---|---|
> | `star_of_stars` | 2 | 600 | **3.06 ms** |
> | `chain` | 485 | 541 | 23.2 ms |
> | `clustered` | 89 | 541 | 235 ms |
> | `dense_small` | 1 | 300 | 259 ms |
>
> §9's `load_subgraph` budget is a measurement of the first row. **77× spread** at comparable
> size, none of it visible before.
>
> **The correction: coverage, not depth.** The obvious table fixes the depth, and the first
> version did. At a fixed depth of 3 the four shapes reach **600, 25, 6 and 300** nodes — so
> that table compares a 600-node problem against a 6-node one and reports the difference as a
> property of the shape. That is D-070's error committed inside the file written to prevent it.
> `depth_to_cover` exists so a shape-crossing measurement has to state which variable it holds
> fixed.
>
> **A second correction, to my own draft.** `Facts` first called its path count "the CTE's row
> count". It is not — T0.1's `UNION` bounds the walk at `reached × (depth+1)`. Renamed
> `simple_paths`, kept because it is what separates a tree from a graph and because it is the
> cost that returns the moment path semantics are reintroduced. Beside `union_bound`, their
> ratio is what T0.1 bought: **22,127× on `dense_small`**, **0.25× on `star_of_stars`** — the
> only fixture that existed when T0.1 was written.
>
> **`clustered` had to be rebuilt once.** The first version linked communities `i -> j, i < j`,
> which reads as dense and is a DAG: measured at 9×, it would have shipped as a weaker
> `star_of_stars` under a name promising a different question. Both directions make each
> community strongly connected — what Louvain and `scc` are looking for — and take the same
> walk to 47×.

### T4.2 — Plan-pinning as a test category — ✅ **DELIVERED, D-089 — and it found two dead indexes**

> **Implemented in the opposite direction from the item, which is why it found something.**
> [`tests/index_plan_tests.rs`](../tests/index_plan_tests.rs) is keyed by **index**, not by
> query: every entry in `ddl::CREATE_INDICES` must name the query that justifies it. The
> query-keyed form the item proposes catches a query that leaves its index — which the three
> reactive tests already do, and they are kept. It cannot catch an index **no query ever seeks
> on**, because there is no query to write an assertion against.
>
> **Two of the six are exactly that**, and both are pure cost — an index write on every insert
> into their table, read by nothing:
>
> * `idx_annotations_label` — nothing in the crate selects from `analytics_annotations` at all.
> * `idx_lc_tgt_active (target_id, valid_to)` — no query seeks on `target_id` as a leading
>   column; `Subgraph::in_adj` is built in Rust from the forward rows the walk already returned.
>
> Verified rather than inferred: `examples/index_coverage_probe.rs` confirms both *would* be
> chosen by a query of the obvious shape, so this is "nothing runs that query", not "the planner
> declines the index". D-059's own note weighs "a fourth index write per assertion" as a price
> worth arguing about, and `idx_lc_tgt_active` is one of those four on the hottest write path.
>
> **Recorded and scheduled, not dropped** — removing an index is a `DROP INDEX` rung, and this
> release's other items are additive. `the_unread_indices_are_the_two_already_known` pins the
> set so a third is a red test.

### T4.3 — A control row in every bench run — ✅ **DELIVERED, D-090**

> **Taken, with one change of form.** The item says "add a control row to every bench group",
> and a rule of that shape is followed until the next group is added in a hurry. The control
> lives in the **constructor**: `controlled_group` is the only way to obtain a
> `BenchmarkGroup` in `budgets.rs`, and it has already added the row before it returns.
> [`tests/bench_control_tests.rs`](../tests/bench_control_tests.rs) keeps the back door shut.
>
> **Measured on the first run that used it** — two back-to-back `traversal` runs:
>
> | | run 1 | run 2 | absolute | as a ratio to control |
> |---|---|---|---|---|
> | `control/select_1` | 1.639 µs | 1.589 µs | −3.0% | — |
> | `three_hop_warm` | 1.704 ms | 1.635 ms | −4.0% | **−1.0%** |
> | `as_of_edges` | 720.3 µs | 700.0 µs | −2.8% | **−0.2%** |
>
> The control absorbs most of the drift, which is the claim. A quiet pair of runs rather than
> the cross-session case D-070 measured at 29%, so this demonstrates the mechanism rather than
> bounding it.
>
> What it does *not* do is normalise automatically — criterion measures each row independently
> and the division is still the reader's. The value is that the divisor is now present in the
> same run, which is what makes the ratio computable at all.

---

### T4.1 — original text

**This is the most important item in the plan.** One fixture shape has produced: D-070's wrong
conclusion, T0.1's invisibility, and D-059's still-open *"the chunk constants are
empty-database figures and need a realistic fixture, which requires deciding what 'realistic'
means."*

Decide what realistic means. Four shapes, each with a stated reason:

| Fixture | Shape | What it is the worst case for |
|---|---|---|
| `star_of_stars` | tree, high fan-out | hub out-degree; the existing one |
| `clustered` | communities with dense intra-links | **path enumeration** (T0.1); Louvain |
| `chain` | long, low branching | recursion depth; snapshot fold length |
| `dense_small` | near-complete, few hundred nodes | the `DISTINCT` sort; byte budget |

Every performance decision entry should name which fixture(s) it was measured on. D-070's
would then have read *"inherent on `star_of_stars`"*, and the gap would have been visible.

### T4.2 — original text

D-042, D-059 and D-064 are one bug three times: a covering index captures a query because it
contains the columns, not because it discriminates. There are now three `EXPLAIN`-asserting
tests, written reactively each time.

Make it a rule: **every query with an index dependency ships with an `EXPLAIN QUERY PLAN`
assertion.** That is currently 5–6 queries. Then a fourth instance of this defect is a red
test rather than a wave.

### T4.3 — original text

D-070 established that this project's absolute timings carry ~29% session noise, which makes
cross-run comparison meaningless and is not visible from a results table. Add a fixed trivial
operation as row zero of every bench group, and report all figures as ratios to it. Cheap, and
it converts a caveat that must be remembered into one that is enforced.

---

## Tier 5 — 1.0 API and operations · ✅ **COMPLETE (D-091, D-092)** — one item carried: the upstream R15 report

### T5.1 — `diagnostic_conn()`, and `raw()` behind a declaration — ✅ **DELIVERED, D-091**

> **Both of the item's premises measured rather than accepted**
> ([`examples/readonly_open_probe.rs`](../examples/readonly_open_probe.rs), libSQL 0.9.30, live
> WAL database with the actor running):
>
> | | `read_conn()` | `diagnostic_conn()` |
> |---|---|---|
> | `SELECT`, `EXPLAIN QUERY PLAN` | allowed | allowed |
> | `INSERT` | refused | refused |
> | `PRAGMA query_only = OFF` | allowed | allowed |
> | `INSERT` after that | **allowed** | **refused** |
>
> The last row is the whole difference and the reason the method exists. Rows two and three
> would look the same for a second `query_only` connection, so
> [`tests/diagnostic_conn_tests.rs`](../tests/diagnostic_conn_tests.rs) asserts the pair in
> **both** directions.
>
> **One finding the item does not anticipate, running the other way.** `CREATE TEMP TABLE`
> **succeeds** on the read-only connection and is refused by `read_conn()` — temp tables live in
> a separate writable temporary database, whereas `query_only` refuses them outright. That is
> the exact mechanism D-050 measured when it removed `TwoPhaseTempTable`. So the stronger
> boundary is not uniformly stronger, and one of D-050's two reasons no longer applies to every
> connection this crate can offer. Recorded, not acted on — D-050's second reason is untouched.
>
> **`raw()` is `#[doc(hidden)]`; the `raw-access` feature was rejected on a specific cost.**
> Cargo features cannot be *required* by a test target except through `required-features`, which
> makes a plain `cargo test` **skip** that binary. The binaries that call `raw()` are
> `storage_boundary_tests` and `wave1_regression_tests` — the §4.7 tripwires. Gating them would
> stop the ordinary `cargo test` running the tests that enforce the section this item is about,
> in order to make a declaration about a hatch. The hatch stays reachable and stops being
> discoverable; its legitimate-use list shrinks from three items to one, *provoking a guard*.
>
> **§4.7 invariant 2 is narrowed, not closed** — the free `register_model` / `upsert_embedding`
> still take a bare connection, and the file is still reachable by any SQLite client.

### T5.2 — R15: defend the load-bearing claim — ✅ **SOAKED AND DEFENDED, D-092 · upstream report still open**

> **The instrument had to be a subprocess runner**, which is the part worth recording. The fault
> is a process-level access violation — not a panic, not a SQLite error, nothing to catch. A
> `#[test]` that provoked it would take its binary down and be reported, as this document
> already documents, as *fewer passing tests and no failures*.
> [`examples/r15_soak.rs`](../examples/r15_soak.rs) re-executes itself with `--child` and tallies
> exit codes.
>
> | arm | shape | result |
> |---|---|---|
> | `claim` | one long-lived `Database`, cadence on, 16 concurrent readers running `load_subgraph` + `reconstruct`, actor saturated | **0 / 10** at 15 s, **0 / 6** at 60 s |
> | `control` | the same, plus a task opening 48 databases concurrently in the same process | **2 / 10** at 15 s |
>
> ~8.7 minutes of continuous load in the claimed-safe shape with no fault, in a session where
> the control faulted twice — both inside the first three seconds, when the open storm ramps.
> The control is what makes the claim arm mean anything.
>
> **The claim is defended in its sharpened form** — *one process, one file, a bounded set of
> connections opened once and never churned* — and R15's row is reworded to say so. It is a rate
> and not a proof, and the trigger is still concurrent *open*.
>
> **Still open:** the upstream report against 0.9.30. The raw reproduction is written; filing it
> needs credentials this project's tooling does not hold.

### T5.3 — Snapshot chain cross-check — ✅ **DELIVERED, D-092**

> `Database::verify_snapshot_chain(ts)` folds from genesis — by withholding the snapshot
> directory, so `snapshot_anchor` finds nothing and the second computation is genuinely
> independent — and compares against the composed answer.
>
> **It reports and does not repair, and that is a decision.** Under Doctrine VI a snapshot is
> disposable, so the repair is *delete the snapshot directory*: one line, available to the
> caller, correct without this function's help, and pinned by a test. What the caller cannot get
> for themselves is the knowledge that the chain diverged, and rewriting the file would destroy
> the only evidence that composition has a defect. A divergence is a wrong **cache**, not a
> corrupt ledger, and the report says so.
>
> **Not scheduled by this crate.** A genesis fold is exactly the cost snapshots exist to avoid,
> on a log large enough for snapshots to matter — the only case where this is worth running. The
> item calls it a scheduling problem; the schedule is the caller's, and the cadence is left
> alone.
>
> **The load-bearing test is the tampered one.** A checker only ever seen to pass is the shape
> this project keeps finding defects in — D-030's `audit_current` reduced to a constant zero;
> D-071's FTS `'integrity-check'` reporting healthy on an emptied index. So the test writes a
> *plausible* wrong snapshot — correctly serialised, named and anchored, with one title altered,
> one concept removed, one edge dropped — and requires the report to name all three.
>
> Two comparison details that would otherwise be silent bugs: `seq_anchor` is reported and never
> compared (the composed answer legitimately anchors at its snapshot plus delta), and edges are
> compared as a **set**, because `MaterializedState::edges` is an unordered `Vec` and a
> reordering is not a divergence.

---

### T5.1 — original text

A read-only diagnostic connection **already exists**: `read_conn()` carries
`PRAGMA query_only = ON`. So the real question is only what to do with `raw()`, and
privatising it does not close what it appears to close:

- §4.7 invariant 2 names **three** holes. Privatising `raw()` leaves the free `register_model`
  and `upsert_embedding`, which take a bare connection.
- `PRAGMA query_only` is per-connection and reversible by its holder in one statement. It is a
  guardrail, not a capability boundary.

**Proposal.** Add `Database::diagnostic_conn() -> Result<Connection>` that opens the file
**read-only at the OS level** (`SQLITE_OPEN_READONLY`), which is a boundary rather than a
pragma, and gives callers their own connection — which is the legitimate need `read_conn()`
does not serve, since it returns a shared `&Connection`. Then put `raw()` behind
`#[doc(hidden)]` or a `raw-access` feature. The hatch remains (D-068 is right that removing it
buys the appearance of a guarantee); using it becomes a declaration rather than a default.

### T5.2 — original text

The mitigation is `RUST_TEST_THREADS = "1"` and the claim carrying it is *"production exposure
is nil by construction — an application opens one `Database` and holds it for its lifetime."*
That claim is doing a lot of work and nothing tests it. Note that `open_inner` itself opens
**three** connections, and the cadence's was added in Wave 4.1.

**Actions.** File the upstream report against 0.9.30 with the raw reproduction (already an
open item). Add a soak test: one long-lived `Database`, heavy concurrent read load across many
Tokio tasks plus a saturated write actor, run for minutes rather than milliseconds. If that is
clean, the claim is defended and can be cited. If it is not, R15 is severe and the plan
changes.

#### Measured during the v0.5.6 Wave 5 session (2026-07-30, libSQL 0.9.30)

R15 fired often enough while finishing Wave 5 to be worth measuring rather than logging. All
runs below are on the shipped mitigation — `RUST_TEST_THREADS = "1"` from `.cargo/config.toml`,
applied to every `cargo test` in the project directory. No run passed `--test-threads` itself,
so these are the numbers a maintainer gets by typing `cargo test`.

| Run | Faults | Documented figure |
|---|---|---|
| Full plain suite, `cargo test --no-fail-fast`, 10 consecutive runs | **5 / 10** | 0 / 30 (0.5.4) |
| `vector_filter_tests` alone, 15 runs | 0 / 15 | 0 / 40 (one binary, 0.5.4) |
| `replay_snapshot_tests` alone, 15 runs | 0 / 15 | — |
| `write_path_tests` alone, 15 runs | 0 / 15 | — |
| `storage_boundary_tests` alone, 15 runs | 0 / 15 | — |
| `doctrine_property_tests` alone, serialised, 4 runs | 3 / 4 | ~3 / 25 (0.5.4) |
| `integrity_property_tests` alone, serialised, 4 runs | 1 / 4 | — |

Faulting targets across the ten suite runs: `storage_boundary_tests` ×2, `vector_filter_tests`,
`replay_snapshot_tests`, `write_path_tests`. Ad-hoc full runs earlier in the same session were
3 / 7, consistent with the measured 5 / 10.

**Three corrections to R15 as written, and the first one matters most.**

1. **"Plain `cargo test` measures 0/30" does not reproduce.** It is 5 / 10 today, on the
   mitigation, on the same dependency version. Either the rate rose with the suite
   (171 → 221 tests since that figure was taken) or the original 0/30 was a lucky streak.
   Either way, R15 currently reads as though serialising libtest *removed* the fault for
   ordinary use, and it did not — it reduced a rate. The risk row and the `.cargo/config.toml`
   comment both need rewording, because a maintainer who reads them and then sees a red build
   will spend the time distinguishing R15 from a real failure that the row was written to save
   them.

2. **Every faulting binary is clean in isolation.** Four binaries that died during suite runs
   are 0 / 15 each when run alone, at 60 total isolated runs with no fault. So the trigger is
   not the binary, not any test in it, and not thread concurrency *within* a binary — that is
   already serialised in both cases. Something accumulates across a `cargo test` invocation
   that per-binary isolation does not reproduce. R15's current framing ("several local
   databases opened and dropped concurrently in one process") does not cover this, since the
   isolated runs open and drop databases the same way.

3. **The residue is not one binary.** R15 names "the two generated-history binaries" and
   `doctrine_property_tests` as the residual case; there are **three** gated targets
   (`doctrine`, `integrity`, `graph`) and `integrity_property_tests` faulted 1 / 4 today.
   `doctrine_property_tests` at 3 / 4 is also an order worse than the recorded ~3 / 25.

**A churn hypothesis was drawn from the table above and then refuted; both are kept, because
the refutation is the useful part.** The suite data — property binaries at 3 / 4, ordinary
binaries at 0 / 15 — reads as though the fault tracked how many databases a process opens and
drops. It does not. A standalone reproducer (below) separates the two variables directly:
**500 sequential opens in one process is 0 / 10, and 32 concurrent opens is 2 / 12.** Churn is
not sufficient and concurrency is, which is what R15 said originally. The suite table cannot
distinguish them, so no inference of that kind should have been drawn from it.

**What the standalone reproduction establishes** (libsql 0.9.30, Windows, release, tokio
multi-thread, one file per task, no Macrame types in the loop — so R15's long-standing claim
that this reproduces outside the crate is now verified rather than inherited):

| Shape | Faults |
|---|---|
| 500 opens, sequential, one process | 0 / 10 |
| 4 / 8 / 16 concurrent opens | 0 / 10 each |
| 32 concurrent opens | 2 / 12 |
| 128 concurrent opens | 5 / 12 |
| 500 concurrent opens | 5 / 10 |
| 128 concurrent opens, **nothing dropped until all have finished** | 3 / 12 |

Every fault is `0xC0000005`; no panics and no SQLite errors accompany any of them. The last
row is the sharp one: holding every handle alive until the end still faults, so this is
concurrent **open**, not an open/drop race, and not teardown. The threshold sits between 16
and 32 concurrent opens on this machine.

**Consequence for the soak.** The proposed soak tests a *load* hypothesis: one open `Database`,
many concurrent readers, a saturated actor. That is now the right shape to test the claim, but
it is testing the *claim*, not the fault — the fault needs concurrent opens, and a single
long-lived `Database` performs its opens once, at construction. So the soak's clean result
would be evidence for the claim rather than a test of the bug, which is what it should be.
**Add a control arm** that does the thing known to fault — a long-lived `Database` exercised
under load *while* a second task opens 32+ databases concurrently in the same process — so the
soak can distinguish "the claim holds" from "the harness never provoked anything".

**One thing this does not explain, and it should not be papered over.** Every Macrame binary is
0 / 15 alone while the suite that runs those same binaries *serially* is 5 / 10, all under
`RUST_TEST_THREADS = "1"`. If concurrent opens are required, the suite-level runs are finding
concurrency somewhere per-binary runs do not, and this data does not say where. `Database::open`
opening three connections (four with the cadence) is the obvious suspect and is not evidence.
Unresolved, and flagged rather than guessed at.

**And the claim itself should be sharpened before it is defended.** *"An application opens one
`Database` and holds it for its lifetime"* is not what the crate does: `open_inner` opens
three connections, four since Wave 4.1 gave the cadence its own. What the soak can actually
test — and therefore what production exposure should be stated as — is **one process, one
file, a bounded set of connections opened once and never churned**. That is a defensible
claim. The current wording is a claim about `Database` handles, and `Database` handles are not
what the fault appears to count.

**One reporting hazard worth writing down while it is fresh.** The fault is a process-level
access violation, so the harness dies mid-binary: the tests that already printed `ok` are
reported, the ones after it are silent, and the binary's `test result:` line never appears.
A script that sums `N passed` across the run therefore returns a *smaller number with no
failures* rather than a red. Anything that gates on this suite must key on the absence of a
per-target result line, not on a pass count — and `--no-fail-fast` is not optional, or
everything alphabetically behind the fault is skipped as well.

### T5.3 — original text

The project's own open item: `write_final` composes onto the previous snapshot, so an error
propagates forward indefinitely with no periodic full fold. The difficulty is real — a full
fold is exactly the cost snapshots avoid — so it is a scheduling problem: fold from genesis at
most once per *N* snapshots or per idle period, compare, and report divergence rather than
repair it silently. Pair it with T1.4's metrics so a divergence is visible.

---

## What not to do

| Rejected | Why |
|---|---|
| Logical archiving (`archived_at` column + background purge) | The marking `UPDATE` is the same order as the copy, so the stall is halved rather than removed; it puts a mutable column on `links`, which is Doctrine III's core; it adds a drift class (marked, never purged); and the hot file does not shrink until the purge runs. **T1.1 gets the same benefit with no schema change.** |
| `Subgraph` integer rewrite for **CPU** | Retired on measurement by D-063 and the measurement stands. The **memory** version (T3.3) is a different argument and is conditionally worth taking. |
| Capping `write_bulk_atomic` | Breaks the guarantee the method exists to provide (D-014). T1.3 instead. |
| `concepts_current` twin for cheap `AtTime` | Appendix C is right: it taxes every query in the system to make one read pattern free, for a ledger whose bitemporal centre of gravity is edges. |
| Shadow swap *without* chunking and catch-up | Does not reduce actor hold at all, and increases total work. |

---

## Sequencing

**v0.5.7 — measurement first.** T4.1 (fixture matrix), T4.3 (bench control), T1.4 (actor
metrics). Nothing else in this plan can be honestly validated before these exist, and T4.1 is
what makes T0.1's numbers this project's own rather than borrowed from a probe.

**v0.5.8 — the read path.** T0.1 (traversal rewrite, with its property test and its correction
to D-070), T3.1, T3.2, T3.4. Highest user-visible payoff, no schema change, no migration rung.

**v0.5.9 — the write path and the schema.** T0.2 (audit opt-in), T1.1 (windowed archive),
T2.1 (`CHECK` on weight, with the baseline re-issue), T0.3 (§4.7 correction), T2.3 (close the
overlap question). One migration rung, taken while D-032 still makes it free.

**v0.6.0 — the tail.** T1.2 (chunked shadow rebuild), T5.1 (`diagnostic_conn` + gated `raw()`),
T5.2 (R15 soak + upstream), T5.3 (snapshot cross-check), T3.3 if the budget measurement
justifies it.

**1.0** — when the four fixtures are green under a stated budget, the actor's tail is
observable in production, and §4.7 lists at most the gaps that are there by decision.

**Deferred beyond 1.0, with a design rather than a decision:** T2.2 (`rowid_pk`), bundled with
concept archival or erasure whenever one of those is actually wanted.
