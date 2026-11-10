# Macrame Update Plan v0.22.0 — conversation branching, ported

**Status:** built and closes on `dev/0.22.0-revA` at `e05d7c1` — the port's track (Stage A/B, M1-M3, D-307...D-313) and the M6 overlay track (D-314 the overlay, D-315 adoption across lineages, D-316 the trunk walk's past fold, Amendment 2 revision 3 as the cycle's spec) with their reviews applied, the sweeps re-run at the release tail (1,048 / 1,026 / 1,074 / 752); merged to `dev/0.22.0` and `main` (both ff at `e05d7c1`) · **Branch:** `dev/0.22.0` from `main` at `5e2855a`
(M0 is the first commit on it)
**From:** 0.21.0 (schema v24, concepts payload v3, snapshot container stamps v24, `DbError` 49
variants, public API 1,994 items, libSQL 0.10.0)
**To:** 0.22.0 (schema v24 → **v25** for the `edge_kinds` register; payload unchanged; `DbError`
49 → **55**; decisions **D-307…D-313**)

The capabilities Silk's WorkLedger needs were decided, built and tested once already, on a branch
`main` never merged. `a65c29e` ("Macrame 0.20.0 draft: conversation branching (M1-M5, M7)",
2026-10-16, on `opticsWolf/0.20.0-dev` and `opticsWolf/v0.19.2-dev`, in no tag) adds fork at a
past instant, a one-stamp mixed `write_batch`, functional edge kinds and pointers, per-lineage
concept reads with `concepts_by_id`, branch-scoped search, and a change feed: 39 files,
+5,180 lines, 29 Rust tests in `tests/v020_conversation_tests.rs` and 9 in
`tests_py/test_conversation.py`. It was built on `v0.19.1-13-g5098722`. `main` then released a
*different* 0.20.0 (the valid-axis fold) and 0.21.0 (the state rule), which reused D-291…D-295
and schema v23 for other things and changed the concept write path underneath the draft.

