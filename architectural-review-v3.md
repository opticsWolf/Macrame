# Macrame — Architectural Review (v3)

**Date:** 2026-08-07
**Version reviewed:** 0.9.0 (schema v10), working tree at `5fa1056`
**Method:** every claim in `architectural-review-v2.md` re-derived from source rather than from prose, plus an independent pass over the areas v2 did not open.
**Status:** extends `architectural-review-v2.md`. v2 is substantially correct — all three of its retractions hold and none of its findings collapses. This file **upholds** ten of its items, **widens** four, **corrects** two, and adds **six new findings**, one of them the most serious open item in the crate.

---

## 1. Executive summary

- **v2's headline conclusion survives.** There is no open code-level 1.0 blocker in the two places the *first* review put one. The D-059 index shipped in v6; the concept payload has carried `embedding_model` since 0.5.6. I re-verified both against `ddl.rs`, the migration rungs, and the tests that pin them, and neither retraction depends on prose.
- **The stale-prose problem is roughly twice as large as v2 reported, and it reaches a normative document.** v2 named four locations for the "single assertion is O(out-degree)" claim. There are **nine**, and two of them are in `§9`, which the architecture set marks normative. One is in `src/connection.rs`'s `CHUNK_BUDGET` doc comment, which v2 did not name.
- **It is not an oversight, which changes the remedy.** `D-127` (`s13-decision-register.md:1528`) records *"Dropping the 'not met at high out-degree' caveat now that the row has a number under budget"* as an option that was **considered and rejected** at 0.8.0 — two releases after the index shipped. The claim was re-affirmed by decision. So the fix is a measurement and a register entry, not an editing sweep, and a sweep alone would be the third time this claim moved without evidence.
- **The bench v2 asked for already exists.** `benches/budgets.rs:1101` (`overlap_guard`) measures `assert_edge` against a hub at degree 0 and degree 2,000 **with the index present**. Its numbers have never been published. This is the cheapest high-value action available: run one bench group, publish two numbers, settle a claim that has now misled two reviews.
- **One new finding is more serious than anything in v2.** A committed table named `macrame_archive_session` silently disarms all three delete guards *and* silences the concepts insert log — and nothing in the crate ever checks that it is absent. §3.1 (N1).
- **No code-level invariant violation was found**, in v2's pass or in mine. The eight doctrine invariants are enforced at the documented points.

---

## 2. v2's findings, re-checked against source

