# Macrame — the road to 1.0

Two releases, 0.13.0 and 0.14.0, and then the version number stops being a
promise about the future.

Source: `docs/Macrame Codebase Review v0.12.0.md` (27 findings) plus three
findings raised after it was written and recorded here for the first time
(§0.3). Every one of the 30 appears exactly once in the coverage table at §1,
against the wave that closes it. Nothing is listed as "later".

---

## 0. What this is, and what it deliberately is not

**It is** the full inventory. The review diagnosed; this prescribes, and it
prescribes for all of it. The organising constraint is the one the user set:
no critical item gets pushed past 1.0 on the grounds that the release was
getting large.

**It is not** a promise that 0.13.0 and 0.14.0 are equal in size. They are not.
0.13.0 is the larger and the riskier, because it holds everything that must
happen while the version number still permits it.

**It is not** a licence to add surface. Every item below either closes a finding
or is a prerequisite for one that does. Two of the review's own recommendations
are reversed here after the paradigm filter, and are recorded in §16 with the
reason rather than quietly dropped.

### 0.1 The split, and why it is this one

The line is not severity and not size. It is this:

> **0.13.0 changes what you can observe and control. 0.14.0 changes what is
> guaranteed.**

That reads as a slogan; it is actually a dependency argument, and it is the
reason the order is not negotiable:

- **The gates cannot be built before the things they gate.** A performance
  regression gate (§5.3) calibrated before the two missing indexes land is
  calibrated against numbers that are about to move by an order of magnitude.
  The indexes go first; the gate goes second.
- **CI design depends on R15's answer.** Whether the generated-history binaries
  can ever rejoin the main suite decides what 0.14.0's CI work even looks like.
  So R15 is measured in 0.13.0 — first — even though its fix, if there is one,
  may land later.
- **`ANALYZE` invalidates the plan-pinning fixture.** Not gradually. The moment
  statistics exist in production and not in the test, the best gate in the repo
  is silently testing a planner nobody runs.
- **`#[non_exhaustive]` blocks every later metric.** The starvation counter
  (§4.1) is a new `MetricsSnapshot` field, and adding a field to an exhaustively
  matchable struct is a break. Harden the surface, then add to it.
- **Documented semantics must precede the API break that encodes them.** `as_of`
  mixes two time axes (§3.1). 0.13.0 documents what it does; 0.14.0 splits the
  parameter. Doing it in one step means shipping a break whose correct shape was
  argued for in the same commit that made it.

### 0.2 Checked and already covered — recorded so they are not re-raised

- **`ANALYZE` needs no migration work.** `verify` checks presence by name and
  already argues for tolerating extra objects
  ([migrations.rs:866](../src/schema/migrations.rs:866)); `refuse_if_occupied`
  excludes `sqlite_%` ([migrations.rs:240](../src/schema/migrations.rs:240)),
  which `sqlite_stat1` matches. No rung, no schema version bump.
- **`sqlite_stat1` is Doctrine VI state.** Derived, rebuildable by re-running the
  command, carrying no assertions. Doctrines III and V do not reach it.
- **`#[non_exhaustive]` does not break the Python bindings.** It blocks
  downstream struct-literal construction and exhaustive patterns, not field
  reads, and `macrame-py` never constructs `MetricsSnapshot` or `KindSnapshot`.
- **Python already has the whole metrics surface**, and in the safer shape —
  `buckets()` is a method there ([observe.rs:108](../bindings/python/src/observe.rs:108))
  and the wheel builds with `--features metrics` unconditionally (D-093). The
  asymmetry runs the other way: the default Rust build has neither.
- **`synchronous = NORMAL` in WAL already skips the per-commit fsync.** There is
  no durability knob to win here, which is half of why `Durability` is rejected
  in §16.
- **`ANALYZE` does not help the vector filter planner**, and the crate says so at
  [vector_filter.rs:73](../src/graph/vector_filter.rs:73). That path measures
  with a capped probe because average rows-per-key cannot estimate multi-hop
  reachability. Nothing below claims otherwise.

### 0.3 Three findings not in the 0.12.0 review

Raised after it was written. Recorded here in full so the review and this plan
together are complete.

**F-28 — the planner has never had statistics, in any database Macrame has
created.** `ANALYZE` and `PRAGMA optimize` appear nowhere in `src/`.
`sqlite_stat1` therefore never exists, and SQLite costs every plan against
built-in defaults: assume ~1M rows, assume each bound equality column divides by
ten. That estimate is *structural* — it is a function of how many columns a
query binds, not of what the table holds.

Which is a restatement of the crate's own worst bug class. From
[index_plan_tests.rs:3](../tests/index_plan_tests.rs:3): *"a covering index
captures a query because it contains the columns, not because it
discriminates."* D-042, D-059 and D-064 are three instances of a planner doing
the only thing available to it. Two indexes lead on `source_id`
([ddl.rs:498](../src/schema/ddl.rs:498),
[ddl.rs:524](../src/schema/ddl.rs:524)) and are separated today by column count
alone. D-059's own numbers — 4.4 ms into an empty table, 1.06 s into a
90,000-edge hub — are what a wrong selectivity guess costs on a skewed graph,
and a code graph is maximally skewed. **Severity: High**, because it is the
generator of a bug class the crate has already paid for three times.

**F-29 — the plan-pinning fixture has no rows and no statistics.**
`every_justified_index_is_the_one_the_planner_picks` runs against
`migrated(&harness)` — a freshly migrated, entirely empty database
([index_plan_tests.rs:123](../tests/index_plan_tests.rs:123)).

Today that is *faithful*, because production has no statistics either. It stops
being faithful the moment F-28 is fixed, and it stops silently: the test still
passes, still asserts a plan, and no longer asserts anything about the planner
that runs in production. **Severity: High once W2 lands, harmless until then** —
which is precisely why the two are one wave and not two.

**F-30 — autocheckpoint is an unbudgeted hold inside the adaptive
controller.** `configure()` ([connection.rs:1974](../src/connection.rs:1974))
never sets `wal_autocheckpoint`, so SQLite's 1,000-page default stands. A
checkpoint firing inside a chunk transaction is attributed to that chunk: 0.12.0
measures `outcome.held` and feeds it to `next_chunk_size`
([connection.rs:1843](../src/connection.rs:1843)), so the controller shrinks the
chunk in response to work the chunk did not do, then grows it back once the
checkpoint is behind it. The controller oscillates against an exogenous signal.
**Severity: Medium**, and it is the one finding that only exists *because* of
0.12.0 — D-146 made hold time a control input, which turned a background cost
into a feedback-loop input.

