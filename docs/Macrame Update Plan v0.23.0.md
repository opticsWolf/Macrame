# Macrame Update Plan v0.23.0 — the lineage-resolved valid fold, and the fork's valid-axis cut

**Status:** proposed (nothing built) — files CodeRadar's **U-2** and **U-3**, the two upstream
requests its `docs/temporal-p5b-and-beyond.md` still names unfiled (§0 2026-11-11 note, §7,
Q-13): U-2 gates P5b.3's branch reads, U-3 gates P5c's lineage fidelity · **Branch:**
`dev/0.23.0` from `main` (M0, this plan, is the first commit on it)
**From:** 0.22.0 (schema v29, concepts payload v3, snapshot container stamps the build's own
version with the read floor at v24, `DbError` 56 variants, public API 2,271 items, libSQL
0.10.0, MSRV 1.88)
**To:** 0.23.0 (schema v29 → **v30** for the fork's valid-axis cutoff; `DbError` 56 → 57 by
the size gate's count at build time; decisions **D-317, D-318**; the surface grows by what the
build lands — counted at the Appendix D re-bless, not prophesied here)

## 1. What this is

CodeRadar's temporal plan (`docs/temporal-p5b-and-beyond.md`, revised 2026-11-09/11) has one
critical path left, and it runs through this crate. Its §7 sequencing table: P5b.3's branch
reads are blocked by U-2, P5c's lineage fidelity by U-3; everything CodeRadar can do alone
(P5b.4 sha addressing, P5b.5 out-of-order ingest, P5d the live tree, P6b, P8) proceeds without
waiting. The plan's Q-13 recommended filing U-2 alone first, then U-3 — this plan does both in
one minor, U-2 first, because they share the fold that honours them and neither is large
enough to carry a cycle alone. Amendment 2's §13 Q8 kept U-3 out of 0.22.0 as "its own
small amendment" ("it concerns the fork, not concept identity"); this plan is that
filing, riding U-2's fold rather than standing alone.

The asks, quoted from its §6:

> **U-2 — A lineage-resolved valid fold.** `reconstruct_as_of` takes no lineage, and
> `reconstruct_on` does not resolve concepts by lineage. CodeRadar needs "state of lineage L,
> valid at V, as believed at R" in one call: the valid-axis counterpart of D-259. Without it,
> every branch read on the valid axis is either trunk-only or a CodeRadar-side fold (F-6).

> **U-3 — Forks with a valid-axis cut.** A fork that records the instant at which the child
> stops seeing its parent *on the valid axis* (the merge-base commit time), honoured by U-2's
> fold. Today's recorded cut is correct for "a branch of beliefs" and wrong for "a branch of
> code" (F3). The two cuts can coexist: recorded for what the branch could know, valid for
> what was true on it.

This is a **read-side cycle with one fork parameter**: no concept write law changes, no
overlay law changes, no new writer verbs. What 0.22.0 built — the ancestry walk (D-259), the
valid fold (D-292/D-310), the overlay's per-lineage concept walk (D-314), the adoption rule
(D-315), the past-instant paste (D-316) — is the machinery U-2 composes; the cycle's work is
the composition, not a new engine.

## 2. Doctrine position

The bitemporal cell has three coordinates a caller can name — lineage, valid instant,
recorded instant — and today the fold surface answers any two of them:

| Lineage | Valid | Recorded | Today |
|---|---|---|---|
| — | — | ✓ | `reconstruct(ts)` |
| — | ✓ | ✓ | `reconstruct_as_of(AsOf::bitemporal(v, r))` |
| ✓ | — | ✓ | `reconstruct_on(ts, branch)` (D-259; concepts by the overlay walk since D-314) |
| ✓ | ✓ | ✓ | **nothing** — the missing cell |

The per-lineage valid read exists only for concepts (`concepts_by_id` behind
`ReadPlan::on(b).valid_at(v)`); entities, links and annotations fall back to the trunk or to a
CodeRadar-side fold, which is exactly the F-6 defect U-2 closes. The composition is not a new
rule: D-259 bounds each ancestor at its fork point on the recorded axis, D-292 selects one
current belief per valid start, D-314 walks concepts nearest-first behind the overlay switch,
D-316 pastes the undecided instants. U-2 states that these compose, and the property tests
prove the composition rather than assert it.