| v2 § | Finding | Verdict | Evidence I checked |
|---|---|---|---|
| 3.1 | "D-059 index not applied" is wrong | **Upheld** | `ddl.rs:524` (baseline `CREATE_INDICES`); `migrations.rs` rung `5→6`; `migration_tests.rs:319–342` drops and re-migrates; `index_plan_tests.rs:81` registry entry; `budgets.rs:1508` drops it to measure the pre-fix arm |
| 3.2 | "payload missing `embedding_model`" is wrong | **Upheld** | `ddl.rs:157–177` — `CREATE_CONCEPTS_LOG_INSERT` writes `'v', 2, … 'embedding_model', NEW.embedding_model`; the `CREATE_TRIGGERS` doc comment at `ddl.rs:546–550` documents the v1→v2 bump and the forward-compat branch |
| 3.3 | "`nul.pdb` committed" is wrong | **Upheld** | `git ls-files` matches nothing for `\.pdb$|nul`; `.gitignore:10` is `*.pdb`. The 1.15 MB file is on disk and untracked; `git status --short` shows only `architectural-review-v2.md` |
| 4.A | D-059 prose is stale and contradictory | **Upheld and widened** | See §2.1 — nine locations, two normative, plus D-127's explicit rejection |
| 4.B | §4.3 restoration note is stale | **Upheld** | `s4-schema.md:364` still carries the 0.5.4 blockquote verbatim |
| 4.C | quickref §8 counts are stale | **Upheld** | `quickref.md:747`: *"296 Rust … 344 Python (measured 2026-08-02)"* against `README.md:142`'s 330/339/362/353 at 2026-08-07 |
| 4.D | §3 crate diagram diverges | **Upheld and widened** | See §2.2 — four divergences, not one |
| 5.1 | Python diagnostic path is an R15 exposure | **Upheld, and the root is one layer lower** | See §2.3 |
| 5.2 | Overlap guard is O(version-count), "undocumented" | **Corrected** | It *is* documented, precisely, at `connection.rs:2264–2269`: *"the rows it returns are the intervals recorded for one `(source, target, edge_type)` — a version count, not an out-degree."* The characteristic is right; "undocumented" is not. What is undocumented is the user-facing complexity claim, which is 4.A |
| 5.3 | No `recorded_at` guard on concept INSERT | **Upheld** | `ddl.rs:592` — `trg_concepts_monotonic_ra` is `BEFORE UPDATE ON concepts`, and there is no INSERT counterpart |
| 6.4 | `raw()` is `#[doc(hidden)]` public | **Upheld** | `connection.rs:925`; the rustdoc above it (lines 876–924) already argues the convention at length. v2's suggested one-line sentinel would be the *only* thing a binding contributor sees, since the rest is `#[doc(hidden)]` and never renders |
| 6.5 | `is_closed` unasserted at algorithm entry | **Upheld and sharpened** | `subgraph.rs:459` says *"Used by tests and `debug_assert`s"* — and there is **no** `debug_assert` anywhere in `src/` that calls it. The rustdoc describes a hardening that was never written. Every real call site is a test (`wave1_regression_tests.rs`, `tests_py/`) |
| 6.1–6.3, 6.6–6.8 | Confirmed architectural items | **Upheld** | Spot-checked `metrics.rs` (queue depth is exposed), `migrations.rs:193` (forward-version refusal), `archive.rs:503` (copy/delete symmetry `debug_assert`) |
| 4.A opt. 2 | "Add a bench measuring `assert_edge` into a hub with the index present" | **Corrected — it exists** | `budgets.rs:1101` `overlap_guard`, registered at `budgets.rs:1601`, arms at degree `0` and `2_000 * scale()`, index present. Its rustdoc states the hypothesis explicitly: *"out-degree should not matter, and if it does the index is not being used the way `the_single_open_probe_seeks_rather_than_scans` says it is"* |

### 2.1 Widening 4.A — the blast radius, and why it is not an editing problem

v2 named four locations. The full inventory:

| # | Location | Text | Normative? |
|---|---|---|---|
| 1 | `README.md:191` | *"258 µs, and **still O(out-degree), not O(1)** (D-059)"* | product front page |
| 2 | `README.md:224` | *"it remains linear in out-degree"* | " |
| 3 | `README.md:235–237` | *"remains linear in out-degree, so a high-degree hub still exceeds it"* | " |
| 4 | `docs/quickref.md:694` (§6.2) | *"**Not met at high out-degree** (D-059): the single-open guard scans the source's whole out-degree, so it is O(degree), not O(1)"* | reference |
| 5 | `src/connection.rs:73–77` (`chunk_rows::EDGES`) | *"…served by `idx_lc_traversal_cover` with only `source_id` bound and therefore scans the whole out-degree. That is a schema defect with a proven fix, recorded in D-059 and **not applied here**."* | rustdoc, ships to docs.rs |
| 6 | **`src/connection.rs:54–63`** (`CHUNK_BUDGET`, "Known limitation") | *"The same 90-edge chunk takes 47.7 ms against an 8,000-edge hub … most of that gap being the schema defect D-059 documents."* | rustdoc — **not named by v2** |
| 7 | **`s6-s10-flows-to-dependencies.md:245`** (§9 budget table) | *"**Not met at high out-degree** … so this is O(degree), not O(1)"* | **normative** — **not named by v2** |
| 8 | **`s6-s10-flows-to-dependencies.md:264`** (§9 chunk row) | *"2.39 ms measured on an empty database; 47.7 ms against an 8,000-edge hub, which is the `trg_links_single_open` index defect (D-059)"* | **normative** — **not named by v2** |
| 9 | `s13-decision-register.md:1504, 1520` (D-118/D-127) | *"the single-open guard **still** scans the source's whole out-degree (D-059), so a high-degree hub still exceeds 5 ms"* | register |

Locations 6 and 8 are the sharpest, because they do not merely repeat a complexity class — they publish **47.7 ms**, which is the *pre-index* measurement from D-059, as the current cost of a 90-edge chunk into an 8,000-edge hub. `ddl.rs:512` states the post-index figure for the identical operation as **8.0 ms and flat**. The same repository publishes both numbers for the same thing, 460 lines apart.

