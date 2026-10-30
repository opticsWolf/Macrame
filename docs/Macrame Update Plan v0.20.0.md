# Macrame Update Plan v0.20.0 — a valid-axis state fold

**Status:** P1–P4 built on `dev/0.20.0`; not released, version label open · **Branch:** `dev/0.20.0` (cut, not merged) · **Date:** 2026-10-29
**From:** 0.19.1 (schema v22, concepts payload v3, snapshot v5, surface 1,909 items + 1 Python keyword)
**To:** 0.20.0 (**schema v22 — unchanged**; payload and snapshot unchanged)
**Supersedes:** this file's own earlier form, a *request* drafted the same day against
`main` at `5098722`. §13 records what the request got wrong.
**Requested by:** CodeRadar P5 (DR-41, commit-driven ingestion),
`CodeRadar/docs/temporal-p5-commit-ingestion.md` §2 and §5 P5b.

`Database::reconstruct(ts)` folds the ledger on `recorded_at` — "state as believed
at `ts`". There is no counterpart bounding the fold on the **valid** axis, so the
state fold is the one read surface in the crate that knows a single clock. This
release gives it the `AsOf` the traversal already takes.

Two items, and the first is the reason the second is decidable at all.

| Item | Work | Schema | Standalone value |
|---|---|---|---|
| P1 — concept valid-interval guard (D-291) | 3 d | v22 unchanged | **yes** — closes a live defect in current-belief reads (§3.3) |
| P2 — `reconstruct_as_of` (D-292) | 3 d | v22 unchanged | needs P1 |
| P3 — plan-gate re-measurement (D-293) | ~4 h | none | — |
| P4 — Python surface (D-294) | ~4 h, optional | none | needs P2 |

**P1 ships first and ships even if P2 is cut.** That is not sequencing for its own
sake: §3.3 shows the writer gap already corrupts `reconstruct(now)` for any caller
that writes `valid_from` out of order, which is a defect today and not a
precondition for tomorrow.

---

## 0. What changed since this was a request

The request's §7 asked the maintainer *"is this wanted at all, and is it wanted
now?"* and said the honest answer from the requesting side was *not urgent*. **That
question is answered: the work is wanted, and this document is to be a plan.**
(Instruction of 2026-10-29. The request's reasoning for why it was not urgent still
stands and is preserved in §12 as the fallback if the release is cut.)

Two findings since changed the shape of the work, and both make it *more* coherent
rather than larger:

1. **There is no `trg_concepts_single_open`** (§5.2). The request said the concept
   half should get "D-060's treatment", which reads as a mirror. D-060 could lean on
   an existing single-open rule to partition against. Concepts have no such rule, so
   a prior decision has to be made first.
2. **The request conflated two different designs** (§3). "Bound the fold on the
   valid axis" and "fold on the valid axis" are not the same operation, they answer
   different questions, and only one of them is what CodeRadar needs. Naming them
   apart dissolves the request's §5.1 entirely.

---

## 1. What already exists (verified against 0.19.1, not assumed)

This is the part that makes the work narrow rather than a new capability. Macrame's
bitemporality is largely built; one surface was left behind.

