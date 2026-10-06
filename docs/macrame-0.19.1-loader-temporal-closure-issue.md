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

## Amendment 3 — Consolidated findings (source + execution, `dev/0.19.1` @ `40ca378`)

**Scope and method.** This amendment consolidates the issue document's
original claims (§§1–7, Appendices A–B), Amendment 1's source-review
findings (§A1–§A5), and Amendment 2's probe results (§B1–§B7) into a single
record, re-verified against the current checkout. Method: full re-read of
the document (640 lines); fresh `rg`/source pass over `src/graph/subgraph.rs`,
`src/graph/builder.rs`, `src/temporal/as_of.rs`, `src/temporal/replay.rs`,
`src/connection.rs`, `src/branch.rs`, and `bindings/python/src/database.rs`;
`git log` confirming `src/` is unchanged since 0.19.0 for every path in
scope (the branch is doc-only on top of `cdc6d74`: `eb368e7` the issue
document, `40ca378` Amendments 1–2). No code or tests were changed; no test
suite was run beyond the Amendment 2 probes, whose binary was deleted
after the run with its load-bearing listing inlined at §B2. Anything below
marked *inference* is stated as one; everything else was read in source,
executed, or both.

### 1. Verdict

1. **The defect is real.** A historical read (`as_of_recorded` set) whose
   endpoint concept retired after the instant silently loses the edge (and
   the node) on the loader path — reproduced by execution (P2: 1 node,
   0 edges, endpoint unhydrated) through exactly the §3 mechanism.
2. **The defect is narrower than the document states.** Edge-only
   retirement does not trigger it (P1: edge present on both builder shapes).
   The necessary condition is *concept* retirement after the instant with
   the edge still spanning it. §2/Appendix B's sketch must be rewritten;
   §§3–4 otherwise stand.
3. **Three surfaces are affected, not two.** Loader, `execute_ids`, and
   `execute`+`AtTime` (transitively, via `execute_ids`). §4.2's ids fix
   repairs all three; no third arm is needed.
4. **The fix direction is sound with two refinements.** `hydrate_at_time`
   supplies the fold/retirement semantics to reuse but is not a drop-in
   (wrong return type, no budget/opt-in, and it is a *private* function);
   §4.3 must additionally specify explicit `Current`/`Omit` semantics on the
   loader.
5. **The acceptance table needs three corrections** (T1's bare-builder
   expectation, T4's "start node only", T6's gating and pre-fix failure
   mode) **and the blast-radius claim needs one scoping fix** (the Python
   binding forwards instants, so "in-crate behavior change is nil" is
   Rust-only).

### 2. Confirmed inventory — what the document gets right

| # | Claim | Evidence |
|---|---|----------|
| F1 | §3 mechanism: walk collects the edge; `hydrate` reads `concepts WHERE retired = 0` (`src/graph/subgraph.rs:1122`); `drop_dangling_adjacency` (`:1083`, impl `:516`) prunes edges to unhydrated endpoints; `check_recorded_reach` runs first (`:951`) | Source re-verified this pass; execution-proven by P2 |
| F2 | §3.1 secondary surface: both `build_sql_with` projections filter `WHERE c.retired = 0` (`src/graph/builder.rs:609,640`) | Source re-verified this pass; execution-proven by P2 (`execute_ids == ["a::x"]`) |
| F3 | Walk, `links_at_tx` fold (`recorded_at <= ?slot`, latest `seq_id` per `(entity_id, branch_id)`, `src/graph/plan.rs` ≈368), `reconstruct`, and the `hydrate_attributes` fold answer correctly at `t1` | Source (Amendment 1) + execution (P2 controls: `reconstruct(t1)` yields `("b-v1", "doc-b")`; direct fold yields both nodes) |
| F4 | Refusal parity is literal: `check_recorded_reach` (`src/graph/builder.rs:843`) delegates to `hot_log_answers_for`, the same guard `hydrate_at_time` uses — one guard, one `RecordedInstantUnreachable`, two call sites | Source re-verified this pass; T7 therefore covers the refusal both arms would raise |
| F5 | Current-belief pin (§4.1): a bare builder at any `now_ts` excludes the now-retired endpoint, because `now_ts` is the valid-time fallback (`TraversalBuilder::valid_instant`), not a recorded instant | Execution-proven by P2 (absent at both `t2` and `t1`); `Current` mode returns only `a::x` |
| F6 | Loader-contract premises: `title` carried by default, `content`/`extra` opt-in (`None` unless requested), mid-hydration `byte_budget` refusal | Execution-proven by P5 (`content() == None` default; `Some("doc-b")` with `.content(true)`; `SubgraphTooLarge { n: 322, budget: 10 }`) |
| F7 | Lower bound: retirement *before* the instant must stay invisible after the fix (T5) | Execution-proven by P4 (loader and fold both exclude) |
| F8 | `retire_edge` (public `src/connection.rs:2539`, internal `:5898`) closes a link row only; it never retires concepts | Source-confirmed (Amendment 1, unchallenged) |
| F9 | `D-085` refusal intact: instants without a mode still yield `AttributeModeUnstated` (`resolved_mode`, `src/graph/builder.rs:1088–1097`; `execute` at `:1070–1079`) | Execution-proven by P2; §4.3 option (a) can reuse it |
| F10 | `BranchView::load_subgraph` (`src/branch.rs:338`) passes no instants | Source re-verified this pass |