**And this was a decision, not a lapse.** `s13-decision-register.md:1528` lists among D-127's rejected options:

> *Dropping the "not met at high out-degree" caveat now that the row has a number under budget — the fixture is not the hazard, the complexity is.*

That reasoning is sound in general and, on the current schema, appears to be arguing about a complexity the v6 rung removed. Two consequences:

1. A prose sweep would be the **third** time this claim moved without a measurement behind it, in a project whose D-088 forbids shipping an unmeasured performance claim.
2. The remedy has to be the measurement — which, per the correction above, is already implemented and simply never run for publication.

**What is true on a v10 file, from the code:** `trg_links_single_open`'s `EXISTS` (`ddl.rs:572–582`) binds `source_id`, `target_id`, `edge_type` and `valid_to` as equalities with `valid_from <>` residual — four equality columns against `idx_lc_open_interval`'s five. `OVERLAP_CANDIDATES` (`connection.rs:2270`) binds three. Neither is out-degree-shaped. Both are bounded by one edge key's *version count*, which `connection.rs:2266` already says in exactly those words.

### 2.2 Widening 4.D — `s0-s3-foundations.md` has four divergences, not one

| Line | Says | Is |
|---|---|---|
| `s0-s3:56` | the `graph/` box in the §2 context diagram contains **`petgraph`** | there is no `petgraph` dependency; `README.md:25` advertises *"zero external dependencies"* for the five analytics, and `graph/algorithms.rs` is native |
| `s0-s3:83` | *"no low-priority transaction may hold the lock longer than one **500–1,000-row chunk**"* | superseded by D-058: `chunk_rows` is 90 / 70 / 600 / 30, and §9 line 263 already marks the 500-row row *superseded*. Same stale constant at `appendices.md:272` and `architecture/README.md:26` |
| `s0-s3:114–119` | the `vector/` tree lists `mod`, `embedding`, `model`, `registry`, `search` | `src/vector/hybrid.rs` (10.5 KB, RRF fusion) is absent from the tree while appearing in the §2 diagram above it |
| `s0-s3:136–142` | the `tests/` tree lists six files, headed by `harness.rs` | there are 33 test files, and `harness.rs` is at `tests/common/harness.rs`. The whole `bindings/` tree — the 0.9.0 product's second surface — does not appear in the crate layout at all |

`quickref.md:120` (§3.3 module map) is current and correct, so this is one document lagging, not a systemic problem.

### 2.3 Deepening 5.1 — the Python exposure is a symptom; the Rust API is the source

v2 is right that `PyDatabase::diagnostic_query` / `explain` open a fresh `SQLITE_OPEN_READ_ONLY` connection per call under a **read** lock (`database.rs:1045–1050`, `1061–1066`; `with_db` takes `inner.read()` at `database.rs:91`), so N Python threads produce N concurrent opens — the R15 shape the project measures at 2/12 faults on 48 threads.

What v2 does not say is where the warning is missing. `bindings/python/src/rows.rs:19–22` **does** carry the caveat, and correctly limits its own claim to *sequential* opens. The place with no caveat at all is `Database::diagnostic_conn` itself (`connection.rs:699–766`): 67 lines of rustdoc, a measured four-row permission table, a section on `CREATE TEMP TABLE` being *more* permissive, an `# Errors` section — and **no mention of R15, of concurrency, or of the fact that this method's entire purpose is to open the file again**. R15 is the crate's top "Known Risk" in the README and it is invisible from the one public API that triggers it on demand.

A Rust caller who spawns per-request diagnostic queries has the same exposure as the Python caller and less warning. Fixing only the binding fixes the symptom.

---

## 3. New findings

### 3.1 N1 — [High] A committed archive-session marker is an undetected disarm switch

**What.** Three delete guards and one log trigger are gated on the absence of a table named `macrame_archive_session` in `main`:

| Object | Gate | Effect if the marker exists |
|---|---|---|
| `trg_concepts_guard_delete` (`ddl.rs:204`) | `WHEN NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='macrame_archive_session')` | `DELETE FROM concepts` is permitted |
| `trg_links_guard_delete` (`ddl.rs:649`) | same | `DELETE FROM links` is permitted |
| `trg_txlog_guard_delete` (`ddl.rs:~665`) | same | `DELETE FROM transaction_log` is permitted |
| `trg_concepts_log_insert` (`ddl.rs:157`) | same | **every concept insert stops writing its ledger row, silently** |

That is Doctrine V (no physical deletion in hot tables) and Doctrine IV (the ledger is a table) both suspended by the presence of one empty table, with no error, no warning and no counter.

**Why the existing argument does not cover it.** `s5-modules.md:633` states the safety case:

> *"`CREATE TABLE macrame_archive_session (x)` is the first statement inside the transaction and `DROP TABLE` is the last, so commit drops it and rollback discards it. There is no crash path that leaves the delete guards disarmed."*

I verified that claim and it holds — `archive.rs:302/401` and `583/683` bracket the marker inside the session transaction, so a crash rolls it back. **The claim is about crashes.** It says nothing about a writer that creates the table and leaves it, and the project's own §4.7 explicitly concedes that raw writers exist: `Database::raw()` is public-but-hidden, the file is reachable by any SQLite client on the machine, and `storage_boundary_tests` exists precisely to assert what the storage layer permits.

**Why this one is different from the other §4.7 holes.** The documented holes are *acts*: a raw writer can insert an overlapping interval, or a row with a negative weight. Each one damages what it touches and is detectable by `audit_current()` or by the loader guard. This one is a **latent mode switch**: one `CREATE TABLE macrame_archive_session (x)` from any client, at any time, permanently converts the ledger into a mutable table for every subsequent writer *including the actor*, and the resulting damage is invisible to every check the crate has.

**Nothing checks for it.** I grepped every use of `ARCHIVE_SESSION_MARKER` and `macrame_archive_session` across `src/`, `tests/` and `docs/`:

- `migrations::verify` (`migrations.rs:900–967`) checks that baseline tables, triggers and indices are **present**, and that the three delete-guard bodies **contain** the marker name. It never checks that the marker table is **absent**.
- `audit_current` (`integrity/audit.rs`) compares `links_current` against the latest-belief projection. Nothing else.
- No test asserts the absence. The tests that touch the marker (`integrity_tests.rs:269`, `migration_tests.rs:1213`, `replay_snapshot_tests.rs:533`) all *create* it deliberately to exercise the gated path.

**Cost of the fix.** One `SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?` inside `verify()`, which already runs at `Database::open` and already reads `sqlite_master` for the checks above — so this is one more row in a query that is already being made. **No schema rung.** The visible cost is that a database carrying a leaked marker is refused at open instead of opened, which is the correct failure and needs a release note.

**Severity: High.** Not because it is likely — it requires a raw writer — but because it is the only place in the crate where a *single, silent, external* act suspends two doctrine invariants at once, and because the check that would catch it is one line in a function that is already reading the table it would read.

### 3.2 N2 — [Medium] R15 is undocumented on `Database::diagnostic_conn`

See §2.3. The binding-level mitigation v2 recommends (a `Mutex` around the diagnostic path) is right and should ship, but the rustdoc on `connection.rs:750` is where a Rust caller decides how to use this, and it currently reads as though repeated opens are free. One paragraph, cross-referenced to the README's R15 row and `examples/r15_soak.rs`.

### 3.3 N3 — [Medium] The O(out-degree) caveat is a standing decision, so retiring it needs a decision

See §2.1. Recording this as a finding separately from 4.A because it changes who has to act: 4.A is an editing task that a doc pass can do; N3 says a doc pass **must not** do it alone, because D-127 rejected exactly that edit on stated grounds. What retires the caveat is `overlap_guard`'s two numbers plus a register entry that supersedes D-127's rejection.

### 3.4 N4 — [Low] `readonly_open_probe` does not probe `ATTACH`

`examples/readonly_open_probe.rs` establishes the read-only boundary with five probes: `SELECT`, `EXPLAIN QUERY PLAN`, `INSERT`, `CREATE TABLE`, `CREATE TEMP TABLE`, `PRAGMA query_only = OFF`, and `INSERT` after it. It is careful, and it found the one genuinely surprising result the crate documents (`CREATE TEMP TABLE` succeeds where `read_conn()` refuses).