So this release is a **port**, not a design: renumber the draft, reconcile it with what 0.20.0
and 0.21.0 decided since its base, and add the two pieces it predates — a supersede for
concepts and a conditional pointer move. The Silk gaps doc ("Macrame gaps for Silk tasks and
decisions", 2026-11-02) names the milestones this plan keeps: M0–M7.

| Item | Source | Work | Schema | Silk unblocks |
|---|---|---|---|---|
| M0 — the stale `fork` docstring | gaps doc | **done** (first commit) | none | — |
| Stage A — mechanical port | draft | 1 d | v25 rung | builds green |
| Stage B — semantic reconciliation | draft × D-291/D-303/D-305 | 2–3 d | none | correctness |
| M5 — fork at an instant or after a concept | draft D-291 → **D-307** | in A/B | none | branch a plan at a decision |
| M1 — one-stamp mixed batch | draft D-292 → **D-308** | in A/B | none | atomic task ops |
| M2 — pointers, functional edge kinds | draft D-293 → **D-309** | in A/B + 0.5 d | **v25** | status / claim / head |
| M7 — `concepts_by_id`, per-lineage concept reads | draft D-294 → **D-310** | in A/B | none | one read per plan |
| M4 — change feed | draft D-295 → **D-311** | in A/B | none | viewers |
| M1b — supersede in one write | **new**, D-312 | 1.5 d | none | task edits in one write |
| M3 — conditional pointer move (`expect=`) | **new**, D-313 | 1 d | none | claims safe across processes |
| M6 — concept shadowing on a branch | gaps doc | **not in 0.22.0** | — | retitle on a branch |

---

## 1. What the trial port showed

A `git cherry-pick -n a65c29e` onto `dev/0.22.0` (2026-11-02, thrown away afterwards) conflicts
in **8 files, 11 hunks**, all of them numbering or neighbouring additions:

| File | Hunks | What collides |
|---|---|---|
| `src/schema/migrations.rs` | 3 | `SCHEMA_VERSION` (draft 23, main 24); the rung list (draft `22→23 edge-kinds`, main `23→24 concept-state-rule`); the baseline DDL block (draft's `CREATE_EDGE_KINDS_TABLE` beside main's v24 marks table) |
| `python/macrame/_macrame.pyi` | 2 | neighbouring additions |
| `bindings/python/src/errors.rs` | 1 | the exception table (draft +5, main's D-295/D-305 provenance) |
| `docs/architecture/README.md`, `appendices.md` | 1 + 1 | version table row, surface count |
| `tests/binding_parity_tests.rs` | 1 | the variant count |
| `tests/common/v11_schema.rs`, `tests/migration_tests.rs` | 1 + 1 | rung fixtures |

`src/connection.rs` (draft +921, main +775 since the base), `src/error.rs`, `src/schema/ddl.rs`,
`src/graph/builder.rs` and `src/temporal/as_of.rs` merge **cleanly as text**. That is the
dangerous half: they will compile, and three of them are wrong (§3). The work in this release is
Stage B, not the conflicts.

The draft's own register text is also a cross-reference problem: it amends D-014, D-214 and
D-223 with "Amended by D-292/D-294" lines, and on `main` those numbers name other decisions.
Every reference moves with the renumbering (§2), including the ones inside rustdoc.

## 2. Numbering

| Draft | Port | Subject |
|---|---|---|
| D-291 | **D-307** | fork at a past instant (`fork_at`) or after a concept's mint (`fork_after`) |
| D-292 | **D-308** | `write_batch`: concepts, retirements, edges, pointer moves under one stamp |
| D-293 | **D-309** | functional edge kinds, `set_pointer` / `pointer` / `pointers`, `link_previous` — schema **v24 → v25** |
| D-294 | **D-310** | concepts read per lineage, held vs named, `concepts_by_id`, branch-scoped search |
| D-295 | **D-311** | change feed over `seq_id`, a gap is a refusal |
| — | **D-312** | supersede: close the open belief and open the next in one write (M1b) |
| — | **D-313** | conditional pointer move, `expect=` (M3) |

Schema: the draft's v23 rung becomes **v24 → v25** (`edge-kinds`); main's v24 rung is
unchanged. `DbError`: the draft's five (`ForkInFuture`, `ConceptNotVisible`,
`FunctionalEdgeConflict`, `BatchTooLarge`, `ChangeFeedGap`) plus D-313's `PointerConflict` —
49 → **55**. `RecordedInstantUnreachable` and `BranchMismatch` already exist on `main` and are
reused, as the draft did.

## 3. Stage B — where the draft is wrong on today's `main`

Each of these merges cleanly and compiles. Each needs a named test that fails on the plain
cherry-pick and passes after the fix — that is the gate (§6, gate 2).

### 3.1 `write_batch` writes concepts around the D-291 guard and the D-303 state rule

The draft's `write_batch_atomic` writes each concept with a bare
`tx.execute(UPSERT_CONCEPT, concept_params(c, stamp))`. On `main` a concept write does three
things, and the draft does one:

1. **The interval guard** (D-291, D-300). Single writes call `reject_overlapping_concept`; the
   chunk path calls `check_batch_against_ledger` against the committed ledger before its first
   insert, using `final_concept_intervals` for within-batch overlaps. The draft's batch runs
   neither, so a batch can open a second interval for a concept — the defect D-291 closed.
2. **The skipped backfill** (D-303). `UPSERT_CONCEPT` declines (zero changed rows) a write
   that starts before the projection's state; the write path then writes the log row itself
   (`record_skipped_backfill`) and inserts the `concept_disorder` mark. The draft's batch treats
   `Ok(0)` as success, so a backfill inside a batch **loses its log row** and its mark.
3. **Error classification** — the draft does this; keep it.

Fix: the batch's concept phase reuses `write_concepts_atomic`'s body (guard, prepared
statement, the `Ok(0)` branch, `classify`) rather than its own loop. Tests: a batch that
backfills a concept writes the log row and the mark (compare with the same rows through
`write_concepts`); a batch that would open a second interval is refused with
`ConceptSingleOpen` and writes nothing.

The draft refused a concept id twice in one batch (`InvalidId`). Keep that: one batch is one
stamp, and `trg_concepts_monotonic_ra` (still on `main`, `BEFORE UPDATE … WHEN NEW.recorded_at
<= OLD.recorded_at`) would refuse the second projection update anyway. Supersede (§4.1) is the
one exception, and it does not go through a second projection update.

### 3.2 The per-lineage concept fold picks the version by `seq_id`, not by D-303's rule

`src/graph/concept_lineage.rs` resolves "the latest version at or before the cutoff" with a
correlated `ORDER BY t.seq_id DESC LIMIT 1` on `idx_txlog_fold_partition`. That was the state
rule before 0.21.0. Since D-303 a concept's state is **its open belief if it has one, otherwise
its latest-starting belief; within one start, the row written last** — and the backfill case is
exactly where the two disagree: a backfill written after the open belief has the higher
`seq_id` and the lower start.

Fix: the log arm uses the same pair the recorded fold uses — `state_rule_owed` and
`DISORDER_CORRECTION*` in `src/temporal/replay.rs`, as `as_of.rs`'s per-chunk fold and
`builder.rs`'s marked window already do — restricted to rows `recorded_at <= cutoff` on the
concept's minting lineage. The projection arm is unaffected (the projection already holds the
D-303 state). Tests: on a fork, an ancestor backfill after the fork point does not change the
branch's reading; a backfill before the fork point is read by D-303's rule, not by `seq_id`; the
branch's reading equals `reconstruct_on` for every concept in `concept_state_rule_tests`'
fixtures.