### 3. Corrections — what the document gets wrong (C1–C8)

**C1. The reproducer does not reproduce (amends §2, §6.1 setup, Appendix B).**
Both concepts in the sketch default to `retired: false` and the only later
write is `retire_edge`, which per F8 cannot starve `hydrate`. P1 executed
the sketch verbatim: bare builder at `t1` → 2 nodes, 1 edge;
`.as_of_recorded(t1)` at `t1` → 2 nodes, 1 edge. Hence §2 step 4 ("without
the `A→B` edge — with *and* without `.as_of_recorded(t1)`") and Appendix B's
"FAILS on 0.19.0 (empty adjacency)" do not describe this code. *Required
doc change:* rewrite the setup helper so that after capturing `t1` it
re-upserts `B` with `retired(true)` (changing the title v1→v2 keeps payloads
distinguishable), keeps the edge spanning the query instant, and loads with
`.as_of_valid(&t1).as_of_recorded(&t1)` for the historical expectation.
*Inference (stated as one):* the live narrowing session's rename fixture
most likely retired `old_name` as a concept when minting `new_name`; the
observed "missing with and without `as_of_recorded`" signature matches the
concept-retirement shape on both counts, and the condensed sketch dropped
the load-bearing step in transcription.

**C2. T1 must not expect the bare builder to restore the endpoint (amends
§6.1 T1, §2 clocks).** Per F5, `load_subgraph_with(builder, t1)` with no
instants on the builder is a current-belief read at a past valid instant,
and under §4.1's own scoping it must continue to exclude the now-retired
endpoint. *Required doc change:* T1 asserts `A→B` present **only** for the
`.as_of_recorded(t1)` builder (with endpoint attributes equal to the at-`t1`
payload, per the P2 fold result); the bare-builder shape becomes the
unchanged-behavior control asserting absence — i.e. T1's second shape and
T3's shape coincide, and the table should say so rather than list them as
independent expectations.

**C3. T4's expectation is "empty graph", not "start node only" (amends
§6.1 T4).** P3 executed the `ZZZ` shape: loader → 0 nodes, 0 edges, no
error; `execute_ids` → `[]`. Mechanism: the loader pushes the start id into
its hydration list, but `hydrate` finds no `concepts` row for an id that
never existed, so nothing lands, and the ids projection's inner join finds
no id. *Required doc change:* expect "empty graph, no error" — or create
the start concept in the fixture if the intent is a one-node-graph
assertion.