It does not probe `ATTACH`. That matters here rather than in general because `diagnostic_query` passes **arbitrary caller SQL** to that connection from Python — it is the only general-purpose SQL surface the binding exposes — and whether an attached database inherits the main connection's read-only flags is exactly the kind of engine-behaviour question this project measures rather than assumes. If it does not inherit them, the "OS-level boundary, not a reversible pragma" claim has a documented exception it does not currently name.

**I did not run it.** This is stated as an unverified gap in a probe, not as a defect. Two rows in an existing example, one run, and it is either closed or it becomes a real finding.

### 3.5 N5 — [Low] Low-priority starvation is unbounded by design and unstated as a hazard

`run_writer_actor` (`connection.rs:1788`) is a `tokio::select!` with `biased;` and high-priority first. `concurrency_tests.rs:101` pins that behaviour deliberately and well — 60 queued chunks, 8 later high-priority asserts, all 8 serviced first — and D-010 calls it *"strict preemptive ordering"*.

The consequence is that a sustained high-priority arrival rate starves the low-priority tier **indefinitely**. There is no aging, no reservation, and no bound. For the intended profile (interactive desktop writes, background bulk at idle) that is the right trade and clearly the intended one. It becomes a hazard for a caller who drives writes from a loop rather than from a UI: `bulk_import`, `archive`, `rebuild_current_chunked` and `write_analytics_annotations` all ride the low tier and can make no progress at all.

It is **observable** — `MetricsSnapshot` exposes `low_depth_mean` and `low_depth_max` (`metrics.rs:454–455`), sampled before each turn, which is precisely the signal — so the gap is documentation, not instrumentation. One paragraph in §5.1 pointing at `low_depth_max` as the detector.

### 3.6 N6 — [Low] The `--all-features` test count is measured by no CI job

`README.md:142` publishes *"330 Rust · 339 with `metrics` · **362 with `--all-features`** · 353 Python"*. `.github/workflows/ci.yml` runs:

- `clippy --all-targets --features "metrics property-tests"` (lint only)
- `cargo check --all-features --all-targets` on the MSRV toolchain (check only, no run)
- `run_rust_suite.py --features metrics`
- `run_rust_suite.py --features property-tests`

No job runs the suite at `--all-features`. The 362 figure is a local measurement that CI cannot regress against, in a repository whose whole discipline is that published numbers have a gate behind them. Either add the combination or publish the two numbers CI actually produces.

### 3.7 N7 — [Info] `nul.pdb` is a symptom worth one line, not a finding

v2 is right that it is untracked and gitignored. It is nonetheless 1.15 MB of build residue in the repository root, and the name means something wrote to a path called `nul` under a shell that does not treat it as the Windows null device (Git Bash / MSYS). If a script in `scripts/` or a workflow redirects to `nul`, it will keep producing these. No action beyond deleting it and, if a redirect is found, changing it to `/dev/null`.

---

## 4. Confirmed strengths

Stated so the bar is visible, and because most of this review is about a documentation defect in a project whose documentation is unusually good.

- **Plan-pinning is a registry, not a reaction.** `index_plan_tests.rs` inverts the direction: every index must name the query that justifies it, `the_unread_index_set_is_empty` makes an unread index a red test, and `every_reproduced_query_still_exists_in_its_source` bounds the copies with `include_str!`. This is the right shape and it is the template for the gate §5 proposes.
- **`migrations::verify` checks guard *bodies*, not just names** (`migrations.rs:944–967`), after D-126 measured that `CREATE TRIGGER IF NOT EXISTS` silently keeps a stale body. That is the exact class of defect N1 sits in — the machinery to catch N1 already exists and is already running; it simply asks one question fewer than it should.
- **The forward-version refusal is present and well-worded** (`migrations.rs:193`), and the 0.9.0 release note publishes the exact refusal text.
- **`doc_sync_tests` gates what it claims to gate** — the `DbError` variant set and Appendix A's method mentions — and is explicit at `doc_sync_tests.rs:21–28` about being deliberately shallow. It does not claim to gate performance prose, which is why §2.1's drift is invisible to it and why §5 proposes a second registry rather than tightening this one.
- **The async→sync boundary is correctly shaped** and each of its four hazards is named in `runtime.rs` / `database.rs` module docs: single `OnceLock` runtime (no cross-runtime `Drop` panic), `frozen` + `RwLock` (no `PyBorrowMutError`), lock acquired **inside** `detach` (no `close()` deadlock), `os.register_at_fork` guard turning a hang into a typed error.
- **Every performance number in the 0.9.0 release note carries a control** (`control/select_1`), a fixture name, and a spread. The 0.9.0 note's "nothing moved, which is the result rather than the absence of one" is the correct way to publish a null result.

