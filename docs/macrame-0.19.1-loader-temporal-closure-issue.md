# `load_subgraph_with` at a historical instant drops edges retired after the instant

| | |
|---|---|
| **Status** | Proposed — root-caused, reproducer kept |
| **Affects** | `macrame-db` 0.19.0 (verified against the crates.io release) |
| **Target** | v0.19.1 |
| **Scope** | Read path only. No schema, migration, or table changes |
| **Companion reproducer** | `core_indexer/tests/repro_walk_retired.rs` in CodeRadar (pure-`macrame` APIs, zero CodeRadar layers; loader tests `#[ignore]`d pending this fix) |

All `src/` references below are relative to the `macrame-db` crate root, with
line numbers from the 0.19.0 release (`≈` — verify on current `main`).

---

## 1. Summary

`Database::load_subgraph_with` answers a historical question with a
present-tense node filter. The walk itself is correct: at a historical instant
it finds edges whose valid interval spans the instant. But node hydration reads
exclusively from live rows (`concepts WHERE retired = 0`), and
`drop_dangling_adjacency` then enforces the closure invariant by pruning every
edge whose endpoint lacks node data. Net effect: **any edge whose endpoint
retired *after* the requested instant is silently missing from a historical
subgraph**, even though the endpoint was fully live at that instant.

For current-belief reads this is documented, deliberate design (§4.1:
retired = not visible). For historical reads it contradicts the bitemporal
contract the `TraversalBuilder` advertises (the BCDM cell: *what did we
believe at `r` about what was true at `v`*). `reconstruct` folds correctly —
only the subgraph-loader path (and the `execute_ids` projection, secondarily)
is affected.

---

## 2. Observed behavior

Minimal shape (timestamps are example values; the property is general):

1. Assert concepts `A`, `B` and edge `A→B CALLS` with
   `valid_from = 2026-01-01T00:00:00.000000Z` (open `valid_to`).
2. Capture `t1 = MAX(recorded_at)` from `transaction_log`.
3. Retire the edge (`retire_edge` with `valid_to` after `t1`).
   The ledger then holds the textbook bitemporal pair:
   - open row, `recorded_at = t1`;
   - closing row, same `valid_from`, `valid_to` set, later `recorded_at`.
   `links_current` holds the closing row only (one row per key — correct).
4. `load_subgraph_with(TraversalBuilder::new(A).max_depth(2), t1)` returns a
   subgraph **without** the `A→B` edge — with *and* without
   `.as_of_recorded(t1)` on the builder.
5. The equivalent SQL executed by hand (walk CTE over `links_current`, edge
   projection with `valid_from <= t1 < valid_to`, and the `links_at_tx`
   recorded fold) **finds** the open row in all shapes.
6. `reconstruct(t1).edges` contains the open row; entity lookup at `t1`
   resolves `B`. Only the loader path is blind.

Real-world instance: a rename fixture (`old_name` → `new_name` between `t1`
and `t2`) — `traverse(out)` at `t1` missed the v1 `CALLS` edge at its own
`MAX(recorded_at)`.

---

## 3. Root cause (verified, step by step)

Narrowing eliminated, in order: the instant handling (`as_of_recorded` does
not help), caller-side misuse (pure-`macrame` reproducer behaves identically),
the data (hand SQL finds the row), the SQL text and bound parameters
(instrumented the loader: `shape=Trunk`, params
`[start, depth, t1, 0.0]` — identical to the working hand query on the same
connection), and the walk itself (`execute_ids` reaches the node; see §3.1).

What remains is the loader's tail in `src/graph/subgraph.rs`
(`load_subgraph_with`, ≈ line 921):

1. The topology loop (walk + projection) collects the edge correctly.
2. `hydrate` (≈ line 1104) fills node attributes with:
   ```sql
   SELECT id, title, content, embedding_model, valid_from, valid_to, extra
   FROM concepts WHERE retired = 0 AND id IN (...)
   ```
   The endpoint retired after the instant has `retired = 1` in the **live**
   table, so it gets no `NodeData`. (Single caller of `hydrate` is the
   loader itself.)
3. `graph.drop_dangling_adjacency()` (≈ line 1081; implementation ≈ line 516)
   enforces the documented closure invariant — *"every id appearing in
   `out_adj` or `in_adj` is a key of `nodes`"* (`src/graph/subgraph.rs`
   ≈ lines 18–60) — by retaining only edges whose endpoints hydrated. The
   historically-live edge is pruned as "dangling".