U-3 adds a second cap to the fork, on the axis the recorded cap does not touch. The two caps
answer different questions — the recorded cap bounds what the branch *could know* (its
parent's post-fork writes are invisible), the valid cap bounds what *was true on it* (the
branch of code stops seeing its parent's state changes at the merge-base). Neither implies the
other; both live on the same `branches` row.

## 3. The items

### 3.1 U-2 → D-317: `reconstruct_as_of` takes a lineage

**API.** `AsOf` gains the lineage: `AsOf::valid_at(v).on(branch)`,
`AsOf::bitemporal(v, r).on(branch)`, and `AsOf::recorded_at(r).on(branch)` (which is today's
`reconstruct_on(ts, branch)` routed through one entry point — the recorded folds keep their
signatures and their fast paths; nothing existing breaks, everything new is a builder
addition). `reconstruct_as_of` routes:

- no `.on()` — today's whole-ledger answers, byte for byte (the D-310 law — a lineage-less
  read names no lineage and answers the creating lineage's row; this is the switch's whole
  reason and it does not move);
- `.on(b)` with no valid instant — the D-259 recorded fold for `b` (today's `reconstruct_on`
  body);
- `.on(b)` with a valid instant — **`reconstruct_valid_on`**, the new fold.

**Python.** `db.reconstruct_as_of(*, as_of_valid=None, as_of_recorded=None, on=None)`;
`on=None` is today's whole-ledger answer. The stub and the wrapper land in the same commit;
the parity gate covers the new keyword.

**The fold's laws.**

- **L1 — the recorded cap stays.** Each ancestor's rows are bounded at its `forked_at` on the
  recorded axis exactly as the D-259 recorded fold bounds them; a store with no valid-axis
  cutoffs answers through the new fold identically to what `concepts_by_id(.on(b).valid_at(v))`
  answers per id today. Gate 8's stations pin this as a no-regression, not a re-derivation.
- **L2 — one concept walk, two surfaces.** The fold's concept leg resolves through
  `resolve_concepts_overlay` at the read's instant — the same walk `concepts_by_id(.on(b)`
  `.valid_at(v)` runs, the same nearest-first law, the same `overlay_engaged` switch, the
  same cuts-as-absent answers, the same D-316 paste for the undecided rows of a `valid_at`
  read. The differential gate (gate 4) forces the two surfaces to agree per id on generated
  ledgers; a fold that re-derives concepts by a different rule than the id read is the
  reader-census failure mode (P-8) in a new coat.
- **L3 — the reader's own rows are uncapped.** The reader's lineage is `dist 0` with no
  cutoff on either axis, as D-259 has it; caps bound inheritance only.
- **L4 — valid selection per lineage.** The D-292 one-current-belief-per-valid-start
  selection runs per row source, on the rows L1 admits; two beliefs that both claim the
  instant refuse by name (`ConceptSingleOpen`, `OverlappingConcept`, `OverlappingInterval`)
  as the whole-ledger fold refuses — the same ledger written by the write guard cannot hold
  one, and the refusal is how a pre-guard ledger is found.
- **L5 — the archive composes.** The valid cold fold already reads the archive's lineage
  shape; the per-lineage cold fold answers `b`'s view from `cold` for rows the archive holds,
  and `archive_hint(conn, branch)`'s per-lineage sentence (D-314's archive arm) is the
  refusal's hint. No new archive machinery; the composition is a test, not a feature.
