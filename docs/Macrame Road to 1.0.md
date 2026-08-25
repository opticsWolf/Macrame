# Macrame — the road to 1.0

Three releases — 0.13.0, 0.14.0 and 0.15.0 — and then the version number stops
being a promise about the future.

Source: `docs/Macrame Codebase Review v0.12.0.md` (27 findings), plus three
findings raised after it was written (§0.3), plus four raised against the 2026
bitemporal literature review after 0.13.0 shipped (§0.4). Every one of the 34
appears exactly once in the coverage table at §1, against the wave that closes
it. Nothing is listed as "later".

0.15.0 was added after 0.13.0 shipped and is the one release here that answers a
use case rather than a finding; §15 says so in its own words rather than
pretending otherwise.

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
are reversed here after the paradigm filter, and are recorded in §19 with the
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
  in §19.
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

### 0.4 Four findings from the 2026 bitemporal literature review (0.13.1)

Raised after 0.13.0 shipped, against Neelamegam, Bhogal, Samimi and Ozturkoglu,
*Comprehensive insights into bitemporal databases: a PRISMA-guided systematic
literature review*, Journal of Data, Information and Management 8(1), 71–96
(2026) — a synthesis of 54 primary studies across 1995–2026. Two of the four are
defects demonstrated in this tree; two are gaps the survey makes it impossible to
keep calling deliberate. All four are scheduled inside the pre-1.0 window, under
the same constraint that governs everything above: no critical item gets pushed
past 1.0 on the grounds that the release was getting large.

**Recorded first, because it changes how the rest of this section reads: the
survey's headline complaint does not apply here.** §7.1 names append-only version
chains as the field's dominant scalability limitation — Lomet's version chains
growing without bound, Eshtay's storage duplication in NoSQL stores, Hou's
snapshot-plus-delta reconstruction cost. That is the problem `archive`,
`archive_windowed`, `rehydrate`, snapshot anchoring, `WalCheckpointPolicy` and
`rebuild_current_chunked` were built for across 0.9.0–0.13.0. The findings below
are the ones that survive after taking credit for that.

**F-31 — `search_vector` returns retired concepts, and `keyword_search` does
not.** Demonstrated, not inferred. `keyword_search` filters `AND c.retired = 0`
([hybrid.rs:103](../src/vector/hybrid.rs:103)) and says why in its rustdoc.
`search_vector` joins `vector_top_k(…)` to the embeddings table and to **nothing
else** ([search.rs:146](../src/vector/search.rs:146)) — there is no `concepts`
join, so there is no `retired` column in scope to filter on. `HybridSearch`
fuses the two lists without a post-fusion visibility pass
([hybrid.rs:~210](../src/vector/hybrid.rs)), so the vector arm carries the
retired row straight through the fusion into the caller's result.

A probe on a two-concept fixture, one retired and nearest the query:

| surface | a retired concept | a concept expired in valid time |
|---|---|---|
| `search_vector` | **returned** | **returned** |
| `keyword_search` | excluded | **returned** |
| `hybrid_search` | **returned**, via the vector arm | **returned** |
| `search_filtered` | excluded | **returned** |

`search_filtered` is safe **by accident of composition, not by decision**: its
candidate set comes from `TraversalBuilder::execute_ids`, whose `build_sql`
closes with `WHERE c.retired = 0` ([builder.rs:229](../src/graph/builder.rs:229)),
and both the pre-filter and post-filter arms intersect against that set. Nothing
in `FilteredVectorSearch` itself knows about visibility. Change the traversal to
return ids without hydrating, and the third surface leaks too.

**This is defect Z's exact shape, one module over.** From
[subgraph.rs:34](../src/graph/subgraph.rs:34): *"`links_current` … carries edges
to retired concepts; `hydrate` filters `retired = 0`. So a retired neighbour left
an `EdgeRef` pointing at a node"*. Wave 1 fixed that instance. The general lesson
— **visibility is enforced at whichever join happens to touch `concepts`, and a
path that never touches `concepts` enforces nothing** — was never generalised, and
the vector path is the path that never touches `concepts`. **Severity: High.** A
soft-deleted concept is retrievable through the crate's flagship retrieval
surface, and the caller most likely to hit it is the one using Macrame as agent
memory, where "retired" means the user asked for it to be forgotten.

**F-32 — no search surface filters concept valid time.** The same probe, right
column: a concept whose `valid_to` is in the past is returned by all four
surfaces. This is uniform, so it is a gap and not an inconsistency, which is why
it is graded below F-31 despite being wider.

It is a gap with a specific consequence: **`as_of` and search cannot be
combined.** `TraversalBuilder::as_of(t)` bounds edges by
`l.valid_from <= ?3 AND ?3 < l.valid_to`
([builder.rs:309](../src/graph/builder.rs:309)) — so the graph half of
`search_filtered` is time-aware and the vector half is not, and the concepts the
traversal reaches are filtered on `retired` but never on their own valid
interval. A caller asking "what was near this query, among what was true last
March" has no way to ask it, and the surface that looks closest to answering
gives an answer that silently mixes March's topology with today's corpus.

The survey's §6.3 and §7 make this the wrong thing to leave open: the ML- and
agent-memory integration it identifies as *"perhaps the most significant open
direction"* is precisely retrieval that carries temporal context, and BiteNet
(Peng et al. 2020) is the only prior work it finds doing it. **Severity: Medium**
as a defect, **High** as the thing that distinguishes this crate from a vector
store with timestamps. Scheduled with F-31 because they are one join.

**F-33 — the two-dimensional index question has never been asked, and the
obvious answer does not fit.** Finding 3.1 splits `as_of` into its two axes at
W7.1. The moment that lands, predicates of the form
`valid_from <= tv < valid_to AND recorded_at <= tx` become expressible and
therefore common, and this crate's indexes are all one-dimensional B-trees.

The survey's §4.2 is the standard citation for that being a problem — standard
relational indexes *"degenerate into full-table or extensive index-range scans"*
on interval-overlap queries. **Its own evidence is more equivocal than the
citation suggests, and the equivocation is the useful part.** Kaufmann et al.
(2015)'s Bitemporal Timeline Index explicitly *declines* to model the two
dimensions as a 2-D spatial structure and keeps **one 1-D index per temporal
domain**, outperforming spatial structures on selection, join and aggregation.
Fig. 9's conclusion is that *no single index structure is optimal for all
bitemporal query workloads*. So the literature does not say "use an R\*Tree"; it
says "measure, and expect the answer to be workload-dependent".

**And the obvious port is arithmetically blocked here, which is worth recording
before someone spends a wave on it.** SQLite's `rtree` module stores coordinates
as **32-bit floats**; `rtree_i32` stores **32-bit integers**. This crate's
timestamps are fixed-width ISO-8601 text with microsecond resolution (D-029), and
a 64-bit microsecond epoch fits neither: float32 carries a 24-bit mantissa, so
epoch-seconds near 1.8 × 10⁹ quantise to roughly 128-second buckets, and int32
overflows in 2038 at second resolution. An R\*Tree over these columns can
therefore only ever be a **coarse bounding-box pre-filter with an exact recheck
against the text columns** — which is a legitimate design, and is how the module
is meant to be used, but it is not the drop-in index the citation implies and it
must never be the authority. **Severity: Medium**, and it is a measurement item
before it is a build item.

**F-34 — every temporal query in this crate is composed in Rust.** There is no
declarative surface: a caller writes `TraversalBuilder`, `HybridSearch`,
`FilteredVectorSearch` and chains them by hand. The survey's §6.2.3 and §7 name
this as the industrial-adoption boundary — Oracle and Leipzig's TPGM+ pairs the
bitemporal property graph with **T-PGQL**, a declarative language, and the survey
treats the language as the deliverable rather than the model.

Recorded as a finding rather than left as taste, because W12 forces the question:
a branch selector, a valid-time instant and a transaction-time instant are three
orthogonal qualifiers on the same read, and adding each of them to each builder
by hand is a combinatorial surface. **Severity: Low today, Medium the moment
branching lands.** This is the one item in this document I would cut first if
0.15.0 grows, and it is scheduled where cutting it is still possible.

---

## 1. Coverage: every finding, and the wave that closes it

| # | Finding | Sev | Wave | Release |
|---|---|---|---|---|
| 2.1 | `links` has no explicit index — four full scans, one per `open()` | High | W3.1 | 0.13.0 |
| 2.2 | `CONCEPTS_ARCHIVABLE` quadratic on `links.target_id` | High | W3.2 | 0.13.0 |
| 2.3 | Per-transaction overhead ~0.8 ms; singular paths pay it per row | Med | W3.4 | 0.13.0 |
| 2.4 | Snapshot work runs on a tokio worker | Med | W8.1 | 0.13.11 ✅ |
| 2.5 | `Subgraph` string-keyed adjacency | Low | W10.3 | 0.14.0 |
| 2.6 | `reject_overlaps_within` O(n²) | Med | W7.5 | 0.13.6 ✅ |
| 3.1 | `as_of` mixes valid time and transaction time | High | W5.6 / W7.1 | both |
| 3.2 | `AtTime` degrades silently after archive | Med | W9.1 | 0.13.16 ✅ |
| 3.3 | Snapshot loader unbounded | Med | W8.2 | 0.13.12 ✅ |
| 3.4 | Future `recorded_at` poisons the clock floor | Med | W7.4 | 0.13.5 ✅ |
| 3.5 | `run_writer_actor` cannot return `Err` | Low | W7.3 | 0.13.4 ✅ |
| 3.6 | `write_annotations_atomic` bypasses `classify` | Med | W7.2 | 0.13.3 ✅ |
| 3.7 | Snapshot rename atomic but not durable | Med | W8.3 | 0.13.13 ✅ |
| 4.1 | No anti-starvation floor on low-priority work | Med | W4.4 (counter), W10.4 (the floor itself) | 0.13.0 / 0.14.0 |
| 4.2 | No cancellation or progress on bulk paths | Med | W7.6 | 0.14.0 ✅ |
| 4.3 | `metrics` off by default | High | W4.5 | 0.13.0 |
| 4.4 | Metrics surface frozen by accident | High | W4.2, W4.3 | 0.13.0 |
| 4.5 | No WAL / checkpoint surface | Med | W5.2 | 0.13.0 |
| 4.6 | Six gaps in the Python surface | Med | W6 | 0.13.0 |
| 4.7 | `Database` is not `Clone` | Low | W11.1 | 0.14.0 |
| 5.1 | R15 reaches the main suite | High | W1 | 0.13.0 |
| 5.2 | Index registry is one-directional | Med | W2.3 | 0.13.0 |
| 5.3 | No performance regression detection | Med | W10.1 | 0.14.0 ✅ |
| 5.4 | No snapshot fuzzing | Low | W8.4 | 0.13.14 ✅ |
| 6.1 | Release history table stops at 0.9.0 | Low | W11.3 | 0.14.0 |
| 6.2 | `Cargo.toml` metrics cost model is false | Med | W4.1 | 0.13.0 |
| 6.3 | Comment-to-code ratio | — | W11.4 | 0.14.0 |
| F-28 | No `ANALYZE`; planner runs on default selectivity | High | W2.1, W2.2 | 0.13.0 |
| F-29 | Plan-pinning fixture has no rows and no statistics | High | W2.4 | 0.13.0 |
| F-30 | Autocheckpoint perturbs the chunk controller | Med | W5.3 | 0.13.0 |
| F-31 | `search_vector` returns retired concepts; `keyword_search` does not | High | W9.3 | 0.13.18 ✅ |
| F-32 | No search surface filters concept valid time | Med | W9.4, W9.5 | 0.14.0 ✅ |
| F-33 | The 2-D index question is unasked and the obvious answer does not fit | Med | W10.6 | 0.14.0 |
| F-34 | No declarative surface; three qualifiers × four builders | Low | W13 | 0.15.0 |
| F-35 | `load_subgraph_with` accepts an instant and reads the present | Med | W7.1 | 0.14.0 ✅ |

