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

## Tier 0 — Measured defects

### T0.1 — The traversal enumerates paths, not nodes *(the headline item)*

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

### T0.2 — Repair costs more than the damage

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

### T0.3 — §4.7 invariant 3 is misstated

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

## Tier 1 — Bounded actor latency

The actor is the right design. Its problem is that three operations are exempt from the
latency bound by contract, and two of the three did not have to be.

### T1.1 — Window the archive *(no schema change)*

`archive(cutoff)` is one transaction whose size is set by how long since the last one.
Replace with `archive_windowed(cutoff, window)`: archive to *T₁*, then *T₂*, … each a complete
atomic session with its own marker, horizon row, and rebuild.

D-012's atomicity requirement is **per session** — copy-then-delete must not be split. N small
sessions satisfy it exactly as one large one does. The only new obligation is that a partial
run leaves a coherent intermediate state, which it does: each session commits a valid horizon.

This is scheduling, not schema, and it is what Appendix C already names as the escape hatch
(*"phased, per-table archiving"*). It should stop being an escape hatch and become the default.

### T1.2 — Chunked shadow rebuild

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

### T1.3 — `write_bulk_atomic` stays uncapped

D-014 is correct: the batch is one act under one stamp, and capping breaks the guarantee the
method exists to provide. But "uncapped and undocumented at the call site" is not the same as
"uncapped by decision." Add an estimated-hold warning above a threshold
(`tracing::warn!` with rows × measured per-row cost), and state the ceiling in rustdoc in
milliseconds rather than in prose. A caller who stalls the UI for 8 seconds should have been
able to predict it from the signature.

### T1.4 — Make the actor observable *(the item that makes "future-proof" mean something)*

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

## Tier 2 — Schema

### T2.1 — `CHECK (weight >= 0.0)` on `links` — **take it**

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

### T2.2 — `rowid_pk INTEGER PRIMARY KEY` on `concepts` — **bundle with erasure, not before**

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

### T2.3 — The overlap guard stays in the actor

Revisit and **confirm** D-060 rather than reopen it. The actor now performs exactly the probe
a trigger would, on an index built for it, once per row with the statement prepared once
(D-064). Moving it into `trg_links_single_open` adds a second probe to every insert on the
path D-059 just finished making fast, to constrain writers who bypassed the actor — and those
writers can bypass `PRAGMA`s too. The §4.7 statement is the right resolution. What is missing
is only that it reads as an open question; close it.

---

## Tier 3 — Read path beyond the traversal

### T3.1 — One `HYDRATE_CHUNK`

The constant is defined twice, at [subgraph.rs:538](../src/graph/subgraph.rs) and
[as_of.rs:24](../src/temporal/as_of.rs), both `400`, with the reasoning written out once.
Two copies of a tuned constant is how they stop being equal. Move to `util`, keep one
rationale.

### T3.2 — `AttributeMode::Current` + `as_of` should not be a `warn!`

Today, an `as_of(Tuesday)` traversal with the default attribute mode returns Tuesday's
topology wearing today's titles, and says so via `tracing::warn!`. A log line is not a
boundary — it is invisible in any application that has not configured a subscriber, which is
most of them at first run.

For 1.0 this should be one of: a typed error unless the caller explicitly opts in
(`.attribute_mode(Current)` stated rather than defaulted when `as_of` is set), or a default
that flips to `AtTime` when `as_of` is present. The first is cheaper and more honest. This is
the last silent-wrong-answer path the v0.5.6 cycle documented but did not close.

### T3.3 — `Subgraph` interning: the argument D-063 did not consider

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

### T3.4 — Pipeline the bulk paths

`bulk_import` and friends `await` each chunk before sending the next
([connection.rs:673](../src/connection.rs)). A 1M-edge import is ~11,000 sequential
round trips through the channel, so the actor idles for one channel hop per chunk and the
64-slot low-priority queue is never more than one deep. Sending *k* chunks ahead and
collecting responses keeps the actor fed without changing chunk size, priority, or the
per-chunk atomicity contract. Measure *k*; 2–4 is likely to capture most of it.

---

## Tier 4 — Measurement discipline *(the systemic fix)*

### T4.1 — The fixture matrix

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

### T4.2 — Plan-pinning as a test category

D-042, D-059 and D-064 are one bug three times: a covering index captures a query because it
contains the columns, not because it discriminates. There are now three `EXPLAIN`-asserting
tests, written reactively each time.

Make it a rule: **every query with an index dependency ships with an `EXPLAIN QUERY PLAN`
assertion.** That is currently 5–6 queries. Then a fourth instance of this defect is a red
test rather than a wave.

### T4.3 — A control row in every bench run

D-070 established that this project's absolute timings carry ~29% session noise, which makes
cross-run comparison meaningless and is not visible from a results table. Add a fixed trivial
operation as row zero of every bench group, and report all figures as ratios to it. Cheap, and
it converts a caveat that must be remembered into one that is enforced.

---

## Tier 5 — 1.0 API and operations

### T5.1 — `diagnostic_conn()`, and `raw()` behind a declaration

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

### T5.2 — R15: defend the load-bearing claim

The mitigation is `RUST_TEST_THREADS = "1"` and the claim carrying it is *"production exposure
is nil by construction — an application opens one `Database` and holds it for its lifetime."*
That claim is doing a lot of work and nothing tests it. Note that `open_inner` itself opens
**three** connections, and the cadence's was added in Wave 4.1.

**Actions.** File the upstream report against 0.9.30 with the raw reproduction (already an
open item). Add a soak test: one long-lived `Database`, heavy concurrent read load across many
Tokio tasks plus a saturated write actor, run for minutes rather than milliseconds. If that is
clean, the claim is defended and can be cited. If it is not, R15 is severe and the plan
changes.

### T5.3 — Snapshot chain cross-check

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