---

## 1. Coverage: every finding, and the wave that closes it

| # | Finding | Sev | Wave | Release |
|---|---|---|---|---|
| 2.1 | `links` has no explicit index — four full scans, one per `open()` | High | W3.1 | 0.13.0 |
| 2.2 | `CONCEPTS_ARCHIVABLE` quadratic on `links.target_id` | High | W3.2 | 0.13.0 |
| 2.3 | Per-transaction overhead ~0.8 ms; singular paths pay it per row | Med | W3.4 | 0.13.0 |
| 2.4 | Snapshot work runs on a tokio worker | Med | W8.1 | 0.14.0 |
| 2.5 | `Subgraph` string-keyed adjacency | Low | W10.3 | 0.14.0 |
| 2.6 | `reject_overlaps_within` O(n²) | Med | W7.5 | 0.14.0 |
| 3.1 | `as_of` mixes valid time and transaction time | High | W5.6 / W7.1 | both |
| 3.2 | `AtTime` degrades silently after archive | Med | W9.1 | 0.14.0 |
| 3.3 | Snapshot loader unbounded | Med | W8.2 | 0.14.0 |
| 3.4 | Future `recorded_at` poisons the clock floor | Med | W7.4 | 0.14.0 |
| 3.5 | `run_writer_actor` cannot return `Err` | Low | W7.3 | 0.14.0 |
| 3.6 | `write_annotations_atomic` bypasses `classify` | Med | W7.2 | 0.14.0 |
| 3.7 | Snapshot rename atomic but not durable | Med | W8.3 | 0.14.0 |
| 4.1 | No anti-starvation floor on low-priority work | Med | W4.4 (counter), W10.4 (the floor itself) | 0.13.0 / 0.14.0 |
| 4.2 | No cancellation or progress on bulk paths | Med | W7.6 | 0.14.0 |
| 4.3 | `metrics` off by default | High | W4.5 | 0.13.0 |
| 4.4 | Metrics surface frozen by accident | High | W4.2, W4.3 | 0.13.0 |
| 4.5 | No WAL / checkpoint surface | Med | W5.2 | 0.13.0 |
| 4.6 | Six gaps in the Python surface | Med | W6 | 0.13.0 |
| 4.7 | `Database` is not `Clone` | Low | W11.1 | 0.14.0 |
| 5.1 | R15 reaches the main suite | High | W1 | 0.13.0 |
| 5.2 | Index registry is one-directional | Med | W2.3 | 0.13.0 |
| 5.3 | No performance regression detection | Med | W10.1 | 0.14.0 |
| 5.4 | No snapshot fuzzing | Low | W8.4 | 0.14.0 |
| 6.1 | Release history table stops at 0.9.0 | Low | W11.3 | 0.14.0 |
| 6.2 | `Cargo.toml` metrics cost model is false | Med | W4.1 | 0.13.0 |
| 6.3 | Comment-to-code ratio | — | W11.4 | 0.14.0 |
| F-28 | No `ANALYZE`; planner runs on default selectivity | High | W2.1, W2.2 | 0.13.0 |
| F-29 | Plan-pinning fixture has no rows and no statistics | High | W2.4 | 0.13.0 |
| F-30 | Autocheckpoint perturbs the chunk controller | Med | W5.3 | 0.13.0 |

Nine High-severity findings. **Eight close in 0.13.0.** The ninth, §3.1, is
documented in 0.13.0 (W5.6) and broken correctly in 0.14.0 (W7.1) — see §0.1 for
why that order and not the reverse.

---

# v0.13.0 — what you can observe and control

Six waves. The release where the pre-1.0 window is spent deliberately rather
than allowed to close.

---

## 2. W1 — Settle R15 before anything depends on the answer

The review's highest-value single item, and it goes first because 0.14.0's CI
work cannot be designed without its result.

The standing mitigation is two-part: `RUST_TEST_THREADS = "1"` in
`.cargo/config.toml`, plus quarantining the generated-history binaries behind
`property-tests`. `Cargo.toml`'s own feature comment records the residue —
`doctrine_property_tests` still faults often enough *serialised* to be unusable
as a gate — and `.cargo/config.toml` records a 93/100 crash rate for the
quarantined step under sustained load. D-147 established that a flaky test had
been biasing that rate downward.

**W1.1 — Reproduce under control. (done, 0.12.1)** Rather than a new harness,
`examples/r15_soak.rs` gained a third arm: `storm` runs the open storm alone —
no soak database, no readers, no actor — with each candidate variable behind its
own flag. Measured at `--opens 48 --secs 2 --runs 6`, debug, reference machine:

Read the counts as **eliminated / not eliminated**, never as rates: D-124
measured this instrument's noise band at ~30 points at n = 20, and these are
n = 6–10. The one row that clears that bar clears it on a 100× volume margin,
not on its fault count.

| configuration | faults | eliminated? |
|---|---|---|
| `--first-use build` | 0/6 | **yes** — at ~880,000 opens per run |
| `--first-use connect` | 6/6 | no — the fault needs `connect()` |
| `--first-use query` | 6/6 | no |
| `--serial-opens` | 6/6 | no — serialising `build()` |
| `--serial-connect` | 5/6 | no — serialising `connect()` |
| `--hold` | 6/6 | no — serialising teardown |
| `--sequential` | 4/6 | no — removing overlap |
| **`--sequential --current-thread`, release** | **2/10** | no — removing overlap *and* migration |

**W1.2 — The sequential-open hypothesis is refuted. (done, 0.12.1)** The plan
said to measure it at harness level and not to put a mutex in `open()` on a
hypothesis. That instruction paid for itself immediately: serialising `build()`
changes nothing, serialising `connect()` changes nothing worth having, and
**`--sequential --current-thread` — one task, one OS thread, no overlap and no
worker migration — still faults 2/10 in release**, with its clean runs reaching
9,792–10,416 opens.

That last arm also matters because the risk row had listed the multi-thread
runtime as the concurrency it *could not locate* since 0.8.0. It is not the
mechanism either.

**R15 is not a concurrency bug**, in any of the three available senses —
simultaneity, thread migration, or teardown. What survives every arm is
cumulative `connect()` volume: `build()` alone is clean at ~880,000 opens per
run, and adding `connect()` kills the process within a few thousand however the
work is spread across threads and time.

**The old diagnosis survived six releases because one row of its reproducer
stopped at 500.** The concurrency sweep was sound; the single sequential control
was three orders of magnitude too short, and every downstream document inherited
its conclusion.

Two consequences, both load-bearing:

- **`RUST_TEST_THREADS = "1"` is not the mechanism it is documented as.** It
  serialises, and serialising does not help. It plausibly lowers
  connections-per-run enough to reduce the rate, which is a different claim from
  the one `.cargo/config.toml` makes, and the 93/100 figure is consistent with a
  volume threshold rather than a race. **No mitigation changes** — the setting
  works, and only its recorded reason was wrong.
- **It discharges an anomaly open since 0.5.6.** `.cargo/config.toml` recorded,
  unexplained, that every binary is 0/15 alone while the serialised suite
  running those same binaries is 5/10, and named `Database::open`'s three
  connections as a suspect it could not evidence. No hidden concurrency is
  needed: the suite makes far more cumulative `connect()` calls than any one
  binary. The same reading covers D-147's five irreconcilable rates — five
  machines running the same work at five speeds is five totals per unit time.
- **The production claim gets stronger.** "One process, one file, a bounded set
  of connections opened once and never churned" is exactly the shape that never
  accumulates `connect()` calls. The exposed party is the test suite, which
  opens thousands of databases per run.

**W1.2a — Fixed total, or a rate?** The one question the matrix does not settle,
and the one that decides whether this is reportable upstream as a leak with a
number attached. Run `--sequential` across several `--opens` and `--secs` and
look for a constant cumulative `connect()` count rather than a constant
duration. Cheap, and it is now the only thing standing between this and a
minimal upstream reproducer.

**W1.3 — Report upstream regardless of outcome.** An access violation in libSQL
0.9.30 is upstream's bug whether or not Macrame can route around it. A minimal
reproducer from W1.1 is worth more to them than a bug report, and worth more to
us than carrying an undiagnosed mitigation to 1.0.

**W1.4 — Record the result as a decision.** Three outcomes, all acceptable, none
silent: mitigation found and quarantine lifted; mitigation found and quarantine
kept for a different reason; or no mitigation, quarantine permanent, and 1.0
ships saying so in the open. **The failure mode this wave exists to prevent is a
fourth: 1.0 ships and nobody re-asked.**

**Timebox: one week.** If W1.1 has not reproduced it under control in a week,
stop, record what was tried, and treat the quarantine as permanent for 1.0. This
is the one wave allowed to end in "we do not know", and it must not be allowed
to eat the release.

---

## 3. W2 — Statistics, and the planner that has never had any

Closes F-28 and F-29. These are one wave because the second is created by the
first.

**W2.1 — `ANALYZE` becomes a budgeted command.** `PRAGMA analysis_limit = 400`
first, then `ANALYZE`, then reset. The limit is what makes the cost roughly
constant instead of proportional to `links_current`, and constant is the only
version of this that can share a write actor with a 3 ms budget.

It is a write. It goes through the actor as `LowPriCommand::Analyze` and gets
its own `CommandKind` — which is free to add here only because W4.2 lands in the
same release; see §0.1.

```rust
/// Refresh the query planner's statistics.
///
/// Bounded by `PRAGMA analysis_limit`, so the hold is a function of the
/// index count and not of the table size. Runs as low-priority work: it is
/// never more urgent than a caller's write.
pub async fn analyze(&self) -> Result<()>;
```

**W2.2 — `PRAGMA optimize` on close, and after bulk load.** The incremental
form: SQLite re-analyses only tables whose statistics it believes are stale, and
is a no-op otherwise. Two call sites — `Database::close`, and at the end of
`write_concepts` / the bulk edge path when a run exceeds a threshold worth
picking by measurement rather than by taste.

**W2.3 — The registry gets a second direction.** Closes §5.2. Today
`index_plan_tests` is keyed by index: every index names a query. The inverse
hole is a query that quietly leaves its index — caught only where somebody wrote
a reactive assertion. Add a query-keyed section covering, at minimum,
`CONCEPTS_ARCHIVABLE`, `LINKS_ARCHIVABLE`, `recorded_at_floor`, and the two new
indexes from W3. Same `include_str!` bounding as the existing entries.

**W2.4 — The fixture gets rows and statistics.** Closes F-29. The plan-pinning
tests must run against a populated, analysed database, because that is what
W2.1 makes production. Concretely: a shared fixture with skewed out-degree — a
few thousand-edge hubs among many two-edge leaves, because uniform data is
exactly the case where statistics and defaults agree and the test proves
nothing.

**Keep an empty-database case as well, explicitly labelled.** A fresh database
before its first `ANALYZE` is a real state Macrame will be in, and the plans it
gets are real plans. Two fixtures, both asserted.

**W2.5 — Record D-149 and D-150.** That `ANALYZE` is bounded-by-construction and
scheduled as ordinary low-priority work (**D-149, done in 0.12.4**); and that plan
pinning is only meaningful against a fixture whose statistics match production's.

*Renumbered:* D-148 went to W1's R15 finding, so every allocation below this line
moves up by one. The register is the authority, not this plan.

---

## 4. W3 — The two indexes, and the query that needs one of them

**W3.1 — `CREATE INDEX idx_links_recorded_at ON links (recorded_at)`.** Closes
§2.1. Four full scans today, including one on every `open()` via
`recorded_at_floor` ([clock.rs:36](../src/util/clock.rs:36)). The `links` table
has a primary key and nothing else; its archive and clock queries both seek on
`recorded_at`.

> **Done, 0.12.6 (D-151), and the justification is narrower than written above.**
> The clock floor is **not** one of the scans — D-150 measured it as a covering
> index seek before this index and it stays one after, so the paragraph's lead
> example was wrong and is corrected in the review §2.1 in place. What the index
> does close is the archiving `SELECT` and `DELETE`, both of which went
> `SCAN links` → `SEARCH links USING INDEX idx_links_recorded_at`.

**W3.2 — `CREATE INDEX idx_links_target ON links (target_id)`.** Closes §2.2.
`CONCEPTS_ARCHIVABLE` ([archive.rs:199](../src/temporal/archive.rs:199)) carries
`OR links.target_id = concepts.id` with nothing to seek on, which is a scan of
`links` per candidate concept.

> **Done, 0.12.6 (D-151).** `SCAN links USING COVERING INDEX` → `MULTI-INDEX OR`
> with both arms seeking, on the `SELECT` and the `DELETE` alike. Shipped with
> W3.1 as the `v10 → v11` rung `links-archive-indices` — the first rung to index
> a frozen ledger table, which is the additive case D-036 named.

