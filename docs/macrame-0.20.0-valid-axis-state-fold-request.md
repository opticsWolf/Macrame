# Request: a valid-axis state fold (`reconstruct` under `AsOf`)

**Status:** request, not a plan. Drafted 2026-10-29 against `main` at `5098722` (0.19.1) by the
CodeRadar side, which is the caller that needs it. Nothing here is built, and nothing here should be
built until §7's question is answered — the ask is narrow and the decisions underneath it are not.

**Requested by.** CodeRadar's P5 (DR-41, commit-driven ingestion): see
`CodeRadar/docs/temporal-p5-commit-ingestion.md` §2 and §5 P5b. CodeRadar is about to start writing
`valid_from` = git commit time while `recorded_at` stays the ingest stamp. Its reads then need a fold
on the axis it is writing, and it must not grow one of its own (§3).

---

## 1. The ask in one paragraph

`Database::reconstruct(ts)` folds the ledger on **`recorded_at`** — "state as believed at `ts`".
There is no counterpart that bounds the fold on the **valid** axis, so a caller holding a
`MaterializedState`-shaped question about world time has nothing to call. Every other read surface in
the crate already takes both instants. The request is to extend `reconstruct` with the same `AsOf`
the traversal already accepts, so that the state fold stops being the one surface that knows only one
clock.

---

## 2. What already exists (verified, not assumed)

This is the part that makes the request narrow rather than a new capability. Macrame's bitemporality
is largely built; one surface was left behind.

| Surface | Valid axis | Recorded axis | Where |
|---|---|---|---|
| `AsOf` itself | `valid` | `recorded`, plus `bitemporal(valid, recorded)` | `src/temporal/as_of.rs:18-48` |
| Edge reader | **yes** — "valid-time graph edges under current belief as of `ts`" | — | `src/temporal/as_of.rs:144,156` |
| Traversal / subgraph loader | **yes** — `as_of_valid`, reaching the walk's join against the `links_at_tx` fold | yes — `as_of_recorded`, belief-at-instant hydration | `src/graph/builder.rs:523`, [D-289](architecture/s13-decision-register.md#d-289), [D-290](architecture/s13-decision-register.md#d-290) |
| Concept visibility at an instant | **yes** — interval applied from the payload | yes | `src/temporal/as_of.rs:482-500` (`concept_visible_at`) |
| Plan cost of carrying both instants | measured and pinned | measured and pinned | `tests/bitemporal_plan_tests.rs:178-281` |
| **State fold (`MaterializedState`)** | **absent** | yes | `src/temporal/replay.rs:830`, `HOT_FOLD` at `:546` |