Ten High-severity findings. **Eight close in 0.13.0.** The ninth, §3.1, was
documented in 0.13.0 (W5.6) and broken correctly in **0.13.2** (W7.1, D-174) —
see §0.1 for why that order and not the reverse. The tenth, F-31, was found after
0.13.0 shipped and closes in 0.14.0; it is a live defect rather than a design
gap, and it is the only High in this document that reached a released version
unrecorded.

**F-35 was found while closing W7.1 and closed in the same change.** It is listed
because a finding discovered during a fix and repaired silently is a finding the
next reader cannot learn from. `Database::load_subgraph_with` bound `now_ts`
where the builder bound the traversal's own instant, so a historical builder
passed to it returned the present with nothing said — F-31's shape in a third
place. See D-175.

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
> **This falsifies the stated premise of §19's rejection of the forced-yield
> policy** — "whether the bound is ever hit in practice is a measurement nobody
> has taken". It has now been taken. What remains open is a judgement rather
> than a measurement: whether a synthetic 64-task burst is evidence about
> production or only about the mechanism. No policy is added here, per this
> wave's own instruction; see §19, which is annotated rather than rewritten.

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

> **Done, 0.12.14 (D-157). The answer is no, and the finding's framing is half
> wrong.** Three rounds, 6,000 × 1 KB concepts through `write_concepts`:
>
> | | longest chunk hold | mean | over budget | wall |
> |---|---|---|---|---|
> | autocheckpoint on | **9.3–10.3 ms** | 2.40–2.44 ms | 24–28 | 304–321 ms |
> | autocheckpoint off | **4.50 ms** | 2.08–2.20 ms | 18–27 | 298–339 ms |
>
> The tail is real: the longest hold halves and the >10 ms bucket is populated
> only with the checkpointer on — that bucket *is* the checkpoint, charged to
> whichever chunk it landed in. But `over_budget` overlaps and wall time is
> identical, so **the controller does not stop oscillating**; it works near the
> budget boundary either way, because of D-090's ~0.8 ms floor and D-146's
> convergence cost. What disabling autocheckpoint removes is an outlier, not an
> oscillation.
>
> And the cost is **deferred, not removed**: the explicit checkpoint at the end
> moved 8,400–9,100 frames in **41–45 ms** with it off, against ~860 frames in
> 5.5–6.2 ms with it on. Same work, relocated out of the bounded path into one
> hold the caller timed. Good for a bulk importer, bad for an interactive
> process — which is why the default stays at 1,000 pages.
>
> `WalCheckpointPolicy` rather than the `Option<u32>` specified above, for
> D-155's reason: in a `Default` struct, a `None` that disables a mechanism
> disables it for everyone who did not mention it — and this is the mechanism
> that keeps the WAL bounded.

**W5.4 — `cache_size`, split.** The writer and the read-only diagnostic
connections have opposite profiles and share a default today. Two knobs, because
one number cannot serve both.

> **Done, 0.12.15 (D-158).** `writer_cache_size` and `reader_cache_size`, both
> `Option<i32>`, applied per connection. Note the shape: these are `Option` and
> not policy enums like W5.1's and W5.3's, and the distinction is the point —
> those two exist because "leave it alone" was ambiguous for a *mechanism* that
> could be off or default, where for a *value* leaving it alone is simply not
> setting the pragma. `None` therefore runs nothing, and the default stays
> SQLite's −2000 rather than being restated here.
>
> SQLite's units are kept: negative is KiB, positive is pages. The test asserts
> the *split* rather than the pragma — a single shared field produces a value
> that is right everywhere, which is invisible to any test that sets one number
> and reads it back.

**W5.5 — `diagnostic_conn` calls `configure()`.** Fixes a genuine inconsistency:
[connection.rs:967](../src/connection.rs:967) opens `SQLITE_OPEN_READ_ONLY` per
call and never configures the connection, so diagnostic reads run with a
different `busy_timeout` and cache size than every other connection in the
process. Split `configure()` into the parts that apply to any connection and the
parts that are writer-only, and call the first from both.

> **Done, 0.12.16 (D-159).** `configure_common` (`busy_timeout`, `cache_size`)
> and `configure_writable` (`journal_mode`, `synchronous`, `foreign_keys`,
> `recursive_triggers`, `analysis_limit`); `diagnostic_conn` calls the first.
> The line is not tidiness — `journal_mode = WAL` is a change to the file, which
> a read-only connection cannot make, and the rest govern writes it cannot
> perform.
>
> The consequential omission was **`busy_timeout`, which defaults to 0** against
> every other connection's 5 s: the shortest fuse in the process, on the surface
> whose stated job is to answer questions when the typed path is already
> suspect, and a direct contributor to the `database is locked` mode R15
> recorded.
>
> **It does not rescue `analyze_tests`**, whose comment named W5.5 as the fix.
> `ANALYZE` is a write, so `analysis_limit` is in the writable half and no
> reachable connection has it. That test now reads `configure_writable` and says
> why its reasoning is unchanged — a scheduled fix that lands and does not fix
> what it was cited for is how a stale comment becomes a wrong one.

**W5.6 — Document the `as_of` axis mix.** Partial close of §3.1. `as_of` on
`TraversalBuilder` conflates valid time and transaction time — Doctrine II says
the two clocks are never mixed, and here they are. 0.13.0 states precisely what
the parameter does today, in the rustdoc and in the temporal spec. **The fix is
W7.1**, and it is a break, which is why it needs its semantics written down and
agreed one release ahead rather than argued in the commit that changes them.