**W3.3 — Re-examine the `OR` after measuring.** An index makes each side
seekable; SQLite may still decline to use two indexes for an `OR` inside a
correlated subquery. If `EXPLAIN QUERY PLAN` still shows a scan after W3.2,
rewrite as a `UNION` of two seekable halves. **Measure before rewriting** — the
rewrite is only justified if the index alone did not do it.

> **Done, 0.12.6 — measured, and the rewrite is not taken (D-151).** SQLite does
> not decline: with `idx_links_target` the subquery plans as `MULTI-INDEX OR`
> with both arms seeking. The `UNION` form was measured anyway rather than
> assumed unnecessary, and it is worse-shaped — before the index existed its
> right half made SQLite build an `AUTOMATIC COVERING INDEX (target_id=?)` at
> query time, which is the planner saying it wanted this index all along.
> `CONCEPTS_ARCHIVABLE` is unchanged.

**W3.4 — Point the singular paths at the bulk ones.** Closes §2.3.
`upsert_concept` and `assert_edge` each pay the ~0.8 ms per-transaction floor for
one row. Documentation, not deprecation: the singular forms are the correct API
for a caller who genuinely has one row, and CodeRadar's original assessment
reached for a new bulk API precisely because it did not find the existing one.

> **Done, 0.12.7.** `assert_edge` and `upsert_concept` now state the floor and
> name their bulk equivalents; Appendix A carries the same note at the point the
> singular calls appear. Writing it turned up a distinction worth having made
> explicit: **`bulk_import` is chunked and `write_bulk_atomic` is not**, and the
> first draft of the `assert_edge` note attributed the unbounded hold to
> `bulk_import`. A caller told only "use the bulk one" can pick the stalling one
> for a batch that had no atomicity requirement, so the note names both and says
> which is which.

**W3.5 — Registry entries for both.** Not optional, and not a follow-up:
`every_index_is_justified` fails if an index is declared without one. The gate
already forces this.

> **Done, 0.12.6.** Both carry `Justification::Query` with the reproduced SQL and
> an `include_str!` bound against `archive.rs`. The three `QUERY_REGISTRY`
> entries 0.12.5 left recording defects were updated with the measured new plans,
> which is the review step they existed to force. A second gate fired as
> designed: `a_version_bump_must_bring_its_own_rung_test` went red on the
> `SCHEMA_VERSION` bump, and the rung test written for it asserts the *plan*
> rather than the index's presence.

---

## 5. W4 — The metrics surface, frozen while freezing is still free

Closes §4.4, §4.3, §4.1, §6.2. Five steps in a fixed order; the ordering is the
whole point.

**W4.1 — Correct the `Cargo.toml` cost paragraph first.** Closes §6.2. It
currently reads:

> with the feature off `HoldTimer` reads no clock and `ActorMetrics` is a ZST
> whose methods are empty, so the actor loop compiles to what it compiled to
> before

0.12.0's W1 made the clock unconditional —
[metrics.rs:179](../src/metrics.rs:179) says so directly. The paragraph is the
stated justification for the off-by-default decision, and it is false. Fix it
before deciding anything that leans on it.

> **Done, 0.12.8.** Corrected in place with the old text quoted, rather than
> rewritten silently: the false half was the stated justification for the
> off-by-default decision that **W4.5 is about to re-examine**, and a
> justification that stood wrong for six releases should be visible to whoever
> re-opens it. The honest split is now stated as a two-row table — the clock is
> paid unconditionally in every build, and the histogram is what the feature
> buys.

**W4.2 — `#[non_exhaustive]` and a private `buckets`.** Four lines and one:
`#[non_exhaustive]` on `CommandKind`, `MetricsSnapshot`, `KindSnapshot`; and
`pub buckets: [u64; BUCKET_COUNT]` becomes `pub fn buckets(&self) -> &[u64]`,
matching what Python already does. Plus a comment on `index()`
([metrics.rs:99](../src/metrics.rs:99)) recording that declaration order is a
persisted contract — the one break the compiler cannot catch, and it binds
Python too, since `BUCKET_BOUNDS_MICROS` is a module constant there.

Free today. Impossible after 1.0. Everything else in this wave depends on it.

> **Done, 0.12.8.** All three types carry `#[non_exhaustive]`; `buckets` is
> private behind `KindSnapshot::buckets(&self) -> &[u64]`, with the Python
> getter switched to the accessor — it was the field's only external reader.
> The `index()` note states the rule that makes the contract keepable rather
> than only naming it: **new variants go at the end**, of the enum and of
> `ALL`. `#[repr(u8)]` pins the discriminants to the declaration order but
> pins them to whatever that order *is*, so it does not make a reorder safe.
> 362 passed across 49 targets with `--features metrics`.

**W4.3 — `Rehydrate` gets its own `CommandKind`.** Closes the live
mis-attribution. [connection.rs:2325](../src/connection.rs:2325) states the
compromise outright — rehydration reports as `Archive` because a variant
addition was a break — so rehydrate holds are attributed to the wrong command in
both languages today. Additive for Python, where `kind` is already a string.
This is the evidence that §4.4 is a real constraint and not a hypothetical: the
codebase has already paid it once.

> **Done, 0.12.9.** The variant is appended at the end per W4.2's rule, and
> `as_str()` gives Python `"rehydrate"` with no binding change — the end-to-end
> Python assertion is a subset check, so it needed no edit.
>
> **Splitting it out would have silently broken the violation counter.**
> `Rehydrate` inherited `Archive`'s budget *exemption* along with its kind, so
> nobody had ever decided it; on its own variant it would have become
> non-exempt, and since a rehydrate is one unchunked transaction moving rows
> back across the file boundary, every one would have counted as a violation —
> making `violations()` useless on any database that rehydrates, which is the
> exact failure the `Archive` exemption exists to prevent, arriving through a
> change made for attribution. It is exempt, now on the merits and on the record.
>
> That in turn exposed the exemption list existing **twice** — in
> `exempt_from_budget` and in `CHUNK_BUDGET`'s rustdoc table — and being one row
> apart, with the documented scope narrower than the enforced one for three
> releases. `the_budget_exemptions_and_their_documented_table_agree` now asserts
> both directions. Plus `a_rehydrate_is_counted_as_rehydrate_and_not_as_archive`,
> whose `Archive` half is the load-bearing one.

**W4.4 — The starvation counter.** Closes §4.1. The actor's `biased` select
([connection.rs:2035](../src/connection.rs:2035)) has no floor: sustained
high-priority traffic can starve low-priority work indefinitely. Count
consecutive high-priority turns taken while low-priority work was queued, expose
it as a new `MetricsSnapshot` field plus one `#[getter]` in `observe.rs`.