The plan pin: the log arm is a new statement shape. It gets an `EXPLAIN QUERY PLAN` pin as every
other fold statement does, and the `CROSS JOIN` lesson from 0.21.0 (a measured plan without a
pin has a shelf life) applies from the first commit.

### 3.3 `check_functional` and the edge write paths

The draft adds `check_functional` to every edge write path (`assert_edge`, `write_bulk_atomic`,
`bulk_import`'s chunks, `write_batch`). On `main` those paths changed: D-301 dropped
`idx_txlog_entity`, D-305 replaced `Overlap::within_batch` with `provenance:
OverlapProvenance`, D-306 put the writer's temporary storage in memory. Check, per path:

- the functional check runs inside the transaction, after `reject_overlaps_within` and before
  the first insert, and its refusal rolls back the whole chunk (bulk_import: that chunk only —
  D-011 is unchanged);
- `FunctionalEdgeConflict` stays at four `String` fields: the draft measured a fifth putting
  `DbError` at 128 bytes, over `result_large_err`; re-measure on `main`, where `OverlapProvenance`
  moved the largest variant (`size_of::<DbError>()` is printed by `error.rs`'s size test);
- an **unregistered** kind costs one indexed `edge_kinds` probe per batch, not per row — the
  90-edge chunk must stay within D-306's figures (gate 6).

### 3.4 The branched pointer read and the log arm's join order

The draft's branched `pointer` read was measured at 4.3 ms → 1.3–1.9 ms after making
`KeySlots::target` optional and resolving in two steps. Those figures are from a shared 2-core
sandbox on libSQL 0.9.30, before D-306. The two-step read runs the lineage log arm, whose join
order 0.21.0 pinned with `CROSS JOIN` (`src/graph/builder.rs`). Re-run
`examples/conversation_probe.rs` on the reference machine and pin the pointer read's plan.

### 3.5 Things that carry over unchanged, and why

- **Fork at an instant** (D-307) needs no read-path change: every reader applies `forked_at` as
  an inclusive cutoff since D-223, and `branches` has had `forked_at <= created_at` since v12.
  `fork_after` resolves to the concept's **mint** (earliest log row on its own lineage). The gaps
  doc asks for "the latest version's `recorded_at`"; the draft considered that and rejected it —
  the latest stamp silently inherits every write between mint and latest. **Keep the mint**, and
  correct the gaps doc.
- **The change feed** (D-311) refuses an archived range with `ChangeFeedGap`. The gaps doc asks
  for a `Change.gap=True` flag instead; the draft rejected that ("a flag a consumer can ignore is
  how a silent miss ships"). **Keep the refusal**; Silk catches it, reloads, and resumes from
  `head_seq()`. Its gap check rests on D-249's `sqlite_sequence` argument, which D-301 did not
  touch.
- **Single-lineage databases** are byte-identical on every read path in the draft (forked paths
  are reached only when `branches` holds more than one row, D-220's condition). Gate 9 holds it.
- The two "correct the gaps doc" items are settled as
  [Amendment 1](Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%201.md): the doc lives nowhere
  on disk, the corrections are the amendment, and the Silk author applies them to their copy.

## 4. New work

### 4.1 M1b — supersede in one write (D-312)

```text
Database::upsert_concept_superseding(ConceptUpsert)           # Rust
db.upsert_concept(concept, *, supersede: bool = False)        # Python
ConceptUpsert(..., supersede=True)                            # per row, write_batch and write_concepts
```

The case: since D-291 a new version of a concept at a later `valid_from` is refused while the
earlier interval is open, so Silk writes two calls (close, then open) and a crash between them
leaves a task with no open belief (Silk PR #3, commit `e3f5666`).

**Semantics.** If the concept has an open belief starting before `concept.valid_from`, close it
at `concept.valid_from` and open the new belief, in one transaction under one stamp. The closed
belief keeps its title, content, `extra` and embedding model; only `valid_to` changes, so as-of
reads before the cut return the old text. No open belief: an ordinary upsert. An open belief
starting **at or after** `concept.valid_from`: still refused with `ConceptSingleOpen` — supersede
only moves forward in valid time.

**Mechanism, and the trigger it must not trip.** Two projection updates under one stamp would
hit `trg_concepts_monotonic_ra`. So the close is written the way D-303's skipped backfill is
written — **a log row, not a projection update**: the old belief restated with
`valid_to = concept.valid_from`, payload composed like `SKIP_BACKFILL_LOG`'s — then the new
belief goes through the ordinary `UPSERT_CONCEPT`, which updates the projection once. Under
D-303 the fold reads: start `s_old`'s last row is the close (closed), start `s_new` is open, so
the state is the new belief — the projection and the fold agree, which is what D-023 requires.
The guard (D-291) sees the closed interval `[s_old, s_new)` and the new `[s_new, …)`: no overlap.

**Open question for implementation (decide in the probe, ≤ 0.5 d):** whether the close row needs
a `concept_disorder` mark. It should not — the close is not a backfill, and the fold's winner
for `s_old` is the close row by D-303's "within one start, the row written last". The probe is a
test that a superseded concept read through every reader (`reconstruct`, `reconstruct_as_of`,
`traverse` under `CURRENT` and `AT_TIME`, the branched fold of §3.2) gives the same answer as the
two-call sequence it replaces, minus the instant with no open belief.

**On a branch.** Concepts live on their minting lineage only (`trg_concepts_cross_lineage`,
D-214/D-260). A branch may supersede a concept **it minted**; superseding an **inherited** one
is still `CrossLineageError`. That is M6 (concept shadowing), which reverses D-214's rule and is
not in this release. The gaps doc's M1b proposes a shadow row for the inherited case; that *is*
M6, and it needs its own decision.

Tests: the close and the open carry the same stamp; a failure in either writes nothing; an as-of
read before the cut returns the old title; a backwards supersede is refused; a branch superseding
its own concept leaves the trunk untouched; a branch superseding an inherited concept is
`CrossLineageError`; `verify_concept_intervals` is clean after a run of supersedes.

### 4.2 M3 — conditional pointer move (D-313)

```text
PointerSet::expect(Option<String>)                             # Rust builder
db.set_pointer(source, kind, target, *, valid_from, expect=UNSET, ...)   # Python
```

`expect` is checked **inside the write actor, in the move's transaction**, against the open target
as that transaction sees it — the same read `link_previous` already makes (D-309). A mismatch is
`PointerConflict { source, kind, expected, current }` and writes nothing. `expect=None` means "no
open pointer" — claim-if-free. Allowed on a `write_batch` pointer row, so a claim and the status
move it implies fail together. This replaces Silk's per-file lock, which is safe in one process
only (two Weave windows on one project can both claim a task).

`PointerConflict` is the 55th variant. Four `Option<String>`/`String` fields — check the size
gate (§3.3) before adding it, and box the payload if it crosses 128 bytes, as D-075 did for
`Overlap`.

Tests: two writers racing a claim — exactly one succeeds and the other sees the winner in
`current`; `expect=None` on a claimed pointer is refused; `expect` on a branch reads the branch's
view (an inherited open target counts); a conflicting pointer row rolls back its whole batch.

### 4.3 M2 additions to the draft's pointer surface

- **`pointer(..., as_of_valid=)`.** The draft's `pointer` takes `as_of_recorded` only; the gaps
  doc asks "what was the status then", which is a valid-time question. Add it, resolving like
  `query_as_of_edges`.
- **`pointers(kind)` and its index.** The draft pushes `(source, kind)` as a prefix key into
  `idx_lc_open_interval`, which leads with `source_id` — right for `pointer(source, kind)`, a scan
  for `pointers(kind)` across all sources (Silk's ready-set query). Probe first: plan and time
  `pointers(kind)` on a 10,000-source fixture. If it scans, add a partial index on
  `links_current (edge_type, source_id) WHERE valid_to = OPEN` **in the same v25 rung**, priced on
  the 90-edge chunk the way D-304 priced the others (gate 6). Do not add it unmeasured.
- **Registering an existing kind as functional.** The draft accepts it and refuses the next
  offending write. The gaps doc asks to refuse registration when a source already has two open
  edges of that kind. Keep the draft's behaviour (a registration-time scan of every lineage's
  view is the cost the draft rejected) and document it on both surfaces; revisit only if Silk
  hits it.

## 5. Order of work

Every step ends green on the full suite and is its own commit.

1. **M0** — done: `_macrame.pyi` and the binding's `fork` docstring say how to write to a branch.
2. **Stage A** — cherry-pick `a65c29e`, resolve the 11 hunks, renumber (§2) in code, rustdoc,
   register and tests; v25 rung and its migration test (`a_v24_database_climbs_to_v25…`); bless
   `public-api.txt`. Gate: the draft's 29 + 9 tests and the whole existing suite green **before**
   any Stage B change — so Stage B's red tests are the only red ones.
3. **Stage B** — §3.1, §3.2, §3.3, §3.4, each as red test → fix. §3.1 and §3.2 first: they are
   correctness, the rest is cost.
4. **M1b** (§4.1), probe then build.
5. **M2 additions** (§4.3), the `pointers(kind)` probe deciding the index.
6. **M3** (§4.2).
7. **Measure** (gates 6, 9), docs, release notes, README schema ladder to v25.

Silk can start its adapter on 0.21.0 now (the gaps doc's "What Silk can do on 0.21 now") and swap
each workaround as the matching commit lands on `dev/0.22.0`. The prototype was written against
the draft's Python surface, so **the draft's Python signatures are the compatibility target**:
`register_edge_kind(kind, functional=False)`, `set_pointer(source, edge_type, target, *,
valid_from, weight, branch, link_previous)`, `write_batch(concepts, edges, retire, pointers, *,
branch)`, `fork(name, frm, *, at, after)`. Additions are keyword-only; nothing the prototype calls
is renamed. Its 21 ledger tests that stop at `edge_kinds` on 0.21.0 are an acceptance check
(gate 10).

## 6. Acceptance gates

| # | Gate | Pass |
|---|---|---|
| 1 | Suites | Rust default, `--no-default-features`, `property-tests`, Python — green on all three CI runners at the release commit (`scripts/run_rust_suite.py`, `tests_py/run_suite.py`) |
| 2 | Stage B red→green | each §3 item has a named test that fails on the bare cherry-pick and passes after its fix |
| 3 | Doctrine property | `doctrine_property_tests` generates `write_batch` and supersede writes, and the fold and the projection agree field by field (the §2.2 coherence of the 0.21.0 plan, extended to the new writers) |
| 4 | Branch coherence | for every concept and pointer on a generated forked ledger, the branched read equals `reconstruct_on` at the branch |
| 5 | Migration | v24 → v25 on a populated 0.21.0 file: `edge_kinds` empty, every other table byte-identical, the snapshot stamp behaviour stated (§7) |
| 6 | Write cost | `chunk_budget/edges/90` and `…into a 8000-edge table` within the 0.21.0 session spread (2.22 / 3.01 ms) with `check_functional` in the path and no kind registered |
| 7 | Plans | every new statement (the branched concept fold, the pointer reads, the feed's range count) has an `EXPLAIN QUERY PLAN` pin |
| 8 | Error size | `size_of::<DbError>()` does not exceed 128 bytes with 55 variants; clippy `result_large_err` clean on current stable |
| 9 | No single-lineage regression | gate-9 stations vs `main-0.21.0` (criterion, same machine) inside ±10%: `assert_edge`, `upsert_concept`, `concepts/70`, `reconstruct_full_fold`, `reconstruct_composed`, `traverse` |
| 10 | Silk acceptance | the WorkLedger prototype's 21 ledger tests pass against a wheel built from `dev/0.22.0` |
| 11 | Docs | perf-claim, currency, link, sync and stability gates green; public surface re-blessed with the count in Appendix D |

## 7. Open questions

1. **Snapshots across v24 → v25.** In 0.21.0 a schema bump made older snapshots unreadable through
   their stamp, and the read folded from genesis until the next snapshot. A rung that only adds a
   sidecar table changes nothing a snapshot holds. Decide in Stage A whether the stamp check
   should accept v24 snapshots on v25, or whether the release notes carry the skip again.
2. **M6, concept shadowing on a branch.** Reverses D-214's rule. Not in 0.22.0. Silk's
   alternative — task text immutable, edits as pointer moves and new concepts — needs no Macrame
   change; ask Silk whether retitling on a branch matters before planning it.
3. **`MAX_BATCH_ROWS` = 2,000.** The draft's cap, from 0.9.30 figures. With D-306 a 2,000-edge
   batch holds the writer ~45 ms (the refitted `estimated_bulk_hold`); the cap may be loose
   rather than tight. Keep it; revisit with a measurement, not by reasoning.
4. **R15 on the draft's tests.** The draft's suites ran on libSQL 0.9.30, where R15 took out a
   binary or two per attempt. Read any crash in the new targets through the runner's protocol
   before calling it a defect — 0.10.0 has not faulted in this repo since the bump.

## 8. Documentation

- Register entries D-307…D-313, written from the draft's D-291…D-295 text with the numbers,
  schema rung and `DbError` count corrected and §3's reconciliations stated where they change
  the draft's claims (the batch's concept phase, the fold's rule).
- The "Amended by" lines on D-014, D-214, D-223 that the draft added, renumbered.
- `docs/quickref.md` §4 ladder to v25; README Schema Versioning row v25; §15 (branching) of the
  architecture for fork-at-an-instant, pointers and lineage concept reads.
- Release notes `docs/releases/v0.22.0.md`, including the Silk-facing list of what each
  workaround in the gaps doc is replaced by.
- okfgraph: the plan, Stage B's findings; under
  `macrame-0.22.0-conversation-port`, one thought records C1 and C2
  ([Amendment 1](Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%201.md)) as superseded asks,
  so a later reading of the gaps doc does not reopen them, and the release, as thoughts under
  `macrame-0.22.0-conversation-port`.
