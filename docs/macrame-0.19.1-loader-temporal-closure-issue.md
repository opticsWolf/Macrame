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
