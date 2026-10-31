# Macrame Update Plan v0.21.0 — one rule for a concept's state

**Status:** P1 shape decided — two-statement shape 5 (§3.6): fold text unchanged, a write-maintained `concept_disorder` table, bounded correction; implementation is next · **Branch:** `dev/0.21.0` from `main` at `634740f` · **Date:** 2026-10-31
**From:** 0.20.0 (schema v23, concepts payload v3, snapshot container v6, public API 1,972 items)
**To:** 0.21.0 (schema v23 → **v24** for the repair rung §3.3; payload unchanged; snapshots from
0.20.0 become unreadable through the schema stamp they carry and are skipped, §3.4)

0.20.0 shipped with one live correctness defect, recorded in its release notes and in the
README's Known Risks: **a backfill overwrites the live `concepts` projection.** This release
fixes it. The fix turns out to be a decision about what a concept's state *is*, not a patch to
one statement, and §2 shows why. The rest of the release is the measurable part of 0.20.0's
"What is not closed" list.

| Item | Work | Schema | Standalone value |
|---|---|---|---|
| P1 — one state rule for concepts (D-303, decided) | 1 d probe + 3 d | v24 rung (§4.3) | **yes** — closes the backfill defect |
| P2 — price what is left in the 90-edge chunk (D-304, draft) | 1 d | none | a decision for the user, with figures |
| P3 — R15 against a clean `main` baseline | ≤ 0.5 d, time-boxed | none | says whether 0.20.0 changed the fault rate |
| P4 — two measurements carried from 0.19 and 0.20 | 0.5 d | none | closes two "not measured" lines |
| P5 — `Overlap::within_batch` becomes a provenance (D-295 follow-up) | 0.5 d, **optional** | none | breaking; ships only if the user wants the break now |

**P1's decision is made (§2.4, accepted 2026-10-31).** Everything else can proceed in
parallel.

---

## 1. The defect, restated precisely

The concept interval guard (D-291) admits a backfill: a past interval that does not overlap
what the ledger holds. The worked case is the one CodeRadar P5 produces, and it is already a
test (`tests/reconstruct_as_of_tests.rs`, `the_backfilled_ledger_answers_each_axis_its_own_way`):

```text
upsert "src/a.rs" title C2, valid [Jun, inf)      -> log seq 1, projection row = C2
upsert "src/a.rs" title C1, valid [Jan, Jun)      -> log seq 2, projection row = C1
```

After the second write:

| Reader | Answers | Should answer |
|---|---|---|
| `concepts` projection: current-mode hydration, FTS, the walk's `retired` filter | C1, valid `[Jan, Jun)` | C2 |
| `reconstruct(now)`, `reconstruct_as_of(recorded_at(now))` | C1, "the row written last" | (§2) |
| `reconstruct_as_of(valid_at(Aug))` | C2 | C2 ✔ |
| `reconstruct_as_of(valid_at(Mar))` | C1 | C1 ✔ |

The projection's answer is wrong by anyone's reading: the concept is valid now as C2, and the
projection says it stopped being valid in June. That is the line in the README, and the
workaround it gives (re-assert the current interval after a backfill) works only because it
writes C2 again, last.

**Why it happens.** `UPSERT_CONCEPT` (`src/connection.rs:5229`) is `INSERT … ON CONFLICT(id) DO
UPDATE SET` every column, unconditionally. The projection holds one row per id, so whatever is
written last *is* the projection. The log row comes from `trg_concepts_log_update`
(`src/schema/ddl.rs:1872`), which fires because that update happened.

---

## 2. Why this is a rule, not a statement

### 2.1 The recorded folds use the same rule as the projection

Every one-row-per-concept reader picks **the latest row by `seq_id`**:

| Reader | Where |
|---|---|
| The hot fold behind `reconstruct` | `src/temporal/replay.rs:559` (`HOT_FOLD`), `:755` (`ANCHORED_HOT_FOLD`) |
| `AttributeMode::AtTime` hydration | `src/temporal/as_of.rs:432` |
| Traversal node visibility at a recorded instant | `src/graph/builder.rs:687` (`concepts_at_instant_sql`) |
| The projection | `UPSERT_CONCEPT`, last writer wins |

The valid fold (`VALID_FOLD`, D-292) is the only concept reader that picks by interval, and it
is the only one that answers the backfill correctly.