**Measure before adding a policy.** Whether a forced yield is needed is a
question the counter answers; adding one now would be fixing a bound nobody has
observed being hit.

> **Done, 0.12.10 (D-153) — and the first measurement hits the bound completely.**
> A 39-edge chunked `bulk_import` raced by 64 concurrent `upsert_concept` calls:
> `starved_turns=63`, `run_max=63`, `turns=70`, identical across five runs. The
> run **equals** the total — the bulk import sat behind every single queued
> high-priority write, with no interleaving at all. The run length is bounded by
> nothing in the crate, only by how much high-priority work the caller offers.
>
> The quiet half is asserted too: a sequential caller queues nothing and reports
> `run_max == 0`, which is what stops the counter reporting starvation on every
> database forever.
>
> **This falsifies the stated premise of §16's rejection of the forced-yield
> policy** — "whether the bound is ever hit in practice is a measurement nobody
> has taken". It has now been taken. What remains open is a judgement rather
> than a measurement: whether a synthetic 64-task burst is evidence about
> production or only about the mechanism. No policy is added here, per this
> wave's own instruction; see §16, which is annotated rather than rewritten.

**W4.5 — `metrics` becomes a default feature.** Closes §4.3, and only now. The
cost is 10–11 relaxed atomics per turn, about 0.01% of the 0.8 ms
per-transaction floor, and ~1.6 KB of counters. The argument is not that it is
cheap; it is that a crate organised entirely around a latency bound ships a
default build that cannot report whether the bound is met. D-093 already made
this argument for the Python wheel and won it. `violations()` is the answer to
the only question the design asks.

Optionally settle the cost empirically first with `write_path` / `chunk_commit`
against their `control/select_1` rows (D-090). Expect it to be unresolvable
under D-070's ~29% session noise — **and treat that as the result**: a cost that
cannot be distinguished from noise by the crate's own benchmark harness is not a
cost worth defaulting off for.

> **Done, 0.12.11 (D-154). The prediction held, but only after the naive
> experiment gave the opposite answer.** One run per arm reported metrics-off as
> 9.0% faster on `assert_edge` and 16.0% faster on `upsert_concept`, both
> `p < 0.05`, with criterion printing *"Performance has improved."* Re-run
> alternating — three rounds under load, then six rounds each on a settled
> machine — **the sign flips between rounds on every row**, every effect is under
> 0.3 sd, and the two write paths are indistinguishable from `control/select_1`,
> which runs no metrics code at all. Quiet-machine bound: **under 0.2% of a
> write, ~0.4 µs on a 246 µs `assert_edge`, consistent with zero**, agreeing with
> the arithmetic that 14 relaxed atomics is ~0.01%. Unresolvable, as predicted —
> and now bounded, which is better.
>
> The atomic count is **11–14 per turn**, not the 10–11 written above: W4.4's
> counter landed in between.
>
> The methodological half is recorded in D-154 because it will recur: criterion's
> `change:` compares against the last *saved* baseline, i.e. a different session,
> and its p-value is computed from within-session variance — so it is
> structurally blind to the between-session confound and will happily certify
> drift as an improvement. This is D-124's retraction arriving from the other
> direction, where the noise argued *for* the decision, which is the harder case
> to notice. Any A/B across a rebuild must alternate arms and read the control
> row first.

**W4.6 — Record D-152 and D-153.**

> **Done, 0.12.10.** Both written. D-152 was nearly missed: seven references to
> it existed across `metrics.rs`, `connection.rs` and `actor_metrics_tests.rs`
> from W4.3's commit, pointing at an entry that had never been written. Nothing
> catches that — `doc_link_tests` checks anchors *between documents* and a
> `D-nnn` in a Rust comment is not a link. Worth knowing: the register can be
> referenced into existence by code and stay empty.

---

## 6. W5 — Tuning, checkpoints, and the WAL

Closes §4.5, F-30, and the constructor sprawl nobody has yet called a finding.

**W5.1 — `Tuning` absorbs the `open*` family.** There are already three
constructors — `open` ([connection.rs:662](../src/connection.rs:662)),
`open_with_cadence` ([connection.rs:673](../src/connection.rs:673)),
`open_with_clock` ([connection.rs:700](../src/connection.rs:700)) — and every
new knob adds a fourth. This is the last chance to consolidate without breaking
anyone twice.

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Tuning {
    /// Pages between automatic WAL checkpoints. `None` disables
    /// autocheckpoint, which is only correct if you call `checkpoint()`.
    pub wal_autocheckpoint: Option<u32>,
    /// Page cache for the write connection.
    pub writer_cache_size: Option<i32>,
    /// Page cache for diagnostic (read-only) connections.
    pub reader_cache_size: Option<i32>,
    pub cadence: Option<SnapshotCadence>,
    pub clock: Option<Arc<dyn Clock>>,
}

impl Database {
    pub async fn open_tuned(path: impl AsRef<Path>, tuning: Tuning) -> Result<Self>;
}
```

`#[non_exhaustive]` and `Default` together mean adding a knob later is additive:
callers write `Tuning { writer_cache_size: Some(-64_000), ..Default::default() }`.
The three existing constructors stay, delegating, and are documented as the
convenience forms they are. **No deprecation in 0.13.0** — `open(path)` is the
right call for most callers and should not acquire a warning.

> **Done, 0.12.12 (D-155). Both attributes specified above turned out to be
> wrong, in opposite ways.**
>
> `#[non_exhaustive]` on the **struct** does not compile: a non-exhaustive
> struct cannot be built with literal syntax outside its own crate at all, and
> `..Default::default()` *is* literal syntax — so the attribute makes the very
> expression it was added to protect an `E0639` for every external caller. (The
> enum rule is different, which is why `CadencePolicy` keeps it.) Growth rests
> on `Default` alone; the cost is that an exhaustive literal breaks when a field
> is added, which is a compile error with an obvious fix.
>
> `cadence: Option<SnapshotCadence>` is a trap in a `Default` struct: `None`
> means *disabled* in the old constructors, so `Tuning::default()` would have
> silently stopped writing snapshot anchors while `open(path)` kept writing
> them. Replaced by `CadencePolicy::{Default, Disabled, Every}`, so that every
> field's absence means *leave it alone* and switching the cadence off has to be
> asked for by name.
>
> Ships with **two** fields, not five: `wal_autocheckpoint` and the two cache
> sizes arrive in W5.3/W5.4, which demonstrates the additive growth on this
> release rather than asserting it about a later one. A documented knob that
> does nothing yet is worse than one that does not exist.