**C4. `execute`+`AtTime` is a third affected surface (amends §1, §4.2, §5).**
P2's fourth block returned only `a::x` for
`.as_of_recorded(t1).attribute_mode(AtTime)` `execute`, because `execute`
hydrates whatever `execute_ids` returns (`builder.rs:1072–1079`) and the
narrowed ids starve the fold before it runs. §1's "only the subgraph-loader
path (and the `execute_ids` projection, secondarily)" is therefore an
undercount. *Consequences:* (a) §4.2's parity goal covers three surfaces —
loader and `execute_ids` directly, `execute` transitively — with no
separate arm; (b) the blast-radius table must state `execute`+`AtTime` is
repaired by the same fix rather than implying it is unaffected; (c) T6 (see
C8) is gated on §4.2.

**C5. `hydrate_at_time` is semantics to reuse, not a function to call
(amends §4.1, §7.1 D-286 row).** Four gaps, three from Amendment 1 plus one
new this pass: (i) it returns `NodeAttributes` (`as_of.rs:59`: id, title,
content, embedding_model, extra) while the loader stores `NodeData`
(`subgraph.rs:112`), which additionally carries `valid_from`/`valid_to` —
the historical interval must come from the payload believed at `r`, not from
today's `concepts` row; (ii) it has no byte-budget accounting, while the
loader refuses incrementally inside the hydration loop (F6); (iii) it has no
content/`extra` opt-in; (iv) *new:* it is a private `async fn`
(`as_of.rs:407`, returning `HashMap<String, NodeAttributes>`), callable only
via the public `hydrate_attributes` (`as_of.rs:290`, returning
`Vec<NodeAttributes>`). *Required doc change:* §4.1 must specify a
loader-specific incremental hydrator (or a shared lower-level fold plus a
visibility decision) that preserves opt-in fields, incremental refusal, and
historical intervals — reusing the fold/retirement predicates, not the
function wholesale. The D-286 row's "shared helper … without coupling"
hedge is the right shape; promote it from aside to requirement.

**C6. The mode decision must cover explicit modes (amends §4.3 item 2).**
The loader never calls `resolved_mode`, so neither §4.3 option (refuse
unstated vs. default `AtTime`) says what an explicit `Current` or `Omit`
means on this API — and either option must not silently override an
explicit mode nor claim `Omit` while hydrating node data for closure (the
closure invariant, `subgraph.rs:18–60` plus `is_closed` at algorithm
entries per D-140, needs node data regardless of attribute policy).
*Required doc change:* a mode-contract matrix (see §4.5 below) decided
before wiring the builder's mode into loading.