- **L6 — no snapshot.** The L×V×R state is not a belief at a recorded instant; like every
  valid-axis read it folds from genesis, ignores snapshots, and is refused as a snapshot
  source (D-298's mark). The doc states this once, beside `reconstruct_as_of`'s existing
  sentence, not as a new rule.

**Refusals.** An unknown lineage is `DbError::UnknownBranch` (`Lineages::shape`, checked
before the walk — the D-069 wrong-looking-right lesson). A valid instant predating everything
is the ordinary empty state, as the whole-ledger valid read answers. No new variant for U-2.

### 3.2 U-3 → D-318: the fork's valid-axis cut

**Schema (the v30 rung, `valid-axis-fork-cutoff`).** `branches` gains
`forked_valid_at TEXT` (nullable; `NULL` everywhere after the migration — the no-cutoff
default). No trigger, no backfill, no other table touched; the rung is the column and
nothing else.

**API.** The valid cut is orthogonal to the recorded `ForkPoint` (now / at / after), so it
rides as a second, optional argument beside any of the three recorded shapes. The exact Rust
spelling is settled at build time with the surface accounting; the recommendation is a
`ForkRequest`-style builder (`Database::fork_with(ForkRequest::new(name, from).at(ts)
.valid_cutoff(v))`) with the existing three methods kept as thin wrappers, because three
duplicated methods would triple the same law. Python gains `valid_cutoff=` keyword-only on
the fork family (additions keyword-only; nothing renamed — the port's compatibility rule).

**Checks and refusals.** The parent-precedes law mirrors the recorded one: a valid cutoff
before the parent's own valid cutoff inherits nothing and refuses
(`DbError::ValidCutoffPrecedesParent`, the estimated 57th variant — the size gate decides the
count). A non-canonical stamp is `InvalidTimestamp` from the shared normaliser. A cutoff on a
root fork refuses — a root has no parent to stop seeing (same shape as the recorded law's
root handling).

**The fold law the cut earns its name by.**

- **L7 — the cap bounds what can begin.** An ancestor row whose `valid_from` is past the
  child's `forked_valid_at` is not state on the child, even at an instant the child's own
  clock could name — this is what makes a forward-written parent supersede (recorded before
  the fork, valid from after the cut) invisible to the child, the case the recorded cap alone
  cannot catch. The cap is applied per ancestor in both folds that bound ancestry (the
  recorded fold keeps its single cap; the valid fold applies both).
- **L8 — no clamping.** A row admitted by both caps is admitted whole: the child sees the
  parent's belief per the row's own interval, and its own acts supersede, close or cut it on
  the child's law. The child stops seeing its parent's *changes*, not the persistence of what
  was already true — a branch of code whose merge-base file row is admitted answers that file
  at every later instant until the child's own edit, which is the answer F3's test asks for.
  The differential grid carries the case: parent forward-writes [T5, ∞) recorded pre-fork,
  child cut at T3, child's read at T4 answers the pre-cut text, never absent.

**What does not change.** The whole-ledger folds, the overlay write law, the adoption rule,
the triggers, the batch paths, the pointer and supersede verbs, the archive predicates — none
gain a parameter. The recorded `forked_at` column and its checks stand unchanged; a fork
without `valid_cutoff` writes the same row bytes as 0.22.0 (the migration gate's
byte-identity law, and gate 8's no-regression).

## 4. Tests (named, red first)

Each item lands with the test that fails on the bare surface, in the file the surface
suggests:

- `tests/reconstruct_valid_on_tests.rs` (D-317): the L×V×R cell on a forked ledger — entities
  and links answer the child's view at a past valid instant under a recorded bound, where
  today's surface has no answer; the trunk spelling and the branch spelling of the same
  instant agree where they should and differ by name where they must; `.on(b)` with no valid
  instant equals `reconstruct_on` byte for byte; a lineage-less valid read is unchanged
  byte for byte.
- `tests/m6_walk_valid_fold_tests.rs` gains the composition pins (L2): the fold's concept leg
  and `concepts_by_id` answer identically per id across the generated grids — the overlay's
  decided rows, the D-316 paste's rows, and the adoption rows the walk admits.
- `tests/fork_valid_cutoff_tests.rs` (D-318): the v30 rung on a populated 0.22.0 file
  (`forked_valid_at` null everywhere, nothing else moved); a fork with a valid cut; the
  forward-written supersede case (L7/L8's grid); the parent-precedes refusal; the root
  refusal; a no-cutoff fork byte-identical to 0.22.0's row.
- `tests_py/test_reconstruct_as_of_on.py`: the wheel's `on=` keyword, the stub, and the
  parity pins; `tests_py/test_fork_valid_cutoff.py` for the fork keyword.
- `tests/doctrine_property_tests.rs` gains the fork-valid-coherence generator: forks with and
  without valid cuts, reads on both axes, the fold against the per-lineage recomposition —
  the same model-based instrument every invariant since 0.5.4 arrived with.

## 5. Staging

1. **M0** — this plan (docs-only commit; `dev/0.23.0` cut from `main`).
2. **M1 — D-317.** `AsOf.on`, the routing, `reconstruct_valid_on` behind it, the composition
   pins; Python in the same commit.
3. **M2 — D-318.** The v30 rung, the fork family's valid cut, the fold's second cap, the
   forward-write grid.
4. **M3 — CodeRadar acceptance.** The P5b.3 branch-read refusal that names U-2 becomes
   buildable reads on Macrame 0.23.0; P5c's A-5 (fork at the merge-base on the valid axis)
   becomes writable; CodeRadar's F3/F-6 tests flip. This plan's acceptance is stated there:
   "a store pinned to 0.23.0 answers `state of L, valid at V, as believed at R` in one call,
   and a fork records both cuts."
5. **M4 — docs.** Register entries D-317/D-318 with the build's own numbers; quickref §4
   ladder to v30; README Schema Versioning row v30 and the release-notes bullet; Appendix D's
   re-bless with the surface delta; the release note, including the snapshot stamp
   re-measured at the tail (the container stamps the build's own version; the read floor
   stays v24); okfgraph thoughts under `macrame-0.23.0-upstream-asks`.

## 6. Acceptance gates

| # | Gate | Pass |
|---|---|---|
| 1 | Suites | Rust default, `--no-default-features`, `property-tests`, Python — green on all three CI runners at the release commit (`scripts/run_rust_suite.py`, `tests_py/run_suite.py`) |
| 2 | Red → green | each §3 item has a named test that fails on the bare 0.22.0 surface and passes after its fix |
| 3 | Doctrine property | the fork-valid-coherence generator: for every generated forked ledger (with and without valid cuts), the lineage-resolved valid fold equals the per-lineage recomposition field by field, and both cuts' laws hold on every generated read |
| 4 | Overlay coherence | per id on every generated grid, the fold's concept leg equals `concepts_by_id(.on(b).valid_at(v))` — one walk, two surfaces, no drift |
| 5 | Migration | v29 → v30 on a populated 0.22.0 file: `forked_valid_at` null everywhere, every other table byte-identical, the snapshot stamp behaviour stated (the container stamps v30; the read floor stays v24) |
| 6 | Plans | every new statement (the per-lineage valid fold, the capped ancestor rows, the cold composition) has an `EXPLAIN QUERY PLAN` pin |
| 7 | Error size | `size_of::<DbError>()` within the 128-byte gate at whatever variant count the build lands (56 or 57) |
| 8 | No single-lineage regression | gate-9 stations vs `main-0.22.0` (criterion, same machine) inside ±10%: `reconstruct`, `reconstruct_on`, `reconstruct_full_fold`, `reconstruct_composed`, `concepts/70`, `traverse` — the recorded paths are untouched, and the pins hold them to it |
| 9 | Docs | perf-claim, currency, link, sync and stability gates green; public surface re-blessed with the count in Appendix D |

## 7. Open questions

1. **The Rust spelling of the fork's second cut** (§3.2). Recommendation: the
   `ForkRequest`-style builder with thin wrappers, for the reason there. Settle against the
   surface-size accounting, not preference.
2. **Does the valid cap ever need to apply to the reader's own rows?** Today's answer: no —
   a cap on the reader would mean the reader stops seeing itself, which no ask names and no
   doctrine supports. If a consumer ever asks, the answer is a new decision, not a parameter
   slipped into this one.
3. **`MAX_BATCH_ROWS` = 2,000** (carried from the 0.22.0 plan's §7 Q3, still open): revisit
   with a measurement, not by reasoning. A probe can ride this cycle's probe budget without
   gating anything.
4. **The retraction verb** (carried from D-315's amendment): "this lineage absent from T
   onward" where the lineage holds an own row starting past T still has no name in today's
   verbs. Gated on a real consumer naming the need; if CodeRadar's P5c becomes that
   consumer, it files the ask and the answer is that act — not a retirement that silently
   stops short.

### What this plan deliberately does not take

Recorded so their absence reads as choice, not oversight — each with the home that keeps
it and the trigger that would move it:

- **Appendix C's two standing measurement items stay in Appendix C.** The first-chunk
  warm-up (D-146) is due sooner only if a visible stall is attributed to a bulk import;
  the R15 cold arm (D-147) only if a retry-budget change or an R15 figure is quoted
  outside `.cargo/config.toml`. Neither trigger has fired; restating them here would be
  a second home for items whose law is "recorded so their absence is read as choice".
- **The Silk ask** (Amendment 2 §13 Q6 — "ask Silk whether retitling on a branch
  matters") rides outside this plan, in parallel and not as a gate; Silk's
  immutable-text practice stays the recommended pattern either way. This cycle's
  consumer is CodeRadar, not Silk.
- **P-9, the pricing probe** is CodeRadar's own item (it counts the heal's write volume
  under the D-299 keep law) and needs no Macrame work; it runs beside this cycle on the
  CodeRadar side. Its result can inform a later Macrame probe, not this plan's gates.
- **The resolved 0.22.0 questions** (snapshots across v24→v25, concept shadowing on a
  branch, the R15 standing rule, P5's `within_batch`) are history in the 0.22.0 plan,
  register and release notes — not restated here.

## 8. Documentation

- Register entries **D-317** (the lineage-resolved valid fold) and **D-318** (the valid-axis
  fork cut), written from this plan's text with the build's own numbers — schema rung,
  `DbError` count, surface delta, and the L1–L8 laws stated as the decisions' claims.
- The "Amended by" lines on D-259 (both caps now live on the ancestry), D-292/D-310 (the
  fold gains the per-lineage leg beside the whole-ledger law), D-314 (the overlay walk is
  the fold's concept leg — L2 names the shared walk).
- `docs/quickref.md` §4 ladder to v30; README's Schema Versioning row v30 and the
  release-notes bullet leading with v0.23.0; §15 (branching) of the architecture for the
  two-cuts coexistence and the fold's composition.
- Release notes `docs/releases/v0.23.0.md`, with the CodeRadar-facing list: what each item
  unblocks (P5b.3's branch reads, P5c's lineage fidelity), the `on=` and `valid_cutoff=`
  keywords, and the F3/F-6 flips.
- CodeRadar's `docs/temporal-p5b-and-beyond.md` gains its dated note when the release
  ships (its 2026-11-11 note already points here for the filing).
- okfgraph: the plan under `macrame-0.23.0-upstream-asks`; Stage findings as they land.