### 2.2 And a doctrine property ties the two together

`tests/doctrine_property_tests.rs:448` states the principle for edges: *at any instant at or
after the newest stamp, the fold and current belief must coincide.* Today the concept side
coincides too, because both are wrong in the same way. **Fixing the projection alone would make
it diverge from `reconstruct(now)`**, which is the drift D-023 and the `links_current` rebuild
machinery exist to prevent on the edge side. So the projection and the recorded folds move
together, or neither moves.

### 2.3 0.20.0 pinned the recorded answer deliberately

Gate 7's third assertion says the recorded read returns C1, "the row written last", and the
test's comment calls both answers right. That was a deliberate position: the recorded axis
answers *what was written as of ts*, and the valid axis answers *what was true at V*. The
position is coherent for the log and wrong for a **state**. `reconstruct` returns one row per
concept, not a list of writes, and a state in which `src/a.rs` stopped existing in June is not
something anyone believed at `now`. Everyone who wrote to this ledger believed C2 was current.

### 2.4 The rule, and the decision it needs

> **A concept's state is its open belief if it has one, otherwise its latest-starting belief.**
> Within one valid start, the belief is the row written last (D-291's definition, unchanged).

As an ordering this is `valid_from DESC, seq_id DESC`, take the first. It is time-independent,
so a projection can hold it without a clock.

Why the two halves are one rule: under D-291 a concept has at most one open belief, and an
open interval `[s, inf)` overlaps every interval that starts after `s`. So if an open belief
exists, it *is* the latest-starting one. The "otherwise" clause only matters for a concept whose
every interval is closed. That includes a concept closed by correction, and a ledger with
pre-guard overlaps, where the rule is still deterministic.

What it does not change:

- **Links.** A link's `entity_id` is `source|target|type|valid_from`, so every fold partition
  of a link already holds one start. `valid_from DESC, seq_id DESC` and `seq_id DESC` select the
  same row there, so one ordering serves both tables.
- **The archive.** `LOG_ARCHIVABLE` (`src/temporal/archive.rs:417`) already treats a concept
  row as superseded only by a newer row *of the same start*. Every start's latest row stays hot,
  which is exactly the set the new rule reads. No change is needed, and §6 gate 6 checks it.
- **In-order histories.** When starts are written in increasing order, the latest-starting
  belief is also the last written, so every existing ledger without a backfill gets the same
  answers as before.

What it does change: **a future-dated version written ahead of time** (`[Jun, Dec)` current,
`[Dec, inf)` written in advance) shows as the state from the moment it is written. That is also
today's behaviour, because it was written last, so nothing regresses. But the rule now says so
on purpose, and the rustdoc must say it too.

**Decided 2026-10-31 (user): accept the rule** and flip gate 7's third assertion to C2, making
`reconstruct(now)` and the projection agree with the valid fold at now. Rejected: keeping
"the row written last" for the recorded fold, which leaves the projection defect unfixable
without the §2.2 divergence. P1 may start.

---

## 3. P1 work, in order

**Shape of record (decided 2026-10-31): two-statement shape 5** — the fold keeps today's text,
and a write-maintained `concept_disorder` table carries the answer to one question: which
partitions the `seq_id` winner is not the rule winner. The write marks a pair when the
projection's update is skipped (the backfill event) and never unmarks; the read runs today's
fold, a one-row existence probe, and — only when marks are owed — a bounded correction whose
rows merge over the fold's in Rust. Design and figures: §3.6. Probe-outcome records: §3.5
(the plan's three shapes, none meeting the gate) and §3.6 (shape 5, meeting it).

### 3.1 Probe first (1 d): can the recorded fold take the new order without a window sort?

This is the risk in P1, and it comes before any change to `src/`. D-254 retired a full window
sort from the hot fold, and `tests/bitemporal_plan_tests.rs` forbids that shape with pinned
VDBE triples. `seq_id DESC` is the order `idx_txlog_fold_partition` delivers for free; the new
order is not. Three shapes to measure, on the D-293 20,000-row fixture and on the release-bench
ledger:

1. **Uniform order** — `ORDER BY json_extract(payload, '$.valid_from') DESC, seq_id DESC` in
   the existing window. Simplest, and likely to sort per partition, which the pins forbid.
2. **Concept arm on `idx_txlog_concept_starts`** — the concept rows get their own arm, served
   by the partial index D-300 added. Its order is `(entity_id, branch_id, valid_from ASC,
   seq_id DESC)`, and the mixed direction does not match the new order in either scan
   direction. If this is the shape that wins, the v24 rung recreates the index with
   `valid_from DESC`. D-300's skip-scan reads `> starts.valid_from ORDER BY … LIMIT 1`, which a
   reversed scan still serves, but its plan pin must be re-run, not assumed.
3. **Seq-latest, then correct** — keep today's window and replace each concept's selected row
   with the latest row at that concept's greatest start, one seek per concept on the partial
   index. The cost scales with the number of concepts, not with the log.

**Gate:** a shape is accepted only if its plan is pinned with the same strength as D-254's
(the plan text and the VDBE triple), and its wall-clock time on the 20,000-row fixture is
within 10% of today's fold. If no shape meets that, P1 stops and the figures come to the user
before anything ships.

The same probe covers `as_of.rs:432` and `builder.rs:687`, which take a bounded id set and a
recorded bound respectively, and are cheaper to change.

### 3.2 The projection write (D-303)

`UPSERT_CONCEPT`'s `DO UPDATE` gains `WHERE excluded.valid_from >= concepts.valid_from`. That
makes the projection follow the rule at write time: an equal start is a correction of the
current belief, a later start is a new version, and an earlier start is a backfill that leaves
the projection alone. FTS follows the projection through its triggers and needs no change.

**A skipped update fires no trigger, so the backfill would write no log row.** The write path
detects the skip (`execute` returns 0 changed rows for a no-op `DO UPDATE`) and inserts the log
row itself. Two writers of one payload is the drift D-056 exists to prevent, so the payload
moves into one macro that expands to the `json_object(...)` body with a column prefix. The
trigger is built from it with `NEW.` and the Rust statement with the prefix of a one-row `VALUES`
CTE. The trigger text must stay byte-identical, and a test asserts it does. That way no rung is
needed for this part, and existing files keep their trigger bodies (the `IF NOT EXISTS` problem
documented at `ddl.rs:1740`).

Atomicity: in the single-concept path, a skipped upsert has written nothing, so the log insert
is the only write and is atomic on its own. The chunked path is already inside one
`BEGIN IMMEDIATE`. In both paths the D-291 guard has already run.

**The skip carries one more write: the mark.** On the same 0-changed-rows signal the write
path inserts `(entity_id, branch_id)` into `concept_disorder` (v24, §3.3) — the pair whose
fold winner is no longer its rule winner. **The mark is never cleared**: a later healing
write must not unmark it, because a cleared mark would let a later write change what a
past read returned (decided 2026-10-31, §3.6; gate 11 is the named test). Marking costs
nothing when the projection update runs; the greatest-start decision that classifies the
write rides the D-291 guard's already-read interval list when the asserted interval is
open (the common path), and pays one `LIMIT 1` seek otherwise (probe-priced: 21–24 µs
worst case; §6 gate 9).

**Spike before building:** confirm on libSQL 0.9.30 that a `DO UPDATE … WHERE` false reports
0 changes through both `Connection::execute` and a prepared `Statement::execute`, and fires
neither `AFTER UPDATE` trigger nor `trg_concepts_monotonic_ra`. The project has been bitten by
assuming SQLite behaviour holds through libSQL before (the shadow-rebuild notes in
`src/integrity/shadow.rs`), so this is a test in the suite, not a note.

**Open sub-decision:** what a backfill's log row records for `extra` when the caller omits it.
The projection rule is preserve-on-omission (D-283), but "the row" being preserved belongs to a
different interval. Proposed: the latest row *of the same start* if one exists, otherwise
`'{}'`. A backfill must never inherit the current version's attributes by accident, since
D-278 made `extra` versioned.

### 3.3 Repairing ledgers that already hold a backfill (v23 → v24)

A 0.20.0 ledger with a backfill has a wrong projection row now, and the new write rule does not
touch it until the concept is written again. The rung re-derives the projection row of every
concept whose projection `valid_from` is not its greatest start in the log, from that start's
latest log row. Every field it needs is in payload v3.

Two traps the rung must handle:

- **The update would fire `trg_concepts_log_update`**, recording a belief change nobody made.
  The rung drops and recreates that trigger around the repair, in the rung's own transaction,
  using the pattern the v8 → v9 and v12 rungs already use (`src/schema/migrations.rs`).
- **The update moves `recorded_at` backwards**, to the chosen row's stamp, so
  `trg_concepts_monotonic_ra` would abort it. It is dropped and recreated the same way.

The rung reports how many rows it repaired through the existing migration tracing, and its
test builds a v23 file with a backfill, opens it, and checks the projection, the FTS row and
the log row count, which must not change.

The same rung **recreates `idx_txlog_concept_starts` with `valid_from DESC`** (shape 5's
correction seeks on it; D-300's skip-scan reads `> starts.valid_from ORDER BY … LIMIT 1`,
which a reversed scan still serves, but its plan pin is re-run, not assumed), and **adds
and fills `concept_disorder (entity_id, branch_id, PK)` from existing history**: the same
partition pass that finds wrong projection rows marks every pair whose written history
ever held a row starting before a start it followed. The mark set is defined over history
(now-or-then disorder), not over the current projection: a pair whose backfill was later
healed is still marked, which is exactly what keeps past reads stable (§3.6).


### 3.4 Snapshots

A snapshot holds folded state. One taken under the old rule holds the wrong row for a
backfilled concept, and composing on it would carry that row forward. The container stamps
`SCHEMA_VERSION` and refuses a mismatch (`src/temporal/snapshot.rs:411`). `newest_usable_snapshot`
skips refused files with a warning (`src/temporal/replay.rs:1433`), so the v24 rung retires
every 0.20.0 snapshot safely. The cost is that `reconstruct` folds from genesis until a new
snapshot is saved, and the release notes must say so. If P1 ships without a rung, bump
`SNAP_FORMAT_VERSION` 6 → 7 instead, which has the same effect.

The new ordering composes across an anchor: taking the maximum by `(valid_from, seq_id)` over
snapshot ∪ hot rows gives the same row as over the whole log. `replay_snapshot_tests` gets a
backfill case that checks this.

### 3.5 Probe outcome (2026-10-31) — no §3.1 shape meets the gate; figures to the user

Session-3 numbers below: rerun 2026-10-31 on an idle machine (CPU 4% at launch, nothing else
running), superseding the first two sessions where background load distorted medians in both
directions. One reading rule the rerun teaches: within a station the **first sweep elevated
— ~1.7× above its own bounded sibling** (the fixture was just built and the cache is cold),
so only bounded-vs-bounded comparisons, against s0's bounded median, are honest. Three
sessions with different load levels keep the same orderings and magnitudes; the verdict has
not moved.

**Nothing below is committed and nothing in `src/` moved.** The probe's station:
`examples/concept_state_rule_probe.rs` (release build; medians of 25 runs after 2 warmups;
row-by-row consumption; winners cross-checked identical between every candidate in every
epoch — one rule, three computations). Fixtures: `mixed_starts` (20,200 concept rows, 400
entities × 20 distinct starts written out of order, plus 200 §1-sequence backfills),
`bench_ledger` (5,000 concepts through the real chunked write path, in order), `mixed_log`
(the bench ledger plus 5,000 links). Findings also live as okf thoughts under
`macrame-0.21.0-state-rule`.

| fixture | s0 today (bounded med) | s1 uniform | s2 concept arm | s3 seq-then-correct | s3m pre-rung | s4 full-order index (extra) |
|---|---|---|---|---|---|---|
| `mixed_starts` 20,200 rows | **23.3 ms** | +77% | **+41% (best-of-session 1: +51%)** | +115% | +137% (full sort+reload per seek) | rule +25% (sort-free), old fold +14% |
| `bench_ledger` 5,000 rows | **9.9 ms** | +61% | +25% | **+1090%** | +1420% | rule +25% (sort-free), old fold +29% |
| `mixed_log` 10,000 rows | **20.8 ms** | +58% | +33% | +700…800% (noisy tail) | +626% | rule ≈+27%, old fold +28% |

- **s2 is the least-bad candidate and still fails.** The union's links arm reuses today's
  shape, so `mixed_log` gains least; on a per-partition basis the concepts arm pays a scan
  that the shipped fold partition partially saves. Across the three sessions s2 on
  `mixed_log` ranges +8%…+33% — the only candidate that ever brushed the gate, and only
  for the mixed log.
- **s3's cost scales with concept count**, as §3.1 predicted: 5,000 seeks ≈ +115 ms at bench
  scale. Out outright.
- **s4 (not §3.1's list; measured for completeness)** replaces `idx_txlog_fold_partition`
  with `(table_name, entity_id, branch_id, json_extract(payload,'$.valid_from') DESC,
  seq_id DESC)`. The rule's order becomes sort-free, but the index widens every entry
  (json_extract per row at write time — a [D-301]-gated write trade not priced here) and
  today's seq-ordered window loses its free order and pays a right-part sort on every read:
  +6…29% measured. The cost lands on all readers, not only the new rule.
- The gate said: no shape within 10% on the 20,000-row fixture → **the figures come to the
  user before anything ships.** Options: (A) §8's fallback — ship nothing of P1, keep the
  README workaround, record the figures; (B) accept s2 under a bench-scale reading of the
  gate (v24 rung + arm-split fold, ~+1/5 on drive-sized reads, ~+50% at deep history,
  D-300's guard plans re-pinned); (C) fund a "shape 5" probe — keep today's window and
  correct only partitions whose winning row's start is not the partition's greatest start
  (healthy ledgers ≈ one extra index pass) — not among the plan's shapes, needs its own
  probe and pin first. **The user funded (C); its outcome is §3.6, and the shape decision
  stands inside that section.**

Baselines taken before any change (gate 10): criterion baseline `main-0.20.0` on this session
— `write_path/upsert_concept` ≈ 0.42 ms, `chunk_budget/concepts/70` ≈ 3.30 ms; both drifted
from the plan's 0.36/2.56 reference figures, which is why the gate is session-relative.
Gate 1 is standing red: `tests/concept_state_rule_tests.rs` (uncommitted — it must be
green before anything of P1 commits) fails the projection assertion on the §1 sequence and
greens on the two companions that pin `>=` against `>`.

### 3.6 Shape-5 outcome and the shape decision (2026-10-31)

`examples/concept_disorder_probe.rs`. Two formulations measured. **The single-statement UNION
form is ruled out twice over**: its per-row filter's bare correlation bound to the wrong
scope (a probe bug that became a lesson — the alias must be named against the outer query),
and, once fixed, the planner still allocated the correction arm a whole-log `SCAN t` with an
*empty* side table and executed it: 176–183 ms against the fold's 23–27 ms on the 20k
station, +70% at bench size. A plan whose shape depends on the side table's emptiness is
not a plan to pin.

**The two-statement formulation meets the gate.** Statement A is today's fold text
*unchanged* (it pins itself); then `SELECT EXISTS (… concept_disorder …) LIMIT 1`
— 0.6 µs per 1,000 calls; then statement B only when the probe says rows are owed:
per marked pair, the rule row on the v24 `valid_from DESC` index, merged over the
fold in Rust by `(branch, entity)`.

| station | s0 (bounded med) | s5-two healthy | s5-two disordered | correction alone | marked pairs |
|---|---|---|---|---|---|
| `bench_healthy` | 10.69 ms | **10.67 ms (≈0%)** | — | never runs | 0/5,000 |
| `bench_disordered` | 10.78 ms | +2.2% | **+5.7% ✓** (re-run: 10.64 vs 10.13 = +5.0%) | 0.54 ms / 0.5 ms | 250/5,000 (5%) |
| `mixed_log` | 21.83 ms | ≈0% | — | never runs | 0/10,000 |
| `mixed_starts` (pathological: 100% marked) | 23.3 ms (clean) | +4% | **33.5 ms = +44%** | 8.5 ms | 400/400 |

Winner cross-checks hold on every station and both forms: s5 equals the rule window wherever
disorder exists, equals s0's winners where none exists, and on `bench_disordered` exactly
250 winners moved — one per backfilled pair.

Write-side prices (gate 9's check): the per-write greatest-start decision ≈ 21–24 µs
(0.42 ms upsert ⇒ +5–6% worst case — and derivable from the D-291 guard's already-read
interval list when the asserted interval is open, the common path); the 70-id batch
decision statement ≈ 0.11–0.15 ms (against the 3.30 ms session chunk baseline ⇒ +3–5%
worst case).

**Decision inside the shape (2026-10-31, taken on the user's agenda): the rule is one rule
for every recorded read.** D-291's bound and D-303's rule are orthogonal: the bound decides
which rows exist for the query (rows recorded ≤ ts), the rule decides which of those wins.
A later write never changes what existed at ts, so a uniform rule cannot produce backward
flips; a flip can only come from mutating the mark set. Hence:

- **The mark is never cleared.** `concept_disorder` lists every pair that was *ever*
  disordered; a healing write does not unmark. Clearing would let a later write change
  what a past read returned — the ledger promises that never happens. The cost is honesty:
  corrections stay owed for since-healed pairs (the heal-epoch re-run below prices their
  correctness; their steady-state cost wants one clean-machine rerun).
- **The correction carries the fold's own recorded bound.** Bound the subquery and the
  whole pair is bounded (`t` is the subquery's own winner, by the join on `seq_id`). A
  read at a past ts admits nothing written after it. Measurement lesson paid for: a
  redundant `t.recorded_at` predicate on the *join* was logically harmless but sent the
  same statement from ~0.5 ms to ~9 ms on libsql once heal rows sat beyond the bound —
  the bounded correction's pin (gate 4) must be taken on a healed-and-`ANALYZE`d ledger,
  and the join predicate must stay exactly the join.
- **A never-marked concept satisfies the rule without correction, by induction**: when
  each write in its history starts at or ahead of the partition's then-greatest start,
  every new row is both the seq winner and the rule winner, bounded or not. That is the
  shape's correctness argument, and the healthy-epoch equality is its empirical statement.

What the rung now carries (§3.3): the v24 recreates `idx_txlog_concept_starts` with
`valid_from DESC` (D-300's skip-scan pins re-run), adds `concept_disorder (entity_id,
branch_id, PK)`, **fills it from existing history** (the same pass that repairs projection
rows marks every pair whose history ever held a backfill), and the read side adds the
probe + correction pair wherever the fold serves the state rule. The bounded form's claim
is asserted too: at any ts the two-statement read equals the rule window over rows
recorded ≤ ts. AtTime hydration and traversal visibility follow the same rule through the
same mechanism (§2.2's coherence), and the future-dated case stays as §2.4 has it.

**Re-run (same day, idle machine)** with the never-cleared table and the bounded
correction, heal epochs added: stability reads (a bounded read at ts_b before and after
later heal writes) return identical winners — Δ0 — and equal the bounded rule window (Δ0)
on both `bench_disordered` (250 heals; currently-disordered 250 → 0; marks persist
250 → 250) and `mixed_starts` (200 heals; currently-disordered 400 → 200; marks persist
400 → 400). Probe lessons paid for on the way: the heal writes must carry the fixture
entity-id spelling *with its prefix* (the first version bound the digits only, wrote
foreign partitions, and healed nobody — caught the next run by an exact-drop assertion);
libsql local keeps per-connection state, so the heal epoch drops both handles before any
post-heal read (a reader predating the writes missed them). The heal-epoch fold timings
were bimodal in every run (plain-fold best 10.2 / med 18 ms within one handler), so the
steady-state never-clear economy is **not** priced here; the clean disordered-epoch figures
above stay the pricing basis, and one more rerun prices the healed steady state.

Gate 8 grows named mutations: *mark left behind* (a skipped upsert that forgets the mark,
caught by the rule-identity test), *correction run for an absent pair* (caught by
fold-vs-s0 equality on healthy ledgers), *a heal clears a mark* and *the bounded correction
unbounds* (both caught by gate 11's stability test). Gate 11 is new: past reads never
change — a `reconstruct_as_of(ts)` read returns identical winners before and after a later
write.

---

## 4. Draft decision-register entry

### D-303 (decided 2026-10-31, not implemented) — A concept's state is its open belief, else its latest-starting belief

Context: the 0.20.0 backfill defect (§1); every one-row concept reader picks by `seq_id`
(§2.1), and the fold-equals-current-belief property ties the projection to the fold (§2.2).

Decision: §2.4's rule, applied by the projection write (`DO UPDATE … WHERE excluded.valid_from >=
concepts.valid_from`, with the write path inserting the log row and the disorder mark on a
skipped update), and by the recorded folds **uniformly — one rule for every recorded read**
(2026-10-31) — through shape 5's probe-and-correction pair (§3.6), by AtTime hydration and
by traversal visibility. Existing projections are repaired and the mark set filled from
history by the v23 → v24 rung. Gate 7's recorded assertion changes from C1 to C2.

Rejected: *fixing the projection alone* (breaks §2.2); *moving the log write off the trigger
entirely* (0.20.0's README proposal: it would stop raw-SQL updates being logged, and it is a
larger schema change than the defect needs); *a clock-based "valid now" projection* (cannot be
held without a write when time passes); *a now/past split of the rule — running the rule only
for reads at the present and keeping §2.1's seq winner for historical reads* (a later write
would change what a past read returned — the ledger promises that never happens); *clearing
`concept_disorder` marks on a healing write* (the same break, by another door).

---

## 5. P2–P5

### P2 — price what is left in the 90-edge chunk (D-304, draft)

`chunk_budget/edges/90` reads 3.99 ms against ≤ 3 ms (D-302 bench, median of three sessions).
D-301 priced each index by dropping it, on the old bench. This item reruns
`examples/edge_chunk_index_cost_probe.rs` on the D-302 bench, then lists **every registered
reader** of the cheapest-to-drop candidates, with `idx_txlog_time` first. D-300 removed its only
consumer in the concepts fold, and the D-196 candidate gate now reads "not used".

**Output is figures and a recommendation, not a dropped index.** D-301's register text says no
index is traded for write speed without the user's say, and that stands. The adaptive chunk
loop (D-143) keeps holding the bound either way.

### P3 — R15 against a clean baseline (time-boxed, ≤ 0.5 d)

0.20.0 could not say whether its crash rate differs from 0.19's, because no baseline was run on
`main` before the release. This item runs the default suite five times on `v0.19.1` and five
times on `v0.20.0`, both from clean release builds, and records the binaries that crash and how
often. It does not attempt a fix: Road to 1.0 W1 owns R15. The point is to stop the next
release note from having to say "not confirmed".

### P4 — two measurements

- **The bounded valid fold at production size** (0.20.0, not closed): wall-clock for
  `reconstruct_as_of` with a recorded bound, on a ledger of the size the release bench builds,
  including the archive-attached arm. An earlier probe's output was not kept, so this is a
  measurement, not a re-statement.
- **C1 — `MAX_EXTRA_BYTES` against real frontmatter**, carried since 0.19.0. A local okfgraph
  bundle is available, so a sample of real frontmatter sizes can be taken (not from `docs/`,
  which carries none). The result is a sentence in the `extra` rustdoc either way.

### P5 — `Overlap::within_batch` becomes a provenance (optional, breaking)

The concept payload has the three-state `OverlapProvenance` (D-295), while the edge payload
keeps a bool, so the valid fold's link arm words a refused *read* as a refused *write*. Widening
the public field breaks callers. 0.21.0 is a pre-1.0 minor and may break, but whether to spend
the break here is the user's call. **Default: carried, not built.**

---

## 6. Acceptance gates

1. **The backfill test goes red first.** Before §3.2 lands, a test asserting the projection
   holds C2 after the §1 sequence fails on `dev/0.21.0`, so the gate proves it can fail.
2. **Gate 7 flips.** `the_backfilled_ledger_answers_each_axis_its_own_way` asserts C2 for the
   recorded read and still asserts recorded = `reconstruct`.
3. **A concept arm in the doctrine property.** The property at `doctrine_property_tests.rs:448`
   gains concept operations that include out-of-order starts, and asserts that
   `reconstruct(now)`'s concepts equal the projection, field by field.
4. **Plan pins.** Shape 5's three statements are pinned with plan text and VDBE triples —
   statement A is today's fold text and inherits its standing pins; the probe and the
   bounded correction get theirs, and `bitemporal_plan_tests` still forbids a whole-log
   window sort. **The correction's pin is taken on a healed-and-`ANALYZE`d ledger too**: the
   probe caught a redundant join predicate turning the same statement from ~0.5 ms to
   ~9 ms once heal rows sat beyond the bound, and a plan that only holds on unhealed data
   is not a pin.
5. **The rung.** A v23 file with a backfill opens repaired: the projection row is correct, the
   FTS row matches it, the log row count is unchanged, and `concept_disorder` holds the
   backfilled pairs — filled from written history, so a pair whose backfill was later
   healed is still marked.
6. **The archive is unchanged.** A backfilled concept archived past its cutoff still
   reconstructs the open belief, both before and after the archive pass.
7. **Snapshots.** A 0.20.0 snapshot is skipped with a warning and the result equals the
   genesis fold. A snapshot taken at v24 composes to the same state as the genesis fold on a
   backfilled ledger.
8. **Mutations, each caught by a named test:**
   - `>=` → `>` in the projection `WHERE`, caught by the correction-at-same-start case;
   - dropping the skipped-update log insert, caught by the log row count;
   - `valid_from DESC` → `ASC` in the correction, caught by gate 2;
   - removing the trigger drop from the rung, caught by gate 5's row count;
   - a mark forgotten on a skipped upsert (*mark left behind*), caught by the rule-identity
     test — the fold then keeps a winner the rule window would replace;
   - a correction run for an absent pair, caught by fold-vs-s0 equality on healthy ledgers;
   - a heal clearing a mark, caught by gate 11 (*a later write changed a past read*).
9. **Release bench.** `write_path/upsert_concept` stays within 10% of the session baseline
   (criterion baseline `main-0.20.0`, 2026-10-31: 0.42 ms) — the projection `WHERE` adds a
   comparison and the backfill branch adds one insert; the probe priced the extra decision
   at 21–24 µs worst case. The `concepts/70` chunk stays within 10% of the same baseline
   (3.30 ms; batch decision ≈ 0.11–0.15 ms priced).
10. **Suites and doc gates** green on both feature sets, with figures recorded against a
    baseline taken **before** P1 starts. 0.20.0 could not compare because it took none.
11. **Past reads never change.** `reconstruct_as_of(ts)` at a fixed ts returns identical
    winners run twice around a later write — the named case: a backfill at t1, a read,
    a healing write at t2, and the same read again, mutant-equal between them. The shape-5
    probe's heal epochs assert it live (Δ0 with real heal rows on disk), and the compound
    mutants (mark cleared; bounded correction unbound) both must fail it.
9. **Release bench.** `write_path/upsert_concept` stays within 10% of 0.20.0's 0.36 ms, because
   the projection `WHERE` adds a comparison and the backfill branch adds one insert. The
   `concepts/70` chunk stays within 10% of 2.56 ms.
10. **Suites and doc gates** green on both feature sets, with figures recorded against a
    baseline taken **before** P1 starts. 0.20.0 could not compare because it took none.

---

## 7. Documentation and knowledge discipline

As in 0.20.0 §9, documentation moves with the code rather than at the end:

- README Known Risks: the backfill row closes. The rule goes into the bitemporal section in one
  sentence.
- `reconstruct`'s rustdoc states the rule, including the future-dated case (§2.4).
- The release notes carry a **behaviour change** entry: `reconstruct` answers differently for a
  ledger that holds a backfill, and old snapshots are skipped.
- The s13 register gets D-303 and D-304 as they are decided, not at the tag.
- **okfgraph:** findings go in as thoughts under topic `macrame-0.21.0-state-rule`, and the
  bundle is re-imported **once per work session** (`okf import --all --bundle-path docs`, with
  `--prune-missing` after a rename). A full re-import measured 353 s for 50 documents on
  2026-10-31. Search the 0.20.0 topic (`macrame-0.20.0-valid-axis`) before re-deriving anything
  about the guard or the valid fold. Thoughts share one title per topic, so wikilinks to them
  resolve to nothing; search by content, not by link.

---

## 8. If the shape fails at implementation

The probe decision is taken: P1 ships on two-statement shape 5 (write side §3.2, read pair
§3.6's record, rung §3.3). The fallback below is now about gates, not analysis: if any
acceptance gate fails to oil in — the §2.2 coherence (gate 3), the healed-ledger plan pin
(gate 4), the past-read stability (gate 11), or the release bench (gate 9) — the same
conclusion from §3.5 stands: **ship nothing of P1's read side**, keep the README workaround,
and publish the figures as the reason. The write side alone cannot ship: it diverges the
projection from the fold (§2.2). P2–P4 do not depend on P1 and ship regardless.
