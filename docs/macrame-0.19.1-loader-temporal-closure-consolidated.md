# `load_subgraph_with` at a historical instant drops edges whose endpoint concept retired after the instant — consolidated

| | |
|---|---|
| **Status** | Landed in 0.19.1 — D-289 (loader arm + mode contract) + D-290 (ids parity); T1–T8 green |
| **Affects** | `macrame-db` 0.19.0 (verified against this checkout: `dev/0.19.1` = 0.19.0 sources; no fix has landed at the time of writing; fixed on `dev/0.19.1` — see the Landing record below) |
| **Target** | v0.19.1 |
| **Scope** | Read path only. No schema, migration, table, or index changes |
| **Supersedes** | `docs/macrame-0.19.1-loader-temporal-closure-issue.md` §§1–7, Appendices A–B, and Amendments 1–3 there — retained as history, not edited (house §7.4) |
| **Companion reproducer** | `core_indexer/tests/repro_walk_retired.rs` in CodeRadar (pure-`macrame` APIs; loader tests `#[ignore]`d pending this fix — un-ignore per §6.3 item 4) |

All `src/` references are relative to the `macrame-db` crate root with line
numbers verified on `dev/0.19.1` @ `8aa74b7` (identical to 0.19.0 on every
path in scope). `bindings/` references are workspace-relative. Every claim
below is tagged with its provenance: **[source]** (read in code),
**[exec]** (observed running throwaway probes P1–P5 on 2026-10-06; probe
binary deleted after the run), or **[inference]** (stated as such).

---

## 0. What was corrected to produce this consolidation

The original issue document was right about the mechanism and wrong about
four things; review added five more findings. All nine are applied below, so
this document does not repeat the original's errors:

- **C1 [exec]. Reproducer rewritten.** The original sketch retired only the
  *edge* — which cannot starve hydration — and does not reproduce (P1: both
  builder shapes return the edge, 2 nodes / 1 edge). The corrected fixture
  retires the *endpoint concept* after the instant (§2). The original
  observation most likely came from a rename fixture that retired `old_name`
  as a concept **[inference]** — its "missing with and without
  `as_of_recorded`" signature matches the concept-retirement shape exactly.
- **C2 [exec]. Two clocks distinguished.** `load_subgraph_with`'s `now_ts`
  is the *valid-time* fallback (`TraversalBuilder::valid_instant`:
  `as_of_valid.unwrap_or(now_ts)`; `src/graph/builder.rs:518` — and
  "`as_of_recorded` never is", `:1243`). A bare builder therefore reads
  **current belief** however far past `now_ts` lies. T1's historical
  expectation belongs solely on the `.as_of_recorded(t1)` shape; the bare
  shape is the unchanged-behavior control.
- **C3 [exec]. T4 corrected.** A start id with no concept row yields an
  **empty graph** (0 nodes, 0 edges, no error; `execute_ids == []`) — P3 —
  not "start node only".
- **C4 [source+exec]. Three affected surfaces, not two.** `execute`
  (`src/graph/builder.rs:1070–1079`) hydrates whatever `execute_ids`
  returns, so `execute`+`AtTime` also loses the node (P2: returns only
  `a::x`). §4.2's ids fix repairs it transitively; no third arm is needed,
  but T6 is gated on §4.2 and its pre-fix failure mode is a *missing node*,
  not a wrong title.
- **C5 [source+exec]. No drop-in helper.** `hydrate_at_time`
  (`src/temporal/as_of.rs:407`) returns `NodeAttributes`, not `NodeData`
  (no `valid_from`/`valid_to`); has no byte budget or content/`extra`
  opt-in; and is a **private** `async fn` — reachable only via the public
  `hydrate_attributes` (`:290`, returns `Vec<NodeAttributes>`). §4.1
  specifies a loader-specific incremental hydrator reusing its
  fold/retirement predicates (P2 called the fold directly and it answers
  correctly; P5 pins the budget/opt-in premises the arm must preserve).
- **C6 [source]. Mode contract completed.** The loader never consults
  `attribute_mode` (rustdoc: "`attribute_mode` is still ignored: hydration
  here is always the live …", `src/graph/subgraph.rs:904`;
  `resolved_mode` at `src/graph/builder.rs:1088–1097` is never called on
  this path). §4.5 decides unstated *and* explicit `Current`/`Omit`
  together, before wiring.
- **C7 [source]. Blast radius is Rust-only.** The Python binding
  (`bindings/python/src/database.rs` `load_subgraph`) forwards
  `as_of_valid=`/`as_of_recorded=` into the builder; only
  `BranchView::load_subgraph` (`src/branch.rs:338`) passes no instants.