---

## 5. Summary table

| # | Sev | Finding | Type | Action |
|---|---|---|---|---|
| N1 | **High** | Committed `macrame_archive_session` disarms three delete guards and the concepts log; nothing checks | Invariant | One `sqlite_master` probe in `verify()`; typed error; test |
| 4.A | **High** | "O(out-degree)" claim stale in **9** locations, 2 normative, 2 publishing the pre-index 47.7 ms | Doc | Measure via `overlap_guard`, then sweep — in that order |
| N3 | **Medium** | The caveat is a standing decision (D-127), not an oversight | Process | Supersede with a new register entry, not an edit |
| 5.1 | **Medium** | Python `diagnostic_query`/`explain` open concurrently under a read lock → R15 shape | Binding | `Mutex` around the diagnostic path |
| N2 | **Medium** | `Database::diagnostic_conn` rustdoc never mentions R15 | Doc | One paragraph at `connection.rs:750` |
| 4.B | Medium | §4.3 restoration note says payloads omit `embedding_model` | Doc | Update for payload v2 (0.5.6) |
| N4 | Low | `readonly_open_probe` does not probe `ATTACH`; reachable from `diagnostic_query` | Unverified | Two rows in the probe; act on the result |
| N5 | Low | Low-priority starvation unbounded by design, observable, unstated | Doc | Paragraph in §5.1 citing `low_depth_max` |
| N6 | Low | `--all-features` test count has no CI job behind it | Gate | Add the job or drop the number |
| 4.C | Low | quickref §8 counts stale (2026-08-02 vs 2026-08-07) | Doc | Replace hard counts with a pointer to the runner |
| 4.D | Low | `s0-s3` §2/§3: `petgraph`, 500–1,000-row chunks, missing `hybrid.rs`, missing `bindings/` | Doc | Reconcile against `quickref.md:120` |
| 6.5 | Low | `is_closed` rustdoc claims `debug_assert`s that do not exist | Doc/hardening | Add the five asserts, or correct the sentence |
| 5.3 | Low | No `recorded_at` monotonicity guard on concept INSERT | Invariant shape | One-line note in §4.3 |
| 6.4 | Low | `raw()` convention is argued only in hidden rustdoc | Hardening | `// convention:` sentinel a binding contributor will see |
| N7 | Info | `nul.pdb` build residue in repo root | Housekeeping | Delete; find the `> nul` redirect |
| 5.2 | — | "O(version-count) undocumented" | **Corrected** | It is documented at `connection.rs:2264–2269` |
| 4.A/2 | — | "Add a hub bench with the index present" | **Corrected** | `budgets.rs:1101` already does this |

---

## 6. Assessment

Macrame 0.9.0 remains, on this evidence, free of open code-level 1.0 blockers. What N1 adds is not a blocker either — it is an unguarded latent mode that requires an external raw writer to reach — but it is the one finding in either review where a doctrine invariant can be suspended silently, and it is a one-line check away from being closed by machinery that already runs at every open.

The dominant theme is unchanged from v2 and is worth restating in its widened form: **this project's documentation is its product, its documentation is unusually honest, and one performance claim has now outlived its correction across nine locations, two normative documents, and one decision that re-affirmed it.** The self-correcting discipline works — it produced the v6 rung, the v2 payload, the plan registry, the body-checking `verify`. What it lacks is a tripwire for *claims*, the way `index_plan_tests` is a tripwire for indices. §5 of the v0.10.0 plan proposes one, built on the same pattern.

The v0.10.0 plan is at [`docs/Macrame Update Plan v0.10.0.md`](docs/Macrame%20Update%20Plan%20v0.10.0.md).