**W5.2 — `Database::checkpoint()`.** Closes §4.5. A `HighPriCommand`, because a
caller asking for a checkpoint is asking for it now, and returning what SQLite
returns — busy/log/checkpointed frame counts — rather than `()`. Without this,
`wal_autocheckpoint: None` is a footgun with no safe counterpart, which is why
W5.2 and W5.3 are one wave.

> **Done, 0.12.13 (D-156). "Returning what SQLite returns" turned out to need
> two pragmas.** A successful `PRAGMA wal_checkpoint(TRUNCATE)` reports
> `busy=0, log=0, checkpointed=0` — the counts describe the WAL *after* the
> operation, and after a truncation there is none. Measured: on a 387-frame WAL,
> `PASSIVE` returns `0, 387, 387` and `TRUNCATE` returns `0, 0, 0`. So the
> obvious implementation returns a report whose counts are structurally zero on
> success. `FULL` for the numbers, then `TRUNCATE` for the file; the second pass
> has nothing left to copy, and `busy` is the union.
>
> `CommandKind::Checkpoint` is budget-exempt, and W4.3's two-directional
> exemption test is what made that a decision instead of an omission — the same
> trap as D-152, one wave later.

**W5.3 — `wal_autocheckpoint` becomes settable.** Closes F-30. A bulk importer
wants it off and one explicit checkpoint at the end; an interactive process
wants the default. **The default does not change** — 1,000 pages stays, because
changing a default is a behaviour change for every existing caller and F-30 is a
control-loop perturbation, not a correctness bug.

Then measure the interaction the finding names: with autocheckpoint disabled and
an explicit checkpoint at the end, does `next_chunk_size` stop oscillating? The
answer belongs in the docs either way, because it is the first documented case
of an exogenous input to the 0.12.0 controller.

**W5.4 — `cache_size`, split.** The writer and the read-only diagnostic
connections have opposite profiles and share a default today. Two knobs, because
one number cannot serve both.

**W5.5 — `diagnostic_conn` calls `configure()`.** Fixes a genuine inconsistency:
[connection.rs:967](../src/connection.rs:967) opens `SQLITE_OPEN_READ_ONLY` per
call and never configures the connection, so diagnostic reads run with a
different `busy_timeout` and cache size than every other connection in the
process. Split `configure()` into the parts that apply to any connection and the
parts that are writer-only, and call the first from both.

**W5.6 — Document the `as_of` axis mix.** Partial close of §3.1. `as_of` on
`TraversalBuilder` conflates valid time and transaction time — Doctrine II says
the two clocks are never mixed, and here they are. 0.13.0 states precisely what
the parameter does today, in the rustdoc and in the temporal spec. **The fix is
W7.1**, and it is a break, which is why it needs its semantics written down and
agreed one release ahead rather than argued in the commit that changes them.

**W5.7 — Record D-150, D-151 and D-154.**

---

## 7. W6 — Python reaches parity, including with what this release added

Closes §4.6. Six gaps, plus everything 0.13.0 introduces.

**W6.1 — The three missing constants.** `MAX_ARCHIVE_SESSIONS` and
`chunk_rows::{EDGES, CONCEPTS, ANNOTATIONS, EMBEDDINGS}`, following
`BULK_ATOMIC_WARN_HOLD`'s own precedent at
[lib.rs:136](../bindings/python/src/lib.rs:136). A few lines. Since 0.12.0 the
`chunk_rows` values are ceilings rather than sizes (D-143/D-146), and the
docstrings must say so — a Python caller reading them as fixed chunk sizes is
reading a 0.11.0 fact.

**W6.2 — `registered_models()` and `declared_dimension()`.** Closes the
write-without-read asymmetry on the vector surface: Python can register a model
and cannot enumerate what is registered.

**W6.3 — Clock injection.** The one gap that costs test *capability* rather than
convenience: `tests_py` cannot assert on `recorded_at` at all today, which is
defect K's exact shape on the side that never received D-062's fix. Needs a
`#[pyclass]` over `FakeClock` with `advance()`, wired through `open_tuned`'s
`clock` field.

**W6.4 — Everything 0.13.0 added.** `analyze()`, `checkpoint()`, `Tuning`, the
new `CommandKind` variants, the starvation counter's `#[getter]`. A binding gap
opened in the same release that created it is a gap that never gets a chance to
become a convention.

**W6.5 — Record `shadow_step`'s omission as a decision.** Beside the `raw()`
sentinel in `lib.rs`'s convention block
([lib.rs:122](../bindings/python/src/lib.rs:122)). Expose it or record why not —
either is fine. Silence is what is not, and that block exists precisely so a
contributor deciding this stands somewhere that tells them.

---

## 8. Acceptance for 0.13.0

1. `cargo clippy --all-targets --all-features` clean. `cargo test` green on two
   consecutive full runs, R15 notwithstanding — and if R15 still makes that
   impossible, W1.4's decision says so in writing.
2. `sqlite_stat1` exists after `analyze()`, and `PRAGMA analysis_limit` bounds
   the hold: measured, not asserted.
3. `index_plan_tests` runs against both fixtures — empty, and populated+analysed
   — and both are green.
4. The two new indexes have registry entries, and the query-keyed section covers
   the four named queries.
5. `EXPLAIN QUERY PLAN` on `CONCEPTS_ARCHIVABLE` shows a seek, not a scan.
6. `cargo build` with no features gives a build whose `metrics()` returns real
   counters.
7. `MetricsSnapshot`, `KindSnapshot` and `CommandKind` are `#[non_exhaustive]`;
   `buckets` is a method in both languages.
8. `Rehydrate` reports as itself in Rust and as its own string in Python.
9. `Tuning` round-trips through `open_tuned`; the three legacy constructors still
   compile unchanged for existing callers.
10. `checkpoint()` returns frame counts, and disabling autocheckpoint plus an
    explicit checkpoint is a documented, tested bulk-import path.
11. Every 0.13.0 addition is reachable from Python.
12. D-148 … D-154 in the register.

---

# v0.14.0 — what is guaranteed

Five waves. Smaller, and almost entirely about making the guarantees hold at the
edges rather than in the middle.

---

## 9. W7 — The correctness fixes, including the ones that break API

**W7.1 — Split `as_of`.** Closes §3.1, on the semantics W5.6 wrote down. Two
parameters — `as_of_valid` and `as_of_recorded` — or a refusal of the ambiguous
pairing, decided on W5.6's documentation. A break, and correct: Doctrine II says
the two clocks are never mixed, and a single parameter that means one thing
sometimes and the other thing otherwise is the mixing.