**C7. Blast-radius nil-change claim is Rust-only (amends §5, §7.3).**
`bindings/python/src/database.rs`'s `load_subgraph` accepts `as_of_valid=`
and `as_of_recorded=` keywords and forwards them into the builder passed to
`load_subgraph_with` — a second in-crate caller that *does* pass instants.
So historical Python callers will see strictly more complete results after
the fix (which is the correction itself, §5's own second bullet — but the
nil-change sentence must be scoped to the Rust API, and the 0.19.1 release
note must cover the Python keyword). Doc-currency rider: the binding's
`AttributeModeUnstatedError` text ("`as_of_valid(t)` or
`as_of_recorded(t)` fixes the *topology*; node attributes are a second,
independent question whose default answer is live text") stays true for
current-belief loads but needs its historical-load caveat once
belief-at-instant hydration lands — add it to §7.3's revisit list alongside
the `load_subgraph_with` "attribute_mode is still ignored" note, the
`Subgraph` closure docs, and the s5 §5.2/§5.4 paragraphs.

**C8. T6's gating and failure mode are misstated (amends §6.1 T6).** T6
(retitle `B` after `t1`; `AtTime` → old title, `Current` → live title) can
pass only after §4.2, not after the loader arm alone — on unfixed code the
AtTime expectation fails with a *missing node* (C4), never reaching any
title comparison. *Required doc change:* state the pre-fix failure mode per
expectation (absence proves the topology defect; a wrong title afterward
would prove an attribute defect) so the pre-fix run teaches the right
lesson. T2/T5 fixtures likewise move from edge retirement to concept
retirement per C1, with retired-at-`t1` absent and retired-after-`t1`
present-only-historically.

### 4. Consolidated fix specification

**4.1 Gating rule (unchanged from the document, reaffirmed).** All new
behavior sits behind `as_of_recorded` being set. With no recorded instant —
including bare past-`now_ts` valid-time reads — SQL text, parameters,
results, and goldens are byte-identical to today. This is what keeps every
existing golden green and what makes T3 a both-versions-green guard for the
§4.1 current-belief design.

**4.2 Loader arm.** When `traversal.as_of_recorded` is set, hydration
answers belief-at-instant under these simultaneous constraints: (a) walk,
CTEs, and `drop_dangling_adjacency` untouched — closure then holds *at the
instant* automatically (live-at-`r` hydrates; never-existed and
retired-as-of-`r` do not; edges to them prune as today); (b) `NodeData`
shape preserved with `valid_from`/`valid_to` taken from the payload believed
at `r` (C5-i); (c) `content`/`extra` remain opt-in builder flags (C5-iii,
F6); (d) byte accounting stays incremental with mid-loop `SubgraphTooLarge`
refusal (C5-ii, F6); (e) pre-hot-log instants refuse via the existing guard
(F4) and corrupt payloads refuse rather than skip; (f) the fold/retirement
predicates are reused from `hydrate_at_time`'s implementation, with the
private-visibility question (C5-iv) settled as a `pub(crate)` exposure or a
new shared lower-level fold — not by routing the loader through the
collect-everything public wrapper.

**4.3 Ids arm.** Both `build_sql_with` projections gain the
instant-aware node filter when `as_of_recorded` is set — a folded-concepts
join over the reached ids mirroring `links_at_tx_cte`, or an equivalent
existence check against the recorded fold — honoring the D-073 both-halves
contract (a walk-only or projection-only fix reintroduces the closed class).
`execute`+`AtTime` is repaired transitively through `execute_ids` (C4); no
third arm, and new goldens cover the historical shapes only (§6.2
unchanged).

**4.4 Valid-time-only reads.** Unchanged (present-tense closure = current
belief about what was true at `v`), per §4.3 item 1's recommendation, which
this consolidation endorses: minimal blast radius, coherent semantics,
callers wanting history set `as_of_recorded`.

**4.5 Mode contract (decides §4.3 item 2 + C6 together).** The decision must
fill every cell before implementation:

| Builder mode | No instants (current belief) | `as_of_recorded` set (historical) |
|---|---|---|
| Unstated | live hydration, as today | (a) `AttributeModeUnstated` refusal, or (b) default `AtTime` |
| `AtTime` | N/A on the loader today (ignored) — define or refuse | belief-at-`r` hydration |
| `Current` | live hydration, as today | live hydration with historical *topology* — define explicitly; must not silently become `AtTime` |
| `Omit` | define: closure still needs node data — say what is stored vs. returned | same, at the instant |

Option (a) matches the house T3.2 stance; option (b) breaks no existing
caller since no in-crate Rust caller passes instants (F10). Either way the
Python error text (C7) is updated to match.

**4.6 Explicitly out of scope (reaffirmed).** `replay.rs` folds,
`links_at_tx_cte`, `walk_cte`, `reconstruct`, branch/lineage resolution,
archive/cold paths; no schema, migration, table, or index changes; no change
to `drop_dangling_adjacency`, the closure invariant and its `is_closed`
asserts, or any current-belief SQL text.

**4.7 Decision records.** D-289 (loader arm + §4.5 mode wiring) and D-290
(ids parity), with D-291 in reserve if the maintainer splits mode from
parity — the document's numbering reservation stands. Each amends D-174 and
D-085 by back-pointer per §7.4 mechanism 1 (bodies untouched; "Amended by
D-…" lines with valid anchors from day one, D-280). The Wave 1 retirement
principle (defect AB/Z cycle, *Macrame Implementation Plan v0.5.6*:
"retirement means *not returned as of the instant asked about*,
uniformly") is the cited precedent: read plainly it is already
instant-parameterized, and the loader's live-row filter is where the
"instant asked about" collapsed to *now* before `as_of_recorded` existed
— so D-289 extends the principle to the surface it was written for rather
than inventing policy.

### 5. Consolidated acceptance criteria (deltas to §6 applied)

| # | Corrected test | Fixture delta vs §6.1 | Historical call | Must hold (pre-fix → post-fix) |
|---|---|---|---|---|
| T1 | Historical loader keeps post-instant-retired edge | **Concept** retirement of `B` after `t1` (C1), edge spanning `t1` | `.as_of_recorded(t1)` loader; bare builder as control | Historical: absent → present with at-`t1` payload; bare: absent → absent (C2) |
| T2 | Historical ids agree with loader | Same fixture as T1 | `execute_ids` with `.as_of_recorded(t1)` | `B` excluded → included |
| T3 | Current belief unchanged | Same fixture as T1 | Bare builder at `now` | Absent → absent (green both versions; guards §4.1) |
| T4 | Never-existed stays absent | Start `ZZZ`, no rows anywhere | Loader + ids at `t1` | **Empty graph / `[]`, no error** → same (C3; green both versions) |
| T5 | Pre-instant retirement stays dropped | **Concept** retirement of `B` before `t1` (C1) | `.as_of_recorded(t1)` loader | Absent → absent (green both versions; pins the lower bound) |
| T6 | Attribute modes | Retitle `B` after `t1` (**plus** T1's fixture shape so the node reaches hydration) | `execute` `AtTime` vs `Current` at `t1` | AtTime: node missing → old title; Current: live title → live title (C8; gated on §4.3+§4.2) |
| T7 | Cold instant still refuses | `t0` predating hot-log coverage | Loader with `.as_of_recorded(t0)` | `RecordedInstantUnreachable` → same (F4; coverage of the existing guard) |

Golden-string and done-criteria deltas: §6.2 stands (byte-identical
current-belief SQL asserted against existing goldens; new goldens for
historical shapes only; plus an `AttributeModeUnstated`-on-loader test iff
§4.5 chooses refusal). §6.3 stands with two additions: the Python
`as_of_recorded=` keyword covered by at least one historical assertion (C7),
and the §7.3 doc-currency list extended with the binding error text (C7).
§6.1's setup helper is rewritten per C1 (concept retirement, canonical
stamps per D-029) and the companion CodeRadar reproducer's `#[ignore]`d
loader tests are un-ignored against 0.19.1 per §6.3 item 4.

### 6. Restated blast radius

- **Rust current-belief reads**: zero change (same SQL, params, results,
  goldens) — F10's caller passes no instants.
- **Historical reads** (`as_of_recorded` set, Rust and Python): strictly
  *more* complete — nodes/edges live-at-the-instant stop being silently
  dropped. Dependence on the narrowed behavior sees more rows; that is the
  correction.
- **Refusals**: `RecordedInstantUnreachable` preserved on both arms (F4);
  corrupt payloads refuse; `SubgraphTooLarge` mid-hydration semantics
  preserved (F6).
- **Python surface**: behavior change is confined to calls passing
  `as_of_recorded=`/`as_of_valid=`; all other Python calls byte-identical
  in effect (C7).

### 7. Provenance ledger — what is proven, what is inferred

- *Proven by source + execution:* F1–F3 (mechanism, ids surface, correct
  controls), C1 (sketch refuted), C3 (empty graph), C4 (third surface),
  F5–F7 (belief pin, contract premises, lower bound), F9 (refusal
  constructible at this call shape).
- *Proven by source alone:* F4 (shared guard function), F8 (`retire_edge`
  scope), F10 (instant-free Rust caller), C5-iv (helper privacy), C6 (mode
  never consulted on the loader path), C7 (binding forwards instants).
- *Inference, stated as such:* the transcription-drop reconciliation of the
  original narrowing-session observation (C1) — consistent with every
  executed signature but not directly observed, since the companion
  CodeRadar fixture was not in this checkout.
- *Superseded text if this amendment is accepted:* §2 step 4, §1's
  affected-surface sentence, §4.1's drop-in paragraph, §4.3 item 2's
  two-option framing, §5's nil-change sentence, §6.1's setup helper and the
  T1/T4/T6 rows, and §7.3's revisit list — each per the C-item deltas above,
  with the original paragraphs retained as history per §7.4.