### 3.1 The secondary finding: `execute_ids` / `build_sql`

`TraversalBuilder::build_sql_with` (`src/graph/builder.rs`, ≈ line 596) and
`LIMITED_PROJECTION` project reached nodes through:

```sql
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
```

The walk reaches the historically-live node; the projection drops it on the
live `retired` flag — openly, in the SQL text. Same present-tense leakage,
second surface. (`execute_ids` was used during narrowing to prove the walk
itself works: it returns the live start node and drops only the retired
endpoint.)

### 3.2 Why this is a gap and not the §4.1 design

§4.1 ("a retired concept is not visible; analytics over a graph is analytics
over what is visible") is coherent for **current-belief** reads. A historical
read asks a different question — one the crate's own BCDM table
(`src/graph/builder.rs`, `as_of_valid`/`as_of_recorded` docs) defines as
*what did we believe at `r`*. At recorded-time `r`, the endpoint was believed
live. Applying the present-tense visibility filter to that question silently
narrows the answer, with no error and no flag — the exact failure class the
crate's `AttributeModeUnstated` refusal (T3.2, D-085) was created to prevent
elsewhere ("past's graph wearing today's titles" was fixed for attributes;
this is its topological twin: past's graph with today's visibility).

---

## 4. Proposed fix

Principle: **make node closure instant-aware; touch nothing else.** The fold,
the walk, the CTEs, and `drop_dangling_adjacency` are all correct — only the
node-attribute source needs to follow the instant, after which closure holds
*at the instant* automatically.

### 4.1 Loader: hydrate from belief-at-instant when one is set

In `load_subgraph_with` (`src/graph/subgraph.rs`), when
`traversal.as_of_recorded` is set, hydrate via the existing
`hydrate_at_time(conn, &ids, recorded_ts, valid)` (`src/temporal/as_of.rs`,
≈ line 407) instead of live `hydrate`:

- It already does everything needed: hot-log guard (same named refusal the
  loader's `check_recorded_reach` produces), chunked reads, latest-row-per-
  entity fold on `recorded_at`, retired-as-of-ts filtering, valid-interval
  filtering, full `NodeAttributes` construction (title/content/
  embedding_model/extra), corrupt-payload refusal instead of silent skips.
- Reuse its chunking/budget pattern so the loader's `byte_budget` refusal
  semantics are preserved (hydrate-then-check, same as today).
- `drop_dangling_adjacency` is then correct unmodified: nodes live-at-the-
  instant hydrate; never-existed and retired-before-the-instant do not; edges
  to them prune exactly as today.

When **no** recorded instant is set (all current-belief reads, including bare
past-`now_ts` valid-time reads), behavior and SQL are byte-identical to today.
This gates the entire behavior change behind `as_of_recorded`, which is also
what keeps every existing SQL golden green.

### 4.2 Ids path: same treatment in the projections

In `build_sql_with` (unlimited projection and `LIMITED_PROJECTION`,
`src/graph/builder.rs`), when `as_of_recorded` is set, the
`WHERE c.retired = 0` node filter must become belief-at-instant — e.g. a
folded-concepts join scoped to the reached ids mirroring `links_at_tx_cte`
(`src/graph/plan.rs`, ≈ line 368), or an equivalent existence check against
the recorded fold. Without this, `execute_ids` keeps answering a narrowed
set while the loader (fixed per §4.1) answers the full one, and the two
paths disagree about what was reachable.

### 4.3 Open design decisions (for the maintainer)

1. **Valid-time-only reads** (past `now_ts`, no `as_of_recorded`): leave
   present-tense closure (current belief about what was true at `v` — the
   concept *is* retired now, so dropping is consistent), or extend the fix?
   Recommendation: leave unchanged — minimal blast radius, coherent
   semantics. Callers wanting history set `as_of_recorded`.
2. **`attribute_mode` on the loader**: currently ignored there ("hydration
   is always the live concept row — deliberate"). Options: (a) mirror
   `execute` and require an explicit mode on historical reads
   (`AttributeModeUnstated`); (b) default historical loads to `AtTime`.
   (a) matches the crate's T3.2 stance; (b) breaks no existing caller.
   The in-crate loader caller (`branch.rs`, `load_subgraph`) passes no
   instants, so either choice is behavior-neutral in-crate.
3. **Decision record**: D-289 (loader, §4.1) and D-290 (ids parity + mode
   wiring, §4.2–§4.3) per §7.2 — amending §4.1 to scope "retired = not
   visible" to current-belief reads, with historical reads hydrating
   belief-at-instant. Process overhead only, per house conventions.

### 4.4 Explicitly out of scope

- `replay.rs` folds, `links_at_tx_cte`, `walk_cte`, `reconstruct`,
  branch/lineage resolution, archive/cold paths — all verified correct during
  narrowing; not touched.
- No schema, migration, table, or index changes (reads existing tables only).
- No change to `drop_dangling_adjacency`, the closure invariant, or any
  current-belief SQL text.

---

## 5. Blast radius and compatibility

- **Current-belief reads** (no `as_of_recorded`): zero change — same SQL,
  same params, same results, same goldens.
- **Historical-instant reads** (`as_of_recorded` set): strictly *more*
  complete — edges/nodes live-at-the-instant stop being silently dropped.
  Anyone depending on the narrowed behavior sees more rows; that is the
  correction itself.
- **Refusals preserved**: pre-hot-log instants still get the named
  `RecordedInstantUnreachable` (the guard lives in both `check_recorded_reach`
  and `hydrate_at_time`); corrupt payloads still refuse rather than skip.
- In-crate, the only `load_subgraph_with` caller passes no instants:
  in-crate behavior change is nil.

---

## 6. Tests to prove it landed

### 6.1 Acceptance tests (new, in `macrame-db`)

Setup helper used by all: open temp store; assert concepts `A`, `B` and edge
`A→B CALLS` open; capture `t1 = MAX(recorded_at)`; retire the edge with
`valid_to > t1`. (Reference implementation: the companion reproducer,
`setup()` — pure-`macrame` calls only.)

| # | Test | Setup extra | Call | Must hold |
|---|------|-------------|------|------------|
| T1 | Historical loader keeps post-instant-retired edge | — | `load_subgraph_with(bare builder, t1)` **and** `.as_of_recorded(t1)` builder | `A→B` present in both; endpoint attributes are the at-`t1` payload |
| T2 | Historical ids agree with loader | — | `execute_ids` with `.as_of_recorded(t1)` | `B` included (matches T1's reachability) |
| T3 | Current belief unchanged | — | bare builder at `now` | `A→B` **absent** (endpoint retired now — pins §4.1 behavior) |
| T4 | Never-existed stays absent | start `ZZZ` (no rows) | loader + ids at `t1` | start node only; no edges; no error |
| T5 | Pre-instant retirement stays dropped | retire with `valid_to < t1` (second fixture) | loader with `.as_of_recorded(t1)` | `A→B` **absent** (was already gone at `t1`) |
| T6 | Attribute modes (via `execute`, if loader takes mode per §4.3) | retitle `B` after `t1` | `AtTime` vs `Current` at `t1` | old title / live title respectively |
| T7 | Cold instant still refuses | `t0` predating hot-log coverage | loader with `.as_of_recorded(t0)` | `RecordedInstantUnreachable`, not an empty graph |

T1 is the test that fails on 0.19.0 and passes after the fix; T3 is the test
that must pass both before and after (guards the §4.1 design).

### 6.2 Golden-string updates

- Current-belief SQL text must remain byte-identical (assert in the new
  tests by comparing `build_sql()` output for a bare builder against the
  existing goldens — no new goldens needed there).
- New goldens only for the historical shapes: loader statement with
  `as_of_recorded` set, and both `build_sql_with` projections with the
  instant-aware node filter.
- If §4.3 option (a) is chosen (require mode), add the
  `AttributeModeUnstated`-on-loader test.

### 6.3 Done criteria for v0.19.1

1. T1–T7 green; no existing test changed except golden updates strictly
   confined to historical-shape SQL.
2. `cargo test` (default + `--no-default-features`, per house MSRV/CI
   practice) fully green; clippy/rustfmt clean.
3. Decision record (D-289, +D-290/D-291) with Evidence lines and suite deltas; loader + builder doc comments updated
   (the `load_subgraph_with` "attribute_mode is still ignored" note and the
   `Subgraph` closure docs gain their instant-aware counterparts).
4. Downstream check: CodeRadar un-ignores its two loader reproducer tests
   against 0.19.1 and they pass (independent confirmation from a real
   consumer fixture).
5. Docs currency per §7.3 (register entries, s5 §5.2/§5.4, README corollary,
   release note, `public-api.txt` only if signatures moved) with
   `doc_link_tests::every_cross_reference_resolves` green.

---

## 7. Decision-register mapping and docs currency

The register is normative in this house (`doc_link_tests::every_cross_reference_resolves` fails CI on dangling anchors — see D-280 — so new entries must ship with correct `<a id>` anchors and backlinks from day one). Highest entry to date is **D-288** (0.19.0); the fix below needs the next numbers.

### 7.1 Existing decisions this affects

| Decision | Why it is in scope | Verdict |
|---|---|---|
| **D-174** (`as_of_valid` / `as_of_recorded`, the BCDM cell) | The gap answers "what did we believe at `r`" with present-tense visibility. The contract is violated, not the implementation detail. | Amended by D-289 (scope note, not a rewrite) |
| **D-085** (historical traversal must state which text) | The loader ignores `attribute_mode` entirely; the fix must decide what a historical load hydrates (see §4.3). | Extended by D-289/D-290 |
| **D-073** (filters appear in walk *and* projection) | The instant-aware node filter must follow the same both-halves contract; a walk-only or projection-only fix reintroduces the class D-073 closed. | Preserved; new tests pin it |
| **D-286** (`hydrate` on the subgraph path: chunking, byte budget, opt-in `extra`) | `hydrate` is the function being branched. Budget-refusal-inside-the-loop and chunk discipline must survive into the historical arm. | Preserved; shared helper if the two arms can share it without coupling |
| **D-022** (concepts never deleted; retirement is the mechanism) | The reason the fix folds the log instead of joining live rows: retired history is queryable *because* nothing is ever deleted. | Context only, unchanged |
| **D-140** (`is_closed` asserts at algorithm entries) | Closure stays enforced — now instant-parameterized. The asserts must stay green without weakening. | Preserved; T-suite asserts it at `t1` |
| Defect Z / Wave 1 lineage (§5.4: closure invariant origin) | The invariant's third stress case: Wave 1 (leak), D-140 (audit), now instant-parameterization. | Cited, not reopened |
| D-029 (canonical timestamps) | The fix's interval predicates rely on lexicographic ordering; no change, but the new tests must use canonical stamps or they prove nothing. | Unchanged; test hygiene |

### 7.2 New decision numbers required

- **D-289** — *Historical loads hydrate belief-at-instant.* The loader fix (§4.1): `hydrate_at_time` arm gated on `as_of_recorded`, instant-parameterized closure, §4.1 scoping (current-belief reads unchanged). References: D-174 (amends scope), D-085 (extends), D-073, D-286, D-022.
- **D-290** — *`execute_ids` node projection matches the loader at instants* (§4.2), **plus** the loader's `attribute_mode` default/require decision (§4.3). If the maintainer prefers one entry per change, the mode decision becomes D-290 and the ids parity D-291. Either way the numbers are reserved in this document so parallel work does not collide.

### 7.3 Architecture docs and README currency (part of "done")

- `docs/architecture/s13-decision-register.md`: D-289 (+D-290/D-291) with Evidence lines (source paths, test names, suite deltas — house style counts suites before/after in the entry).
- `docs/architecture/s5-modules.md`: §5.2 (traversal fidelity) and §5.4 (subgraph loader) gain the instant-aware-closure paragraphs; the `load_subgraph_with` "attribute_mode is still ignored" note is updated or removed, not left to rot.
- `README.md`: the bitemporal table (which cites D-174) gains the one-line corollary — historical loads hydrate belief-at-instant per D-289 — or an explicit pointer. README cites decisions by anchor; keep them resolving.
- Release note for 0.19.1 + `api-review-0.19.1.md` if the house requires one per release (0.19.0 has `api-review-0.19.0.md`); `public-api.txt` re-blessed **only** if a public signature moves (the §4.1 shaping avoids it; §4.3 option (a) would not).
- `docs/architecture/appendices.md` (Appendix A normative API): only if signatures change; otherwise untouched.
- Cross-reference check `doc_link_tests::every_cross_reference_resolves` must pass — every new `s13` anchor link in code comments, README, and s5 must resolve, per the D-280 precedent.

### 7.4 Amendment mechanics (house rule — how the register changes)

The register is an audit trail first, documentation second: **existing
entries are never silently rewritten; corrections arrive as new, linked
text.** Three mechanisms, in order of how much they touch the old entry:

1. **New numbered entry + back-pointer (the norm for later releases).**
   The old body stands untouched as history; a pointer line is added
   ("Corrected by D-076", "Amended by D-…"). This is the D-070→D-076
   model, and it is what this fix uses: D-174 and D-085 keep their bodies,
   each gaining an "Amended by D-289" line.
2. **Marked amendment inside the same entry** ("Amendment 1…N", the D-281
   model). Reserved for elaborations of *that same* decision landing with
   its own feature — not for a later release scoping an older decision.
   Does not apply here.
3. **Letter-suffixed siblings** (D-008a/D-008b). For closely-related
   decisions landing atomically with the same change. Viable if the loader
   fix and the ids-projection fix land as one commit (D-289/D-289a style),
   but the house default for cross-release corrections is fresh numbers, so
   D-289/D-290 remains the recommendation.

Supporting precedents: D-037 corrected a false claim in §4.1 *"in place
with the correction visible, not overwritten"*; D-040 records divergences
*"rather than erased."* Mechanical constraint: `doc_link_tests` fails CI
on dangling anchors, so every back-pointer must ship as a valid link from
day one (D-280 documents exactly this trap) — which is why anchor
validity is a done-criterion in §6.3, not an afterthought.

## Appendix A. Reference ledger shape (from the live narrowing session)

After step 3 of §2 (edge retired; `t1 = 2026-10-06T17:05:59.420081Z`):

`links` (history — both rows present, correct bitemporal pair):

| source | target | valid_from | valid_to | recorded_at |
|---|---|---|---|---|
| caller | old_name | …59.419866Z | 9999… (open) | t1 (…59.420081Z) |
| caller | old_name | …59.419866Z | …59.471933Z | …59.472242Z |
| caller | new_name | …59.474526Z | 9999… (open) | …59.474692Z |

`links_current`: the closing old_name row + the open new_name row (one row
per key — correct).

`transaction_log`: `I, I` on entity `caller|old_name|CALLS|<valid_from>`
(recorded `t1`, then retirement time) — the retirement is a superseding
insert under the same edge key, which is why the open row is unrecoverable
from `links_current` alone and only the recorded fold (or `links`) sees it.

## Appendix B. Condensed minimal test (drop-in sketch)

```rust
#[test]
fn historical_loader_keeps_edge_retired_after_instant() {
    let rt = current_thread_rt();
    let dir = tempfile::tempdir().unwrap();
    let db = rt.block_on(Database::open(dir.path().join("t.db"))).unwrap();
    for id in ["a::x", "a::y"] {
        rt.block_on(db.upsert_concept(
            ConceptUpsert::new(id, id)
                .content("{}")
                .valid_from("2026-01-01T00:00:00.000000Z")
                .valid_to("9999-12-31T23:59:59.999999Z")
                .retired(false),
        )).unwrap();
    }
    rt.block_on(db.assert_edge(
        EdgeAssertion::new("a::x", "a::y", "CALLS")
            .valid_from("2026-01-01T00:00:00.000000Z")
            .weight(1.0)
            .properties("{}"),
    )).unwrap();
    let t1: String = max_recorded_at(&db); // SELECT MAX(recorded_at) ...
    rt.block_on(db.retire_edge(
        "a::x", "a::y", "CALLS",
        "2026-01-01T00:00:00.000000Z", "2027-01-01T00:00:00.000000Z",
    )).unwrap();

    let sub = rt.block_on(db.load_subgraph_with(
        &TraversalBuilder::new("a::x").max_depth(2).as_of_recorded(&t1),
        &t1, 10_000_000,
    )).unwrap();
    assert!(sub.out_edges("a::x").iter().any(|e| e.node(&sub) == "a::y"),
        "edge retired after t1 must survive a historical load at t1");
}
```

FAILS on 0.19.0 (empty adjacency — the reported gap); must PASS on 0.19.1.
The full companion file additionally covers the bare-builder shape, the
`reconstruct` control (passes on both versions), and an external-db probe
harness.

## Amendment 1 — Codebase review

**Review scope:** current `dev/0.19.1` sources in `src/graph/subgraph.rs`,
`src/graph/builder.rs`, `src/temporal/as_of.rs`, and `src/connection.rs`.
This amendment records source-review findings; it does not change the code.

### A1. Core mechanism confirmed, but only for a concept retired after `r`

The loader path described in §3 is present: `load_subgraph_with` checks
recorded-time reachability, projects the topology, calls `hydrate`, then calls
`drop_dangling_adjacency`. `hydrate` reads `concepts WHERE retired = 0`. Both
`build_sql_with` projections in `builder.rs` likewise join `concepts` and keep
`c.retired = 0`. By contrast, `hydrate_at_time` folds concept payloads from
`transaction_log`, rejects payloads retired at the requested instant, and
filters their valid intervals. Thus the root cause and the secondary
`execute_ids` mismatch are supported by the implementation **when an endpoint
concept was live at `r` but is retired in current belief**. The loader already
calls `check_recorded_reach`; T7 is coverage of an existing refusal, not a new
guard requirement.

### A2. The stated minimal reproducer does not produce the reported failure

In §2 and Appendix B, both concepts are created with `ConceptUpsert::new` (whose
default is `retired: false`), and the only later write is `retire_edge`. That
operation closes/asserts a **link** row; it does not retire either endpoint
concept. With `valid_to = 2027-01-01` and the query valid instant at
`2026-01-01`, the replacement edge is still valid at the query instant. `B`
therefore still matches the loader's live-concept hydration query, so
`drop_dangling_adjacency` has no missing endpoint to prune. As written, this
fixture should retain `A→B` on 0.19.0 and does not prove §3's root cause.

To reproduce the reported defect, keep `A→B` available at the queried valid
time and write a later concept version for `B` with `retired(true)` after
capturing `t1` (preserving the concept's other fields/interval). Then load at
both axes explicitly—e.g. `.as_of_valid(&t1).as_of_recorded(&t1)`—and verify
that the recorded-time result keeps `B` and `A→B`. The present `concepts` row
will now be retired, while the payload folded at `t1` will not be. The
edge-only retirement in the current sketch may be removed; if retained, it
must not be relied on to stand in for concept retirement.

Also distinguish the two clocks in §2/T1: `load_subgraph_with`'s `now_ts` is
the valid-time fallback (`TraversalBuilder::valid_instant`), not a recorded
instant. A bare builder passed `t1` still reads **current belief**. Under the
scope proposed in §4.1, a current-belief read at a past valid instant must
continue to exclude a concept retired now. Therefore T1 must not expect the
bare-builder shape to restore the endpoint; that is the current-belief control
(and should remain absent). The historical expectation belongs on the builder
with `as_of_recorded(t1)` set.

### A3. The proposed helper is not a drop-in loader hydrator

`hydrate_at_time` returns `NodeAttributes`, whereas the loader stores `NodeData`,
which also carries `valid_from` and `valid_to`. The latter must come from the
concept payload believed at `r` for a historical load—not from today's
`concepts` row. In addition, loader `content` and `extra` are opt-in builder
flags, and its existing `hydrate` accounts bytes as rows enter the graph,
refusing as soon as `byte_budget` is crossed. `hydrate_at_time` chunks SQL but
collects all returned attributes into a `HashMap` and has no loader byte-budget
or content/extra controls. Calling it and checking the budget afterwards would
not preserve the loader's incremental allocation/refusal behavior and could
load content/extra that the caller did not request.

Accordingly, §4.1 should treat `hydrate_at_time` as the fold/retirement
semantics to reuse, not as a complete drop-in implementation. The loader path
needs to preserve its opt-in fields and incremental budget accounting, and
supply historical `valid_from`/`valid_to` as well as the historical attributes.
A shared lower-level fold or a loader-specific incremental hydrator can do
that; the amendment does not choose between them.

### A4. Attribute-mode policy needs to cover explicit modes too

The loader's rustdoc currently says `attribute_mode` is ignored and hydration
always uses the live concept row; `load_subgraph_with` does not call
`resolved_mode`. The §4.3 choice between refusing an unstated historical mode
and defaulting to `AtTime` is therefore incomplete unless it also says what an
explicit `Current` or `Omit` means on this API. Make that contract explicit
before wiring the builder's mode into loading. In particular, the change must
not silently override a caller's explicit mode, nor claim `Omit` while still
requiring/hydrating node data to establish subgraph closure.

### A5. Acceptance-test corrections

- **T1:** use a post-`t1` concept retirement as in A2; assert the edge only for
the recorded-time builder. Keep the bare/current-belief case as an unchanged
control that excludes the now-retired endpoint. If checking content or `extra`,
opt into `.content(true)` / `.extra(true)`; those fields are omitted by default.
- **T2/T5:** assert ids/closure against the concept's recorded retirement
state, not merely an edge retirement. Retired-at-`t1` should remain absent;
retired-after-`t1` should be present only in the recorded-time result.
- **T4:** with `ZZZ` having no concept row at all, the loader's hydration finds
no `NodeData`, and the ids projection's inner join finds no id. The expected
result is zero nodes/ids and no edges—not “start node only.” Alternatively,
create the start concept if the intended assertion is a one-node graph.
- **T7:** retain as regression coverage for the existing
`check_recorded_reach` refusal before the loader query.

These corrections narrow the issue to the bitemporal visibility leak the code
actually exhibits, preserve the stated no-change rule for current-belief reads,
and make the reproducer capable of distinguishing the two.

## Amendment 2 — Empirical review (probes run against `dev/0.19.1`)

**Review scope:** where Amendment 1 read source, this amendment *ran* five
throwaway probes against this checkout (`dev/0.19.1`, which is 0.19.0's
sources plus the document — no fix has landed). Session date 2026-10-06, so
Appendix B's literal timestamps resolve against a live clock in the same era
that produced Appendix A's stamps. The probe binary
(`tests/amendment2_probes.rs`) was deleted after the run to keep the tree at
doc-only changes; the load-bearing listing is inlined at §B2 and the setup is
§6.1's helper otherwise. Checks printed actual values rather than asserting,
so a wrong prediction still reports what the code does.

### B1. Appendix B's sketch does not reproduce — now confirmed by execution

P1 ran the sketch exactly as written (concepts live, `retire_edge` the only
later write, `valid_to = 2027-01-01`, both loads at `now_ts = t1`):

- bare builder at `t1`: **2 nodes, 1 edge** — `A→B` present;
- `.as_of_recorded(t1)` at `t1`: **2 nodes, 1 edge** — `A→B` present.

So §2.4 ("a subgraph **without** the `A→B` edge — with *and* without
`.as_of_recorded(t1)`") and Appendix B's "FAILS on 0.19.0 (empty adjacency)"
are not what this code does for edge-only retirement. Amendment 1 §A2's
source-level prediction is confirmed by execution. The likely reconciliation
of the original observation is an inference, stated as one: the actual
narrowing-session fixture was a rename — and minting `new_name` plausibly
retired `old_name` as a *concept* — which is precisely §B2's shape. The
observed signature (missing **with and without** `as_of_recorded`) matches
the concept-retirement shape on both counts; the condensed sketch dropped
the load-bearing step when transcribing it.

### B2. The corrected reproducer reproduces §3 exactly

P2 kept the edge open and re-upserted the **endpoint concept** with
`retired(true)` after `t1` (title changed v1→v2 so payloads are
distinguishable). On unfixed code:

```rust
let hist = db.load_subgraph_with(
    &TraversalBuilder::new("a::x").max_depth(2).as_of_recorded(&t1),
    &t1, 10_000_000).await?;
// → 1 node, 0 edges, a::y not hydrated        — §3's gap, as described

let ids = TraversalBuilder::new("a::x").max_depth(2).as_of_recorded(&t1)
    .execute_ids(db.read_conn(), &t1).await?;
// → ["a::x"]                                  — §3.1, as described

let attime = TraversalBuilder::new("a::x").max_depth(2).as_of_recorded(&t1)
    .attribute_mode(AttributeMode::AtTime)
    .execute(db.read_conn(), &t1).await?;
// → only a::x                                 — see §B3: new finding

let state = db.reconstruct(&t1).await?;
// → state.concepts["a::y"] == ("b-v1", "doc-b") — fold control, correct
```

Controls all held: bare builder at `t2` **and** at `t1` — edge absent
(current belief regardless of `now_ts`, as Amendment 1 §A2 says T1 must
expect); `Current` mode — only `a::x`; instants without a mode —
`AttributeModeUnstated`. Called directly, the fold the fix reuses answers
the bitemporal question correctly:
`hydrate_attributes(conn, &[both ids], &AsOf::bitemporal(t1, t1), AtTime)`
returned `[a::x "a-v1", a::y "b-v1"]`. That is §4.1's premise confirmed by
execution, and it also shows T1's attribute assertion
("endpoint attributes are the at-`t1` payload") is checkable once the arm
exists — subject to the `NodeData` `valid_from`/`valid_to` gap in §A3.

The lower bound held too (P4): with the concept retired **before** `t1`, the
`.as_of_recorded(t1)` load still drops the edge and the fold excludes the
node — retired-as-of-`ts` must stay invisible after the fix, which is what
§6.1 T5 wants, now pinned with a concept retirement rather than an edge one.

### B3. New finding: `execute` + `AtTime` is a third affected surface

P2's fourth block: `execute` with `.as_of_recorded(t1).attribute_mode(AtTime)`
returns **only `a::x`** — the node believed live at `t1` never reaches AtTime
hydration. The mechanism is visible in `execute`'s own body: it calls
`execute_ids` first and hydrates whatever ids survive
(`let ids = self.execute_ids(conn, now_ts).await?;` then
`hydrate_attributes(conn, &ids, &as_of, mode)`), so §3.1's narrowed ids starve
the fold before it runs.

This means §1's sentence — "only the subgraph-loader path (and the
`execute_ids` projection, secondarily) is affected" — is an undercount:
**any `AtTime` consumer of `execute` loses the node**, through the same root
cause. Three consequences for the plan:

1. §4.2's ids fix repairs `execute`'s AtTime arm automatically, because
   `execute` builds on `execute_ids`. No separate arm is needed, and the
   blast-radius table should say so rather than imply `execute` is unaffected.
2. T6 (§6.1) is gated on §4.2, not only on the loader arm — and on unfixed
   code its AtTime expectation fails with a **missing node**, not a wrong
   title. T6's description should say which failure mode proves which defect,
   or the pre-fix run teaches the wrong lesson.
3. §4.2's parity framing ("the two paths disagree") is really three surfaces
   that must agree at instants: loader, `execute_ids`, and `execute` — the
   first two directly, the third transitively.

### B4. T4's expectation is wrong — confirmed by execution

P3: start `zzz` with no concept row anywhere. The loader returns an **empty
graph** (0 nodes, 0 edges, no error) and `execute_ids` returns `[]`. The
loader does push the start id into its hydration list ("plus the start itself
so a lone node still loads as a one-node graph"), but `hydrate` finds no live
row for an id that never existed, so nothing lands. §6.1 T4's "start node
only" should read "empty graph, no error" — as Amendment 1 §A5 said; this is
the executing confirmation.

### B5. The loader-contract premises §4.1 leans on hold

P5, on a plain two-node graph: the default load carries `title` but
`content() == None`; `.content(true)` loads it; a 10-byte budget refused with
`SubgraphTooLarge { n: 322, budget: 10 }`. So the historical arm must
preserve opt-in content/`extra` and the mid-hydration budget refusal, exactly
as §A3 argued — `hydrate_at_time` wholesale would break both.

### B6. §5's blast-radius claim needs one correction: the Python binding

`BranchView::load_subgraph` indeed passes no instants, as §5 says. But the
**Python binding is a second in-crate caller that does**:
`bindings/python/src/database.rs`'s `load_subgraph` accepts `as_of_valid=`
and `as_of_recorded=` keywords and forwards them into the builder passed to
`load_subgraph_with`. So "in-crate behavior change is nil" is true of the
**Rust** API only; a Python caller passing `as_of_recorded=t1` will see
strictly more complete results after the fix. That is §5's own second bullet
(the correction), but the nil-change sentence should be scoped, and the
release-note item should cover the Python keyword.

One doc-currency item rides along: the binding's
`AttributeModeUnstatedError` text says "`as_of_valid(t)` or
`as_of_recorded(t)` fixes the *topology*; node attributes are a second,
independent question whose default answer is live text". Under §4.1 that
stays true for current-belief loads but needs its historical-load caveat
once belief-at-instant hydration lands — add it to §7.3's list of
comment/doc sites to revisit.

### B7. What checked out as written

- The fold predicates are what §3's hand-SQL claim needs: `links_at_tx` takes
  `recorded_at <= ?slot`, latest `seq_id` per `(entity_id, branch_id)`
  (`src/graph/plan.rs` ≈368), and `hydrate_at_time` folds concepts per entity
  under the same `<=`, skipping retired payloads and invalid valid intervals
  (`src/temporal/as_of.rs` ≈407).
- The refusal parity §4.1 asserts is literal, not merely equivalent:
  `check_recorded_reach` **delegates to** `hot_log_answers_for`, the same
  function `hydrate_at_time` guards with — one guard, one error, two
  call sites. T7 therefore exercises the same refusal both arms would raise.
- The walk, the `DISTINCT` projection, and the byte accounting match §3's
  description line for line.
- Nothing in Amendment 1 is retracted by execution; §A1, §A3, §A4, §A5 now
  have run behind them as well as read.
- The register hook D-289 can cite predates the two-axis split but reads
  correctly today: Wave 1's retirement decision (defect AB/Z cycle,
  *Macrame Implementation Plan v0.5.6*) — "retirement means *not returned as
  of the instant asked about*, uniformly". Stated plainly, that principle is
  already instant-parameterized; the loader's live-row filter is where the
  "instant asked about" collapsed to *now* in 0.5.6-era code, before
  `as_of_recorded` existed. §4.1 is that principle finally reaching the
  surface it was written for, rather than a new policy.