**W7.2 — `write_annotations_atomic` goes through `classify`.** Closes §3.6. It
bypasses the classification the non-atomic path applies, so the same input
produces different stored state depending on which entry point was used. One of
the two is wrong; `classify` is the one with the tests.

**W7.3 — `run_writer_actor`'s `Err` path.** Closes §3.5. It returns
`Result<()>` and can only ever return `Ok(())`
([connection.rs:2023](../src/connection.rs:2023)). Either give it a failure it
can actually report — a poisoned connection, a channel invariant violated — or
change the signature. A `Result` that is structurally always `Ok` trains readers
to skip it.

**W7.4 — Refuse a future `recorded_at`.** Closes §3.4. `recorded_at_floor`
([clock.rs:36](../src/util/clock.rs:36)) takes `MAX(recorded_at)` with no upper
bound, so one row stamped in 2087 — a clock skew, a bad import, a test fixture
that escaped — permanently pins the floor and every subsequent write inherits
it. Bound it at open, and refuse writes stamped beyond a tolerance rather than
absorbing them silently.

**W7.5 — `reject_overlaps_within`.** Closes §2.6. O(n²) over the batch. Sort by
`(source, target, edge_type, valid_from)` and check adjacent pairs; the guard's
semantics do not change.

**W7.6 — Bulk paths report progress and accept cancellation.** Closes §4.2.
`low_chunked` discards `written` on error ([connection.rs:1841](../src/connection.rs:1841)),
so a caller whose 20,000-row import fails at row 19,000 is told only that it
failed. Return the partial count in the error. Cancellation is the larger half —
a `CancellationToken` checked between chunks, which the chunk loop's shape
already makes natural.

**W7.7 — Record D-155:** which axis `as_of` now means, and what a caller who
wants the other one calls instead.

---

## 10. W8 — The snapshot becomes durable and bounded

**W8.1 — `spawn_blocking` around snapshot save and load.** Closes §2.4.
Serialisation and file I/O currently run on a tokio worker, which is exactly what
`spawn_blocking` exists to prevent.

**W8.2 — Snapshot format v3: bounded and checksummed.** Closes §3.3. bincode's
`DefaultOptions` carries an `Infinite` limit; serde's cautious-capacity blunts
the single huge `Vec::with_capacity`, so the practical failure is not one
catastrophic allocation but a deserializer working through a corrupt stream to
exhaustion. A v3 header with a declared length and a checksum turns that into an
immediate, named error.

**W8.3 — fsync the directory after rename.** Closes §3.7. The rename is atomic
and the directory entry is not durable until the directory itself is synced —
the standard POSIX gap, and it matters on the crash path the snapshot exists
for.

**W8.4 — Fuzz the loader.** Closes §5.4. `cargo-fuzz` over the v3 format, seeded
with valid snapshots. W8.2 gives it something to assert: a corrupt input should
produce a named error, never a panic and never an allocation storm.

**W8.5 — Record D-156:** the v3 header, and why a format that a corrupt stream
can walk to exhaustion is not acceptable in a file the crash path depends on.

---

## 11. W9 — Temporal completeness

**W9.1 — `hydrate_at_time` past the archive horizon.** Closes §3.2. Once rows
are archived, `AtTime` reconstruction silently returns less than the truth — it
reads the live tables and the archived interval is simply absent. Two options:
union the cold log into the fold, or return a named horizon error. **The error
is acceptable and silence is not** — Doctrine III says assertions are superseded,
not deleted, and a reconstruction that quietly omits superseded state is
reporting something the doctrine says cannot happen.

**W9.2 — Prove it.** A test that archives, then reconstructs across the horizon,
and asserts the chosen behaviour. This is the finding most likely to be
"fixed" by a change nobody can demonstrate.

---

## 12. W10 — The gates

**W10.1 — Performance regression detection.** Closes §5.3. D-055 keeps benches
out of CI as gates, and D-070's ~29% session noise is why. That argument holds
for wall-clock timings; it does not hold for everything worth gating. Two things
are stable enough to assert:

- **Plan shape.** `EXPLAIN QUERY PLAN` output is deterministic given a fixture
  and its statistics. W2.4 makes that fixture exist. This is a real gate.
- **Operation counts.** Rows scanned, chunks issued, transactions opened — these
  do not move with machine noise. A query that starts scanning where it used to
  seek shows up here as an integer change, and that is exactly the D-042/D-059/
  D-064 bug class arriving as a red test instead of as a support ticket.

Wall-clock benchmarks stay advisory, as D-055 says. This wave does not overturn
D-055; it observes that D-055's reasoning was about timings specifically.

**W10.2 — `PRAGMA optimize` gets a scheduled call site.** Whatever W2.2's
measurement recommended, made real and tested.