| Surface | Valid axis | Recorded axis | Where |
|---|---|---|---|
| `AsOf` itself | `valid` | `recorded`, plus `bitemporal(valid, recorded)` | `src/temporal/as_of.rs:18-55` |
| Edge reader | **yes** — "valid-time graph edges under current belief as of `ts`" | — | `src/temporal/as_of.rs:144,156` |
| Traversal / subgraph loader | **yes** — `as_of_valid` | yes — `as_of_recorded`, belief-at-instant hydration | `src/graph/builder.rs`, [D-289](architecture/s13-decision-register.md#d-289), [D-290](architecture/s13-decision-register.md#d-290) |
| Concept visibility at an instant | **yes** — interval applied from the payload | yes | `concept_visible_at`, `src/temporal/as_of.rs:482` |
| Concept fold at an instant | — | yes | `fold_concepts_at`, `src/temporal/as_of.rs:402` |
| Plan cost of carrying both instants | measured and pinned | measured and pinned | `tests/bitemporal_plan_tests.rs:178-281` |
| **State fold (`MaterializedState`)** | **absent** | yes | `src/temporal/replay.rs:830`, `HOT_FOLD` at `:546` |

Four pieces of infrastructure already exist and the plan is largely the work of
reusing them rather than writing new ones:

* **`concept_visible_at(payload, valid)`** (`as_of.rs:482`) is already exactly the
  per-row predicate, already `pub(crate)`, and already shared by `hydrate_at_time`
  and the loader's historical arm so the two "cannot disagree about what an instant
  shows" (D-289). It handles the payload-version gate, retirement-at-the-instant,
  and a v1 payload's absent bounds as unbounded.
* **`reconstruct_from`** (`replay.rs:853`) already splits the whole of `reconstruct`
  except the line that picks a base, added at 0.15.19 for review C-18. The new entry
  point composes here rather than copying the ATTACH bracket — D-227's rule, which
  that function's own doc comment cites.
* **`fold_delta(conn, query, params)`** (`replay.rs:1728`) already takes its
  parameters as an argument, so a second placeholder for the valid instant needs no
  signature change.
* **`idx_txlog_fold_partition`** on `(table_name, entity_id, branch_id, seq_id DESC)`
  (`ddl.rs:1514`) is exactly the index P1's probe needs: `table_name = 'concepts'
  AND entity_id = ?` seeks on the two leading columns. **So there is no new index and
  no schema rung in this release** — the same free ride D-060's probe got from
  `idx_lc_open_interval` (D-059).

## 2. Why the caller cannot do this itself

CodeRadar's own rules forbid it, and they were written to protect this boundary
rather than to defer work: its **INV-6** puts every at-T answer in the engine, and
its **F-6** forbids a second fold implementation in shipped code, with the stated
failure mode that two folds will diverge and the Python-side one will be the wrong
one. It has one standing exception — a measurement script
(`scripts/verify_history.py`) that replays the log and is imported by nothing.

A valid-axis fold written above the engine would also be wrong on its own terms: it
would have to re-derive snapshot composition and the archive reach check (§5.3),
both of which live inside `reconstruct` and neither of which is visible from outside
the crate.

---

## 3. The two designs, and which one is being asked for

The request treated "a valid-axis fold" as one thing. It is two, they answer
different questions, and the difference decides the whole plan.

Both start from the same SQL skeleton. `HOT_FOLD` (`replay.rs:546`) narrows with
`ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id
DESC)` under `WHERE recorded_at <= ?1`. The question is what the valid instant does
to that.

### 3.1 Design A — bound the state (narrow on recorded, then filter on valid)

Keep the narrowing exactly as it is, and apply `concept_visible_at(payload,
Some(V))` to each winning row in `fold_delta`.

**This is already the crate's composition elsewhere.** `fold_concepts_at` narrows on
`seq_id DESC` under `recorded_at <= R` and *then* `concept_visible_at` filters the
winner — and `concept_visible_at`'s own comment says why the predicate is applied in
Rust after narrowing: "the interval lives inside the JSON payload and the fold has
already narrowed to one row per entity".

What it answers: *of the state the ledger held at R, which concepts claimed to be
valid at V?* For `AsOf::bitemporal(V, R)` that is a defensible bitemporal cell, and
it is **nearly free** — one predicate call per row, no new SQL, no new decisions.

What it cannot answer: anything about V that current belief is not currently *about*.
Only one row per concept is ever consulted, so if the latest row's interval excludes
V the concept is absent, whatever earlier rows said.

### 3.2 Design B — fold on the valid axis (select the row whose interval contains V)

Narrow on the valid axis: among rows recorded by R, pick the row for each entity
whose valid interval contains V.

What it answers: *what was true at V?* — including when current belief is about a
different period entirely.

### 3.3 CodeRadar needs B, and the worked example says why

P5 writes one log row per file per commit, each with `valid_from` = that commit's
time. For one file concept there are therefore *many* rows with *different* valid
intervals, and "the state of the repository at commit V" means picking the row whose
interval contains V.

Design A cannot express that. It would return the latest-written row and then ask
whether it covers V — so a query at an old commit returns either today's file or
nothing.

Worked, with the `seq_id` order P5 actually produces when it backfills:

```text
ingest commit C2  ->  log row  seq 1,  concept "src/a.rs", valid [Jun, inf)
backfill commit C1 -> log row  seq 2,  concept "src/a.rs", valid [Jan, Jun)

reconstruct_as_of(AsOf::valid_at("Mar"))
  Design A: narrow -> seq 2 (latest written). Interval [Jan, Jun) contains Mar. -> C1's state.   correct here, by luck
  reconstruct_as_of(AsOf::valid_at("Aug"))
  Design A: narrow -> seq 2. [Jan, Jun) excludes Aug. -> absent.                                 WRONG: C2 was valid at Aug
  Design B: among rows, the one whose interval contains Aug -> seq 1 -> C2's state.              correct
```

**And the same arithmetic is a defect in the crate today, with no new surface
involved.** `reconstruct(now)` narrows on `seq_id DESC`, so after that backfill the
*current* state of `src/a.rs` is C1's — the older commit's — because it was written
last. The `concepts` projection agrees, since an upsert replaces the row and
`concepts.id` is `TEXT NOT NULL UNIQUE` (`ddl.rs:648`). Nothing warns.

That is why P1 is not merely a precondition. **A caller that writes `valid_from` out
of order gets a wrong answer from `reconstruct(now)` on 0.19.1**. The guard refuses the overlapping cases, not this one: a backfill that does not overlap what the ledger holds is a legitimate history and is admitted. What `reconstruct(now)` returns for such a ledger is a read question, and P2 answers it with `reconstruct_as_of`. P1 does not claim to fix it.

**Decision: Design B**, with Design A's predicate reused unchanged for the
`recorded` bound. Rejected: *Design A alone* (cannot answer the question asked, §3.3);
*both, behind a flag* (two foldings of one axis, which is D-227's failure mode with
the halves differing in semantics rather than in spelling).

---

## 4. Proposed surface

Minimal, additive, and declining to touch the most-called reader in the crate — the
same reasoning `query_as_of_edges` used when it gained a lineage
(`as_of.rs:152-156`):

```rust
// New. `AsOf` already carries both instants and already has a constructor for
// each case, so this signature adds a type, not a convention.
pub async fn reconstruct_as_of(&self, at: AsOf) -> Result<MaterializedState>;