- **C8 [exec]. T2/T5 fixtures use concept retirement** (retired-at-`t1`
  stays absent — P4; retired-after-`t1` present-only-historically — P2).
- **C9 [source — new in this consolidation]. Doc paths in the currency
  list were wrong.** There is no `docs/api-review-0.19.0.md` or
  `./public-api.txt`: the files are
  `docs/architecture/api-review-0.19.0.md` (per-release reviews back to
  0.14.0) and `docs/architecture/public-api.txt`. The cross-reference gate
  is `tests/doc_link_tests.rs`. §7 applies the corrected paths. Also
  verified for §7: D-288 is the highest register entry; s5 §5.2 lives at
  `docs/architecture/s5-modules.md:470` and §5.4 at `:831`; the README
  bitemporal row cites D-174 (README line 23).

---

## 1. Summary

`Database::load_subgraph_with` answers a historical question with a
present-tense node filter — **when the endpoint is a concept retired after
the requested instant**. The walk itself is correct at historical instants:
it finds edges whose valid interval spans the instant. But node hydration
reads exclusively from live rows:

```sql
SELECT id, title, content, embedding_model, valid_from, valid_to, extra
FROM concepts WHERE retired = 0 AND id IN (...)
```

(`hydrate`, `src/graph/subgraph.rs:1103–1161`), and
`drop_dangling_adjacency` (`:1083`, impl `:516`) then enforces the closure
invariant (*"every id appearing in `out_adj` or `in_adj` is a key of
`nodes`"*, `:18–60`, checked by `is_closed`, `:548`) by pruning every edge
whose endpoint lacks node data. Net effect: **any edge whose endpoint
concept retired *after* the requested instant is silently missing from a
historical subgraph**, even though the endpoint was fully live at that
instant **[source, exec]**.

For current-belief reads this is documented, deliberate design (§4.1:
retired = not visible). For historical reads it contradicts the bitemporal
contract the `TraversalBuilder` advertises (the BCDM cell: *what did we
believe at `r` about what was true at `v`*). `reconstruct` folds correctly.
Three read surfaces are affected: the subgraph loader, the `execute_ids`
projection (directly), and `execute`+`AtTime` (transitively through
`execute_ids`).

---

## 2. Reproducer (corrected)

Necessary condition: the endpoint **concept** retires after the instant
while the edge still spans it. Minimal shape (timestamps are example
values; the property is general):

1. Assert concepts `A`, `B` and edge `A→B CALLS` with
   `valid_from = 2026-01-01T00:00:00.000000Z` (open `valid_to`).
2. Capture `t1 = MAX(recorded_at)` from `transaction_log`.
3. **Re-upsert `B` with `retired(true)`** (change the title v1→v2 so the
   payloads are distinguishable; keep the interval open). The edge stays
   open — no `retire_edge` is needed, and the original sketch's
   `retire_edge`-only step is *insufficient* (C1).
4. `load_subgraph_with(TraversalBuilder::new(A).max_depth(2)
   .as_of_recorded(&t1), t1)` returns **1 node, 0 edges — `B` not
   hydrated** [exec: P2]. The bare builder at `t1` *and* at a later `t2`
   likewise returns 1 node, 0 edges — current belief regardless of
   `now_ts` (C2) [exec: P2].
5. Controls that pass on unfixed code: `reconstruct(t1).concepts["B"]`
   carries the v1 payload; `hydrate_attributes` over the bitemporal
   `(t1, t1)` fold returns both nodes with v1 titles; `execute_ids`
   returns `["A"]` (the §3.1 narrowing); `execute`+`Current` returns
   `["A"]`; instants without a mode refuse `AttributeModeUnstated`
   [exec: P2].
6. Negative result, recorded so the old sketch is not re-tried: with
   concepts live and `retire_edge` (valid_to 2027-01-01) as the only later
   write, **both** builder shapes return the edge (2 nodes, 1 edge)
   [exec: P1].

Real-world instance: a rename fixture (`old_name` → `new_name` between
`t1` and `t2`) — `traverse(out)` at `t1` missed the v1 `CALLS` edge at its
own `MAX(recorded_at)`. Reconciliation is **[inference]**: minting
`new_name` most likely retired `old_name` as a concept, which is exactly
this section's shape; the condensed sketch dropped that step.

---

## 3. Root cause (verified step by step)

Narrowing eliminated, in order: instant handling (`as_of_recorded` does not
help on unfixed code), caller-side misuse (pure-`macrame` probes behave
identically), the data (the fold sees the row), the walk (`execute_ids`
reaches the live start node), and the SQL text (the loader's walk +
projection is the shared `walk_cte` plus a `DISTINCT` edge projection with
`l.valid_from <= ?3 AND ?3 < l.valid_to`, `src/graph/subgraph.rs:921–1010`;
params `[start, depth, valid_instant, min_weight, …]` via `bind_params`,
`src/graph/builder.rs:728`). What remains is the loader's tail:

1. The topology loop collects the edge correctly at the instant.
2. `hydrate` (`src/graph/subgraph.rs:1103`) fills node attributes from live
   rows only (`WHERE retired = 0`, `:1122`) — in `HYDRATE_CHUNK`-sized
   queries (defect AE), accounting bytes incrementally into the caller's
   running total and refusing `SubgraphTooLarge` mid-loop rather than at
   the end; `content`/`extra` are opt-in flags that govern whether fetched
   bytes enter the payload and budget, not whether they cross the wire
   (D-286). The now-retired endpoint gets no `NodeData`.
3. `drop_dangling_adjacency` (`:1083`) retains only edges whose endpoints
   hydrated. The historically-live edge is pruned as "dangling".

### 3.1 The `execute_ids` surface

`build_sql_with` (`src/graph/builder.rs:595`) appends one of two
projections, both filtering the present tense in the SQL text:

```sql
-- unlimited (:604):
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0 ...
-- limited (LIMITED_PROJECTION, :634): the same join/filter LEFT JOINed
-- onto a walk-row-count anchor, so the count survives even when every
-- reached concept is retired
```

The walk reaches the historically-live node; the projection drops it on the
live `retired` flag. Same leakage, second surface — and the limited form's
own comment anticipates the all-retired case, which is this defect wearing
its projection hat.

### 3.2 `execute` inherits it

`execute` (`src/graph/builder.rs:1070–1079`) resolves the mode, calls
`execute_ids`, then hydrates whatever ids survive:

```rust
let mode = self.resolved_mode()?;
let ids = self.execute_ids(conn, now_ts).await?;
crate::temporal::as_of::hydrate_attributes(conn, &ids, &as_of, mode).await
```

§3.1's narrowed ids therefore starve AtTime hydration before it runs —
third surface, same root cause, no separate arm required to fix it (C4).

### 3.3 Why this is a gap and not the §4.1 design

§4.1 ("a retired concept is not visible; analytics over a graph is analytics
over what is visible") is coherent for **current-belief** reads. A
historical read asks what the crate's own BCDM table
(`as_of_valid`/`as_of_recorded` docs, `src/graph/builder.rs:465–520`)
defines as *what did we believe at `r`*. At recorded-time `r`, the endpoint
was believed live. Applying the present-tense visibility filter to that
question silently narrows the answer with no error and no flag — the exact
failure class `AttributeModeUnstated` (D-085) was created to prevent
elsewhere ("past's graph wearing today's titles" was fixed for attributes;
this is its topological twin). House precedent agrees: Wave 1's retirement
decision (defect AB/Z cycle, *Macrame Implementation Plan v0.5.6*) —
"retirement means *not returned as of the instant asked about*,
uniformly" — is already instant-parameterized on its face; the loader's
live-row filter is where "the instant asked about" collapsed to *now* in
code predating `as_of_recorded`. The fix extends the principle to the
surface it was written for rather than inventing policy.

---

## 4. Proposed fix

Principle: **make node closure instant-aware; touch nothing else.** The
fold, the walk, the CTEs, and `drop_dangling_adjacency` are all correct —
only the node-attribute source follows the instant, after which closure
holds *at the instant* automatically. Everything below is gated on
`as_of_recorded` being set; with no recorded instant — including bare
past-`now_ts` valid-time reads — SQL text, parameters, results, and goldens
are byte-identical to today (this is what keeps every existing golden green
and what makes T3 a both-versions-green guard).

### 4.1 Loader: belief-at-instant hydration with loader semantics

In `load_subgraph_with`, when `traversal.as_of_recorded` is set, hydrate
from the recorded fold under six simultaneous constraints:

1. **Reuse the fold/retirement predicates, not the function.**
   `hydrate_at_time` (`src/temporal/as_of.rs:407`) already encodes the
   needed semantics — hot-log guard via the *same*
   `hot_log_answers_for` the loader's `check_recorded_reach`
   (`src/graph/builder.rs:843`) delegates to (one guard, one
   `RecordedInstantUnreachable`, two call sites); `ROW_NUMBER() OVER
   (PARTITION BY entity_id …)` latest-row-per-concept fold under
   `recorded_at <= ts` (partitioning on `entity_id` alone is deliberate
   for concepts — one concept row per id across the ledger, per the
   comment at `:430–450`); retired-as-of-`ts` payloads skipped ("not
   visible, and not an error either"); valid-interval filtering applied in
   Rust against the payload's bounds (v1 payloads without bounds treated
   as unbounded); corrupt payloads raising `ReplayCorrupt` instead of
   skipping; payload-version gate. But it returns the wrong type, has no
   budget or opt-in, and is private — so the loader needs its own
   incremental hydrator (or a shared lower-level fold plus a `pub(crate)`
   exposure), not a call into it.
2. **`NodeData` shape preserved.** `NodeData`
   (`src/graph/subgraph.rs:112`: `title`, `content?`, `embedding_model?`,
   `extra?`, `valid_from`, `valid_to`) must carry the interval from the
   payload believed at `r`, not from today's `concepts` row.
3. **Opt-in fields stay opt-in.** `content`/`extra` flags keep their
   D-286 semantics (govern budget entry, per `hydrate`'s construction).
4. **Budget refusal stays incremental.** Bytes accumulate per landed node
   against the caller's running total; refusal fires mid-loop
   (`SubgraphTooLarge { n, budget }`) — never allocate-then-check.
5. **Chunk discipline preserved** (`HYDRATE_CHUNK` queries, defect AE).
6. **`drop_dangling_adjacency` and `is_closed` untouched.** Nodes
   live-at-`r` hydrate; never-existed and retired-as-of-`r` do not; edges
   to them prune exactly as today — closure becomes instant-parameterized
   with no invariant change (D-140 asserts stay green unweakened).

### 4.2 Ids path: same treatment in both projections

In `build_sql_with`, when `as_of_recorded` is set, the
`WHERE c.retired = 0` node filter in *both* projections becomes
belief-at-instant — e.g. a folded-concepts join over the reached ids
mirroring `links_at_tx_cte` (`src/graph/plan.rs:368`), or an equivalent
existence check against the recorded fold — honoring the D-073 both-halves
contract (a walk-only or projection-only fix reintroduces the closed
class). Without this, `execute_ids` keeps answering the narrowed set while
the fixed loader answers the full one. `execute`+`AtTime` is repaired
transitively (C4).

### 4.3 Valid-time-only reads: unchanged

Past `now_ts` with no `as_of_recorded` keeps present-tense closure
(current belief about what was true at `v` — the concept *is* retired now,
so dropping is consistent). Minimal blast radius, coherent semantics;
callers wanting history set `as_of_recorded`.

### 4.4 Open design decision (for the maintainer): the mode contract

One decision, decided whole before wiring the builder's mode into loading
(C6). Rows = builder mode, columns = read kind:

| Builder mode | No instants (current belief) | `as_of_recorded` set (historical) |
|---|---|---|
| Unstated | live hydration, as today | (a) `AttributeModeUnstated` refusal, or (b) default `AtTime` |
| `AtTime` | N/A on the loader today (ignored) — define or refuse | belief-at-`r` hydration |
| `Current` | live hydration, as today | live hydration with historical *topology* — explicit; must not silently become `AtTime` |
| `Omit` | define: closure still needs node data — say what is stored vs. returned | same, at the instant |

Option (a) matches the house T3.2 stance and reuses the existing, proven
refusal (F9 in the review record); option (b) breaks no existing Rust caller
(F10). Either way the change must not silently override an explicit mode,
and the Python `AttributeModeUnstatedError` text ("…default answer is live
text") gains its historical-load caveat (C7).

### 4.5 Explicitly out of scope

- `replay.rs` folds, `links_at_tx_cte`, `walk_cte`, `reconstruct`,
  branch/lineage resolution (`resolve_for`,
  `src/graph/lineage.rs:171`), archive/cold paths — verified correct
  during narrowing; not touched.
- No schema, migration, table, or index changes (reads existing tables
  only).
- No change to `drop_dangling_adjacency`, the closure invariant and its
  `is_closed` asserts, or any current-belief SQL text.

---

## 5. Blast radius and compatibility

- **Rust current-belief reads** (no `as_of_recorded`): zero change — same
  SQL, same params, same results, same goldens (the only in-crate Rust
  caller, `BranchView::load_subgraph`, passes no instants).
- **Historical reads** (`as_of_recorded` set — Rust *and* Python):
  strictly *more* complete. Anyone depending on the narrowed behavior sees
  more rows; that is the correction itself.
- **Python surface**: behavior change is confined to calls passing
  `as_of_recorded=`/`as_of_valid=`; all other Python calls are unaffected
  in effect. The 0.19.1 release note covers the Python keyword (C7).
- **Refusals preserved**: pre-hot-log instants still get the named
  `RecordedInstantUnreachable` on both arms; corrupt payloads still refuse
  rather than skip; `SubgraphTooLarge` mid-hydration semantics preserved.

---

## 6. Tests to prove it landed

### 6.1 Setup helper (all tests; corrected per C1)

Open temp store; assert concepts `A`, `B` and edge `A→B CALLS` open
(`valid_from = 2026-01-01T00:00:00.000000Z`, open `valid_to`); capture
`t1 = MAX(recorded_at)`; **re-upsert `B` with `retired(true)`**
(title v1→v2 so payloads are distinguishable; interval kept open; edge
left spanning `t1`). All stamps canonical (D-029). If asserting `content`
or `extra`, opt into `.content(true)` / `.extra(true)` — both are `None`
by default.

### 6.2 Acceptance tests (new, in `macrame-db`)

| # | Test | Call | Must hold (pre-fix → post-fix) |
|---|---|---|---|
| T1 | Historical loader keeps post-instant-retired edge | `.as_of_recorded(t1)` loader; bare builder as control | Historical: absent → present with at-`t1` payload; bare: absent → absent |
| T2 | Historical ids agree with loader | `execute_ids` with `.as_of_recorded(t1)` | `B` excluded → included |
| T3 | Current belief unchanged | Bare builder at `now` | Absent → absent (green both versions; guards §4.1) |
| T4 | Never-existed stays absent | Loader + ids from start `ZZZ` at `t1` | **Empty graph / `[]`, no error** → same (green both versions) |
| T5 | Pre-instant retirement stays dropped | Second fixture: concept retired with effect before `t1`; loader with `.as_of_recorded(t1)` | Absent → absent (green both versions; pins the lower bound) |
| T6 | Attribute modes | Retitle `B` after `t1` (on T1's fixture shape so the node reaches hydration); `execute` `AtTime` vs `Current` at `t1` | AtTime: node missing → old title; Current: live title → live title (gated on §4.2 + §4.4) |
| T7 | Cold instant still refuses | Loader with `.as_of_recorded(t0)`, `t0` predating hot-log coverage | `RecordedInstantUnreachable` → same (covers the existing guard) |

T1 is the test that fails pre-fix and passes after; T3–T5 and T7 must pass
both before and after. T6's pre-fix failure mode is a *missing node*
(C4/C8) — the table states it so the pre-fix run teaches the right lesson.

### 6.3 Golden-string updates

- Current-belief SQL text remains byte-identical: assert in the new tests
  by comparing `build_sql()` output for a bare builder against the
  existing goldens — no new goldens there.
- New goldens only for the historical shapes: loader statement with
  `as_of_recorded` set, and both `build_sql_with` projections with the
  instant-aware node filter.
- If §4.4 chooses refusal, add the `AttributeModeUnstated`-on-loader
  test; plus at least one historical assertion through the Python
  `as_of_recorded=` keyword (C7).

### 6.4 Done criteria for v0.19.1

1. T1–T7 green; no existing test changed except golden updates strictly
   confined to historical-shape SQL.
2. `cargo test` (default + `--no-default-features`, per house MSRV/CI
   practice) fully green; clippy/rustfmt clean.
3. Decision record (D-289, +D-290/D-291) with Evidence lines and suite
   deltas; loader + builder doc comments updated (the
   `load_subgraph_with` "attribute_mode is still ignored" note at
   `src/graph/subgraph.rs:904` and the `Subgraph` closure docs gain their
   instant-aware counterparts).
4. Downstream check: CodeRadar un-ignores its two loader reproducer tests
   against 0.19.1 and they pass (independent confirmation from a real
   consumer fixture).
5. Docs currency per §7.3 (register entries, s5 §5.2 at
   `docs/architecture/s5-modules.md:470` / §5.4 at `:831`, README
   corollary, release note incl. the Python keyword,
   `docs/architecture/api-review-0.19.1.md` if the house requires one per
   release — 0.19.0 has `docs/architecture/api-review-0.19.0.md`;
   `docs/architecture/public-api.txt` re-blessed **only** if a public
   signature moves) with `doc_link_tests::every_cross_reference_resolves`
   (`tests/doc_link_tests.rs`) green.

---

## 7. Decision-register mapping and docs currency

The register is normative in this house (`every_cross_reference_resolves`
fails CI on dangling anchors — D-280 — so new entries ship with correct
`<a id>` anchors and backlinks from day one). Highest entry to date is
**D-288** (verified); the fix needs the next numbers.

### 7.1 Existing decisions this affects

| Decision | Why it is in scope | Verdict |
|---|---|---|
| **D-174** (`as_of_valid` / `as_of_recorded`, the BCDM cell) | The gap answers "what did we believe at `r`" with present-tense visibility. The contract is violated, not the implementation detail. | Amended by D-289 (scope note, not a rewrite) |
| **D-085** (historical traversal must state which text) | The loader ignores `attribute_mode` entirely; §4.4 decides what a historical load hydrates. | Extended by D-289/D-290 |
| **D-073** (filters appear in walk *and* projection) | The instant-aware node filter follows the same both-halves contract; a walk-only or projection-only fix reintroduces the class D-073 closed (the loader's own comment restates it). | Preserved; new tests pin it |
| **D-286** (`hydrate` on the subgraph path: chunking, byte budget, opt-in `extra`) | `hydrate` is the function being branched. Budget-refusal-inside-the-loop, `HYDRATE_CHUNK` discipline, and opt-in flags must survive into the historical arm (§4.1 items 3–5). | Preserved; shared lower-level fold if the arms can share it without coupling |
| **D-022** (concepts never deleted; retirement is the mechanism) | The reason the fix folds the log instead of joining live rows: retired history is queryable *because* nothing is ever deleted. | Context only, unchanged |
| **D-140** (`is_closed` asserts at algorithm entries) | Closure stays enforced — now instant-parameterized. The asserts stay green without weakening. | Preserved; T-suite asserts it at `t1` |
| Defect Z / Wave 1 lineage (§5.4: closure invariant origin) | The invariant's third stress case: Wave 1 (leak), D-140 (audit), now instant-parameterization. Wave 1's retirement principle is the cited precedent (§3.3). | Cited, not reopened |
| D-029 (canonical timestamps) | The fix's interval predicates rely on lexicographic ordering; the new tests must use canonical stamps or they prove nothing. | Unchanged; test hygiene |

### 7.2 New decision numbers required

- **D-289** — *Historical loads hydrate belief-at-instant.* The loader fix
  (§4.1) plus the mode contract (§4.4): incremental historical hydrator
  gated on `as_of_recorded`, instant-parameterized closure, §4.1 scoping
  (current-belief reads unchanged). References: D-174 (amends scope),
  D-085 (extends), D-073, D-286, D-022.
- **D-290** — *`execute_ids` node projections match the loader at
  instants* (§4.2; repairs `execute`+`AtTime` transitively). If the
  maintainer prefers one entry per change, the mode decision becomes
  D-290 and ids parity D-291; either way the numbers are reserved here so
  parallel work does not collide.

### 7.3 Architecture docs and README currency (part of "done")

- `docs/architecture/s13-decision-register.md`: D-289 (+D-290/D-291) with
  Evidence lines (source paths, test names, suite deltas — house style
  counts suites before/after in the entry).
- `docs/architecture/s5-modules.md`: §5.2 (traversal fidelity) and §5.4
  (subgraph loader) gain the instant-aware-closure paragraphs; the
  `load_subgraph_with` "attribute_mode is still ignored" note
  (`src/graph/subgraph.rs:904`) is updated or removed, not left to rot.
- `README.md`: the bitemporal table row (which cites D-174) gains the
  one-line corollary — historical loads hydrate belief-at-instant per
  D-289 — or an explicit pointer. README cites decisions by anchor; keep
  them resolving.
- Python binding: the `AttributeModeUnstatedError` text
  (`bindings/python/src/errors.rs`) gains its historical-load caveat once
  belief-at-instant hydration lands (C7).
- Release note for 0.19.1 (covering the Python keyword) +
  `docs/architecture/api-review-0.19.1.md` if the house requires one per
  release; `docs/architecture/public-api.txt` re-blessed **only** if a
  public signature moves (§4.1's shaping avoids it; §4.4 refusal would
  not).
- `docs/architecture/appendices.md` (Appendix A normative API): only if
  signatures change; otherwise untouched.
- `tests/doc_link_tests.rs::every_cross_reference_resolves` must pass —
  every new `s13` anchor link in code comments, README, and s5 must
  resolve, per the D-280 precedent.

### 7.4 Amendment mechanics (house rule — how the register changes)

Existing entries are never silently rewritten; corrections arrive as new,
linked text. Three mechanisms, in order of how much they touch the old
entry: (1) new numbered entry + back-pointer — the norm for later releases
(the D-070→D-076 model; what this fix uses: D-174 and D-085 keep their
bodies, each gaining an "Amended by D-…" line); (2) marked amendment inside
the same entry (the D-281 model — same-release elaborations only, does not
apply here); (3) letter-suffixed siblings (D-008a/D-008b — viable if both
fixes land atomically, but fresh numbers D-289/D-290 remain the
recommendation). Supporting precedents: D-037 corrected a false claim in
§4.1 *"in place with the correction visible, not overwritten"*; D-040
records divergences *"rather than erased."* Mechanical constraint: the link
gate fails CI on dangling anchors, so every back-pointer ships as a valid
link from day one (D-280) — which is why anchor validity is a
done-criterion in §6.4, not an afterthought.

---

## Landing record (0.19.1)

What §4 specified is what landed, with two corrections found during implementation:

- **§4.4 decided as option (a).** An unstated mode with a recorded instant refuses (`resolved_mode`, D-085); `AtTime` hydrates belief-at-the-instant, `Current` keeps live text over historical topology, `Omit` keeps topology only (`src/graph/subgraph.rs`: `hydrate_historical`, `hydrate_topology_only`). The shared fold/visibility predicates live once in `src/temporal/as_of.rs` (`fold_concepts_at`, `concept_visible_at`), with `hydrate_at_time` refactored onto them — behavior identical, one definition.
- **C10 — §5's Python bullet corrected: Python `load_subgraph` is byte-identical, not more complete.** The binding offers no `attribute_mode` keyword and promises live attributes, so it states `Current` on the caller's behalf (`bindings/python/src/database.rs`) rather than leaving the mode unstated to refuse. A Python historical load therefore keeps live text over the corrected topology — the same shape as 0.19.0, with no new refusal and no silent text change. The "strictly more complete" claim holds for the Rust API only. A keyword that would let Python callers ask for `AtTime` is follow-up work, not this fix.
- **Plan gates re-examined, not just updated (`tests/bitemporal_plan_tests.rs`).** The concepts fold is a new `transaction_log` consumer, so D-196/D-254 were re-measured: the transaction-time triple moved `(4, 3, 5)` → `(4, 4, 9)` (the fold's own seek plus its partial window sort), and the two-dimensional candidate is now reached on its leading column by the concepts fold only — a measured wash (`(4, 4, 9)` → `(4, 3, 10)`), still declined, with the gate re-pinned to those exact terms. All pin updates confined to historical-shape SQL per §6.3.

Tests landed: `tests/loader_history_tests.rs` (T1–T8 plus historical-SQL pins — T6's `Current` arm pins absence, proving the mode never silently becomes `AtTime`; T7 archives through a raw connection with deterministic stamps because `read_conn` is read-only) and one `tests_py` pin through `as_of_recorded=` (C7; requires a maturin run — unwitnessed in this environment, CI confirms). Full gates: default suite green (lib 167, all integration binaries incl. 4 property suites under `property-tests`), `--no-default-features` green, doc tests green, clippy/rustfmt clean. Full-parallel `cargo test` segfaults intermittently on this Windows host **with and without the fix** (baseline reproduces) — a libsql-under-load environment flake, worked around by serial/partitioned runs; not a product signal.

Remaining follow-ups (not this fix): CodeRadar un-ignores its two loader reproducer tests against 0.19.1 (§6.4 item 4); 0.19.1 release note covering the Python keyword; `docs/architecture/api-review-0.19.1.md` only if the house requires one per release — the public surface is unchanged (`api_growth_tests` green), so there is nothing to review.

## Appendix A. Ledger shapes

### A.1 Edge-retirement shape (from the live narrowing session — does NOT reproduce)

After the original §2 step 3 (`t1 = 2026-10-06T17:05:59.420081Z`):

`links` (both rows present, correct bitemporal pair):

| source | target | valid_from | valid_to | recorded_at |
|---|---|---|---|---|
| caller | old_name | …59.419866Z | 9999… (open) | t1 (…59.420081Z) |
| caller | old_name | …59.419866Z | …59.471933Z | …59.472242Z |
| caller | new_name | …59.474526Z | 9999… (open) | …59.474692Z |

`links_current`: the closing old_name row + the open new_name row (one row
per key — correct). `transaction_log`: `I, I` on entity
`caller|old_name|CALLS|<valid_from>`. With both endpoint concepts live,
both builder shapes return the edge at `t1` (P1) — this shape is retained
here only so nobody re-narrows it.

### A.2 Concept-retirement shape (the corrected reproducer — reproduces)

After §2 step 3 of this document: `links`/`links_current` hold the open
`A→B` row spanning `t1` (unchanged — no `retire_edge` needed); the live
`concepts` row for `B` carries the v2 title with `retired = 1`; the
`transaction_log` `concepts` fold at `t1` still yields the v1 payload
(`title` v1, `retired` 0, open interval). Hence the walk and every fold
see `B`-at-`t1` as live, while `hydrate`'s `WHERE retired = 0` and both
`execute_ids` projections see today's retired row and drop it — the exact
split §3 describes, observed as loader → 1 node / 0 edges,
`execute_ids` → `["A"]`, fold/reconstruct → `B` live with v1 payload (P2).

---

## Appendix B. Corrected minimal test (drop-in sketch)

```rust
#[tokio::test]
async fn historical_loader_keeps_edge_whose_concept_retired_after_instant() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("t.db")).await.unwrap();
    // A live; B v1 live.
    db.upsert_concept(ConceptUpsert::new("a::x", "a-v1")
        .content("doc-a")
        .valid_from("2026-01-01T00:00:00.000000Z")
        .valid_to("9999-12-31T23:59:59.999999Z")
        .retired(false)).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("a::y", "b-v1")
        .content("doc-b")
        .valid_from("2026-01-01T00:00:00.000000Z")
        .valid_to("9999-12-31T23:59:59.999999Z")
        .retired(false)).await.unwrap();
    // Edge open across t1.
    db.assert_edge(EdgeAssertion::new("a::x", "a::y", "CALLS")
        .valid_from("2026-01-01T00:00:00.000000Z")
        .weight(1.0)
        .properties("{}")).await.unwrap();
    let t1: String = max_recorded_at(&db); // SELECT MAX(recorded_at) ...
    // THE load-bearing step the original sketch lacked: retire the
    // ENDPOINT CONCEPT after t1. Edge stays open.
    db.upsert_concept(ConceptUpsert::new("a::y", "b-v2")
        .content("doc-b")
        .valid_from("2026-01-01T00:00:00.000000Z")
        .valid_to("9999-12-31T23:59:59.999999Z")
        .retired(true)).await.unwrap();

    // Historical shape: the gap. FAILS pre-fix (1 node, 0 edges);
    // must PASS post-fix with the at-t1 payload on B.
    let hist = db.load_subgraph_with(
        &TraversalBuilder::new("a::x").max_depth(2).as_of_recorded(&t1),
        &t1, 10_000_000).await.unwrap();
    assert!(hist.out_edges("a::x").iter().any(|e| e.node(&hist) == "a::y"),
        "edge live at t1 must survive a historical load at t1");

    // Current-belief control: stays absent both pre- and post-fix.
    let bare = db.load_subgraph_with(
        &TraversalBuilder::new("a::x").max_depth(2),
        &t1, 10_000_000).await.unwrap();
    assert!(!bare.out_edges("a::x").iter().any(|e| e.node(&bare) == "a::y"),
        "current belief excludes the now-retired endpoint");

    // Projection parity (T2): pre-fix ["a::x"], post-fix includes "a::y".
    let ids = TraversalBuilder::new("a::x").max_depth(2)
        .as_of_recorded(&t1)
        .execute_ids(db.read_conn(), &t1).await.unwrap();
    assert!(ids.iter().any(|i| i == "a::y"),
        "execute_ids must agree with the fixed loader at t1");
}
```

All builder methods, upsert setters, `retire_edge`'s absence here, and the
`Subgraph`/`execute_ids` accessors above are the public API this checkout
compiles against (the P1–P5 probes compiled and ran verbatim against it).

---

## Appendix C. Review provenance

- **Read:** the original document end to end; `src/graph/subgraph.rs`
  (loader `:921–1010`, `hydrate` `:1103–1161`, `drop_dangling_adjacency`
  `:516`, closure docs `:18–60`, `is_closed` `:548`, `NodeData` `:112`);
  `src/graph/builder.rs` (projections `:595–650`, `bind_params` `:728`,
  `check_recorded_reach` `:843`, `walk_cte` `:912`, `execute` `:1070–1079`,
  `resolved_mode` `:1088–1097`, `valid_instant` `:518`, instant setters
  `:465–520`); `src/temporal/as_of.rs` (`NodeAttributes` `:59`,
  `hydrate_attributes` `:290`, `hydrate_at_time` `:407–560`);
  `src/graph/plan.rs` (`links_at_tx_cte` `:368`);
  `src/graph/lineage.rs:171` (`resolve_for`); `src/connection.rs`
  (`retire_edge` `:2539`, internal `:5898`); `src/branch.rs:338`;
  `bindings/python/src/database.rs` (`load_subgraph`) and
  `bindings/python/src/errors.rs` (`AttributeModeUnstatedError`).
- **Executed:** probes P1 (sketch as written — edge present both shapes),
  P2 (concept-retirement shape — gap reproduced; all controls held),
  P3 (nonexistent start — empty graph), P4 (pre-instant retirement —
  stays dropped), P5 (opt-in content, `SubgraphTooLarge { n: 322,
  budget: 10 }`). Binary deleted after the run; tree stayed doc-only.
- **Inferred (stated as such):** the transcription-drop reconciliation of
  the original live-session observation (C1) — consistent with every
  executed signature, but the companion CodeRadar fixture was not in this
  checkout.
- **New in this consolidation (C9):** the §7.3 doc paths
  (`docs/architecture/api-review-0.19.0.md`,
  `docs/architecture/public-api.txt`, `tests/doc_link_tests.rs`) and the
  strengthened citations throughout (§§3–4 quote exact predicates, comments,
  and line numbers re-verified on `8aa74b7`).