**W10.4 — Decide the low-priority fairness floor, on evidence that is not a
synthetic burst.** Added 0.12.10, after W4.4's counter falsified the premise
§16 rejected this on ([D-153](architecture/s13-decision-register.md#d-153)).

W4.4 established the mechanism is unbounded and trivially reachable:
`run_max=63` out of 63 starved turns, deterministic, a background `bulk_import`
sitting behind *every* queued interactive write. What it did not establish is
that any real workload does that — 64 tasks spawned at once is a burst nobody
has claimed is representative, and a caller writing sequentially queues nothing
at all.

So this wave is **a measurement first and a policy only if it earns one**, in
that order:

1. **Get a realistic reading.** Drive `low_starved_run_max` from a workload
   shaped like use rather than like a stress test — the shapes already in
   `benches/budgets.rs` and the `*_diag` examples are the candidates, since they
   exist to model real paths. A run length in the low single digits is the tiers
   working; a run that tracks offered load is the defect.
2. **Only then, if the reading justifies it, add the floor.** The obvious form
   is "after N consecutive starved turns, take one low-priority command", which
   costs one branch per turn and bounds the run at N by construction.
3. **Whatever the answer, record it and close §16's entry properly.** The
   rejection currently stands annotated rather than resolved, and inheriting it
   unread is the failure this wave exists to prevent.

**The counter must not be the only evidence.** It was added in the release that
would ship the fix, so a before/after taken with it alone measures the fix
against itself. That is why this is a 0.14.0 wave and not a 0.13.0 one.

**W10.3 — `Subgraph` interior, if measurement justifies it.** Closes §2.5.
String-keyed adjacency; index-based would be faster. **Benchmark first, and be
prepared to close this as "not worth it".** A `Subgraph` big enough for this to
matter may be rarer than the finding assumes, and this is the lowest-value item
in either release.

---

## 13. W11 — The 1.0 freeze audit

**W11.1 — Decide `Database: Clone`.** Closes §4.7. It holds an `Arc` internally;
not being `Clone` pushes every multi-consumer caller into wrapping it in a
second `Arc`. Either derive it or document why not. **Post-1.0 this is
additive** — adding `Clone` never breaks anyone — so this is the one item here
that is genuinely safe to defer, and it is included because deciding it costs
ten minutes and leaving it undecided costs every new caller five.

**W11.2 — Walk the public surface once, deliberately.** `cargo public-api`
against 0.13.0, and for every item ask whether it is intended to be supported
for the life of 1.x. Everything that is not either becomes `#[doc(hidden)]`,
gets `#[non_exhaustive]`, or is removed now. This is the last time the answer is
free.

**W11.3 — Documentation sweep.** Closes §6.1: the release history table in
`docs/architecture/README.md` stops at 0.9.0, three releases behind. D-144
already named doc drift as a category; this is the sweep that clears it.

**W11.4 — Comment-to-code ratio: no action, recorded.** Closes §6.3. The ratio is
unusually high, and it is unusually high because the comments carry measurements
and decision rationale that would otherwise exist nowhere — D-059's four timing
figures live in a comment in `ddl.rs`. That is a deliberate property of this
codebase, not an accident to correct. Recorded so it is not raised again as a
finding.

**W11.5 — Record D-157: what 1.0 freezes and what it does not.**

---

## 14. Acceptance for 0.14.0

1. `as_of`'s two axes are separate parameters or an explicit refusal, and
   Doctrine II is not violated by any public API.
2. Both annotation paths produce identical stored state for identical input.
3. Snapshot v3 round-trips; a corrupted snapshot produces a named error; the
   fuzzer runs clean for one hour on the v3 corpus.
4. Reconstruction across the archive horizon either returns the truth or names
   its refusal — with a test that demonstrates which.
5. Plan-shape and operation-count gates run in CI and fail on an induced
   regression. Verified by inducing one.
6. `cargo public-api` diff against 0.13.0 reviewed item by item, with the review
   recorded.
7. Every finding in §1 is closed, or explicitly recorded as not-to-be-fixed with
   a reason. Zero silent carries.
8. D-155 … D-157 in the register.

---

## 15. What 1.0 promises after this, and what it does not

**Promises.** The public API is stable for 1.x. The on-disk format is stable or
migrated by a rung. `CHUNK_BUDGET` is observable in the default build, and
`violations()` is the honest answer to whether it is met. Query plans are
pinned against a fixture whose statistics match production's. The snapshot
format is bounded and checksummed. Every doctrine has a test that fails when it
is violated.

**Does not promise.** That `CHUNK_BUDGET` is always met — it is a budget the
actor steers on, and on a populated `links` table it is still missed by ~0.2 ms
at the floor. That benchmarks are reproducible across sessions; D-070 says
otherwise and that is a property of the measurement, not of the code. That
concurrent opens are safe on Windows, unless W1 says they are. That single-row
writes are fast; they cost the ~0.8 ms transaction floor by construction, and the
bulk paths exist for that reason.

**The distinction is the deliverable.** A 1.0 that promises less and means all of
it is worth more than one that promises a latency bound it cannot measure — which
is the release Macrame would have shipped if §4.3 had gone the other way.

---

## 16. Rejected before starting

**`synchronous` / a `Durability` knob.** In WAL mode `NORMAL` already skips the
per-commit fsync and syncs at checkpoints. `FULL` buys durability against OS
crash at a cost that would land squarely inside `CHUNK_BUDGET`; `OFF` risks
corruption on a ledger whose entire premise is that assertions are never lost.
There is no defensible third setting, so there is no knob.

**`journal_mode`.** WAL is not a preference here. The single-writer actor, the
concurrent readers, and `busy_timeout = 5000` are one design, and it is WAL's.
Exposing this exposes a way to break the architecture from the outside.

**`foreign_keys` / `recursive_triggers`.** Doctrine enforcement. The delete
guards and the log triggers are how Doctrines III and V are made real; a caller
who can turn them off can violate the doctrines through the supported API.

**`query_only`.** The read-only path already opens `SQLITE_OPEN_READ_ONLY`
(D-091). A second, weaker mechanism for the same guarantee is a way to get it
wrong.

**`page_size`.** Only settable before the first write, so it is an
open-time-only knob whose wrong value is unfixable without a full rebuild. Not
worth the support burden for a number almost nobody should change.

**Raw pragma passthrough (`db.pragma("...")`).** This is what CodeRadar's
complaint literally asks for and it is the wrong shape. It reintroduces exactly
what `raw()` is refused for at [lib.rs:122](../bindings/python/src/lib.rs:122):
an escape hatch through which the single-writer property, and every latency
argument resting on it, can be dissolved from outside. A typed `Tuning` struct
answers the real need — a caller who wants to tune — without answering "let me
run arbitrary DDL against the writer's connection".

**`bulk_session()`.** Proposed earlier in this analysis and withdrawn. It
introduces a mode into a modeless design, mutates writer-connection state while
other callers' commands are already queued behind it, and would apply
bulk-tuned settings to interactive writes that happen to arrive during the
session. `Tuning` at open plus explicit `checkpoint()` and `analyze()` gets the
same result with no mode and no shared mutable state.

**Changing the `wal_autocheckpoint` default.** F-30 is a control-loop
perturbation, not a correctness bug. Making it settable is the fix; changing the
default for every existing caller to address it is not.

**A forced-yield policy for low-priority starvation.** W4.4 adds the counter, not
the policy. Whether the bound is ever hit in practice is a measurement nobody has
taken, and a fairness mechanism added on a hypothesis is a scheduling change with
no evidence behind it.

> **Annotated 0.12.10 — the premise above is no longer true (D-153).** The
> measurement has been taken and the bound is hit totally: `run_max=63` out of
> 63 starved turns, deterministic across five runs. The second sentence's
> reasoning still stands on its own terms — a fairness mechanism needs evidence
> — but it can no longer claim there is none. Re-read this rejection rather than
> inheriting it; what is genuinely still open is whether a 64-task synthetic
> burst is evidence about production workloads or only about the mechanism, and
> that is a judgement for the maintainer.
>
> **Resolved as scheduled, not as taken: this is now W10.4 in 0.14.0.** The
> policy is neither shipped blind nor dropped — W10.4 gets a realistic reading
> first, precisely because the counter was added in the release that would carry
> the fix, and a before/after taken with it alone measures the fix against
> itself.