> **Done, 0.12.17 (D-160).** The mix is sharper than "conflates valid and
> transaction time": the topology compares `ts` to `links.valid_from/valid_to`
> (valid time, current belief — Doctrine VIII met), `AttributeMode::AtTime`
> compares the *same* `ts` to `transaction_log.recorded_at` (transaction time,
> which is `reconstruct`'s axis), and `Current` compares it to nothing at all.
>
> Concretely: a title corrected today to fix a 2020 typo does **not** appear in
> `as_of("2020-06-01") + AtTime`, because the correction was recorded after
> `ts`. Right answer to "what did we believe in 2020", wrong answer to "what was
> true in 2020" — and the name promises the second.
>
> **What no combination gives today** is the thing `as_of` is named for applied
> to attributes: what was true at `t` as best we now know. There is no
> valid-time read of concept attributes anywhere, because concepts are versioned
> in the log rather than as intervals. So W7.1 is a design decision, not a
> predicate change — which is the argument for the release of notice.
>
> Written into the `as_of` rustdoc and into §5.2 of the module spec, both as
> tables, plus D-160.

**W5.7 — Record D-150, D-151 and D-154.**

> **Done, 0.12.17.** All three were written when the work landed rather than
> batched here — D-150 in 0.12.5, D-151 in 0.12.6, D-154 in 0.12.11 — so this
> item carried no work of its own and rides W5.6's bump.
>
> The wave shipped **six** decisions the plan did not name: D-155 (`Tuning`),
> D-156 (`checkpoint`), D-157 (`wal_autocheckpoint`), D-158 (split cache),
> D-159 (`diagnostic_conn`), D-160 (the `as_of` axis mix). Worth noting as a
> pattern rather than an accident: a wave item that says "record D-nnn" is
> planning for the decisions already foreseen, and every wave so far has
> produced more decisions than it predicted — which is the argument for writing
> them at the commit rather than at a checkpoint item, since the checkpoint
> item's list is written before the work.

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

> **Done, 0.12.18.** Six constants, not three: `MAX_ARCHIVE_SESSIONS` plus the
> four `chunk_rows` ceilings, flat rather than as a namespace (D-161). A
> `chunk_rows` submodule would need its own `.pyi` and its own entry in
> `test_stubs.py`; a dict would lose `Final[int]`, which is what `mypy --strict`
> reads — so the two gates that already keep this surface honest only work on
> flat `Final` names. The ceiling-not-size correction is in the Rust
> registration comment and repeated in the stub.
>
> **The dev install was shadowed and the suite had been testing a 0.12.0
> extension.** `site-packages` held a non-editable `macrame-db 0.12.0` whose
> real `macrame/` directory won over the editable path entry, so
> `macrame.__version__` read 0.12.0 against a source tree at 0.12.17. Nothing
> caught it because `test_packaging`'s version check compares the *installed*
> wheel to the *installed* binding — both stale, and agreeing. Removed and
> reinstalled with `maturin develop` from the repo root (the root `pyproject`
> is the maturin project; `bindings/python` has no `pyproject` and installing
> from there produces a `macrame_py` distribution that ships only `_macrame`).
> 357 passed, 2 skipped.

**W6.2 — `registered_models()` and `declared_dimension()`.** Closes the
write-without-read asymmetry on the vector surface: Python can register a model
and cannot enumerate what is registered.

> **Done, 0.12.19.** `registered_models()` and `declared_dimension()` on
> `Database`, both reading the schema rather than a cache (D-037), both
> returning names `register_model` takes back. The dimension **raises** on an
> unknown model rather than answering `None` — the value's use is allocating a
> buffer of that many floats, and `None` there is a zero-length buffer plus a
> failure somewhere else (D-162).
>
> The asymmetry's cost was more specific than "inconvenient": the only way to
> ask *is this model registered?* was `register_model` again, which is a write
> issued as a question and which **succeeds** in the case being checked for.
> 361 Python passed.

**W6.3 — Clock injection.** The one gap that costs test *capability* rather than
convenience: `tests_py` cannot assert on `recorded_at` at all today, which is
defect K's exact shape on the side that never received D-062's fix. Needs a
`#[pyclass]` over `FakeClock` with `advance()`, wired through `open_tuned`'s
`clock` field.

> **Done, 0.12.20.** `_FakeClock` with `advance()` / `peek()`, and
> `Database._open_with_clock(path, clock, ...)` going through `open_tuned`'s
> `clock` field as specified.
>
> **One deviation, and it is the shape rather than the substance.** §14.6
> already recorded `open_with_clock` as deliberately not exposed, so shipping
> it as the plan describes would have silently reversed a decision. It is a
> `testing.rs` hook instead: underscore-prefixed, out of `__all__` and out of
> the stub, taking a `_FakeClock` rather than a `Clock` — so *arbitrary time in
> a production ledger*, which is what §14.6 objects to, is still not reachable,
> while *a known stamp in a test ledger* now is. The entry is qualified in
> place and a test asserts the seam stays private (D-163).
>
> **A defect in shared code, found by writing the tests.** `to_duration`, which
> `archive_windowed`'s window also uses, answered a negative `timedelta` with a
> `TypeError` saying a `timedelta` was expected, and accepted `timedelta(0)`
> while refusing `0`. Both now raise `ValueError` about the sign. The existing
> refusal test enumerated `0, -1, nan, inf` and no `timedelta` at all, which is
> why it survived four releases.
>
> 368 Python passed.

**W6.4 — Everything 0.13.0 added.** `analyze()`, `checkpoint()`, `Tuning`, the
new `CommandKind` variants, the starvation counter's `#[getter]`. A binding gap
opened in the same release that created it is a gap that never gets a chance to
become a convention.

> **Done, 0.12.21.** `analyze()`, `optimize()`, `checkpoint()` returning a
> `CheckpointReport`, and the three tuning knobs as keyword arguments on
> `open`.
>
> **`Tuning` does not cross as a type** (D-164). It exists in Rust to solve a
> problem Python does not have — a growing option set that cannot go into a
> signature without breaking callers — and Python has keyword defaults for
> exactly that. What crosses is the *shape of the defaults*: absent leaves the
> mechanism alone, never disables it, which is D-155's lesson at a boundary
> where it is easier to get wrong, since a keyword default is invisible at the
> call site. `wal_autocheckpoint` takes `None` / `"disabled"` / a positive page
> count and refuses `0`, a string for the third state because a string cannot
> be produced by the arithmetic mistake D-157 refuses.
>
> **Two of the three knobs are not observable from Python**, and the tests say
> so rather than asserting something false: `wal_autocheckpoint` and
> `writer_cache_size` are applied to the write connection, which no Python
> caller can reach. A `== 64` assertion through `diagnostic_query` would be
> wrong *and* would keep passing after the feature broke.
>
> **Verified rather than re-added**, as the plan's own note anticipated: the
> new `CommandKind` variants and the starvation getters shipped in W4.3/W4.4.
> `KindMetrics.kind` reads `CommandKind::as_str()` through the crate, so a new
> variant arrives with its own string without a binding change — asserted for
> `"checkpoint"`. 380 Python passed.

**W6.5 — Record `shadow_step`'s omission as a decision.** Beside the `raw()`
sentinel in `lib.rs`'s convention block
([lib.rs:122](../bindings/python/src/lib.rs:122)). Expose it or record why not —
either is fine. Silence is what is not, and that block exists precisely so a
contributor deciding this stands somewhere that tells them.

> **Done, 0.12.22 — recorded as not exposed** (D-165). Unlike `raw()`, this is a
> judgement rather than an invariant: `shadow_step` is public and safe in Rust.
> What does not cross is its *obligation* — the `epoch` from
> `ShadowOutcome::Started` must return to `ShadowStep::Swap`, and losing it
> swaps a stale projection over a live one **silently**, because the swap
> succeeds. In Rust two types the caller cannot fabricate carry that; across
> this boundary they become two `#[pyclass]`es whose only job is to be handed
> back correctly, which converts an enforced obligation into a convention.
> `rebuild_current_chunked` is the loop, is exposed, and cannot get the epoch
> wrong, and nobody has yet brought the between-steps use to this surface.
>
> **Also made enforceable rather than merely written.** The convention block
> was a comment, and a comment stops a contributor who reads it and not a
> `#[pymethods]` block added a year later for a plausible reason.
> `test_packaging.py` now asserts all three absences — `raw`, `read_conn`,
> `shadow_step` — each with its own argument in the message, so adding one
> means answering the register rather than deleting a line. 381 Python passed.
>
> If a caller appears, the epoch should cross as an opaque handle rather than
> an integer. Waiting is not caution: a type invented for a hypothetical caller
> is a type nobody can check against a real use.

---

## 8. Acceptance for 0.13.0

> **Closed, and the version bumped to 0.13.0 on 2026-08-15.** All twelve items
> resolved. Two of them resolved as *corrections rather than confirmations*, and
> those are the ones worth carrying forward:
>
> - **Item 2** asked for `analysis_limit`'s bound *measured, not asserted*, and
>   the measurement refuted the claim it was checking
>   ([D-166](architecture/s13-decision-register.md#d-166)). The pragma is in
>   force and worth 3–4×, but `analyze()` holds the write lock **19.1 ms at
>   40,000 edges** against a 3 ms budget, and `budget_violations()` had been
>   reporting it all along. The instrument worked; nobody had read it.
> - **Item 4** miscounts its own spec — W2.3 names three queries plus two
>   indexes, not four queries. All are covered.
>
> Item 10 also turned up a rustdoc that had described the wrong method since
> 0.12.13 ([D-167](architecture/s13-decision-register.md#d-167)), and closing
> the wave surfaced a feature-off build broken for two releases by an example
> added during it ([D-169](architecture/s13-decision-register.md#d-169)) — a
> checklist item verified once, then invalidated by later work in the same wave.
>
> Final state: **391 Rust across 34 targets, 377 with `--no-default-features`,
> 381 Python**, clippy clean under `--all-features`, schema v11, decisions
> D-001…D-169.

1. `cargo clippy --all-targets --all-features` clean. `cargo test` green on two
   consecutive full runs, R15 notwithstanding — and if R15 still makes that
   impossible, W1.4's decision says so in writing.
2. `sqlite_stat1` exists after `analyze()`, and `PRAGMA analysis_limit` bounds
   the hold: measured, not asserted.

   > **Measured, and the measurement refutes half of it** (0.12.23, D-166).
   > `sqlite_stat1` exists — `tests/analyze_tests.rs`. The bound does not hold
   > as D-149 stated it. `examples/analyze_hold.rs` times the crate's own hold
   > beside the same file analysed with the pragma off and on:
   >
   > | edges | crate's hold | limit off | limit 400 |
   > |---|---|---|---|
   > | 10,000 | 5.26 ms | 18.4 ms | 6.01 ms |
   > | 40,000 | 19.1 ms | 78.6 ms | 19.4 ms |
   >
   > The pragma **is** applied — the crate's hold tracks the capped arm, which
   > is the only way to establish it, since the connection that runs `ANALYZE`
   > is the actor's and no test can reach it. It is worth 3–4×. But the capped
   > time grew 3.2× over a 4× table, so it is a constant factor and not an
   > independence: `analyze()` on a 40,000-edge ledger misses `CHUNK_BUDGET` by
   > ~6×.
   >
   > **The instrument that catches this already existed and nobody had read
   > it.** `Analyze` is not budget-exempt, so `metrics().budget_violations()`
   > returns `("analyze", 1)` after one call — and always would have. That is
   > the case this item's "measured, not asserted" wording was written for.
3. `index_plan_tests` runs against both fixtures — empty, and populated+analysed
   — and both are green.
4. The two new indexes have registry entries, and the query-keyed section covers
   the four named queries.

   > **Met; the count in this line is its own miscount.** W2.3 names *three*
   > queries — `CONCEPTS_ARCHIVABLE`, `LINKS_ARCHIVABLE`, `recorded_at_floor` —
   > plus the two new indexes, which belong to the index-keyed `REGISTRY` rather
   > than to `QUERY_REGISTRY`. All three query entries exist and are asserted
   > against both fixtures; both indexes have registry entries.
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

    > **Met, 0.12.24.** Frame counts: `tests/checkpoint_tests.rs`. The pairing
    > was already tested over `write_concepts`
    > (`a_disabled_autocheckpoint_lets_the_wal_grow`); what this item names is
    > the **bulk-import** path, so
    > `a_bulk_import_with_the_checkpointer_off_reclaims_and_keeps_its_rows`
    > runs it over `bulk_import` and adds the assertion a file-size check
    > cannot make — that the 3,000 edges are still readable *after* the WAL is
    > truncated. Growing a WAL and reclaiming it says nothing about whether the
    > frames reached the database or were discarded.
    >
    > **Reading the rustdoc to check "documented" found it wrong** (D-167).
    > `checkpoint`'s summary line was *"Rebuild `links_current` from `links`…"*
    > — W5.2 inserted the method above `rebuild_current` in 0.12.13 and the
    > one-line doc stayed put, so `rebuild_current` had no documentation and
    > `checkpoint`'s index entry described a different operation. Eleven
    > releases. The appendix gate could not see it: it asks whether a method is
    > *named* in Appendix A, and `rebuild_current` still was.
    >
    > `every_public_database_method_has_a_doc_comment` now closes that,
    > verified by injection. Scoped to the handle rather than
    > `#![warn(missing_docs)]`, which reports 266 items crate-wide. The same
    > rustdoc also still claimed `TRUNCATE` alone where the method has run
    > `FULL` then `TRUNCATE` since it shipped — D-156 had it right and the
    > method's own docs did not.
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

> **Sharpened 0.13.1, against the 2026 survey.** The finding was written as an
> ambiguity in one parameter. It is larger than that, and the survey makes the
> larger version hard to unsee: Macrame reaches its two axes through **two
> unrelated mechanisms**. `as_of` filters live rows by valid time
> ([builder.rs:309](../src/graph/builder.rs:309)); `reconstruct` folds
> `transaction_log` to a transaction-time instant. There is no query for the
> cell where they cross — *what did we believe at T\_tx about what was true at
> T\_vt* — which is the question Jensen and Snodgrass's BCDM defines a
> bitemporal database as answering.
>
> This does not change W7.1's deliverable, and it does change its acceptance:
> splitting the parameter is only half done if the two halves still route to two
> mechanisms that cannot be composed. Setting both must be expressible, or
> refused by name — and F-33/W10.6 is the question of what that costs, which is
> why it is scheduled immediately after.

> **✅ Shipped 0.13.2 ([D-174](../docs/architecture/s13-decision-register.md#d-174)).**
> Expressible, not refused. `as_of` is gone; `as_of_valid` and `as_of_recorded`
> are independent and compose, and setting both is the BCDM cell.
>
> The sharpening is what made the work large. The rename was the small half; the
> large half is that `as_of_recorded` **had nowhere to read from** — valid time
> was reachable from a traversal and transaction time only from `reconstruct`,
> which folds the whole state and cannot be walked. So the walk now takes its
> edges from one of two relations exposing the same six columns: `links_current`
> under current belief, or a fold of `transaction_log` bounded at the instant.
> The fold is exact because links are strictly append-only, so the last log row
> per entity at or before `r` *is* what `links_current` held at `r`.
>
> It reads the **hot** log and refuses rather than guessing:
> `RecordedInstantUnreachable` names the instant and names `reconstruct`, which
> takes the archive path. Conservative by one bit — whether anything was *ever*
> archived, not whether this instant survived it — because the cutoff is not
> recorded hot-side (D-132 refused that marker).
>
> W10.6 now has something to measure, which it would not have had under the
> refuse-by-name reading, and §14's acceptance item 11 is reachable.
>
> Two things found on the way and closed with it: `AtTime` never consulted a
> concept's own valid interval (the smaller half of D-160, now bounded against
> the interval the *payload* recorded), and F-35/D-175.

**W7.2 — `write_annotations_atomic` goes through `classify`.** Closes §3.6. It
bypasses the classification the non-atomic path applies, so the same input
produces different stored state depending on which entry point was used. One of
the two is wrong; `classify` is the one with the tests.

> **✅ Shipped 0.13.3 ([D-176](../docs/architecture/s13-decision-register.md#d-176)).**
> It routes through `classify` with a new `WriteOp::Annotation`, and a missing
> concept is now `DbError::NotFound(concept_id)` instead of a bare
> `FOREIGN KEY constraint failed` out of a chunk of up to
> `chunk_rows::ANNOTATIONS` rows.
>
> **The finding's stated consequence is wrong and the finding is still real.**
> There is no non-atomic entry point — `write_analytics_annotations` is the only
> way in — so stored state never differed. What differed was what the caller was
> told, and that is enough.
>
> The omission had a *correct* argument behind it, which is why it survived:
> `analytics_annotations` carries no triggers, so `abort_kind` can only answer
> `NotAGuard` and `classify` would hand back what it was given. Sound about the
> guards, silent about the foreign key onto `concepts`, which the engine enforces
> itself. A guard vocabulary covering every guard is not one covering every
> failure.
>
> The detector matches the **extended result code**, so nothing here depends on
> wording — `abort_kind`'s one-place-for-text doctrine is unweakened rather than
> extended. Extended and not primary: code 19 also covers the canonical-timestamp
> CHECK on the same table, and a test pins that discrimination.
>
> **Carried, not closed: this covers the annotation path only.** `links` and
> `concepts` carry foreign keys too, and a violation on either still surfaces as
> the engine's own `FOREIGN KEY constraint failed` naming no row. §3.6 named one
> path and this closes that path; the argument for closing it applies to the
> other two unchanged, and they were left because each has its own guards and
> its own tests and W7.2's scope was this one. Recorded here as well as in
> [D-176](../docs/architecture/s13-decision-register.md#d-176), because a
> rejection inside a register entry is reasoned but not tracked, and this one
> should be picked up rather than rediscovered.

**W7.3 — `run_writer_actor`'s `Err` path.** Closes §3.5. It returns
`Result<()>` and can only ever return `Ok(())`
([connection.rs:2023](../src/connection.rs:2023)). Either give it a failure it
can actually report — a poisoned connection, a channel invariant violated — or
change the signature. A `Result` that is structurally always `Ok` trains readers
to skip it.

> **✅ Shipped 0.13.4 ([D-177](../docs/architecture/s13-decision-register.md#d-177)).**
> The signature. The first option was examined and there is nothing for an
> actor-level `Err` to carry: per-command failures go back on the command's own
> responder (D-014's rule that a failed assertion must not kill the writer), a
> dropped responder is documented as not an actor error, and the `else` exit is
> reachable only after `Database` was dropped — which drops the `JoinHandle`
> too, so no status could be read.
>
> **The finding understates itself.** "Trains readers to skip it" is what it
> cost a reader; what it cost the code is that `close()` matched
> `Ok(res) => res?` beside the `JoinError` arm, so two failure paths appeared
> where one existed — and the real one, a **panicked** actor with the caller's
> writes going nowhere, had no test. The mapping is now `writer_exit`, pinned
> against a real `JoinError` from a panicking task.
>
> A *poisoned connection* was the finding's own suggestion and is rejected with
> its reason recorded: the cheap probes for it are writes, so the detector would
> take the write lock every turn to guard against a bug no path currently has.

**W7.4 — Refuse a future `recorded_at`.** Closes §3.4. `recorded_at_floor`
([clock.rs:36](../src/util/clock.rs:36)) takes `MAX(recorded_at)` with no upper
bound, so one row stamped in 2087 — a clock skew, a bad import, a test fixture
that escaped — permanently pins the floor and every subsequent write inherits
it. Bound it at open, and refuse writes stamped beyond a tolerance rather than
absorbing them silently.

> **✅ Shipped 0.13.5 ([D-178](../docs/architecture/s13-decision-register.md#d-178)).**
> `DbError::FutureRecordedAt` at open, governed by
> `Tuning { future_stamps: FutureStampPolicy::Default | Tolerance(d) | Allow }`
> — `WalCheckpointPolicy`'s shape, for D-155's reason. Default tolerance is a
> day, generous because what is being caught is out by *years*; a tight bound
> would catch timezone-confused hosts instead. Crossed to Python in the same
> release as `future_stamps=None | seconds | "allow"` and
> `FutureRecordedAtError`.
>
> **The second clause is answered by the first, and the other reading of it is
> refused with its reason.** `recorded_at` is never caller-supplied — every one
> is `clock.now()` inside the actor — so "refuse writes stamped beyond a
> tolerance" is the writes already in the file, which is what
> `recorded_at_floor` being the only cited location implies. Read instead as a
> per-`now()` skew guard, it needs a reference the wall clock cannot move; a
> monotonic anchor does not advance across suspend on any target here, so a
> laptop resumed after a week is indistinguishable from a host skewed by a
> week, and clamping writes a stamp claiming the transaction happened last
> Tuesday.
>
> **The guard's first catch was this project's own fixtures.** `tests_py`'s
> clock `START` was 2030 — a fake set ahead of the wall clock, which is exactly
> the "test fixture that escaped" the finding names. It was wrong in every test
> in the file and observable in the one that reopened.

**W7.5 — `reject_overlaps_within`.** Closes §2.6. O(n²) over the batch. Sort by
`(source, target, edge_type, valid_from)` and check adjacent pairs; the guard's
semantics do not change.

> **✅ Shipped 0.13.6 ([D-179](../docs/architecture/s13-decision-register.md#d-179)).**
> Sorted and swept, `n log n`, and the semantics are unchanged as specified.
> The 20,000-edge batch that held the actor for **18.1 s** now holds it for
> **2.2 s** — the fan-out shape's own cost, so the shape term left
> [D-081](../docs/architecture/s13-decision-register.md#d-081)'s
> `estimated_bulk_hold` at the same time and it was re-fitted against fresh
> measurement.
>
> **"Check adjacent pairs" is not sufficient and the rewrite does not do it.**
> That construction is correct for plain intervals; it needs every adjacent pair
> to be *eligible*, and two are not — identical `valid_from` is re-assertion,
> and two open intervals belong to `trg_links_single_open`. `[5,20)`, `[5,6)`,
> `[7,8)` skips the first pair for equal `valid_from`, finds a clean gap in the
> second, and never looks at the plain overlap between the first and third. The
> sweep therefore carries the widest `valid_to` reached so far, plus a second
> one restricted to closed intervals, and advances in runs of equal
> `valid_from`. The failing case is a test.
>
> One thing does change that the item does not mention: the error names the
> *earlier* interval as the existing one. The pairwise version named whichever
> the caller passed first, and within one batch under one stamp that ordering
> means nothing.

**W7.6 — Bulk paths report progress and accept cancellation.** Closes §4.2.
`low_chunked` discards `written` on error ([connection.rs:1841](../src/connection.rs:1841)),
so a caller whose 20,000-row import fails at row 19,000 is told only that it
failed. Return the partial count in the error. Cancellation is the larger half —
a `CancellationToken` checked between chunks, which the chunk loop's shape
already makes natural.

> **✅ Shipped 0.13.8 ([D-181](../docs/architecture/s13-decision-register.md#d-181)).**
> Both halves, on all four chunked paths — `bulk_import`, `write_concepts`,
> `upsert_embeddings` and `write_analytics_annotations` — because the defect is
> `low_chunked`'s and not `bulk_import`'s.
>
> **The count comes back in a new error type, not the existing enum.** The four
> now return `Result<usize, BulkInterrupted>`, where `BulkInterrupted` is
> `{ written, cause }`. A new `DbError` variant was the obvious move and is the
> wrong one: every existing `matches!(err, DbError::SingleOpenViolation { .. })`
> would keep compiling and quietly stop matching, which is the worst outcome
> available to a change whose point is that callers should see *more*. A
> distinct type breaks those call sites at compile time instead — four of them
> in this repo's own suite, each of which wanted `.cause`. `From<BulkInterrupted>
> for DbError` keeps `?` working and drops the count, which puts the decision to
> ignore it at the call site.
>
> **Cancellation turned out to be the smaller half, not the larger one.** The
> chunk loop is already between transactions several times a second, so a
> `CancelToken` is one atomic load per chunk and needs no dependency — the item
> guessed right about the loop's shape. What the item did not anticipate is that
> the check has to sit *after* the "no rows left" test: a token tripped once the
> last chunk has committed reports success, because a race between two of the
> caller's own threads should not decide whether a finished import counts as one.
>
> `BulkProgress` (`written`, `total`, `rows`, `held`) after every commit, with
> `held` being the actor's own measurement — the number the D-058 controller
> steers on, not a wall-clock approximation of it.
>
> Python gets `progress=` and `cancel=` keyword-only on all four, a `CancelToken`
> class, `BulkCancelledError`, and a `written` attribute on every exception these
> paths raise. Two facts are the binding's own: the call holds the GIL released
> for its whole run, so the cancelling thread is by construction a different one;
> and a progress callback that raises cancels the write and propagates, rather
> than letting a broken progress bar read as a finished import.

**W7.7 — Record D-155:** which axis `as_of` now means, and what a caller who
wants the other one calls instead.


> **✅ Shipped 0.13.10 (recorded as
> [D-183](../docs/architecture/s13-decision-register.md#d-183), because D-155 was
> taken by 0.12.12's `Tuning` decision before this item was written and
> renumbering it would break the entries that cite it).**
>
> **The prose this item asks for already existed, and checking it against the
> crate is what the item was worth.** [D-174](../docs/architecture/s13-decision-register.md#d-174)
> answered *which axis* in 0.13.2 and `as_of_valid`'s rustdoc answers it at
> length. `DbError::AttributeModeUnstated` — the error a caller meets when they
> ask about the past — did not: it carried `as_of: String` and rendered
> *"traversal as_of(…) did not state an attribute mode"*, naming a method
> removed in 0.13.2. It was fed by `as_of_valid.or(as_of_recorded)`, so a caller
> who set the belief instant was told about `as_of`, and a caller who set both
> was told about one.
>
> This is [D-180](../docs/architecture/s13-decision-register.md#d-180) a second
> time, found the same way: by auditing a documentation item, because nothing in
> either suite asserted the sentence.
>
> The variant now carries `StatedInstants` — `Valid`, `Recorded` or `Both`,
> never neither, since the error exists *because* an instant was set — rendered
> as the calls that produce it. That is W7.7's second clause answered in the
> error itself: a caller who wanted the other axis reads what to call from what
> they were handed. Python gets `as_of_valid` and `as_of_recorded` in place of
> `as_of`.
>
> **The rustdoc gate had been red since 0.13.2.** `unresolved link to
> Self::as_of` in `attribute_mode`'s own doc comment — the method the error
> points callers at — plus three more from 0.13.2 and 0.13.5. CI runs that step
> under `RUSTDOCFLAGS: -D warnings`, so it was a failing build and not a
> warning. All four fixed.

---

## 10. W8 — The snapshot becomes durable and bounded

**W8.1 — `spawn_blocking` around snapshot save and load.** Closes §2.4.
Serialisation and file I/O currently run on a tokio worker, which is exactly what
`spawn_blocking` exists to prevent.


> **✅ Shipped 0.13.11 (recorded as
> [D-184](../docs/architecture/s13-decision-register.md#d-184)).**
>
> **The note that covered this was about a different axis.**
> [§5.5](../docs/architecture/s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)
> says snapshotting runs on the read side and never touches the write
> connection, which is true and is about the *connection*. bincode over the
> whole state, zstd over the result, a write and an `fsync` are synchronous to
> the last instruction, and they ran on whichever tokio worker was polling
> `write_final` — the pool the actor's own I/O is scheduled on. §9 budgets that
> at two seconds for 100K edges.
>
> **The load half was the easier one to miss.** `snapshot_anchor` decompresses
> and deserializes a whole `MaterializedState` on the async path of every
> composing `reconstruct`, which is the ordinary read path and has no shutdown
> to hide behind.
>
> Both go to `spawn_blocking`, one hop each: the write covers the save and the
> retention pass that follows it, the read covers the whole scan rather than
> one file, since the loop stops at the first usable snapshot and the common
> case reads exactly one. `save_snapshot` and `load_snapshot` stay synchronous
> — benches, tools and the suite call them from no runtime at all.
>
> **A lost thread means different things at the two ends, deliberately.** A
> `spawn_blocking` task cannot be cancelled, so a `JoinError` is a panic. On the
> write side that is the file `close()` promised to have written and it becomes
> `ReplayCorrupt`; on the read side it joins the incompatible and unreadable
> files the scan already skips — `None`, and fold from genesis. That closes a
> panic path too: inline, a panic in the loader unwound through `reconstruct`
> and took the caller's task with it, so one corrupt file could stop a process
> that had a correct answer available. **W8.4 fuzzes for exactly those
> panics**, and this is what happens to the ones it has not found yet.
>
> `newest_anchor_on_disk` stayed inline on purpose: one bounded `read_dir`, no
> file opened, run once when the cadence starts.
>
> **W8.5's "Record D-156" will not work either.** D-156 is 0.12.13's
> `checkpoint()` decision, the same collision W7.7 hit with D-155. Read the
> remaining "Record D-15x" items as *record a decision*.


**W8.2 — Snapshot format v3: bounded and checksummed.** Closes §3.3. bincode's
`DefaultOptions` carries an `Infinite` limit; serde's cautious-capacity blunts
the single huge `Vec::with_capacity`, so the practical failure is not one
catastrophic allocation but a deserializer working through a corrupt stream to
exhaustion. A v3 header with a declared length and a checksum turns that into an
immediate, named error.


> **✅ Shipped 0.13.12 (recorded as
> [D-185](../docs/architecture/s13-decision-register.md#d-185)).**
>
> **The failure is not the one "unbounded" suggests, and the item says so.**
> serde's cautious capacity already blunts the single huge allocation; what was
> left is a deserializer working through a corrupt stream until something runs
> out, on the file the crash path reaches for first. So the fix is to know the
> bytes are wrong before deserializing any of them.
>
> v3 carries `payload_len`, `plain_len` and a CRC-32 over the first 34 header
> bytes **and** the payload. Checks run framing → integrity → size: the length
> catches truncation and appended junk without hashing; the **checksum runs
> before zstd is handed a byte**, which is what closes §3.3; `plain_len` is
> enforced *during* decompression, bounding the reader to `plain_len + 1` so a
> frame that expands further stops one byte over the line rather than doing the
> work first and complaining after. The bincode limit is then the buffer's own
> length, replacing `Infinite`.
>
> **Both lengths sit under the checksum**, which is what makes a declared bound
> a bound rather than a suggestion — and is why there is no arbitrary size cap
> beside it. CRC-32 is detection, not authentication; the unit tests forge a
> *valid* checksum on purpose, so what they test is the reader after integrity
> has been satisfied deliberately.
>
> **Damage got its own name, and that is [D-069](../docs/architecture/s13-decision-register.md#d-069)
> in a place D-069 did not reach.** Every failure of `load_snapshot` was
> `ReplayCorrupt { seq: 0 }` — the variant meaning *the ledger is damaged*,
> carrying a sequence number that cannot exist. Three subjects, three names:
> `SnapshotIncompatible` (another build wrote it), `ReplayCorrupt` (the ledger),
> `SnapshotCorrupt` (the cache, and deleting the file is the repair).
>
> **§5.5's own header description had been wrong since 0.5.5** — "ten bytes",
> diagram stopping at offset 10, while D-054's note further down the same
> section already said eighteen. A correction filed beside the passage instead
> of into it, which is [D-183](../docs/architecture/s13-decision-register.md#d-183)'s
> shape a third time.
>
> v2 files are refused as `SnapshotIncompatible` and the scan folds from the
> log. That is the whole migration — nothing is in a snapshot that is not also
> in the ledger.


**W8.3 — fsync the directory after rename.** Closes §3.7. The rename is atomic
and the directory entry is not durable until the directory itself is synced —
the standard POSIX gap, and it matters on the crash path the snapshot exists
for.


> **✅ Shipped 0.13.13 (recorded as
> [D-186](../docs/architecture/s13-decision-register.md#d-186)).**
>
> **Atomic answers which file is at the name; it does not answer whether the
> name is on the disk.** The `fsync` before the rename covers the file's bytes,
> and the rename is a change to the *directory*. What a power loss takes in that
> window is the pointer — an intact snapshot under a name nothing looks for,
> while the newest name that still resolves is an older file. `save_snapshot`
> flushes the directory after the rename now.
>
> **The honest size of it is a slower start, never a wrong answer.** Folding
> from the previous anchor is correct by construction, because
> [Doctrine VI](../docs/architecture/s0-s3-foundations.md) makes a snapshot
> disposable. What rests on it is §5.1.7: `close()` promises a final anchor, and
> the crash where that anchor is the difference between a fast restart and a
> full fold is precisely the crash the guarantee was missing. A failed flush is
> therefore reported rather than logged — the file exists and is readable, so
> the error means *this function cannot promise the name outlives a power loss*,
> and `close()` is the caller whose promise that is.
>
> **Deletions get no flush, and the asymmetry is the argument.** A deletion a
> crash undoes resurrects a valid snapshot, which the next pass deletes again; a
> creation a crash undoes loses the anchor.
>
> **Windows gets a no-op with a name.** There is no directory `fsync`;
> `FlushFileBuffers` wants write access a directory handle does not grant, and
> the volume-wide call needs administrative privileges and flushes every open
> file on the volume. NTFS's metadata journal stands in for it — a weaker
> statement, assuming NTFS or ReFS — and it is written into the branch's own
> docs, because a gap closed on one platform and silently open on another is a
> false belief rather than a known defect.
>
> **These are the crate's first `#[cfg]` platform arms.** The `unix` body does
> not compile locally, so it was checked by temporarily building it under
> `#[cfg(windows)]` (compiles, clippy-clean) and CI's three-OS matrix runs it
> for real on two of three legs. No test can assert durability without cutting
> the power; what the new tests assert is that a directory descriptor accepts
> `fsync` at all (POSIX permits `EINVAL`), that the non-unix arm really is
> inert, and that the publish step is still a rename.


**W8.4 — Fuzz the loader.** Closes §5.4. `cargo-fuzz` over the v3 format, seeded
with valid snapshots. W8.2 gives it something to assert: a corrupt input should
produce a named error, never a panic and never an allocation storm.


> **✅ Shipped 0.13.14 (recorded as
> [D-187](../docs/architecture/s13-decision-register.md#d-187)).**
>
> **The obvious version of this item produces a fuzzer that tests the checksum
> four hundred million times.** W8.2 put a CRC-32 over 34 header bytes and the
> whole payload — exactly the shape coverage-guided mutation cannot solve — so a
> target handed raw bytes clears the magic in seconds and then never reaches
> zstd or bincode, which are the two components §3.3 named. W8.2 made this
> format fuzz-hostile on purpose, and W8.4's real content is getting past its
> own defences.
>
> **Three targets, one per layer**, with the inner two handed a container the
> harness builds around their input: `snapshot_container` (framing),
> `snapshot_payload` (plaintext → `bincode`'s decoder under W8.2's limit), and
> `snapshot_frame` (a declared length plus arbitrary payload bytes → zstd and
> the `plain_len` bound). The third is the one the checksum cannot help with: a
> decompression bomb's checksum is *correct*, so only the declared bound stands
> between the reader and full expansion. That is where **"never an allocation
> storm"** stops being rhetorical, and it is asserted by libFuzzer's
> `-malloc_limit_mb`, which is the tool built for it.
>
> **Seeds are generated, never committed** — a corpus of valid v3 files is
> correct until `SNAP_FORMAT_VERSION` next moves, after which the session starts
> from nothing while still looking seeded. Every seed is read back *out of* a
> snapshot `save_snapshot` wrote, so none of it is a second description of the
> writer.
>
> **It runs in CI and nowhere else, and the deterministic half is the answer to
> that.** `cargo-fuzz` needs nightly and libFuzzer and does not support Windows.
> So `src/temporal/snapshot.rs` asserts the same properties **exhaustively** in
> every `cargo test` on every platform: every single-bit flip of a real snapshot
> refused, every truncation and extension refused, every plaintext mutation
> behind a correct checksum answered rather than survived, and a 64 MiB bomb
> stopping 1,025 bytes in. Verified by mutation — disabling the checksum
> comparison turns it red at bit 0 of byte 10, inside `taken_at_micros`, a field
> nothing else guards.
>
> **§8 had been using the word "fuzz" since 0.4.0 for a differential oracle over
> generated histories** — a property test, and a good one, but not a fuzzer, and
> nothing in the repository generated unstructured input for anything. Fourth
> documentation finding of this wave, same shape as the others.


**W8.5 — Record D-156:** the v3 header, and why a format that a corrupt stream
can walk to exhaustion is not acceptable in a file the crash path depends on.


> **✅ Shipped 0.13.15 (recorded as
> [D-188](../docs/architecture/s13-decision-register.md#d-188)).**
>
> **The decision this item names was written when the work landed.** It is
> [D-185](../docs/architecture/s13-decision-register.md#d-185), from 0.13.12,
> and the number above is wrong for the second time in two waves — D-156 is
> 0.12.13's `checkpoint()`. **D-157 is `wal_autocheckpoint`, so W11.5's number
> is taken as well.**
>
> **So the item became the audit it implies, and the audit found something.**
> The wave's four items produced four entries, all cross-linked, and every
> coverage row names the release that closed it. What no gate covered was the
> crate's own shape: §3's layout tree lost `util/crc32.rs` in 0.13.12 and never
> gained `fuzz/` in 0.13.14 — three releases, every gate green, in the document
> a reader opens to find out what exists. The tree states its own exemption
> (*"the `tests/` and `bindings/` entries are shapes rather than inventories"*),
> which is a claim that everything else is a list, and it was not one. The same
> entry still said **28** integration targets against 35 on disk, so the line
> converted to a shape was carrying an inventory in its only number; the number
> is removed rather than corrected.
>
> **Fifth documentation finding of the wave, and the first one a gate could
> have caught.** The other four were found by writing the next version of a
> passage and checking the current one — a method that works and only inspects
> what someone happens to be rewriting. `doc_sync_tests` now walks `src/` and
> requires every module's file name to appear in §3's block: shallow on
> purpose, one direction only, and red on its first run with
> `["util/crc32.rs"]`.
>
> **It covers one of the two things found, and D-188 says so.** `fuzz/` is a
> directory, and a rule over the top level needs an exemption list — a second
> place for the same drift to hide — to guard an event that has happened twice
> in fourteen months.


---

## 11. W9 — Temporal and visibility completeness

**W9.1 — `hydrate_at_time` past the archive horizon.** Closes §3.2. Once rows
are archived, `AtTime` reconstruction silently returns less than the truth — it
reads the live tables and the archived interval is simply absent. Two options:
union the cold log into the fold, or return a named horizon error. **The error
is acceptable and silence is not** — Doctrine III says assertions are superseded,
not deleted, and a reconstruction that quietly omits superseded state is
reporting something the doctrine says cannot happen.


> **✅ Shipped 0.13.16 (recorded as
> [D-189](../docs/architecture/s13-decision-register.md#d-189)).**
>
> **The error, as the item says.** `hydrate_at_time` folds the hot
> `transaction_log`; `archive` moves *superseded* rows out of it, which is
> exactly what a past instant asks for. The newest row per entity never moves,
> so `reconstruct(now)` is safe by construction and this arm looked safe with
> it. For a `ts` back in an archived generation the fold found nothing, the
> entity was dropped, and the return type is a `Vec` in which absent means
> retired, means never existed, and means *the answer is in the other file*.
>
> **Nothing new was invented.** `hot_log_answers_for` and
> `RecordedInstantUnreachable` are W7.1's, written in 0.13.2 for
> `TraversalBuilder::as_of_recorded`. The crate had the right refusal and one
> of the two surfaces that fold the log had it — the one that arrived first.
> The guard now sits at the read rather than at the caller, so `execute` pays
> the reach test twice; `hydrate_attributes` is `pub` and reachable without any
> traversal, and a guard that lives at the caller is one the next caller does
> not inherit.
>
> **Scoped to the arm that folds.** `AtTime` has four cells and three of them
> read live `concepts`, which an archive cannot shorten — a concept is
> archivable only when retired, and the live arms filter retired. The test
> asserts that half too.
>
> **§5.6 had three stale claims**, all corrected here: *"never touches the
> log"*, the pre-0.13.2 `ts` signature, and a `tracing::warn!` D-085 replaced
> with a typed error in 0.6.0. `quickref` carried the same signature and the
> wrong return type.

**W9.2 — Prove it.** A test that archives, then reconstructs across the horizon,
and asserts the chosen behaviour. This is the finding most likely to be
"fixed" by a change nobody can demonstrate.

> **✅ Shipped 0.13.17 (recorded as
> [D-190](../docs/architecture/s13-decision-register.md#d-190)).**
>
> **The error names `reconstruct`, so the test is about `reconstruct`.** W9.1
> took the second exit — refuse, and redirect — which is only the right choice
> if the operation it redirects to answers. 0.13.16 shipped without anything
> checking that. A refusal that redirects is a claim about another operation,
> and the claim is the testable part.
>
> **The round trip, not the branch.** `what_the_hot_log_refuses_the_archive_
> path_still_answers` archives across the horizon, requires the refusal, then
> requires `db.reconstruct(instant)` — same connection, plus the archive path
> the error says is missing — to return what the hot reader returned *before*
> the archive ran. That value is read and compared against, never written as a
> literal in both places: a literal passes when both readers are wrong the same
> way.
>
> **Mutation-discriminated.** Forcing `hot_log_reach` to answer `Covers`
> unconditionally — the plausible wrong fix — fails it with `left: None`, which
> is §3.2's own shape one layer down.
>
> **The last arm is the horizon.** `reconstruct` with no archive path refuses
> too, so the boundary is a property of the ledger and not of one reader. The
> fixture is a `FakeClock` and could not be anything else — under the wall
> clock all three generations land in one microsecond and no cutoff falls
> between them — and it is now shared with W9.1's test.


W9.3 to W9.6 were added in 0.13.1 and close F-31 and F-32. They are in this wave
rather than W7 because they are the same claim as W9.1 pointed at a different
surface: **a read that quietly omits — or quietly includes — what the ledger says
is invisible is reporting something the doctrine says cannot happen.**

**W9.3 — one visibility predicate, applied where the join is, not where it is
convenient.** Closes F-31. `search_vector` must exclude retired concepts, and
after it does, `hybrid_search` is fixed by construction because its vector arm
*is* `search_vector`.

The implementation is a join, and the choice of which join is the whole decision:

- **Join `concepts` inside `search_vector`.** One predicate, one place, every
  caller covered including the two that compose it. Costs a join against a
  `TEXT PRIMARY KEY` on `k` rows — `vector_top_k` has already reduced the corpus
  to `k` by the time this runs, so it is `k` index seeks and not a scan.
- **Filter after the fact in each caller.** Three places to keep in step, and it
  is what produced this finding — `keyword_search` did its half and nothing
  propagated the obligation.

The first. **And it changes what `top_k` means, which must be decided rather
than discovered**: filtering after the index has already chosen `k` rows returns
fewer than `k`. Either the index is asked for `k′ > k` and the surplus absorbs
the retired rows — the escalation `FilteredVectorSearch::run_post_filter`
already implements for exactly this problem — or `top_k` becomes a ceiling
rather than a count. The former, because the latter is a silent behaviour change
for every existing caller, and because the machinery exists two modules away.


> **✅ Shipped 0.13.18 (recorded as
> [D-191](../docs/architecture/s13-decision-register.md#d-191)).**
>
> **The plan's design, taken as written.** `search_vector` joins `concepts` and
> applies one predicate, so `hybrid_search`'s vector arm and `PostFilter` are
> fixed by construction rather than by being remembered.
>
> **There were three readers, not two — the item's finding rather than its
> implementation.** `FilteredVectorSearch::run_pre_filter` scores candidate
> rows straight out of the embedding table and never touches `search_vector`.
> Fixing only `search_vector` would have left `search_filtered` honouring a
> retirement under `PostFilter` and ignoring it under `PreFilterCTE` — a wrong
> answer selected by a byte estimate, which reproduces on one machine and not
> the next. The predicate is a spliced constant and the third reader splices it
> too; with that arm reverted the gate fails at k=1 with the two strategies
> returning different concepts.
>
> **`top_k` is a count and stays one.** The escalation is a loop rather than
> the up-front selectivity estimate, because nothing else on this path needs
> the corpus size and computing it up front would put a `COUNT(*)` on every
> vector search to serve the case that almost never arises. Termination is by
> exhaustion: k′ doubles until the index has been asked for the whole table, at
> which point what came back **is** every visible neighbour.
>
> **Deferred deliberately:** `hybrid_search`'s inheritance is asserted in W9.6,
> which covers all four surfaces at once. `quickref` also carried two stale
> signatures in the same block — `keyword_search` and `reciprocal_rank_fusion`
> — corrected here.

**W9.4 — search reads at an instant, or says it does not.** Closes F-32.
`search_vector`, `keyword_search` and `hybrid_search` gain an optional valid-time
instant; with it set, the `concepts` join W9.3 introduces also bounds
`c.valid_from <= t AND t < c.valid_to`. Absent, behaviour is today's — the
current corpus — because D-155's lesson is that an absent knob must leave the
mechanism alone.

**The instant is the same parameter W7.1 splits `as_of` into**, and this is the
ordering argument for W9 following W7: adding a time parameter to search before
the crate has settled what a time parameter *means* would ship a third spelling
of the axis confusion 3.1 exists to end.

> **✅ Shipped 0.13.19 (recorded as
> [D-192](../docs/architecture/s13-decision-register.md#d-192)).**
>
> **The plan's design, taken as written.** All three surfaces take an optional
> `as_of_valid`, and with it set the `concepts` join W9.3 introduced also bounds
> `c.valid_from <= t AND t < c.valid_to`. Absent, the statement is byte-for-byte
> 0.13.18's.
>
> **The constant became a function of the parameter index.** The instant binds
> at `?4` in the vector search, `?3` in the keyword search, and after a variadic
> candidate chunk in `PreFilterCTE`, so one spliced string could not serve all
> three. It is the *index* that is spliced and never the timestamp, which stays
> a bound parameter on every path. `keyword_search`'s own `AND c.retired = 0` —
> the copy W9.3 left behind — folds into the shared clause in the same edit,
> because this is exactly the release that would have made the two disagree.
>
> **There were four readers.** `FilteredVectorSearch` takes no instant of its
> own; it reads the traversal's `as_of_valid`. A knob here would permit a
> filtered search whose filter is historical and whose ranking is not — the
> axis confusion §3.1 exists to end, with a setting attached. And it is the
> *stated* instant rather than `execute`'s `now_ts`, because `now_ts` is always
> present and binding it would valid-time-bound every filtered search ever
> written.
>
> **Transaction time is refused, not deferred.** The DiskANN index holds one row
> per concept and keeps no history, so there is no past vector to search at an
> `as_of_recorded`. That question goes to the ledger.
>
> Red first at the value on both gates. The discriminating arm is the instant
> *inside* the closed interval: a bound written as `valid_to = the sentinel`
> passes "absent" and "after" and fails only there.


**W9.5 — decay, once the instant exists.** The survey's §6.3 identifies
agent-memory retrieval that carries temporal context as the field's emptiest
quadrant, with BiteNet its only cited instance. Once W9.4 gives search an
instant, weighting a hit by the age of what it matched is arithmetic on numbers
already in hand.

**The trap is the sign, and it is worth naming before anyone writes the
multiply.** `search_vector` returns a **distance** and its results ascend —
`tests_py/test_vector.py::test_vector_search_scores_ascend`. `hybrid_search`
returns a **fused score** and its results descend —
`test_hybrid_scores_descend`. A decay factor in (0, 1] multiplied into a
similarity correctly penalises age; the same factor multiplied into a distance
makes stale rows look *nearer*. So decay is defined against similarity, and the
distance surface either converts or divides — and whichever it does is a test,
not a comment.

Scope discipline: one `half_life` parameter, defaulting to off, applied at
ranking and never at storage. Nothing about an embedding changes because time
passed; only its rank does.

> **✅ Shipped 0.13.20 (recorded as
> [D-193](../docs/architecture/s13-decision-register.md#d-193)).**
>
> **The trap is real on one surface and not the other, and that is the
> finding.** `search_vector` returns a distance, so it converts — similarity
> is `(2 - d) / 2`, clamped non-negative before the multiply, converted back so
> the score is still a distance. `keyword_search` returns bm25, which arrives
> *negative* with magnitude growing in relevance: it is a negated similarity
> already, so the plain multiply is correct there. **The operation that would
> have been the bug on the vector surface is the right one on the keyword
> surface**, which is why they are two functions with two tests rather than one
> shared helper written once and wrong in one place.
>
> **A half-life without an instant is refused**, `HalfLifeWithoutInstant`, with
> the fix in the sentence. Age is relative to something and no read path here
> reads a wall clock — that is what makes these answers pinnable at all.
>
> **Re-ranking a top-k is not the top-k of the re-ranking.** A decaying surface
> reads `rerank_depth(top_k) = max(5 × top_k, 50)` first — `HybridSearch::depth`'s
> rule, promoted to a shared function. A bound, not a guarantee, and stated as
> one. In `HybridSearch` the decay reaches each **arm**, because RRF adds ranks
> and a factor on the fused score would leave both orderings untouched.
>
> **`search_filtered` is not offered it.** The two strategies hold different
> pools, so decay inside each would make the answer a function of the byte
> estimate — the one thing D-050 forbids.
>
> Mutation-discriminated on both surfaces, with two different mutations:
> multiplying the distance fails the vector assertion, and dropping decay from
> the keyword arm fails the keyword one. The *sign* mutation on the keyword arm
> fails nothing, because there it is not a mutation — finding that out
> corrected this item's first draft.


**W9.6 — prove all three the way W9.2 proves W9.1.** A fixture with a retired
concept nearest the query and a valid-time-expired concept second, asserted
across all four search surfaces including `search_filtered` — whose safety is
currently accidental (F-31) and which must therefore be pinned as a
*requirement* rather than left to keep passing by composition.

> **✅ Shipped 0.13.21 (recorded as
> [D-194](../docs/architecture/s13-decision-register.md#d-194)).**
>
> **The gate is `tests/search_visibility_tests.rs`, and it asks five questions,
> not four.** `search_filtered` is asked once per strategy: the predicate is
> spliced into two statements that share no query, no access path and no
> ordering mechanism, and a visibility rule that holds under one plan and not
> the other is a wrong answer selected by a byte estimate.
>
> **The fixture ranks both hidden concepts first, on every surface, before it
> hides them.** Content is `TERM` repeated `8 - rank` times and padded to a
> constant eight tokens, so bm25 has term frequency as its only variable and
> the keyword order is the same total order as the vector order — asserted,
> not assumed, by the first of the test's three claims. Without it, "absent" is
> satisfied by a corpus that was never within reach.
>
> **The two are hidden by different mechanisms**, a `retired` flag and a
> `valid_to`, because they are separate terms of one clause and a surface can
> splice one and forget the other. `top_k` is required to stay a count in the
> same assertion: three asked for, three live concepts back.
>
> **The companion test is the absent knob.** With no instant stated the
> retirement still applies and the ended validity does not — so a build that
> valid-time-bounds every search whether or not one was asked for fails here,
> which the gate above alone would not catch.
>
> Mutation-discriminated three ways, each red at the value: dropping the
> valid-time terms from `visible_concept`, widening `VISIBLE_CONCEPT` to admit
> a retirement, and — the one the per-surface tests could not reach —
> dropping the instant from `run_pre_filter`'s splice alone, which fails at
> `search_filtered/PreFilterCTE` and nowhere else.


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

> **✅ Shipped 0.13.22 (recorded as
> [D-195](../docs/architecture/s13-decision-register.md#d-195)).**
>
> **The plan-shape half already existed**, in `index_plan_tests`, against the
> populated and analysed fixture W2.4 built. What this release adds is the
> operation-count half and one thing the plan asserted about the item: that
> "rows scanned" is not available. `sqlite3_stmt_scanstatus` needs
> `SQLITE_ENABLE_STMT_SCANSTATUS`, and `PRAGMA compile_options` on the vendored
> engine does not list it — measured, in `examples/opcode_probe.rs`, not
> assumed. The honest substitute is the **program** that would do the scanning.
>
> **`EXPLAIN` gives three integers that move with the plan and with nothing
> else**: cursors opened, seeks issued, b-tree rewinds. `tests/operation_count_
> tests.rs` pins the triple for the six queries `index_plan_tests` justifies an
> index with, plus a control that no index serves.
>
> **No single one of the three means what it looks like it means.** An index
> range scan with an open-ended bound rewinds its cursor exactly as a table scan
> does: the fold's `recorded_at <= ?1` carries `(2, 0, 1)` and the control
> carries `(1, 0, 1)`. The **triple** separates them and no component does,
> which is asserted rather than left implied — otherwise someone eventually
> simplifies the gate to `rewinds == 0` on a query where a rewind is correct.
>
> **The gate is strictly finer than the registry's assertion, demonstrated
> rather than claimed.** Add one column the covering index does not carry: the
> same index is still picked, `plan.contains(name)` still passes, and `opens`
> goes 1 → 2 because the row now has to be read. That gap *is* D-042's class —
> an index that captures a query without covering it. `EXPLAIN QUERY PLAN` does
> print the word `COVERING`, so a plan assertion could have caught this one;
> the registry keys on the index name on purpose, and an integer is a better
> instrument than a word for *how much* is read when coverage is lost.
>
> **The fixture moved to `tests/common/plan_fixture.rs`** and both files use it.
> Two costs are only comparable if the rows and the statistics behind them are
> the same rows and statistics, which is [D-088](../docs/architecture/s13-decision-register.md#d-088)'s
> rule; a copied fixture is how "the populated fixture" quietly becomes two.
>
> **D-055 is not overturned.** Wall-clock benchmarks stay advisory, exactly as
> it says. This observes that its reasoning was about timings.


**W10.2 — `PRAGMA optimize` gets a scheduled call site.** Whatever W2.2's
measurement recommended, made real and tested.

**W10.4 — Decide the low-priority fairness floor, on evidence that is not a
synthetic burst.** Added 0.12.10, after W4.4's counter falsified the premise
§19 rejected this on ([D-153](architecture/s13-decision-register.md#d-153)).

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
3. **Whatever the answer, record it and close §19's entry properly.** The
   rejection currently stands annotated rather than resolved, and inheriting it
   unread is the failure this wave exists to prevent.

**The counter must not be the only evidence.** It was added in the release that
would ship the fix, so a before/after taken with it alone measures the fix
against itself. That is why this is a 0.14.0 wave and not a 0.13.0 one.

**W10.5 — Split `CommandKind::Analyze`, then decide the budget exemption.**
Added 0.12.25, after §8's acceptance measured `ANALYZE`'s hold and found
[D-149](architecture/s13-decision-register.md#d-149) had overstated its bound
([D-166](architecture/s13-decision-register.md#d-166),
[D-168](architecture/s13-decision-register.md#d-168)).

`analyze()` misses `CHUNK_BUDGET` by ~6× on a populated ledger — 19.1 ms at
40,000 edges against 3 ms, measured — and will always miss it: `ANALYZE` is one
indivisible statement whose cost tracks the data, and `analysis_limit` damps it
3–4× without changing the shape. So `budget_violations()` names `analyze` after
every call, forever, which is the noise that teaches people to stop reading the
list.

**The exemption cannot be decided while the kind is shared.** `Analyze` covers
`optimize()` as well as `analyze()`, and `close()` calls `optimize()`
unconditionally. Exempting the kind silences the automatic path — a ~19 ms hold
on every handle close, reported nowhere — and that is the call nobody chose to
make. This is [D-152](architecture/s13-decision-register.md#d-152)'s lesson
arriving from the other side: there a shared kind *granted* an exemption nobody
had decided; here a shared kind would *launder* one.

So, in order:

1. **Split the kind** into `Analyze` and `Optimize`. `CommandKind` is
   `#[non_exhaustive]`, but `ALL`, `as_str()`, the exemption table and the
   Python string surface all move together — and D-152's own finding was that a
   split silently flips the new variant's exemption, so both must be stated
   explicitly at the split rather than inherited.
2. **Then decide each on its merits.** The explicit call is a caller asking for
   it, which is what every current exemption has in common. The one `close()`
   makes is not.
3. **Whichever way it goes, the table needs a `Bound` the kind can state.**
   Every existing row has one — frames accumulated, session row count. "The
   size of the table, damped 3–4×" is not a bound, and a row that cannot fill
   that column is the table admitting what it exists to prevent. If no honest
   bound exists, that is an argument against exempting rather than a formatting
   problem.

**Not deferred for caution.** Leaving it counted is the conservative option and
is what 0.13.0 ships: a permanent, documented, expected violation is worse noise
but better information than a silence nobody decided.
`analyze_is_not_budget_exempt_and_that_is_deliberate` pins it so the exemption
has to be argued rather than typed, and `ANALYSIS_LIMIT`'s rustdoc says why
lowering the limit is not the fix — that would buy the number by sampling too
little to separate the two `source_id`-leading indices, which is the whole
reason for having statistics.

**W10.3 — `Subgraph` interior, if measurement justifies it.** Closes §2.5.
String-keyed adjacency; index-based would be faster. **Benchmark first, and be
prepared to close this as "not worth it".** A `Subgraph` big enough for this to
matter may be rarer than the finding assumes, and this is the lowest-value item
in either release.

**W10.6 — The two-dimensional index question, measured before it is answered.**
Closes F-33. W7.1 makes bitemporal predicates expressible; this asks what they
cost and what, if anything, should be built for them.

**Measurement first, and the fixture already exists.** W2.4 builds a populated,
analysed plan-pinning fixture and W10.1 gates plan shape against it. So the
question is answerable with what those two waves leave behind: write the
cross-axis predicate W7.1 enables, take `EXPLAIN QUERY PLAN` and an operation
count, and see whether the planner seeks or scans. **This may close as "no index
needed", and that is a legitimate outcome** — the crate's ledger is
`recorded_at`-ordered by construction and `idx_txlog_time` already exists, so the
transaction-time bound may seek perfectly well on its own.

If it does not, the options in the order the evidence supports them:

1. **A second one-dimensional index, per domain.** What Kaufmann et al. (2015)'s
   Bitemporal Timeline Index does, and what it beat the spatial structures with.
   Cheapest to try, cheapest to revert, and it is the shape every index in this
   crate already has.
2. **A covering composite** leading on whichever bound discriminates. This is
   D-042/D-059/D-064's playbook, and W2's statistics are what finally make the
   planner able to choose between two candidates on selectivity rather than on
   column count.
3. **An R\*Tree bounding-box pre-filter, with an exact recheck.** Last, and only
   with its ceiling written into the code: `rtree` coordinates are **float32**
   and `rtree_i32` is **int32**, so neither can hold a microsecond epoch —
   float32 quantises epoch-seconds to ~128-second buckets, int32 overflows in
   2038. It can bound a candidate set; it can never be the authority, and the
   recheck against the canonical text columns is not optional. A design that
   forgets the recheck returns *nearly* the right answer, which is the worst
   failure mode available to a ledger.

**Whatever the answer, it is a decision-register entry.** The one thing this
wave must not produce is an index added because a paper said B-trees are
insufficient for a query nobody has timed.

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
9. No search surface returns a retired concept — asserted across all four,
   including the one that is currently safe by accident (W9.6).
10. A search at an instant returns what was valid at that instant, and a search
    with no instant returns today's corpus. Both demonstrated on one fixture.
11. The cross-axis predicate W7.1 enables has a recorded plan shape and an
    operation count, and W10.6's conclusion — index or no index — is a register
    entry with the numbers in it. *(W7.1 shipped 0.13.2 and made the predicate
    expressible; the measurement is W10.6's.)*

---

# v0.15.0 — branching

**This is the first release in this document driven by a use case rather than by
a finding, and the exception is stated rather than smuggled.** §0 says every item
either closes a finding or is a prerequisite for one that does. W12 does neither.
It is here because the crate's owner named a use it must serve before 1.0 freezes
the surface — **an agentic harness whose conversation tree forks at every turn** —
and because the alternative is worse: branching touches the schema, the
projection, the read builders and the Python surface, so a 1.0 that freezes
without it freezes against it.

W13 is a finding (F-34) and is here because branching is what makes it bite.

---

## 15. W12 — Branching: transaction time becomes a tree

### 15.1 What a branch is, stated before anything is built

The temptation is to treat a branch as a third axis beside valid time and
transaction time. It is not, and getting this wrong is how the schema acquires a
column that means nothing precise.

> **A branch is transaction time with a tree order instead of a line order.**

Valid time answers *when was this true in the world*. Transaction time answers
*when did we come to believe it*. A bitemporal ledger assumes belief arrives in
one sequence — the second axis is a total order because there is one history of
what the database was told. A branch is a fork in **that** sequence: two
divergent lineages of belief over the same valid-time domain.

Everything else falls out of this, which is the test of whether the framing is
right:

- **Valid time is untouched.** `valid_from` and `valid_to` mean exactly what they
  mean today, and no query over them changes.
- **Doctrine III is strengthened, not weakened.** Assertions are superseded,
  never deleted — and a branch never rewrites its parent, it appends elsewhere.
  Branching is the most append-only operation in the crate.
- **Doctrine II gains a clause, not an axis.** Two clocks on every row becomes
  two clocks and a lineage.
- **`recorded_at` stays a total order *within* a branch** and becomes a partial
  order across them, ordered by `(branch ancestry, recorded_at)`.

That last point is where the survey's distributed-systems material actually lands
for an embedded engine. §7 raises hybrid logical clocks for reconciling
retroactive updates across eventually-consistent replicas — irrelevant to a
single-writer single-file crate, and rejected in §16 on those grounds. But the
*problem* HLCs solve is turning a partial order over concurrent lineages into a
deterministic total one, and **branching creates that problem locally**, with the
branch id where the node id would be. If Macrame ever needs an HLC it will be for
this, not for replication.

### 15.2 The storage model: shared ledger, logical versions

The survey supplies the precedent directly. §6.3, on Gancarski et al. (1999)'s
Database Version model: valid and transaction time attached to *complete database
states* rather than to individual tuples, *"supports branching valid-time
histories, which is difficult to achieve in conventional temporal designs"*, and
— the load-bearing clause — *"separating logical database versions from physical
storage"*. That is 1999, and it is the only branching architecture in 54 papers.

Its lesson is the opposite of the obvious design:

- **Rejected: a `branch_id` tag interpreted per row.** Every table gains a
  column, every index gains a lead column, every query gains a predicate, and a
  fork means writing a row per inherited fact. Rost et al. (2022) and Hou et al.
  (2023) both measure the analogous choice for *time* — timestamps modelled as
  ordinary properties — and both find query cost growing with history. The same
  argument applies to lineage modelled as an ordinary property.
- **Taken: a branch is a named logical version over a shared physical ledger.**

```sql
CREATE TABLE branches (
    id          TEXT PRIMARY KEY,          -- ULID
    name        TEXT NOT NULL UNIQUE,
    parent_id   TEXT REFERENCES branches(id),  -- NULL for the root only
    forked_at   TEXT NOT NULL,             -- parent's recorded_at at the fork
    created_at  TEXT NOT NULL
);
```

`main` is the root: `parent_id IS NULL`, `forked_at` the epoch sentinel. Ledger
tables gain `branch_id TEXT NOT NULL DEFAULT 'main'` — the default is what makes
the migration a rung and not a rewrite.

A read on branch B resolves B's ancestry: rows written on B, plus rows written on
each ancestor A **before the fork point on the path down from A**. Copy-on-write.
A branch that has asserted nothing costs one row in `branches` and reads exactly
its parent.

**The fork must be O(1), and that requirement is what selects this design.** A
conversation tree forks at every turn; a fork that copies rows is a fork nobody
can afford at that rate. One `INSERT` into `branches` and nothing else.

### 15.3 The hard part, named rather than deferred

`links_current` is the projection that makes traversal fast, and it is
branch-agnostic today. Three ways out, and the crate should take them in this
order:

1. **Resolve at read.** `links_current` gains `branch_id`; a traversal joins the
   ancestry chain. Fork stays O(1); traversal gains a factor of chain depth, and
   `idx_lc_traversal_cover` gains `branch_id` as its lead column — a fourth index
   write per assertion on a table that already takes four.
2. **Hybrid: materialise the trunk, resolve the twigs.** A branch materialises
   its own projection only once it exceeds a row threshold. This is D-007's
   pattern exactly — two strategies, an arithmetic cost model, and the estimate
   returned to the caller — and `FilteredVectorSearch`'s pre-filter/post-filter
   choice is the working precedent for how it is exposed.
3. **Materialise per branch.** Storage multiplied by branch count and an O(rows)
   fork. Correct for a handful of long-lived branches; wrong for the use case
   that motivates the wave. Recorded so that it is rejected on evidence rather
   than re-proposed.

**(1) first, measured, with (2) as the escape hatch.** The measurement is the
deliverable: depth-3 traversal on a chain of 1, 10 and 100 branches, against the
same fixture unbranched.

### 15.4 What ships, and what deliberately does not

**In:**

- `Database::fork(name, from) -> BranchId`, `branches()`, and a branch-scoped
  read view. The view is a handle, which is why **W11.1 (`Database: Clone`) is a
  prerequisite** — a branch view is the same actor with a lineage attached, and
  cloning is how it stops being a second `Arc` in every caller's code.
- **`diff(a, b)`** — what branch A asserts that B does not. For the motivating
  use case this is the payload: *what did this exploration conclude that the
  trunk does not know*. It is also cheap in this design, because divergence is
  exactly the set of rows carrying the branch's own id.
- **Abandonment.** A conversation tree discards most of what it grows, so
  `archive` gains a branch-aware arm: an abandoned branch's rows are a contiguous
  archivable set by construction, which is the cheapest archive predicate in the
  crate.
- **Schema rung to v12**, with `a_version_bump_must_bring_its_own_rung_test`
  enforcing it and the existing-rows default proving the migration is additive.
- **The Python surface, in the same release.** W6's finding was that a binding
  gap opened in the release that created the feature never becomes a convention.

**Out, and stated as a decision rather than an omission:**

- **Merge.** Reconciling two belief lineages requires answering *which assertion
  wins*, and there is no doctrine-neutral answer — Doctrine III says assertions
  are superseded, and a merge has to choose a superseder on the caller's behalf.
  Fork, read, diff and abandon is a complete and useful feature without it. Merge
  is its own decision and should not be smuggled in under branching's schema
  rung.
- **Cross-branch traversal.** An edge from a node on one branch to a node on
  another is either a lineage violation or a merge in disguise. Refused, named,
  and tested.

### 15.5 The use case, written down because this document requires it

An agentic harness storing a conversation tree: each turn is a concept, each
reply an edge, each *alternative* continuation a fork. The requirements this
imposes, and which W12 is measured against:

| requirement | why the design meets it |
|---|---|
| fork at every turn | one row in `branches`, no copying (§15.2) |
| read a leaf's full lineage cheaply | ancestry resolution, depth-bounded (§15.3) |
| abandon most branches | branch-scoped archive predicate (§15.4) |
| ask what a branch concluded | `diff` (§15.4) |
| never corrupt the trunk | append-only by construction — Doctrine III (§15.1) |

---

## 16. W13 — A query AST, not a macro

Closes F-34, and is scoped by what W12 does to the read surface. After branching
there are three orthogonal qualifiers on every read — a branch, a valid-time
instant, a transaction-time instant — and four builders that would each need all
three.

**What this is:** a typed intermediate representation that the existing builders
lower into, so the three qualifiers are expressed once and composed rather than
re-implemented per surface.

**What this is not:** a `macrame_query! { MATCH … AT VALID … }` procedural macro.
That is the most-requested shape and the wrong first step — a proc-macro that
emits SQL strings is the least testable artefact this crate could produce, it
reintroduces string-splicing on the read path that D-039 removed from the
traversal CTE, and it fixes a syntax before anyone has used the semantics. The
survey's own precedents (T-PGQL, temporal XQuery, SQL:2011 statement modifiers)
are compilers over an algebra; the algebra is the part worth having, and a text
syntax over a working AST is additive whenever someone wants it.

**This is the first item to cut if 0.15.0 grows.** Said in §0.4 and repeated here
so it is visible from both places.

---

## 17. Acceptance for 0.15.0

1. A fork is O(1) in rows written, demonstrated by a test that forks 1,000 times
   and asserts the row count in every ledger table is unchanged.
2. A branch reads its parent's history and its own, and the trunk is byte-identical
   before and after a child branch is written to and abandoned.
3. Traversal cost against branch-chain depth is measured and recorded, and the
   strategy choice (§15.3) is a decision-register entry with the numbers in it.
4. Cross-branch edges are refused with a named error, with a test.
5. `diff(a, b)` returns exactly the assertions carrying `a`'s lineage and no
   others, over a fixture where the two branches disagree about the same edge.
6. Schema v12 migrates a populated v11 database with every existing row on
   `main`, and the rung test passes.
7. Python reaches the whole of it in the same release, including `diff`.
8. Merge and cross-branch traversal are recorded in §16 as refused, with reasons.

---

## 18. What 1.0 promises after this, and what it does not

**Promises.** The public API is stable for 1.x. The on-disk format is stable or
migrated by a rung. `CHUNK_BUDGET` is observable in the default build, and
`violations()` is the honest answer to whether it is met. Query plans are
pinned against a fixture whose statistics match production's. The snapshot
format is bounded and checksummed. Every doctrine has a test that fails when it
is violated. **No read surface returns what the ledger says is invisible** — the
promise F-31 showed was never actually held. **The two time axes are separately
addressable**, and where they cannot be composed the refusal is named.
**Branching is a supported topology**, with an O(1) fork and a trunk that a child
cannot corrupt.

**Does not promise.** That `CHUNK_BUDGET` is always met — it is a budget the
actor steers on, and on a populated `links` table it is still missed by ~0.2 ms
at the floor. That benchmarks are reproducible across sessions; D-070 says
otherwise and that is a property of the measurement, not of the code. That
concurrent opens are safe on Windows, unless W1 says they are. That single-row
writes are fast; they cost the ~0.8 ms transaction floor by construction, and the
bulk paths exist for that reason. That branches can be **merged** — they can be
forked, read, compared and abandoned, and merge is refused with a reason (§19).
That traversal cost is independent of branch-chain depth; §15.3 says which
strategy ships and what it costs. That there is a query *language*; W13 delivers
an algebra, and a syntax over it is a 1.x addition if anyone wants one.

**The distinction is the deliverable.** A 1.0 that promises less and means all of
it is worth more than one that promises a latency bound it cannot measure — which
is the release Macrame would have shipped if §4.3 had gone the other way.

---

## 19. Rejected before starting

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

### Added 0.13.1, after the 2026 bitemporal survey

Four proposals that came in with §0.4's findings and are refused here rather than
scheduled. Recorded because each is the *obvious* next step from the survey, and
an unrecorded rejection is one that gets re-proposed every time someone reads the
paper.

**Hybrid logical clocks and CRDT conflict resolution for edge replicas.** The
survey's §7 raises this as the cloud-native gap, and it is a real gap in the
field. It is not one here: Macrame is a single-writer, single-file embedded
engine, and the property that makes retroactive edits reconcilable — one actor,
one connection, one total order on `recorded_at` — is doctrine rather than
accident. Adopting an HLC now would add a clock nothing can currently make
disagree. **What is worth carrying forward is the observation, not the
mechanism**: the partial-order problem an HLC solves is the one *branching*
creates locally (§15.1), with the branch id where the node id would be. If a
clock is ever needed it will arrive from W12, not from replication.

**An R\*Tree as the bitemporal index.** Not rejected — deferred to a
measurement, W10.6 — but the *drop-in* version is rejected outright, and for an
arithmetic reason rather than a design preference: SQLite's `rtree` coordinates
are float32 and `rtree_i32`'s are int32, and this crate's timestamps are
microsecond ISO-8601 text (D-029). Neither type can hold the value. An R\*Tree
here can bound a candidate set and must never be the authority; a design without
the exact recheck returns nearly the right answer, which on a ledger is the worst
available failure.

**A `macrame_query!` procedural macro.** See W13. The algebra is the part worth
having; a macro that emits SQL strings reintroduces exactly what D-039 removed
from the traversal CTE, and it fixes a syntax before anyone has used the
semantics.

**Merge, as part of branching.** §15.4. Fork, read, diff and abandon is a
complete feature. Merging two belief lineages requires deciding which assertion
supersedes which on the caller's behalf, and Doctrine III has no neutral answer
to that. It is its own decision and must not ride in on W12's schema rung.
