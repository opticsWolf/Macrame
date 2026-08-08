# Macrame Update Plan — v0.12.0

**From:** 0.11.0 (schema v10)
**To:** 0.12.0 (**schema v10 — no rung**)
**Source:** [Appendix C](architecture/appendices.md#named-for-0120-and-it-is-one-item), which names one item, opened by [D-143](architecture/s13-decision-register.md#d-143)
**Shape:** one write-actor change, its documentation consequences, and a test rewrite that is the point rather than the fallout.

---

## 0. What this release is, and what it deliberately is not

**It is:** the chunk loop stops sizing by a hand-fitted row count and starts sizing by how long the last chunk actually took. The four `chunk_rows` constants survive as **upper bounds** rather than as the criterion.

**It is not a performance release.** Bulk import does not get faster; on a populated database it gets slightly slower, because the loop converges toward a smaller chunk than 90 and [D-058](architecture/s13-decision-register.md#d-058) measured that smaller chunks cost total throughput. What it buys is that §5.1.5's rule stops depending on a constant fitted at one population, which is the defect [D-143](architecture/s13-decision-register.md#d-143) recorded.

**It is not preemption**, and §3 is the section that says so.

**No schema rung. No public API change.** Every type that changes is crate-private.

---

## 1. The structural fact everything follows from

**Chunking is caller-side.** `bulk_import` splits the batch and `low_chunked` (`src/connection.rs:1682`) sends chunks one at a time, awaiting each response before sending the next. The Write Actor receives an already-sized chunk and runs it as one transaction.

Two things follow, and both are load-bearing:

* There is **already a per-chunk feedback point** — the caller learns each chunk's outcome before it sends the next. Adaptive sizing needs no new plumbing, only a richer response.
* There is **no place inside the actor** where "stop adding rows" could mean anything. The rows arrived together.

A design that assumed the actor owned the loop would have put the clock in the wrong place. It does not.

---

## 2. Where the clock is read — **in the actor, around its own transaction**

`Instant::now()` before `BEGIN`, elapsed after `COMMIT`, returned with the row count.

**Not per row inside the transaction.** The actor has no rows left to decline (§1), and a clock read per row adds cost to the very path being bounded.

**Not on the caller side around `send + rx.await`.** That interval includes time spent queued behind high-priority commands, so a chunk that waited 40 ms behind a UI write would shrink itself for no reason — punishing low-priority work for exactly the strict preemption [D-010](architecture/s13-decision-register.md#d-010) gives it deliberately. This is the trap in the obvious implementation and it is why the measurement has to come from inside.

**One `Instant::now()` per transaction moves out of the `metrics` feature gate.** [D-079](architecture/s13-decision-register.md#d-079) already times per-command holds, but behind `metrics`, and sizing has to work in default builds. The clock read is unconditional from 0.12.0; the histogram stays gated.

---

## 3. What happens to a chunk in flight — **nothing; it commits in full**

This is the section a reader will come looking for, because "stop on elapsed time" reads as preemption.

§5.1.5's founding fact is that **SQLite's write lock is not preemptible**. A transaction that has begun can only be shortened by rolling it back, which converts a latency overshoot into a write failure — strictly worse than the overshoot. So the budget governs the **next** chunk.

**Time-based chunking is feedback, not preemption.** Three consequences, all to be stated in the rustdoc rather than discovered:

* **A single-chunk batch gets no protection at all.** The first chunk is sized by the ceiling, so the small-batch worst case is exactly what it is today.
* **Convergence takes one to two chunks.** A 50,000-row import overshoots on chunk 1 and is at its target from chunk 2 or 3.
* **The transient overshoot is bounded by the ceiling**, which is one of the two reasons the `chunk_rows` constants are kept rather than deleted. The other is §4.

---

## 4. Predictable transaction size — **given up, deliberately, and three things move**

### 4.1 `chunk_rows` means "maximum", not "the size"

It is public API. Its rustdoc changes from stating a size to stating a ceiling, and says what the loop does underneath. [D-014](architecture/s13-decision-register.md#d-014)'s `write_bulk_atomic` is untouched — caller-sized, one transaction, exempt by design and still the escape hatch for anyone who needs one stamp.

### 4.2 §5.1.6 gains a second sentence, and it is the real cost of this release

§5.1.6 already says each chunk is a distinct learning event and that `reconstruct(ts)` mid-write observes a prefix. What changes is that **the number and boundaries of those events become machine- and load-dependent**: the same input replayed produces a different `transaction_log` grouping, and "roughly 100 distinct stamps" becomes a figure that depends on the machine it ran on.

**Doctrine III and IV are undisturbed.** `seq_id` stays strictly monotonic, every `recorded_at` is honest about when the database learned those facts, and nothing is overwritten. What is lost is *reproducibility of the grouping*, which was never promised and was true by accident of a fixed constant.

It is written down because §5.1.6's own closing line is that **fidelity is a parameter, never a silent default** — and a boundary that moves with the machine is a fidelity property. A reader who needs a reproducible grouping is pointed at the atomic variant.

### 4.3 The test rewrite is the executable form of §5.1.6

`bulk_import_is_atomic_per_chunk_not_overall` (`tests/concurrency_tests.rs:290`) currently builds `chunk_rows::EDGES + 2` rows and asserts *exactly* `EDGES` survive a mid-batch failure. That assertion is about where the boundary falls, and the boundary is what stops being fixed.

**Rewritten to assert the property, not the position:**

* a non-empty **prefix** commits — `bulk_import` is not all-or-nothing;
* the chunk containing the failure rolls back **whole** — the good row sharing it leaves nothing;
* every committed chunk carries **one** `recorded_at` — a chunk is one transaction under one stamp;
* `audit_current` reports zero drift afterwards.

Kept and rewritten rather than deleted: it is the clearest existing statement of the trade a caller has to plan for, and a property test is a stronger form of it than a position test was.

---

## 5. The floor: **35 rows**, and why a floor exists at all

Without a floor the loop is honest to the letter of the rule and pathological in practice: on a database large enough, it converges toward single-row transactions, where [§5.1.5](architecture/s5-modules.md#515-cooperative-chunking--the-golden-rule)'s measured **~0.8 ms fixed cost** per transaction (BEGIN, COMMIT, fsync under `synchronous = NORMAL`) is nearly the whole budget and almost no work gets done.

**35 is chosen, and the arithmetic is stated so it can be checked and revised.**

| fixture | per-row | 35 rows | fixed-cost share |
|---|---|---|---|
| populated, 8,000 edges ([D-143](architecture/s13-decision-register.md#d-143)) | ~95 µs | **≈4.1 ms** | ~19% |
| empty ([D-058](architecture/s13-decision-register.md#d-058), marginal at n≈90) | ~22 µs | **≈1.6 ms** | ~51% |

**On a populated table 35 rows misses the 3 ms budget, in steady state, by ~1.1 ms.** That is a deliberate trade and the plan states it rather than implying convergence fixes it: the loop will sit at its floor on a large database and stay ~37% over. The alternative — floor at [D-143](architecture/s13-decision-register.md#d-143)'s measured 20 — meets the budget there and pays more fixed-cost overhead on every chunk of every bulk write, permanently, against [D-058](architecture/s13-decision-register.md#d-058)'s measurement that chunking already costs ~11% throughput on 1,000 edges.

**The latency argument survives the miss, which is what makes 35 defensible.** §5.1.5 justifies the 3 ms bound as "3 ms of queueing plus the interactive write's own ≤ 5 ms, inside a 60 Hz frame" — 8 ms against 16.7. At 4.1 ms of queueing that becomes **9.1 ms**, still inside the frame. The budget is a target the loop aims at, not a guarantee it can make, and §5.1.5 will say so in those words.

The floor is a named constant carrying its measurement in a comment, the same discipline `chunk_rows::EDGES` already follows, so a different machine is a comment revision rather than an archaeology exercise.

---

## 6. The response struct

```rust
/// What one chunk transaction cost, reported by the actor to the caller-side
/// chunk loop.
///
/// `held` is measured *inside* the actor, around the transaction, and therefore
/// excludes time the command spent queued — see §2 of the 0.12.0 plan for why
/// the caller cannot measure this for itself.
struct ChunkOutcome {
    rows: usize,
    held: std::time::Duration,
}
```

Crate-private, so no public API change. Every `LowPriCommand` chunk responder carries `Result<ChunkOutcome>` in place of `Result<usize>`; `low_chunked` sums `rows` and feeds `held` to the controller. Non-chunk commands are untouched.

---

## 7. The controller

A pure function, so it is testable without a database or a clock:

```rust
fn next_chunk_size(current: usize, held: Duration, budget: Duration,
                   floor: usize, ceiling: usize) -> usize
```

* **over budget** — shrink proportionally to the overshoot, with a margin: `current * budget / held`, scaled by 0.9. Proportional because the overshoot ratio is the best available estimate of how wrong the size is, and this is what makes convergence one or two chunks rather than a slow walk.
* **comfortably under** (`held < budget / 2`) — grow additively, capped at `ceiling`. Additive because growing is the direction that can breach the bound, and it is the direction with no urgency.
* **otherwise** — hold.
* Always clamped to `[floor, ceiling]`, where `ceiling` is the path's existing `chunk_rows` constant and `floor` is §5's 35.

Multiplicative-decrease / additive-increase, for the standard reason: back off fast from a bound you are exceeding, approach it slowly.

`low_chunked` changes from taking pre-split `Vec<C>` to taking the whole batch plus a ceiling, and splitting as it goes.

---

## 8. Work items

| # | Item |
|---|---|
| **W1** | `ChunkOutcome`; actor times its own transaction; the timing clock leaves the `metrics` gate. |
| **W2** | `next_chunk_size` as a pure function, with unit tests: converges from above in ≤2 steps, respects both clamps, is a no-op in the dead band, and never returns 0. |
| **W3** | `low_chunked` splits adaptively; the four bulk paths pass their constant as the ceiling. |
| **W4** | Rewrite `bulk_import_is_atomic_per_chunk_not_overall` per §4.3. |
| **W5** | Documentation: `chunk_rows` rustdoc (ceiling, not size), §5.1.5 (the bound is a target the loop aims at), §5.1.6 (machine-dependent boundaries), §9's chunk rows, quickref. |
| **W6** | Measure convergence on the [D-088](architecture/s13-decision-register.md#d-088) matrix — chunk sizes and holds for a 5,000-edge import per shape — and register the published figures in `perf_claim_tests` under `example:` evidence ([D-142](architecture/s13-decision-register.md#d-142)). |
| **W7** | Register entries; release note; version bump. |

**W2 before W3.** The controller is the only part with a right answer that can be checked without measurement, and a controller written inside the loop it drives is a controller nobody can test.

---

## 9. What must be true before this is called done

* Both supported suite configurations green through `scripts/run_rust_suite.py`, and the rustdoc gate (`--docs`) green — 0.10.0 shipped a release without it ([D-144](architecture/s13-decision-register.md#d-144)).
* `clippy --all-targets` clean under `-D warnings`.
* **W6 measured, not asserted.** [D-055](architecture/s13-decision-register.md#d-055) still rules out gating on absolute timings; the claim that the loop converges is a claim about *shape* — sizes settle and stop moving — and that is what gets published.
* The convergence figures name their fixture ([D-088](architecture/s13-decision-register.md#d-088)) and publish their control ([D-090](architecture/s13-decision-register.md#d-090)). [D-145](architecture/s13-decision-register.md#d-145) is three days old and its lesson is that a figure without its control beside it can be read backwards.

---

## 10. Rejected before starting

* **Clock inside the transaction, per row** — §2; the actor has no rows to decline and the read costs the path being measured.
* **Timing on the caller side** — §2; it measures the queue, and would shrink chunks as a punishment for correct preemption.
* **Aborting a chunk that overruns** — §3; the lock is not preemptible, and a rollback turns a latency miss into a write failure.
* **No floor** — §5; converges toward single-row transactions on a large database.
* **Floor at 20** — §5; meets the budget where 35 does not, and pays fixed-cost overhead on every chunk of every bulk write to do it.
* **Deleting the `chunk_rows` constants** — they are the ceiling that bounds the transient overshoot in §3, and the public surface callers already read.
* **Raising the 3 ms bound to what the path costs** — [D-143](architecture/s13-decision-register.md#d-143) rejected this already: the bound is a latency contract with the high-priority tier, not an observation about the write path.
* **Making chunk boundaries reproducible with a seed or a fixed-size mode** — a second sizing mode is a second thing to test and document, for a property §5.1.6 never promised; the atomic variant already serves the caller who needs one stamp.
