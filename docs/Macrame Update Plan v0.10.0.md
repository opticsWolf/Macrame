# Macrame Update Plan — v0.10.0

**From:** 0.9.0 (schema v10)
**To:** 0.10.0 (**schema v10 — no rung**)
**Source:** [`architectural-review-v3.md`](../architectural-review-v3.md), which extends `architectural-review-v2.md`
**Shape:** one invariant closed, one performance claim settled by measurement, one binding hardened, and a documentation reconciliation that has now misled two reviews.

---

## 0. What this release is, and what it deliberately is not

**It is not 1.0.** Neither review found an open code-level 1.0 blocker, and that is precisely the reason not to tag one yet: the case for 1.0 currently rests on prose that contradicts the code in nine places, including two documents the architecture set marks normative. A 1.0 whose most-cited performance claim is unmeasured is a 1.0 that inherits the problem permanently, because [D-036](architecture/s13-decision-register.md#d-036) freezes what 1.0 declares. 0.10.0 is the release that makes the 1.0 claim checkable.

**It climbs no schema rung.** Every change below is runtime behaviour, documentation, a bench publication, or a test. `user_version` stays at **10**. A 0.9.0 database opened by 0.10.0 runs no migration, and — unlike every release since 0.7.0 — this one is reversible: a 0.10.0 database can be opened by 0.9.0.

**It is `0.10.0` and not `0.9.1` for one reason**, the same shape as 0.9.0's: `DbError` is not `#[non_exhaustive]` (`src/error.rs:23`), so W2's new variant breaks any consumer that matches it exhaustively. Reading and formatting errors is unaffected. That is the whole of the API break.

**Explicitly out of scope.** The register's own standard for a deferral is that *"an omission with no trigger condition is indistinguishable from an oversight"* (`s13-decision-register.md:727`), so each row carries the condition that would make it due — not only the reason it is not due now.

| Deferred | Why it is safe for 0.10.0 | What would make it due |
|---|---|---|
| Any schema rung | Nothing here needs one, and a release with no rung is the first since 0.7.0 a user can roll back | Any change requiring a new invariant. **Corollary: such a change does not belong in 0.10.0** — it would spend the reversibility that is this release's main feature |
| `rehydrate_windowed` | [D-132](architecture/s13-decision-register.md#d-132) declined it to preserve atomicity. No wave here touches the archive path or its workload, so the risk profile is unchanged | A caller reporting the 1.1 s write-lock hold at n=10,000 as a real latency problem. Post-1.0 track |
| `LIMIT`-capped overlap guard | The guard is O(version count per edge key), the count is small, and the archive reclaims superseded rows. Review v3 §2 corrected v2's "undocumented" — `connection.rs:2264–2269` states it precisely | A workload churning one edge key thousands of times *without* archiving. **But see W1.3: the property has no decision-register entry, only a rustdoc one, and D-134 is where it gets one** |
| `AtomicU64` for `SystemClock`'s floor | The Write Actor is single-threaded and is the only caller on any supported path; the `Mutex<SystemTime>` is uncontended | A downstream caller surfacing real contention by sharing one `Clock` across workload threads — which `Send + Sync` permits and nothing promises |
| Low-priority aging / a third actor tier | [D-010](architecture/s13-decision-register.md#d-010) and [D-011](architecture/s13-decision-register.md#d-011) chose strict preemption deliberately, and `concurrency_tests.rs:101` pins it. W4.5 documents the consequence; it does not renegotiate it | A caller demonstrating indefinite low-tier starvation under a *supported* profile. W4.5's job is to make that reportable by naming the detector |
| Adopting `rustfmt` | A repo-wide reformat is its own commit and its own decision (`ci.yml:95–103`) | Nothing here. Doing it inside this release would bury the granular doc↔code traceability that is the release's entire value. Schedule as a discrete PR **after** 0.10.0 |
| Fixing R15 | Upstream fault in libSQL 0.9.30. The crate does not own it and must not claim to. W4.1 **bounds** the exposure; W4.2 documents it | An upstream fix, or a version bump that changes the fault rate. Track in the README's Known Risks row; see §7's release-note constraint |

### 0.1 Checked and already covered — recorded so they are not re-raised

Two items from the surrounding review discussion were investigated and need **no wave**:

- **`links_current` / pre-v7 cold files carry no `weight` CHECK.** Already named in the §4.7 gap table: `s4-schema.md:443` row 3 states the residual and the enforcement point verbatim — *"`links_current` and pre-v7 cold files, which carry no such `CHECK` — so the loader guard stays"* — citing [D-039](architecture/s13-decision-register.md#d-039) and [D-083](architecture/s13-decision-register.md#d-083), with [D-103](architecture/s13-decision-register.md#d-103) recording the same residual on the Python side. Verified rather than assumed; no sentence to add.
- **The overlap guard's O(version-count) characteristic.** Documented in rustdoc (`connection.rs:2264–2269`) but **absent from the register** — `rg 'version count' docs/` returns nothing. That is a real gap, and it is folded into W1.3 rather than given a wave of its own, because D-134 is where the replacement complexity claim is ratified anyway.

---

## 1. Wave order, and why it is not negotiable

W1 must precede W3. Review v3 §3.3 (N3) establishes that the "O(out-degree)" caveat was **re-affirmed by decision** at 0.8.0 — [D-127](architecture/s13-decision-register.md#d-127) lists *"Dropping the 'not met at high out-degree' caveat"* among its **rejected** options. Editing the nine locations without a measurement would be the third time this claim moved with no evidence behind it, in a project whose [D-088](architecture/s13-decision-register.md#d-088) forbids exactly that.

```
W1  measure          →  W3  reconcile the prose  →  W5  gate it
W2  close N1         (independent)
W4  the small true things  (independent)
```

---

## 2. W1 — Settle the single-assertion complexity claim by measurement

**The bench already exists.** `benches/budgets.rs:1101` (`overlap_guard`, registered at `:1601`) runs `assert_edge` against a hub at degree `0` and degree `2_000 * scale()`, **with `idx_lc_open_interval` present**, and its own rustdoc states the hypothesis: *"out-degree should not matter, and if it does the index is not being used the way `the_single_open_probe_seeks_rather_than_scans` says it is."* It has never been published. This wave is one bench run and a decision.

### W1.1 — Add an 8,000 arm

D-059's original evidence is stated at 8,000 edges (`ddl.rs:509`: 47.7 ms pre-index, 8.0 ms post-index and flat). `overlap_guard` stops at 2,000, so a reader comparing the two is comparing different scales. Change the arm list to `[0, hub, 4 * hub]` — three points, which is what shows *flat* rather than *small*.

```rust
// benches/budgets.rs — overlap_guard
for degree in [0usize, hub, 4 * hub] {
```

### W1.2 — Run it, one group per process

Per [D-127](architecture/s13-decision-register.md#d-127)'s own finding that R15 reaches the benches and kills every later group:

```bash
cargo bench --bench budgets -- overlap_guard
```

Median of three, with `control/select_1` from the same session quoted beside the result — the [D-070](architecture/s13-decision-register.md#d-070) rule about ~29% session noise applies, and the single-assertion row is exactly where a 13% unexplained excursion has already been reported once (`README.md:221`).

### W1.3 — Decide, and record it

Three outcomes, all acceptable, one required:

| If `overlap_guard` reads | Then | Prose becomes |
|---|---|---|
| **flat** across 0 / 2K / 8K | the caveat is retired | "single assertion is O(version count per edge key), flat in out-degree since the v6 rung — measured at N µs / N µs / N µs" |
| **growing with degree** | the index is not being chosen on this path | a new `EXPLAIN` assertion is the fix, not a rung. Locate it before touching any prose |
| **flat but slow at 8K** | something else scales | attribute it before publishing; do not carry the D-059 attribution forward by default |

**Record `D-134`** in `s13-decision-register.md`: *the single-assertion complexity caveat is retired (or relocated) on measurement, superseding D-127's rejection.* The entry must name D-127 explicitly and say what changed — that the rejection was correct on the evidence available and the evidence has now been taken.

**D-134 also gives the replacement claim a register home, which it currently lacks.** `rg 'version count' docs/` returns nothing: the guard's actual complexity — O(version count per edge key) — lives only in a rustdoc comment (`connection.rs:2264–2269`). A property that governs a published budget and exists in exactly one non-normative location is one refactor away from being lost, and losing it is how the *wrong* complexity got published in the first place. D-134 states it, names the archive as what caps the count, and points at `connection.rs:2270`'s `OVERLAP_CANDIDATES` as the statement it is a property of. This also converts §0's `LIMIT`-cap deferral from an argument into a citation.

**Acceptance:** three numbers with a control, an entry `D-134` that carries both the retired claim and the replacement one, and a decision recorded either way. A wave that measures and finds the caveat *justified* is a successful wave.

---

## 3. W2 — Close the archive-session disarm switch (review v3 §3.1, N1)

**The defect.** A committed table named `macrame_archive_session` in `main` silently disarms `trg_concepts_guard_delete`, `trg_links_guard_delete` and `trg_txlog_guard_delete`, and silently stops `trg_concepts_log_insert` from writing ledger rows. Doctrine IV and Doctrine V both suspended, with no error and no counter. Nothing in the crate checks the table is absent.

The existing safety argument (`s5-modules.md:633`) is about **crashes** and is correct about them — `archive.rs:302/401` and `583/683` bracket the marker inside the session transaction, so a rollback discards it. It says nothing about a raw writer, and §4.7 concedes raw writers exist.

### W2.1 — `verify()` refuses a leaked marker

`migrations::verify` (`migrations.rs:900–967`) already reads `sqlite_master` to check presence of tables, triggers and indices, and already checks the three guard bodies for the marker *name*. Add the absence check to the same pass — it is one more row out of a query already being made, and it runs at every `Database::open`.

```rust
// after the DELETE_GUARDS body check
if has("table", ARCHIVE_SESSION_MARKER) {
    return Err(DbError::ArchiveSessionLeaked {
        marker: ARCHIVE_SESSION_MARKER,
    });
}
```

### W2.2 — The typed error

A new variant, because reusing `DbError::Migration` would say "your schema is wrong" about a database whose schema is right. The message has to carry the remedy, since the fix is a single `DROP TABLE` a user can run:

> `the archive-session marker table 'macrame_archive_session' is present as committed state. While it exists, the delete guards on concepts, links and transaction_log are disarmed and concept inserts write no transaction_log row. An archive session creates and drops this table inside one transaction, so it should never be visible here — something wrote it outside the actor. Drop it (DROP TABLE macrame_archive_session) and audit for deletions and missing log rows since it appeared.`

This is the API break that makes the release 0.10.0. `tests/doc_sync_tests.rs::the_documented_error_enum_matches_the_code` will go red until §7's reproduction of `DbError` is regenerated — that is the gate working, not an obstacle.

### W2.3 — Python surface

`bindings/python/src/errors.rs` gains `ArchiveSessionLeakedError`, grouped under `IntegrityError` (it is a ledger-integrity condition, not a validation or writer condition). Update `python/macrame/_macrame.pyi`, `python/macrame/__init__.py`'s `__all__`, and the §14 error tree. `tests_py/test_stubs.py` and `test_errors.py` pin the count and the grouping, so both will need the new class.

### W2.4 — Tests

| Test | Asserts |
|---|---|
| `a_leaked_archive_session_marker_is_refused_at_open` | create the marker through `raw()`, close, reopen → `ArchiveSessionLeaked` |
| `a_normal_archive_leaves_no_marker_and_reopens_clean` | control — the happy path must not trip the new check |
| `an_archive_interrupted_by_rollback_leaves_no_marker` | the existing crash-safety claim, now asserted rather than argued |
| `the_marker_check_names_the_remedy` | the message contains `DROP TABLE` — the error is the only place a user learns the fix |

**The control arm is not optional, and its docstring must carry the reason.** A check that refuses healthy databases is worse than no check, and this project has that shape on record (`migrations.rs:152–162`, on why presence-by-name was chosen over a count). The false-positive analysis rests on one property: *the marker is never committed state* — `archive.rs:302/401` and `583/683` bracket it inside the session transaction, and `verify()` reads committed state, so it cannot see an in-flight session. Put that sentence in the test's docstring. Without it, a future maintainer looking to save an open-time `sqlite_master` read has no record of why the check is safe where it is, and the obvious "simplification" is to move it onto a hot path where it *would* see an in-flight session and refuse a healthy database mid-archive.

**Scope the fault injection before writing the third test.** `an_archive_interrupted_by_rollback_leaves_no_marker` only earns "asserted rather than argued" if the interruption is real. A test that opens a transaction, creates the marker, and issues a clean `DROP` asserts nothing — it re-runs the happy path. The existing marker tests (`integrity_tests.rs:269`, `migration_tests.rs:1213`) all create the marker *deliberately* to exercise the gated path, so none of them is a template. Two viable mechanisms, decide in-wave:

| Mechanism | Shape | Cost |
|---|---|---|
| **Forced abort inside the session** (preferred) | drive `archive()` against a fixture rigged so a statement *after* `CREATE TABLE {MARKER}` fails — e.g. a cold-file path made unwritable, or an injected error in the copy phase — then reopen and assert the marker is gone | needs one injection seam in `archive.rs`, `#[cfg(test)]`-gated |
| **Process kill mid-transaction** | spawn a child that opens, begins the session, and is killed between `CREATE TABLE` and `DROP` | closest to the real crash, but adds a process-spawning test to a suite already running `RUST_TEST_THREADS = 1` for R15 |

Prefer the forced abort: it tests SQLite's rollback of uncommitted DDL, which is the mechanism the claim actually depends on, and it does not add a spawn to an R15-sensitive suite. If neither seam is acceptable, **say so and drop the test** rather than shipping one that asserts the happy path under a crash-safety name.

**Acceptance:** the four tests; §4.7 gains a row stating that the marker's absence is now enforced at open; a release-note paragraph, because this refuses a database that 0.9.0 would have opened.

---

## 4. W3 — Reconcile the D-059 prose (review v3 §2.1)

**Gated on W1.** Nine locations, in the order a reader meets them:

| # | File | What to do |
|---|---|---|
| 1–3 | `README.md:191, 224, 235–237` | table cell + two prose passages → W1.3's wording |
| 4 | `docs/quickref.md:694` (§6.2) | same |
| 5 | `src/connection.rs:73–77` (`chunk_rows::EDGES`) | **delete** *"a schema defect with a proven fix, recorded in D-059 and not applied here"* — it is applied, and this ships to docs.rs |
| 6 | `src/connection.rs:54–63` (`CHUNK_BUDGET`) | replace the 47.7 ms figure — it is the **pre-index** measurement presented as current |
| 7 | `s6-s10-flows-to-dependencies.md:245` (§9, **normative**) | same as 4 |
| 8 | `s6-s10-flows-to-dependencies.md:264` (§9, **normative**) | replace 47.7 ms with `ddl.rs:512`'s post-index 8.0 ms, or with W1's figure |
| 9 | `s13-decision-register.md:1504, 1520` | do **not** rewrite history. Append a superseding note pointing at D-134, in the register's established style |

Locations 6 and 8 are the priority: they publish 47.7 ms as a current cost for an operation `ddl.rs:512` measures at 8.0 ms. Two numbers for the same thing, 460 lines apart.

**Acceptance:** `rg -n 'O\(out-degree\)|not applied here|47\.7' README.md docs/ src/` returns only lines that carry a D-134 reference or an explicit historical marker.

---

## 5. W4 — The small true things

Independent of W1–W3; each is self-contained **except that W4.3 runs before W4.1** — see the note under the table.

| # | Item | Change | Source |
|---|---|---|---|
| **W4.3** | `readonly_open_probe` does not probe `ATTACH` | two rows in `examples/readonly_open_probe.rs` — `ATTACH` a scratch file, then `INSERT` into it. If writes land, the four-row permission table at `connection.rs:720–730` gains a fifth row and `diagnostic_query`'s docstring says so. **Run first** | v3 N4 |
| W4.1 | Python diagnostic path is an R15 concurrent-open shape | a `Mutex` (std, not tokio — the critical section is a `block_on`, not an await point) guarding `diagnostic_conn()` use in `database.rs:1045` and `:1061`. Bounds this path to one outstanding open; touches neither the typed read path nor the per-call-open semantic | v2 §5.1 |
| W4.2 | `Database::diagnostic_conn` rustdoc never mentions R15 | one paragraph at `connection.rs:750`, cross-referencing the README's R15 row and `examples/r15_soak.rs`. **The Rust API is the source; W4.1 fixes only the Python symptom** | v3 N2 |
| W4.4 | §4.3 restoration note says payloads omit `embedding_model` | strike or update `s4-schema.md:364` — false since 0.5.6 Wave 1 | v2 §4.B |
| W4.5 | Low-priority starvation unstated | a paragraph in §5.1: strict preemption is unbounded by design, and `MetricsSnapshot::low_depth_max` (`metrics.rs:455`) is the detector | v3 N5 |
| W4.6 | quickref §8 counts stale | replace the hard counts at `quickref.md:747` with a pointer to `scripts/run_rust_suite.py`, so they cannot drift again | v2 §4.C |
| W4.7 | `s0-s3-foundations.md` §2/§3 divergences | `:56` drop `petgraph` (native since 0.5.x); `:83` replace "500–1,000-row chunk" with D-058's per-path sizes — same stale constant at `appendices.md:272` and `architecture/README.md:26`; `:114` add `vector/hybrid.rs`; `:136` fix the `tests/` tree and add `bindings/`. Reconcile against `quickref.md:120`, which is correct | v3 §2.2 |
| W4.8 | `is_closed` rustdoc claims `debug_assert`s that do not exist | `subgraph.rs:459` says *"Used by tests and `debug_assert`s"* and none exists in `src/`. **Write the asserts** — `debug_assert!(g.is_closed())` at the entry of `dijkstra`, `astar`, `scc`, `k_core`, `louvain` — rather than weaken the sentence: `subgraph.rs:30` says every algorithm assumes closure and none re-checks it, so the assert is the auditable form of a live assumption | v2 §6.5 |
| W4.9 | No `recorded_at` monotonicity guard on concept INSERT | one sentence in §4.3 recording the asymmetry **and why a `BEFORE INSERT` guard would not close it** — a raw writer can flip PRAGMAs too | v2 §5.3 |
| W4.10 | `raw()`'s convention lives only in hidden rustdoc | a `// convention (D-068/D-091):` sentinel at `connection.rs:925` and beside the Python surface list in `bindings/python/src/lib.rs`, where a binding contributor adding `raw()` would be standing | v2 §6.4 |
| W4.11 | `--all-features` test count has no CI job | add the run to `ci.yml`, or drop "362" from `README.md:142`. Prefer adding — `--all-features` is `metrics` + `property-tests`, and the R15 interaction between them is currently unmeasured | v3 N6 |
| W4.12 | `nul.pdb` residue | delete it; find the `> nul` redirect (Git Bash treats `nul` as a filename) | v3 N7 |

**W4.3 is sequenced ahead of W4.1, because it determines whether W4.1 is a complete mitigation or a partial one.** W4.1 bounds *concurrent opens*, which is the R15 shape. It does nothing about what a *single* caller can do through the connection once open. If `ATTACH` on a `SQLITE_OPEN_READ_ONLY` connection yields a writable attachment, then `diagnostic_query` — the only arbitrary-SQL surface the binding exposes — is a back door around the "OS-level boundary, not a reversible pragma" claim, and that is a different defect with a different fix (refusing `ATTACH` in `diagnostic_query`, or amending the claim). Running the two-row probe first costs one `cargo run --example` and decides which of those W4.1 is part of.

**W4.1 and W4.2 then ship together or not at all** — a mitigation in the binding with no warning on the API it wraps just moves the exposure to Rust callers.

**On W4.1's choice of `std::sync::Mutex`:** the critical section is `runtime().block_on(async { diagnostic_conn().await?; collect(…).await })`, which is a synchronous region on the calling Python thread — the `.await` points are inside `block_on`, not across the guard. Each Python call owns its own thread for the duration, so a second caller blocking on the mutex blocks a thread that had nothing else to do. `std` is cheaper and needs no runtime handle. **The condition under which this stops being true**: if the diagnostic path ever moves to sharing a tokio worker thread between two calls, a `std` guard held across a yield point becomes a deadlock, and the choice must be revisited. The current `with_db` + `block_on` shape prevents that; note it beside the mutex so the constraint travels with the code.

---

## 6. W5 — A tripwire for claims

The failure this release exists to correct went unnoticed for four releases because **nothing gates prose against measurement**. `doc_sync_tests.rs:21–28` is explicit that it pins the API surface and nothing else, and that shallowness is right for what it does. The gap is a different kind of check.

`index_plan_tests.rs` is the template: a **registry keyed by the thing being claimed**, where adding a claim without its justification is a red test. Apply the same inversion to performance claims.

### W5.1 — Draft the schema and dry-run it *before* implementing

The third test below is only sound if claims are keyed by **`(operation, metric)`** rather than by their numeric value. Keying on the number makes `2.39 ms` appearing legitimately for two genuinely different operations a red test — and a gate that cries wolf on its first week is a gate the team learns to ignore, which is strictly worse than the drift it was built to catch. This project has that failure on record twice (`ci.yml:95–103` on why rustfmt is advisory; `doc_sync_tests.rs:21–28` on why the API check is deliberately shallow).

So: write the schema, hand-populate it with W3's nine locations, and check by inspection that no two entries collide before a line of test code exists.

```rust
/// A published performance claim, and what substantiates it.
struct Claim {
    /// What is being measured. The *key*, with `metric` — never the number.
    operation: &'static str,        // "single edge assertion"
    metric: &'static str,           // "latency, median, reference hardware"
    /// Verbatim fragment as it appears in the document.
    text: &'static str,
    /// The document it appears in, via include_str!.
    doc: &'static str,
    /// The criterion group that measures it, as named in benches/budgets.rs.
    bench_group: &'static str,
    /// The register entry that last ratified it.
    decision: &'static str,
}
```

The nine W3 locations collapse to **two** `(operation, metric)` keys — *single edge assertion / latency* and *chunk commit, edges, 90 rows / latency* — which is exactly why the drift was invisible: nine texts, two facts, no structure connecting them.

### W5.2 — The three tests

All compile-time `include_str!`, all cheap:

1. **`every_claim_still_appears_in_its_document`** — a claim edited or deleted without updating the registry goes red. This is `every_reproduced_query_still_exists_in_its_source` applied to prose.
2. **`every_claim_names_a_bench_group_that_exists`** — the group name must appear in `benches/budgets.rs`. A claim whose bench is deleted or renamed goes red.
3. **`one_operation_metric_key_carries_one_value`** — group the registry by `(operation, metric)` and require every member's `text` to be consistent. This is the check that would have caught 47.7 ms and 8.0 ms published for the same operation, 460 lines apart. Named for the key, not for the number, so its soundness is visible from its name.

**Seed it with the nine locations from W3**, which is the point: the registry's first entries are the ones that just rotted.

**Acceptance:** the schema is dry-run against the nine locations *before* implementation, and the dry-run is recorded in `D-135` — including any location that would not fit the key, because that is the design feedback.

**What this deliberately does not do.** It does not assert that any number is *correct* — that is what the benches and the reference hardware are for, and [D-055](architecture/s13-decision-register.md#d-055) rules out making them CI gates. It asserts that every published claim is **traceable to a bench that exists and a decision that ratified it**. That is the property this release found missing.

**Record `D-135`:** *performance claims get a registry, on the D-089 pattern.*

---

## 7. Acceptance for the release

| Gate | Condition |
|---|---|
| Schema | `user_version` still **10**; `migration_tests` shows no new rung; a 0.10.0 database opens under 0.9.0 |
| Suite | Rust green at `metrics`, at `property-tests`, and — new — at `--all-features` (W4.11); Python green; `mypy --strict` clean |
| `doc_sync_tests` | green after §7's `DbError` reproduction is regenerated for W2.2 |
| `perf_claim_tests` | new, green, seeded with W3's nine locations |
| Measurement | `overlap_guard` at 0 / 2K / 8K published with `control/select_1` beside it |
| Register | `D-134` (the caveat, settled), `D-135` (the claim registry). W2 folds into `D-136` if the marker check warrants its own entry |
| Release note | `docs/releases/v0.10.0.md` — **must** lead with W2's behaviour change, since it refuses a database 0.9.0 would open; **must** say "bounds" and not "fixes" about R15 (below) |
| Rollback | stated as available, which no release since 0.7.0 could say |

**One release-note constraint worth stating in the plan rather than discovering while writing it.** W4.1 and W4.2 harden a known risk; they do not close it. R15 is an upstream access-violation in libSQL 0.9.30 that this crate explicitly does not own — Doctrine I, the boundary is sacred. The note must therefore say that 0.10.0 **bounds the R15 exposure in the Python diagnostic path**, and must not imply the fault is resolved. The README's Known Risks row stays exactly where it is; retiring it would require an upstream fix, which is not this release and is not this project's to make. A release that hardens a risk and a release that closes one are read very differently by anyone deciding whether to upgrade, and only one of those is true here.

---

## 8. Sequencing

| Order | Wave | Blocks | Notes |
|---|---|---|---|
| 1 | **W1** measure | W3, W5 | One bench run. Do this first; everything about the headline claim depends on the answer |
| 1 | **W2** the marker | — | Parallel with W1. The only invariant work in the release. Scope the fault injection (W2.4) before writing the third test |
| 2 | **W4.3** the `ATTACH` probe | W4.1 | One `cargo run --example`. **May promote itself to a finding**, which is why it precedes the mitigation it would change |
| 3 | **W4.1 + W4.2** | — | Ship together. W4.1's completeness depends on W4.3's result |
| 4 | **W3** prose | W5 | Only after W1.3's decision is recorded |
| 5 | **W5.1** registry schema | W5.2 | Dry-run against W3's nine locations on paper, before any test code |
| 6 | **W5.2** the three tests | — | Seeded from W3 |
| 7 | **W4.4–W4.12** | — | Any time. W4.8's asserts want one full **debug** suite run — if one goes red, that is W4.8 finding a real closure violation, not W4.8 being blocked |

W1 and W2 are the release. W4.3 is the cheapest thing on the list and the one most able to change another wave's scope, which is why it sits third rather than last. Everything else is reconciliation that W1 and W2 make honest.

---

## 9. What 0.10.0 buys toward 1.0

After this release, the 1.0 argument rests on checked claims rather than on prose:

- the eight invariants are enforced **and** the one silent suspension path is refused at open (W2);
- the most-cited performance claim in the project is either measured or retired, with a register entry either way (W1);
- the R15 exposure is documented at its source and bounded at the binding (W4.1–W4.2);
- and a claim that rots is a red test rather than the next reviewer's finding (W5).

1.0 then becomes what it should be — a freeze decision under [D-036](architecture/s13-decision-register.md#d-036) about a primary key and a public surface — rather than a bet on documentation that two consecutive reviews have now found contradicting the code.