So the semantics a valid-axis fold needs are *already decided elsewhere in the crate*:
`concept_visible_at(payload, valid)` is exactly the per-row predicate, `valid_from` is already in the
log payload and already extractable (`json_extract(payload, '$.valid_from')` appears in the plan
gate's index candidate at `tests/bitemporal_plan_tests.rs:281`), and
[D-290](architecture/s13-decision-register.md#d-290) has already measured what adding a valid bound
does to a transaction-log range query.

What is missing is one fold and the three decisions in §5.

---

## 3. Why the caller cannot do this itself

CodeRadar's own rules forbid it, and they were written to protect this boundary rather than to defer
work: its **INV-6** puts every at-T answer in the engine, and its **F-6** forbids a second fold
implementation in shipped code, with the stated failure mode that two folds will diverge and the
Python-side one will be the wrong one. It has one standing exception — a measurement script
(`scripts/verify_history.py`) that replays the log and is imported by nothing.

A valid-axis fold written above the engine would also be wrong on its own terms: it would have to
re-derive snapshot composition and the archive reach check (§5.3), both of which live inside
`reconstruct` and neither of which is visible from outside the crate.

---

## 4. Proposed shape

Minimal, additive, and declining to touch the most-called reader in the crate — the same reasoning
`query_as_of_edges` used when it gained a lineage (`src/temporal/as_of.rs:152-156`):

```rust
// New. `AsOf` already carries both instants and already has a constructor
// for each case, so this signature adds a type, not a convention.
pub async fn reconstruct_as_of(&self, at: AsOf) -> Result<MaterializedState>;

// Unchanged, and defined in terms of the new one:
//   reconstruct(ts)         == reconstruct_as_of(AsOf::recorded_at(ts))
//   reconstruct_on(ts, br)  == the lineage-resolved form of the same
```

`AsOf { valid: Some, recorded: None }` asks "what was true at V, under current belief".
`AsOf::bitemporal(v, r)` asks "what did we believe at R about what was true at V" — the question
Doctrine VIII exists to make answerable, and the one a retroactive correction makes interesting.
`AsOf { valid: None, recorded: None }` is `now` on both, which is what `AsOf::now()` already means.

One fold implementation, parameterised — not a second one beside it. The crate's own stated reason
applies ([D-227](architecture/s13-decision-register.md#d-227)): "a read spelled twice drifts, and the
half nobody calls is the half that drifts first."

---

## 5. The three decisions this needs before any code

### 5.1 The tiebreak on the valid axis

`HOT_FOLD` picks one row per entity with
`ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC)` under
`WHERE recorded_at <= ?1` (`src/temporal/replay.rs:546-553`). `seq_id DESC` is insertion order, which
is the right tiebreak for "latest belief" and the wrong one for "what was true at V": two rows whose
valid intervals both contain V are not ordered by when they were written.

The question is what the winner is when more than one row's interval contains the instant. Candidate
answers, none free: the latest `valid_from` (reads as "the most recent truth about V", and is what a
bitemporal reader probably expects); the latest `seq_id` among containing rows (reads as "the most
recently stated truth about V", which is the bitemporal answer and collapses to the current one when
`recorded` is also bound); or refuse, on the grounds that §5.2 says it should not happen.

### 5.2 Overlapping valid intervals — guarded for edges, not for concepts

This is the decision the other two depend on, and it has a sharper answer than expected.

**Edges are guarded.** [D-060](architecture/s13-decision-register.md#d-060) refuses two closed
intervals for one `(source, target, edge_type)` that overlap in valid time, in the write actor
(`reject_overlapping_interval`, `src/connection.rs:6021`), precisely because
`query_as_of_edges` at an instant inside both "returned one relationship as two edges, which every
weighted algorithm downstream then double-counted". That is the same failure a valid-axis fold would
hit, found once already on the axis this request is about. So for edges, §5.1 can refuse rather than
choose: the state the tiebreak would adjudicate cannot be written.

**Concepts are not.** `reject_overlapping_interval` has exactly one call site and it takes an edge
(`src/connection.rs:5466`). `ConceptUpsert.valid_from` is caller-supplied
(`src/connection.rs:594`) and nothing compares it against the intervals already in the log for that
id — the `concepts` projection's `id NOT NULL UNIQUE` constrains the *current-state row*, not the
history behind it. Today that is harmless because the one caller who writes concepts at scale passes
`now`, so intervals arrive in order and never overlap by construction.

**P5 is precisely the caller that breaks that.** It writes `valid_from` = git commit time, out of any
relation to write order, for every version of every file in a repository's history. A re-ingested
range, a corrected parser, or two branches touching one path can each produce two log rows for one
concept id whose valid intervals overlap — and the first reader that would notice is the fold this
document is asking for.

So the honest shape of this decision is: **either the concept half gets D-060's treatment before the
valid-axis fold exists, or the fold must define §5.1's tiebreak and document that it is adjudicating
something the writer permits.** The first is the better answer by the crate's own precedent. The
second is a fold that silently picks between two contradictory truths, which is the class of silence
the doctrine is organised against.

### 5.3 Snapshots and the archive boundary

`reconstruct` composes onto `newest_usable_snapshot`, which picks by sequence and accepts a snapshot
whose `state.timestamp <= ts` (`src/temporal/replay.rs:1244-1257`). That timestamp is a **recorded**
instant: a snapshot is "state as believed at R", which is not "state valid at V", so a valid-axis
fold cannot reuse the existing snapshots and must not try. Three ways out, in increasing cost:
fold from genesis on the valid axis and say so in the rustdoc (correct, costs what the whole log
costs — the same sentence `reconstruct` already carries for the no-snapshot case); compose only when
`recorded` is bound and the snapshot's instant is at or before it, then apply the valid predicate to
the result (sound only if the predicate is a pure row filter, which `concept_visible_at` suggests it
is); or valid-axis snapshots of their own, which is a new artifact class and almost certainly not
worth it.

`hot_log_reach`'s `PredatesRecordedHistory` arm also needs a reading on the new axis: "nothing had
been recorded by `ts`" and "nothing was valid at `ts`" are different claims, and the second is
answerable from a ledger that was written entirely later — which is precisely CodeRadar's case, since
it will ingest a decade of commit time in one afternoon of recorded time.

---

## 6. Cost, and what the plan gates should be asked

[D-290](architecture/s13-decision-register.md#d-290) re-ran the D-196/D-254 gates when the concepts
fold became a new consumer of `transaction_log` ranges, and found the transaction-time triple moving
`(4, 3, 5)` → `(4, 4, 9)` while "the valid-time control and the cross-axis equality did not move". A
valid-axis fold is a second new consumer of those ranges and should be measured the same way rather
than blessed: the expectation to test is that the valid bound reaches the plan *here* even though
`tests/bitemporal_plan_tests.rs:191` pins that it does not reach the traversal's plan, because this
fold reads the log directly rather than joining a materialised fold. If that expectation is wrong,
the index candidate at `:281` — `(recorded_at, json_extract(payload, '$.valid_from'))`, whose second
column D-290 found dead — becomes live for the first time, and the arithmetic that rejected it should
be re-run rather than inherited.

---

## 7. The question for the maintainer

**Is this wanted at all, and is it wanted now?** The honest answer from the requesting side is that
it is not urgent. CodeRadar's P5a ships without it by enforcing that ingest only ever moves forward,
so recorded order equals commit topological order and today's `reconstruct` answers correctly by
construction. What P5a cannot do is accept history out of order — backfilling an old branch,
ingesting a second remote, re-ingesting a range after a parser improvement — and each of those is a
real future need rather than a present one.

So the request is filed, not pressed. If the answer is "when someone has the out-of-order case in
hand", that is the right answer and this document is the record of what it will need.

---

## 8. Proposed register row (draft)

> `<a id="d-291"></a>`**D-291 — `reconstruct` under `AsOf`: a valid-axis state fold (0.20.0).**
> [D-289](architecture/s13-decision-register.md#d-289),
> [D-290](architecture/s13-decision-register.md#d-290) (the measured precedent for a valid bound on a
> `transaction_log` range), [D-227](architecture/s13-decision-register.md#d-227) (one lowering, not
> two), [D-285](architecture/s13-decision-register.md#d-285) (payload ceiling),
> [D-049](architecture/s13-decision-register.md#d-049) (snapshot composition, which this axis cannot
> reuse). Evidence: *(to be filled by the implementation)*.
>
> The state fold was the one read surface that knew a single clock. `AsOf` already carried both
> instants and the traversal already took them, so this is the fold catching up with the vocabulary
> rather than new bitemporality. Requested by CodeRadar P5/DR-41, which writes `valid_from` = git
> commit time and cannot fold on the axis it writes without a second fold implementation its own F-6
> forbids.
>
> Carries with it: the concept half of
> [D-060](architecture/s13-decision-register.md#d-060) (§5.2 — `reject_overlapping_interval` guards
> edges only, and a caller-supplied `valid_from` makes overlapping concept intervals writable).
>
> Rejected: *a valid-axis `reconstruct` beside the recorded one* (two folds drift — D-227);
> *valid-axis snapshots* (a new artifact class for a read that can fold from genesis and say so);
> *a caller-side fold* (F-6 on the CodeRadar side, and it would have to re-derive snapshot
> composition and the archive reach check, neither visible from outside the crate).

---

## 9. What the requester does meanwhile

P5a, forward-only, with the refusal recorded as an invariant rather than a warning. If this request
is declined, nothing in CodeRadar breaks; what stays impossible is retroactive ingestion, and that
stays impossible *loudly* — the ingest refuses the commit rather than writing it and letting the
reads sort it out.