// Unchanged, and defined in terms of the new one:
//   reconstruct(ts)        == reconstruct_as_of(AsOf::recorded_at(ts))
//   reconstruct_on(ts, br) == the lineage-resolved form of the same
```

| `AsOf` | Question | Mechanism |
|---|---|---|
| `{ valid: None, recorded: Some(R) }` | state as believed at R | today's path, byte-identical SQL |
| `{ valid: None, recorded: None }` | now on both | `AsOf::now()`, today's path at `now` |
| `{ valid: Some(V), recorded: None }` | what was true at V, under current belief | Design B fold, no recorded bound |
| `{ valid: Some(V), recorded: Some(R) }` | what we believed at R about what was true at V | Design B fold under `recorded_at <= R` |

The fourth row is the cell Doctrine VIII exists to make answerable, and the one a
retroactive correction makes interesting.

**One fold implementation, parameterised — not a second one beside it**
([D-227](architecture/s13-decision-register.md#d-227)): "a read spelled twice
drifts, and the half nobody calls is the half that drifts first."

---

## 5. The decisions underneath

### 5.1 The tiebreak on the valid axis — dissolved, not decided

The request asked what wins when more than one row's interval contains the instant,
and offered three answers (latest `valid_from`; latest `seq_id` among containing
rows; refuse).

**With P1 landed, the question cannot arise.** If the writer refuses overlapping
valid intervals for one concept id, at most one row's interval contains V, and the
fold has nothing to adjudicate. This is exactly §5.2's observation about edges,
applied to the axis the request is about: *the state the tiebreak would adjudicate
cannot be written.*

So §5.1 is not a decision this plan makes. It is a decision P1 removes — and that is
the strongest argument for doing P1 first, stronger than precedent. **A fold that
picks between two contradictory truths is the class of silence the doctrine is
organised against**, and no tiebreak rule avoids that; it only chooses which
contradiction to report.

For a database written *before* P1 — history already holding overlaps — see §5.4.

### 5.2 Overlapping valid intervals — guarded for edges, not for concepts

**Edges are guarded.** [D-060](architecture/s13-decision-register.md#d-060) refuses
two closed intervals for one `(source, target, edge_type)` that overlap in valid
time, in the write actor (`reject_overlapping_interval`, `src/connection.rs:6021`),
precisely because `query_as_of_edges` at an instant inside both "returned one
relationship as two edges, which every weighted algorithm downstream then
double-counted". That is the same failure a valid-axis fold would hit, found once
already on the axis this plan is about.

**Concepts are not.** `reject_overlapping_interval` has exactly one call site and it
takes an edge (`src/connection.rs:5466`, `&EdgeAssertion`). `ConceptUpsert.valid_from`
is caller-supplied (`src/connection.rs:594`) and nothing compares it against the
intervals already in the log for that id — the `concepts` projection's
`id NOT NULL UNIQUE` constrains the *current-state row*, not the history behind it,
and the history is the only thing a fold reads. Verified: every `overlap` reference
in `src/` belongs to the edge guard.

**And "D-060's treatment" is bigger than a mirror, because there is no
`trg_concepts_single_open`.** The concepts triggers are FTS maintenance, log
mirroring, monotonic `recorded_at`, the delete guard, cross-lineage and branch
immutability (`src/schema/ddl.rs`) — no interval guard of any kind. D-060's hardest
part was *partitioning* a new overlap check against an existing single-open rule:
`defer_to_single_open` (`src/connection.rs:5987`) returns true exactly when both
intervals are open, which is exactly `trg_links_single_open`'s `WHEN` clause, and
without that partition the general check shadowed `SingleOpenViolation` and broke
three tests — "defect Q's shape reintroduced by a fix".

Concepts have no rule to defer to, which cuts two ways:

* There is nothing to partition against, so the guard is in that sense simpler.
* But **nothing guarantees one open interval per concept either**. A writer that
  leaves every version open — `[v1, inf)`, `[v2, inf)`, … — produces a history in
  which *every* pair overlaps. Design B is then undecidable for that concept
  regardless of any tiebreak.

So P1 has to settle the single-open question before the overlap question, and the
plan takes the position that **a concept has one belief in force per valid start, and at most one of those beliefs is open**, by the same argument `trg_links_single_open` rests on for links. The belief for a start is the latest row written for it, which is the definition `links_current` keeps for edges (D-060). The first draft of this section read *every* row ever written as the history to check, and that refuses every later version of a concept: a superseded open row would still block the next open interval. The decision on 2026-10-29 was current belief, and the guard is built to it (D-291). The two guards then
partition the concept space the way D-060's two partition the edge space.

D-060's cost argument does not transfer and does not need to: its probe rides
`idx_lc_open_interval` because the three equality columns plus the `valid_from`
range are that index's shape; P1's probe rides `idx_txlog_fold_partition` on
`(table_name, entity_id)` instead (§1).

### 5.3 Snapshots and the archive boundary

`reconstruct` composes onto `snapshot_anchor` → `newest_usable_snapshot`, which
picks by sequence and accepts a snapshot whose `state.timestamp <= ts`
(`replay.rs:1244-1257`). **That timestamp is a `recorded` instant**: a snapshot is
"state as believed at R", which is not "state valid at V", so a valid-axis fold
cannot reuse the existing snapshots and must not try.

**Decision: fold from genesis on the valid axis, and say so in the rustdoc.** That
is the sentence `reconstruct` already carries for the no-snapshot case, and
`reconstruct_from(conn, ts, archive_path, None)` already expresses it — `base: None`
is a fold from genesis, so the new entry point passes `None` and composes nothing.

Rejected: *composing when `recorded` is bound and the snapshot's instant is at or
before it, then applying the valid predicate to the result* — this is sound only if
the valid predicate is a pure row filter over the composed state, which is Design A,
and §3 chose B; under B the snapshot has already discarded the rows the fold needs
to choose among. *Valid-axis snapshots of their own* — a new artifact class, and
almost certainly not worth it for a read that can fold from genesis and say so.

**`hot_log_reach` needs a reading on the new axis, and it is not the existing one.**
`HotLogReach::PredatesRecordedHistory` (`replay.rs:1337`) means "nothing had been
recorded by `ts`, and nothing has ever been removed from the log, so no other file
could hold it either", and `reconstruct_from` answers the empty state with
`predates_recorded_history = true`. On the valid axis that reasoning does not hold:
**"nothing was valid at V" is answerable from a ledger written entirely later**,
which is precisely CodeRadar's case, since it will ingest a decade of commit time in
one afternoon of recorded time.

**Decision:** the reach test stays a *recorded*-axis test and is consulted only for
the recorded bound. With `valid: Some(V), recorded: None` the reach question is asked
at `now`, which is `HotLogReach::Covers` by the function's own cheap arm whenever the
log is intact. `predates_recorded_history` keeps its exact current meaning and is
**never set from a valid-axis miss** — an empty result on the valid axis is an
ordinary answer, not a statement about the ledger's horizon. The flag's doc comment
gains the distinction, because a field whose meaning is "one of two things" is the
defect [D-121](architecture/s13-decision-register.md#d-121) added it to prevent.

**Correction (0.20.0, P2, D-292).** The reach argument above holds for the recorded fold and not for the valid one. `LOG_ARCHIVABLE` keys on `entity_id`, so a concept's superseded start can leave the hot log while it is still the belief for its own interval, and the newest-row argument does not cover that. As built, a valid read consults the archive whenever one is present, takes the hot log alone only when `log_integrity` says no rows left it, and never takes the reach shortcut. `predates_recorded_history` is not set from a valid miss, as decided here.

### 5.4 Databases that already hold overlaps

P1 refuses new overlaps. It cannot un-write old ones, and the plan says so rather
than discovering it: **a ledger written before 0.20.0 by a caller that supplied its
own `valid_from` may already hold overlapping concept intervals**, and no migration
can repair that, because choosing which of two contradictory assertions to keep is
exactly the judgement the crate refuses to make on the caller's behalf
([Doctrine III](architecture/s0-s3-foundations.md#doctrine-iii) has no neutral
answer).

**Decision: the fold refuses, naming the concept and both intervals.** A new
`DbError` variant (`OverlappingValidIntervals { id, a, b }`, 47 → 48 variants) rather
than a silent pick. The alternative — pick one and carry on — would make the fold's
answer depend on which contradiction it met first, which is the double-counting
failure D-060 closed, moved from edges to concepts and from "two rows" to "two
truths".

**Superseded in the build (0.20.0, P2, D-292).** No new variant. P1 had already added `OverlappingConcept` and `ConceptSingleOpen`, the valid fold reuses them, and a link uses `OverlappingInterval`. `DbError` stays at 49 variants. The refusal is the one this section chose; only the variant names differ from the sketch above.

**What reuse cost, and how it was paid (0.19.4, D-295).** Reusing the write guard's variants meant reusing its vocabulary, and the payload's `within_batch` bool had only the two write cases. A refused *read* — where the caller asserted neither interval — had to borrow one, so it printed "the asserted" about a row it had merely found. The bit that was missing went into the payload rather than into a 50th variant: `ConceptOverlap` now carries an `OverlapProvenance` with `AlreadyRecorded`, `SameBatch` and `BothRecorded`. Both write messages are unchanged. Taken in 0.19.4 because `ConceptOverlap` has not shipped, which makes the field free to widen now and breaking afterwards.

A `verify`-time probe reports the same condition ahead of a read, so an operator can
find it without waiting for a query to fail. Rejected: *refusing to open such a
file* (the overlap affects one concept's history; the rest of the ledger is sound and
every other read still answers); *a repair tool that keeps the latest `seq_id`*
(that is the arbitrary choice §3.3 shows is wrong, shipped as a utility).

---

## 6. Draft decision-register entries

Both are drafts. They enter `s13-decision-register.md` as part of the implementing
commits, not after — see §9.

### D-291 (draft)

> `<a id="d-291"></a>`**D-291 — A concept has one belief in force per valid start, at most one of them open, and no
> overlapping intervals, refused in the write actor (0.20.0, P1).**
> [D-060](architecture/s13-decision-register.md#d-060) (the edge half, and the
> reasoning this reuses), [D-014](architecture/s13-decision-register.md#d-014) (one
> writer, so the window does not exist rather than being narrow),
> [Doctrine II](architecture/s0-s3-foundations.md#doctrine-ii),
> [Doctrine III](architecture/s0-s3-foundations.md#doctrine-iii). Evidence: `src/connection.rs` (`CURRENT_CONCEPT_INTERVALS`, `reject_overlapping_concept`, `reject_concept_overlaps_within`, `check_batch_against_ledger`), `src/error.rs` (`OverlappingConcept`, `ConceptSingleOpen`), `tests/concept_interval_guard_tests.rs`, `tests/branch_write_tests.rs` (the trunk re-upsert pinned at its seeded start), `tests_py/test_vector.py` (the ranking fixture closes its earlier start).
>
> `reject_overlapping_interval` guarded edges only, and `ConceptUpsert.valid_from` is
> caller-supplied, so a caller writing its own valid time could record two
> contradictory intervals for one concept id with nothing to stop it. The failure is
> not hypothetical and not confined to a future reader: the fold narrows on
> `seq_id DESC`, so the *last written* row is current belief, and a caller that
> backfills history gets an older state as the current one from `reconstruct(now)`
> on 0.19.1.
>
> Unlike the edge case there was no single-open rule to partition against, so this
> decides that question too: a concept has **one belief in force per valid start, at most one of them open**, by
> `trg_links_single_open`'s argument. The probe rides `idx_txlog_fold_partition` on
> its two leading columns, so no index and no schema rung.
>
> What it does not cover: raw SQL against the same file, and history written before
> this release (§5.4 — refused at read time, naming both intervals, because no
> migration can choose between two assertions).
>
> Rejected: *a trigger* (D-060's reasoning — it adds a probe to every concept
> insert, and the callers are going through the actor anyway); *refusing in a pure
> validator* (no connection, and at the API boundary it is a check-then-write race
> against the actor's insert); *deduplicating in every reader* (pushes the decision
> onto every reader written from now on).

### D-292 (draft)

> `<a id="d-292"></a>`**D-292 — `reconstruct` under `AsOf`: the state fold learns the
> valid axis (0.20.0, P2).** [D-291](#d-291-draft) (what makes the fold decidable),
> [D-289](architecture/s13-decision-register.md#d-289),
> [D-290](architecture/s13-decision-register.md#d-290) (the measured precedent for a
> valid bound on a `transaction_log` range),
> [D-227](architecture/s13-decision-register.md#d-227) (one lowering, not two),
> [D-049](architecture/s13-decision-register.md#d-049) (snapshot composition, which
> this axis cannot reuse), [D-121](architecture/s13-decision-register.md#d-121) (the
> empty-state flag, whose meaning this does not widen). Evidence: the register entry, [D-292](architecture/s13-decision-register.md#d-292).
>
> The state fold was the one read surface that knew a single clock. `AsOf` already
> carried both instants and the traversal already took them, so this is the fold
> catching up with the vocabulary rather than new bitemporality.
>
> **It selects rather than filters**, and that is the whole of the design: bounding
> the recorded fold's winner against V would answer "is current belief about V",
> not "what was true at V", and the two differ exactly when a caller writes history
> out of order — which is the caller that asked. `concept_visible_at` is reused
> unchanged for the recorded bound, so the two surfaces cannot disagree about what
> an instant shows (D-289's rule, extended to a third reader).
>
> Folds from genesis on the valid axis: a snapshot's `timestamp` is a recorded
> instant, so the existing snapshots describe a different axis and composing onto
> them would discard the rows the fold must choose among.
>
> Rejected: *bounding instead of selecting* (above); *a valid-axis `reconstruct`
> beside the recorded one* (two folds drift — D-227); *valid-axis snapshots* (a new
> artifact class for a read that can fold from genesis and say so); *a caller-side
> fold* (F-6 on the CodeRadar side, and it would have to re-derive snapshot
> composition and the archive reach check, neither visible from outside the crate);
> *setting `predates_recorded_history` on a valid-axis miss* (it would make one
> field mean two things, which is the defect D-121 exists to prevent).

---

## 7. Work breakdown

### P1 — the concept valid-interval guard (3 d)

1. **`Interval` for concepts.** Reuse the existing arithmetic; `Interval::overlaps`
   is already the crate's single definition of overlap and D-060's note explains why
   the decision stays in Rust while the SQL narrows to a provable superset.
2. **`reject_overlapping_concept_interval`**, called from the concept-upsert arm of
   the actor and from the batch path inside its transaction — mirroring
   `reject_overlapping_interval`'s two call sites. Probe:
   `transaction_log WHERE table_name = 'concepts' AND entity_id = ?`, narrowed to
   `valid_from < :new_valid_to`.
3. **`defer_to_single_open` for concepts**, so open+open reports the single-open
   violation and closed+closed reports the overlap, partitioning the space rather
   than layering.
4. **Batch self-check.** A batch is checked against *itself* before the transaction
   opens, because the database cannot see rows that are not in it yet and a
   self-contradicting batch would otherwise commit its own overlap. D-060 learned
   this; do not re-learn it.
5. **Errors.** One new variant for the read-time refusal (§5.4) and reuse of the
   existing single-open variant where it fits. Count the surface delta.

### P2 — `reconstruct_as_of` (3 d)

1. **`VALID_FOLD`** beside `HOT_FOLD`, selecting on the valid axis. A separate
   constant rather than a branch inside the shared text, so the current-belief
   statement stays byte-identical and every golden pins it by construction rather
   than by careful reading — D-290's reasoning, which it applied for exactly this.
2. **`reconstruct_as_of`** composing through `reconstruct_from(.., base: None)`.
3. **The cold arm.** `cold_fold` needs the same treatment or an explicit refusal;
   decide by whether the archive's own `recorded_at` bound can coexist with a valid
   selection, and write the answer down either way.
4. **`hot_log_reach` reading** per §5.3, with `predates_recorded_history` left
   strictly alone and its doc comment extended.
5. **Rustdoc**: the genesis fold, the cost, and the refusal of §5.4.

### P3 — plan-gate re-measurement (~4 h)

Not optional, and not a formality. D-290 re-ran the D-196/D-254 gates when the
concepts fold became a new consumer of `transaction_log` ranges and found the
transaction-time triple moving `(4, 3, 5)` → `(4, 4, 9)` while "the valid-time
control and the cross-axis equality did not move".

**This fold is a second new consumer and must be measured the same way rather than
blessed.** The expectation to test is that the valid bound reaches the plan *here*
even though `tests/bitemporal_plan_tests.rs:191` pins that it does not reach the
traversal's plan — because this fold reads the log directly rather than joining a
materialised fold. **If that expectation is wrong**, the index candidate at `:281`
— `(recorded_at, json_extract(payload, '$.valid_from'))`, whose second column D-290
found dead — becomes live for the first time, and the arithmetic that rejected it is
re-run rather than inherited.

**Outcome (D-293), and where it contradicted this section.** The expectation above
was that the valid bound reaches the plan. In the form P2 shipped it did not:
`(?1 IS NULL OR recorded_at <= ?1)` cannot seek. P3 replaced that form with two
shapes, the open `VALID_FOLD` and `VALID_FOLD_BOUNDED`, whose bound is in the text.
The bounded form seeks `recorded_at` on the hot log up to about half the log, and
scans only when the bound keeps every row. The candidate index
`(recorded_at, json_extract(payload, '$.valid_from'))` is reached by the bounded
valid fold and by the recorded fold, on its leading column, and never by the open
fold; its second column stays dead, as D-290 found. The archive's cold side seeks
only at the smallest bounds and scans its fold-partition index from a few thousand
rows. The choice depends on the statistics and on how many rows the bound keeps, so
the pins name their bound. D-293 has the measurements.

### P4 — Python surface (~4 h, optional)

`reconstruct_as_of` through the binding, with the mode contract spelled the way
D-289's `attribute_mode` was: an unstated instant means the documented default, not
a guess. Cut first if the release grows.

---

## 8. Acceptance gates

Numbered so a landing can be reported against them. Each is a test that fails before
the change and passes after, not a description of the change.

1. **The guard refuses a second overlapping closed interval** for one concept id,
   naming both, and the refusal is classified.
2. **The guard refuses a second open interval** for one concept id, as a
   single-open violation rather than as an overlap — the partition, pinned.
3. **A batch that contradicts itself is refused before the transaction opens**, with
   no row written.
4. **Non-overlapping successive intervals are admitted** — the positive control, so
   the guard is not a refusal of everything.
5. **`reconstruct(ts)` is byte-identical** — the captured SQL text of the recorded
   path, compared before and after, the way D-243 compared twenty-seven texts.
6. **`reconstruct_as_of(AsOf::recorded_at(ts))` equals `reconstruct(ts)`** on a
   populated ledger, which is what makes the old entry point a definition rather
   than a parallel implementation.
7. **§3.3's worked example**, both instants, as a test: the backfilled ledger, a
   query at `Aug` returning C2's state, and the same query on the recorded axis
   returning C1's. The two answers differ and both are right — that is the gate.
8. **`AsOf::bitemporal(V, R)`** returns the state believed at R restricted to what
   was valid at V, with a retroactive correction in the fixture so the cell is not
   trivially equal to either axis alone.
9. **A valid instant before the ledger's first `recorded_at`** answers an ordinary
   state — not an error, and with `predates_recorded_history` **false**. This is the
   §5.3 distinction, and it is the gate CodeRadar's decade-in-an-afternoon case
   needs.
10. **A pre-existing overlap is refused at read time**, naming the concept and both
    intervals, and `verify` reports the same condition.
11. **Plan gates re-measured** (P3), with the terms pinned exactly — which fold
    reaches which index on which column — so either term breaking re-opens the
    arithmetic for real rather than silently. **Met by [D-293](architecture/s13-decision-register.md#d-293):** `mod valid_fold_plan` in `src/temporal/replay.rs` pins each shape at a named bound, and the candidate's reach on its leading column.
12. **Mutation probes.** Each of gates 1–4 and 7 is verified by mutation, not merely
    by passing: remove the guard's call site and gates 1–3 fail; invert the valid
    selection and gate 7 fails. **A gate with no contrast between its healthy and
    unhealthy output is decoration**
    ([D-236](architecture/s13-decision-register.md#d-236)).

Suite deltas to report at landing: Rust, feature-off, and Python counts, plus the
`DbError` variant count and the public surface item count against 1,909.

---

## 9. Documentation and knowledge discipline — not a closing step

**The architecture documentation is kept current *while* implementing, in the same
commit as the code it describes, not in a pass afterwards.** This is a standing rule
of this repository rather than a preference for this release, and it has a gate:
`tests/doc_currency_tests.rs` exists because documentation drift is a *category*
rather than an incident ([D-144](architecture/s13-decision-register.md#d-144)), and
it already holds five assertions that will fail if this release's documentation
lags.

What that means concretely for each landing commit:

1. **The decision register** (`docs/architecture/s13-decision-register.md`) gains the
   entry — D-291 with P1, D-292 with P2, D-293 with P3, D-294 with P4 — with the **Evidence** line filled in with
   real paths and test names, not left as the draft's placeholder. An entry whose
   evidence line is a promise is the half that drifts first.
   `the_file_table_offers_the_decisions_the_register_actually_holds` checks the file
   table against the register's highest decision, so adding D-291 without updating
   the table fails the suite.
2. **The revision history row** in `docs/architecture/README.md`, and the document
   version in its header table.
   `the_revision_history_reaches_the_release_the_crate_is_at` and
   `the_header_names_the_version_the_document_describes` both gate this.
3. **The affected architecture sections** — `s4-schema.md` §4.2 and §4.7 for what the
   storage layer still permits that the API now refuses (D-060's own precedent for
   saying so in both places, and
   [D-074](architecture/s13-decision-register.md#d-074) for why one place rather
   than three); `s5-modules.md` for `replay.rs` and the actor; `appendices.md` plus
   `public-api.txt` for the surface delta, held by `scripts/check_public_api.py` in
   CI and `tests/public_path_tests.rs` in `cargo test`.
4. **The release note** `docs/releases/v0.20.0.md`, including a **"What is not
   closed"** section. §5.4's unrepairable pre-existing overlaps and anything P3
   leaves extrapolated belong there, stated rather than implied.
5. **This plan document** is updated when the implementation contradicts it. The
   0.16.0 cycle's recorded lesson is that "the plan was wrong in public more often
   than it was right", and the value came from correcting it in place rather than
   quietly diverging.

### Findings and thoughts go into the project's okfgraph database

The repository now carries a local okfgraph index of its own documentation —
`okfgraph.toml` pins `bundle_root = "docs"` and `db_path = "docs-graph.db"`, both
gitignored as local tooling artifacts. Use it as the working memory for this
release:

* **Record findings and reasoning as thoughts**, under one topic for the release, so
  a narrowing that took a day is searchable afterwards rather than living only in a
  session transcript:

  ```bash
  okf ingest --kind thoughts --topic macrame-0.20.0-valid-axis --thoughts "<finding>"
  ```

  On first use, check whether this writes a file under `docs/` as well as a row in
  the database. If it does, decide deliberately whether those files are tracked —
  they would otherwise arrive in the next commit unreviewed.
* **Re-import after documentation edits**, so the index tracks the tree rather than
  drifting from it:

  ```bash
  okf import --all --bundle-path docs            # after edits
  okf import --all --bundle-path docs --prune-missing   # after a rename or delete
  ```

  **Batch this once per work session rather than running it after each edit.** The
  delta is computed per *directory*, not per file: measured on this release's own
  commits, editing one document in `docs/` reported "1 changed dir(s), 22 changed
  files out of 49 total" and re-embedded all 22 in **263 s** on the GPU.

  Note which direction is expensive, because it is the one this section asks for.
  The directory hash is computed recursively but the re-import set is that
  directory's direct children, so a file edited in `docs/architecture/` invalidates
  `docs/` as well and queues **38 of 49** files — the 22 above is the *best* case,
  for a root-level edit. Editing an architecture document and a release note in the
  same sitting costs no more than editing either alone, which is the whole argument
  for batching. A rename needs `--prune-missing`, or the old filename's concept
  survives as an orphan with no source file — the id is path-derived, so a rename is
  a delete plus an add rather than an update. Both findings are written up as Issue G
  in the dated okfgraph issue file beside the sibling checkout, with the probe that
  measures the fan-out without paying for a re-embed.

* **Search before re-deriving.** The 0.19.1 narrowing record
  (`macrame-0.19.1-loader-temporal-closure-consolidated.md`, corrections C1–C9) is
  in the index, and this release touches the same predicates. Two of this plan's own
  findings came out of searching it rather than re-reading the code.
* **Know the index's limits.** okfgraph 0.10.2 has link-resolution defects that make
  cross-reference queries unreliable on this corpus — 71 reported broken links, 69
  of them artifacts. They are recorded outside this repository, beside the sibling
  checkout, in `okf-graph-issues-20261029.md` (with its repro scripts in
  `okf-graph-issues-20261029-repro/`), because they are okfgraph's defects and not
  this project's. Semantic and full-text search are sound; the
  graph edges are not, so do not use `traverse` to conclude that nothing references
  something.

---

## 10. Cost, and what this plan does not settle

**Cost.** The fold is O(log) from genesis with no snapshot composition, which is what
`reconstruct` already costs on a ledger with no usable snapshot, and the rustdoc says
so. P3 measures whether the valid bound reaches the plan; nothing here assumes it.

**Not settled, and deliberately left to the implementation:**

* **The cold arm** (P2.3). Whether a valid selection can coexist with the archive's
  recorded bound is a real question and the answer may be "refuse on the valid axis
  when the fold needs the archive", which is honest and narrow.
* **Links.** This plan is about the concept half. `query_as_of_edges` already reads
  the valid axis for edges under current belief, and whether `MaterializedState.edges`
  should carry a valid bound too is a separate question with a separate guard
  already in place (D-060). Not in scope; named so it is not assumed closed.
* **Valid-time-only reads elsewhere** are unchanged: present-tense closure is
  current belief about what was true at `v`. D-289's sentence, still true.

---

## 11. Carried from 0.19

* **C1 — `MAX_EXTRA_BYTES` against OKFgraph's real frontmatter**, optional in the
  0.19.0 plan and still unchecked. Newly cheap: an okfgraph bundle is now local.
  Note that `docs/` itself carries **no** frontmatter, so a representative sample has
  to come from a bundle that does.
* **The blob scan's cost at the largest ladder sizes** is extrapolated from two
  points, not measured. Unchanged by this release; recorded so it is not rediscovered.

---

## 12. If this release is cut

The requesting side ships without it. CodeRadar's P5a enforces that ingest only ever
moves forward, so recorded order equals commit topological order and today's
`reconstruct` answers correctly by construction. What P5a cannot do is accept history
out of order — backfilling an old branch, ingesting a second remote, re-ingesting a
range after a parser improvement — and each of those is a real future need rather
than a present one. The refusal is recorded as an invariant rather than a warning:
the ingest refuses the commit rather than writing it and letting the reads sort it
out.

**P1 should land even then.** §3.3's defect does not require the new surface to
exist, and the guard is what makes the forward-only invariant enforced by the engine
rather than promised by the caller.

---

## 13. What the request got wrong

Kept because the 0.16.0 cycle's lesson was that correcting a plan in place is worth
more than a plan that was never wrong.

* **It treated the fold as one design.** §3 splits it; the split dissolves the
  request's §5.1 entirely rather than answering it.
* **Its §5.1 offered three tiebreak answers as if one had to be chosen.** With the
  writer guarded, none is needed. The request's own §5.2 had the argument — "the
  state the tiebreak would adjudicate cannot be written" — and did not apply it to
  its own §5.1.
* **It read the writer gap as a precondition.** It is a live defect in current-belief
  reads (§3.3), which raises P1's priority above the fold it was filed to enable.
* **"The concept half gets D-060's treatment" understated the work** by assuming a
  single-open rule existed to partition against (§5.2).
* **It credited §5.3's option 2** ("compose when `recorded` is bound, then apply the
  valid predicate") as "sound if the predicate is a pure row filter, which
  `concept_visible_at` suggests it is". The predicate *is* a pure row filter — which
  is exactly why it cannot do the fold's job under Design B, since the snapshot has
  already discarded the rows the fold must choose among.
* **An earlier session's note on this document claimed finding AA of
  `Macrame Implementation Plan v0.5.6` as prior art for the concept gap.** It is not.
  AA is about links only — `trg_links_single_open`, `(source, target, edge_type)`,
  `query_as_of_edges` — and it was closed completely by D-060 in Wave 2.2, taking
  defect AG with it. It had no concept half. AA is the *precedent* this plan leans
  on, with a worked repro and a chosen implementation site, and it is not evidence
  that the concept gap was known.
