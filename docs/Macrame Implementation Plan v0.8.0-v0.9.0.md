# Macrame — Implementation Plan v0.8.0 → v0.9.0

**Status:** proposed, 2026-08-01. Supersedes the withdrawn v0.7.5 consolidation proposal and
the v0.8.0 outline that preceded it.
**Basis:** a read of the crate, the docs, the four workflows, the CI history and both
registries, plus direct reproduction or measurement of every claim below — including
`examples/concepts_rebuild_probe.rs`, written for this plan, which **refuted the shape D-084
specifies for its own migration** and established the one that works.

**Two releases, one theme.** 0.8.0 spends the last cheap API break and the last cheap schema
rung this project will get. 0.9.0 builds concept archival on top of them, needing no migration
at all. Erasure is refused rather than deferred, and [§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)
says why.

---

## 0. The standing rule: documentation moves with the code

**This is a requirement of every item in this plan, not a phase at the end of it.** It is
stated first because the two defects that shaped this plan were both documentation defects that
no test could see: [D-089](architecture/s13-decision-register.md#d-089) and
[D-087](architecture/s13-decision-register.md#d-087) each said *"scheduled for 0.7.0"*, 0.7.0
shipped as something else, and **nothing went red**. A decision recorded as scheduled has no
tripwire. That is what [A5](#a5--a-decision-that-names-a-release-must-not-outlive-it) exists to
end.

### 0.1 The rule

> **No item in this plan is complete until the documents it invalidates are correct.** Not
> "documented afterwards", not "noted in the commit message" — the same commit, or the item is
> not done. Every item below carries an explicit **Documents** list, and that list is part of
> its exit gate.

### 0.2 The document set, and who owns what

| Document | Normative? | Changes when |
|---|---|---|
| [`s0-s3-foundations.md`](architecture/s0-s3-foundations.md) | **The doctrine.** | Only by amendment, with the amendment stated as such. §2 touches this. |
| [`s4-schema.md`](architecture/s4-schema.md) | **Normative.** | Any DDL, trigger, index or constraint moves. |
| [`s5-modules.md`](architecture/s5-modules.md) | descriptive | A module's responsibility or a public type's shape moves. |
| [`s6-s10-flows-to-dependencies.md`](architecture/s6-s10-flows-to-dependencies.md) | §7 normative | Errors (§7), performance figures (§9), test strategy (§8). |
| [`s11-s12-milestones-and-risks.md`](architecture/s11-s12-milestones-and-risks.md) | descriptive | A milestone's status or a risk's mitigation changes. **Currently stale at 0.5.4 — see [A3](#a3--make-the-documents-describe-this-codebase).** |
| [`s13-decision-register.md`](architecture/s13-decision-register.md) | **The record of intent.** | Every item here adds an entry, written *when the decision is taken*. |
| [`s14-python-bindings.md`](architecture/s14-python-bindings.md) | descriptive | The binding surface or its reasoning moves. |
| [`appendices.md`](architecture/appendices.md) | **Appendix A normative** | The public API surface; the glossary; the deferred list (Appendix C). |
| [`architecture/README.md`](architecture/README.md) | index | Decision range, document list. |
| [`quickref.md`](quickref.md) | derived | Any of the above. It is a *projection*, so it is updated last and never edited first. |
| `README.md` | public face | **Published on crates.io and PyPI.** Any user-visible claim. |
| This plan | delivery record | Each item gains a delivery blockquote, in the house style of the v0.6.0 and v0.7.0 plans. |

### 0.3 How the register entry is written

Not a changelog line. The register's value is that it records **the alternative that was
rejected and the flaw that disqualified it** — which is why it can be read years later by
someone about to re-propose the rejected thing. Every entry in
[§6](#6-decision-entries-this-plan-creates) carries: the decision, the doctrine and decisions it
touches, the measurement if there is one, the fixture the measurement was taken on
([D-088](architecture/s13-decision-register.md#d-088)), and an explicit `Rejected:` list.

**Corrections go in place, marked as corrections.** When this plan's own reasoning turns out to
be wrong — and [B4](#b4--schema-v8-the-last-cheap-rung) is already an example, since a probe
refuted D-084's stated migration shape — the entry says so rather than quietly describing the
version that worked.

### 0.4 What is mechanically enforced, and what is not

Enforced today: `doc_link_tests` (every cross-reference in `docs/architecture/` resolves; the
nav chain is unbroken; no document is unchecked), `doc_sync_tests` (the documented `DbError`
enum matches `error.rs`; every public `Database` method appears in Appendix A),
`fixture_matrix_tests::every_performance_decision_names_its_fixture`.

**Not enforced, and this plan adds two:**

- [A5](#a5--a-decision-that-names-a-release-must-not-outlive-it) — a decision entry naming a
  release that has shipped, with no delivery record, fails the suite.
- [B1](#b1--subgraph-nodedata-edgeref-fields-private-accessors-public--the-break)'s exit gate
  extends `doc_sync_tests` to `Subgraph`'s public surface, because Appendix A and `quickref.md`
  both quote its field list and B1 changes it.

`README.md` and `quickref.md` remain unenforced by construction — they are prose projections —
so they are named explicitly in each item's **Documents** list instead.

---

## 1. The two releases at a glance

| | 0.8.0 | 0.9.0 |
|---|---|---|
| Theme | the last cheap break, the last cheap rung | concept archival |
| API break | **yes**, Rust only ([B1](#b1--subgraph-nodedata-edgeref-fields-private-accessors-public--the-break)) | no |
| Schema rung | **v7 → v8** ([B4](#b4--schema-v8-the-last-cheap-rung)) | **none** |
| Python API | **one narrow break** — `NodeData.content` becomes `str \| None` ([B7](#b7--the-binding-surface-the-stub-and-the-wheel)). The interning is invisible: [D-101](architecture/s13-decision-register.md#d-101) pre-paid for it | additive ([C5](#c5--the-binding-catches-up-with-archival)) |
| Doctrine | erasure refused, recorded ([§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)) | Doctrine V's archive path extended to concepts |

Track A lands on `main` continuously and ships *in* 0.8.0 — it simply does not wait for it.
A1 must land now because `main` is red today.

---

## 2. Doctrine position: erasure is refused, archival is sanctioned

**This is a doctrine decision, not a feature decision, and it is taken here so nothing
downstream has to keep asking.**

### 2.1 Erasure violates three doctrines by name and undermines a fourth

Physical removal of a concept and its ledger entries — GDPR-style right to erasure — is not
"in tension with" the doctrine. It contradicts it:

| | |
|---|---|
| **[III](architecture/s0-s3-foundations.md#doctrine-iii)** | "The past is never rewritten; it is only ever superseded. **This is what makes the transaction-time axis honest.**" Erasure rewrites it. |
| **[IV](architecture/s0-s3-foundations.md#doctrine-iv)** | "**a ledger whose history can be compacted away beneath it is not a ledger.**" Erasure compacts `transaction_log`. |
| **[V](architecture/s0-s3-foundations.md#doctrine-v)** | "**Absence of data must always be explained by the ledger, never caused by a mistake.**" Erasure produces absence that *cannot* be explained, because a tombstone naming the erased subject is itself the record that had to go. |
| **[VIII](architecture/s0-s3-foundations.md#doctrine-viii)** | `reconstruct(ts)` promises belief as of `ts`. After an erasure it returns a different answer for a `ts` *before* the erasure — silently, retroactively, with no parameter declaring it. |

Doctrine V decides it on its own. V is a claim about **explicability**, not about mechanism, and
no implementation of erasure can satisfy it.

### 2.2 What to do instead, which costs nothing

**Keep identifying content out of the ledger.** Store a pseudonymous id in `concepts`; keep the
identifying material in a table the application owns and can delete. Erasure is then a row
deletion in the caller's store, and Macrame's ledger stays intact and honest — it only ever held
an opaque key. This is the standard answer for append-only stores, it belongs at the application
layer alongside the GNN features [Appendix C](architecture/appendices.md#appendix-c--future-considerations-deliberately-deferred)
already puts there, and it requires **no change to Macrame at all**.

**Rejected: crypto-shredding** (encrypt the erasable fields, destroy the key). It preserves III,
IV and V — no row is removed — and it is the right answer for many ledgers. Here it costs the
two features that make this a knowledge base: FTS5 indexes plaintext and embeddings are computed
from plaintext, so encrypting `title` and `content` disables keyword and vector search over
exactly the fields worth searching. A subset-of-fields variant pushes the decision of *which*
fields back to the application, which is where §2.2's answer already puts the whole problem.

**Recorded as refused, not deferred.** [D-022](architecture/s13-decision-register.md#d-022) and
Appendix C currently read as *not yet*, which is how an obligation with no trigger survives
indefinitely. [D-117](#6-decision-entries-this-plan-creates) replaces that with a refusal and a
pointer to the alternative.

### 2.3 Concept archival is the sanctioned exit, and traceability answers its open question

Doctrine V names the archive path as the way rows leave: *"Rows leave the hot database only
through the archive path, which runs inside a declared archive session and is verified before
anything is removed."* Nothing is destroyed; the cold file is ATTACHed and the fold unions both,
so `reconstruct` keeps working across the boundary. Archival is doctrine-compatible **by
construction**.

[Appendix C](architecture/appendices.md#appendix-c--future-considerations-deliberately-deferred)
leaves one question open: whether a concept returning from cold "should reacquire its old
identity or be treated as a new assertion." **The requirement that archival remain traceable
through the bitemporal ledger answers it.** If rehydration wrote a fresh `transaction_log`
entry, the concept would appear to have been *learned* at rehydration time — the valid-time
facts right and the transaction-time axis lying, which is precisely what Doctrine III exists to
protect. Therefore:

> **Rehydration is a physical move back, not a write.** It mints no new transaction-time facts
> and is invisible to both clocks — the mirror image of archival, inside the same declared
> session, with the horizon updated.

That is derived rather than chosen, and it closes the question rather than deferring it
([D-121](#6-decision-entries-this-plan-creates)).

---

## Track A — lands on `main` now, ships in 0.8.0

### A1 · The gate has to be able to say why it failed — **first, and alone**

**`main` is red.** Runs `30706318073` (`2490987`) and `30706474231` (`bea57a8`), both
documentation-only commits, both failed `test (windows-latest)` at the property-tests step. The
complete diagnostic is four lines: *attempt 1 failed, attempt 2 failed, attempt 3 failed,
property tests failed three times.* `test (with R15 retry)` and `test (ubuntu-latest)` passed in
both runs; the seven CI runs before them were green.

**Why the budget was wrong.** `ci.yml` justifies three attempts with "R15 has always passed on
re-run" — true of the *main* suite, where `RUST_TEST_THREADS = "1"` applies. The quarantined
binaries are quarantined precisely because serialising does not fix them:
[R15](architecture/s11-s12-milestones-and-risks.md#r15) records `doctrine_property_tests` at
**3 faults in 4 runs** in isolation. Three attempts against a per-attempt rate near 0.75 fails
about 40% of the time — the 2-in-9 now observed.

**The count is the smaller half.** Nothing distinguishes *R15 killed the process* from *a
property found a real defect*. [D-107](architecture/s13-decision-register.md#d-107)'s
`run_suite.py` already solved this for Python — four outcomes, retry only `CRASH`. The lesson
was never brought back to Rust.

**Steps.**
1. `scripts/run_rust_suite.py` (or `.rs` as a `cargo xtask`; Python is cheaper and matches
   `run_suite.py`'s shape). Parse per-binary `test result:` lines from `cargo test --no-fail-fast`.
2. Classify: a binary that ran and printed no summary → `CRASH`. A summary with `N failed` →
   `FAILED`. Fewer summaries than binaries invoked → `INCOMPLETE`. Green summaries with a
   non-zero exit → `TEARDOWN`.
3. Retry `CRASH` only, three times. Return `FAILED` / `INCOMPLETE` / `TEARDOWN` on attempt 1,
   with the failing test named in a `::error::` annotation.
4. Replace **both** inline `for attempt in 1 2 3` loops in `ci.yml`. Two loops with different
   correct retry counts is the defect restated.

**Exit gate.** An injected `panic!` in one property test reports `FAILED` on attempt 1 naming
the test. A binary killed mid-run reports `CRASH` and is retried. Both verified by injection,
the way [P8](Macrame%20Python%20Bindings%20Plan%20v0.7.0.md) verified the stub. `main` green.

**Documents.** `s13` (D-110); `s6-s10` §8 (testing strategy — the Rust gate now has the same
shape as the Python one); `s11-s12` R15 row (the mitigation list gains the classifier);
`README.md` Testing block.

**Rejected.** *Five retries* (lowers a rate, keeps the property that genuine red is
indistinguishable from noise). *`continue-on-error`* (deletes the signal instead of reading it).
*Dropping the property binaries* (the only model-based checks of the doctrine invariants —
[D-030](architecture/s13-decision-register.md#d-030) is why they exist).

> **Delivered 2026-08-02** — `scripts/run_rust_suite.py`, both `ci.yml` loops replaced,
> [D-110](architecture/s13-decision-register.md#d-110).
>
> Five outcomes, not four. `BUILD` was added because a compile failure produces an empty
> target list, and an empty list satisfies every "all targets green" check vacuously — it
> would have been classified `TEARDOWN`, which is a wrong and confidently-worded answer.
> Classified first, before anything reads a section list.
>
> **The plan's Rejected list is overturned by the plan's own step 2, and this is the one
> substantive change.** *Five retries* was rejected because more attempts mean more chances
> to launder a real failure into a pass — true of a loop, false once `FAILED` returns on
> attempt 1 and is never retried. With the objection gone, the arithmetic decides: against
> the 9-in-15 measured for `doctrine_property_tests`, three attempts go red ≈22% of the time
> and six ≈5%. **The quarantined step ships at six; the main suite stays at three.** Step 4's
> "two loops with different retry counts is the defect restated" was right about the defect
> and wrong about the fix — the defect was two *unclassified* loops, and one classifier with
> a per-step budget is not the same thing. `tests_py/run_suite.py` stays at three and its
> comment, which claimed to match `ci.yml`, now says which step it matches and why not the
> other.
>
> **Both injections run, and the classifier is verified against a captured real run.**
> `panic!` → `FAILED` on attempt 1, naming `injected_panic_must_be_reported_as_failed`, no
> attempt 2. `std::process::abort()` → `CRASH`, retried, naming `bench_control_tests`. Six
> further cases were classified offline against the real 27-target output, including a
> summary deleted from the *middle* of the run to confirm alignment names the crashed target
> and not its neighbour. Injections reverted; working tree clean apart from this work.
>
> **What the exit gate cannot say yet.** "`main` green" is not verifiable from here — it
> needs a push and a CI run. Locally the suite is **305 passed across 27 targets, exit 0**.
> `actions/setup-python@v5` was added to the `test` job: `python` and `python3` are not the
> same name on both runner images, and a gate that exists on one OS is not a gate.
>
> Also updated: `docs/quickref.md` §8 gains the run instruction, because the reason to prefer
> the script over `cargo test` is invisible from the command line — bare `cargo test` under
> R15 returns a *smaller* green.

### A2 · Look through R15's upstream window

`libsql` max-stable is **0.9.30, 19 March 2026**, unmoved since the fault was reported against
it on 31 July. A **`0.10.0-pre.4`** exists, 2 June 2026, never compiled here.

R15 currently costs `RUST_TEST_THREADS = "1"`, the `property-tests` quarantine, both retry
loops, the crash-retry half of `run_suite.py`, a Python suite forbidden `pytest-xdist`, the
longest row in the risk register, and — per A1 — a red `main`. If the fault is gone in the 0.10
line, all of it becomes deletable.

**A probe, not a dependency change.** Run `examples/r15_soak.rs`'s two arms and
`tests_py/probes/r15_concurrent_open.py` against `0.10.0-pre.4` at the widths that produced
2/10 and 2/12 on 0.9.30. The control arm exists and is what makes a clean result mean anything.

**Exit gate.** Fault counts at matched widths on both engines, recorded either way. A
pre-release that does not build unchanged is itself the answer.

**Documents.** `s13` (D-111); `s11-s12` R15 row (this is the row's fifth measurement and it
carries its own history — append, do not replace); `s6-s10` §10 if the version moves.

**Sequenced second**, before Track B: the answer changes how much of A1's classifier 0.9.0 keeps.

> **Delivered 2026-08-02** — the answer is **no**, and nothing is deletable.
> [D-111](architecture/s13-decision-register.md#d-111).
>
> `0.10.0-pre.4` resolves, compiles unchanged, and passes **305/305 on the first attempt** —
> a genuinely useful side result, because it means an eventual 0.10 upgrade is a version bump
> and not a port. And R15 reproduces on it: Rust `control` **1/10** at 48 concurrent opens,
> Python probe **1/12** at the same width, Rust `claim` **0/10**.
>
> **The decisive row is the baseline, and it is the one that looks like good news.** Re-run in
> the same session on the same machine, `0.9.30` faulted **0/10** against the 2/10 recorded
> for it at 0.6.0. Had this probe run only the new engine, a clean arm would have read as
> *fixed*; had it compared 1/10 against the historical 2/10, as *improved*. Side by side the
> old engine scored better than the new one, so the only defensible reading is that **ten runs
> cannot distinguish these rates in either direction**. That is `r15_soak`'s own control-arm
> argument applied one level up — between engines rather than between arms — and it is why
> the exit gate's "fault counts at matched widths" was run as a paired comparison rather than
> against the numbers already in the register.
>
> The question the probe can settle is binary, and it is settled by direct observation. No
> sample size worth paying for would change the decision, because **any** non-zero rate keeps
> every mitigation. A1's classifier keeps all of its job in 0.9.0.
>
> **Two findings on the way.** `bindings/python/Cargo.toml` pins `libsql` independently of the
> root, so a dependency move is a two-manifest edit — deliberate, documented there, and
> self-detecting: the first wheel build failed on `libsql::Error` mismatching across two
> resolved versions, which is a compile error and the right failure. The whole probe ran in a
> detached `git worktree`, so the working tree never held a dependency change that was never
> going to be committed.
>
> `s6-s10` §10 needed no edit: the version does not move.

### A3 · Make the documents describe this codebase

| Where | Says | Is |
|---|---|---|
| `README.md:138` | Test suite: **240+** | 296 plain / 305 with `metrics` / 344 Python |
| `README.md:137` | Schema version **7 (v8 indices in flight)** | 7. Nothing is in flight. After B4, 8 |
| `README.md:125` | a **v8** row in the schema-version table | v8 does not exist; the row describes an intention |
| `README.md:182` | Single assertion ≤ 5 ms → **—** | §9 says **not met at high out-degree** ([D-059](architecture/s13-decision-register.md#d-059)); a dash reads as *unmeasured*, not *known to miss* |
| `quickref.md:171`, `:680` | "scheduled for removal in **0.7.0**" | ×2 |
| `python.yml:4`, `wheels.yml:166`, `s14:515` | "the **337**-test suite" | 344 |
| `s11-s12:17` | "**Status against these gates (0.5.4)** … M3 delivered except hybrid RRF, which has no FTS5 table behind it" | FTS5 arrived on the `v4 → v5` rung; M3 is complete. Three releases stale |

The README rows are urgent: 0.7.0 is published and that file is the crates.io and PyPI front
page. The last row is the instructive one — §11's status paragraph has been wrong since 0.5.5
and nothing could catch it, because `doc_sync_tests` pins the error enum and Appendix A's method
list and nothing else. **A milestone table nobody updates is worse than no milestone table.**
Date it; M5's "measured, not gated" correction ([D-055](architecture/s13-decision-register.md#d-055))
is load-bearing and must survive the edit.

**Documents.** All of the above, plus `architecture/README.md`'s decision range.

> **Delivered 2026-08-02.** Every row re-measured rather than taken from this table.
>
> **Counts, measured today:** **296** Rust default · **305** `metrics` · **316** `property-tests`
> · **344** Python, all green. The plan's table listed three of those four; `property-tests` at
> 316 was missing, and it is the one a reader would most want, because it is the number the
> quarantined CI step produces. All four now appear in `README.md` and `quickref.md` **with the
> date they were measured**, which is the part that makes the next drift visible.
>
> **The "v8" rows were worse than stale — they were never true.** `README.md:125` and
> `quickref.md:171` both carried a **v8** row for the two unread indices. `SCHEMA_VERSION` is 7
> with rungs 2→3→4→5→6→7, and `idx_annotations_label` / `idx_lc_tgt_active` are in
> `ddl::CREATE_INDICES` — the **v7 baseline**, created on every fresh database. There is no v8
> rung and there never was; what was scheduled for 0.7.0 was their *removal*. So the rows
> described a migration that would delete them as though it had already added them. Both now
> say what is actually on disk, and that dropping them is what a v8 rung will be for (B4).
> `architecture/README.md:43` carried the same error inside a historical 0.6.0 row and its
> factual half is corrected in place.
>
> **`README.md:182`'s dash is the row that mattered most**, because a published front page said
> `≤ 5 ms | —` and a dash reads as *unmeasured* when §9 has known since 0.5.5 that the budget is
> **not met at high out-degree** — the single-open guard scans the source's whole out-degree, so
> it is O(degree), not O(1) ([D-059](architecture/s13-decision-register.md#d-059)). No number was
> invented for it: D-059's figures are for 90-row chunks into a hub, and using them for a single
> assertion would be a category error. The cell states the shape and cites the decision.
>
> **The §11 status paragraph was three releases stale in the direction that matters.** It was
> headed *(0.5.4)*, claimed M3 incomplete "except hybrid RRF, which has no FTS5 table behind
> it", and claimed M5's benchmark gates "do not exist". `concepts_fts` arrived on the `v4 → v5`
> rung at 0.5.5b ([D-051](architecture/s13-decision-register.md#d-051)), so the paragraph spent
> three releases denying a feature that shipped. **M1–M5 are all delivered**, it is dated, and
> [D-055](architecture/s13-decision-register.md#d-055)'s *measured, not gated* correction is
> restated explicitly rather than left to survive an edit — it is load-bearing and an
> unqualified "the gates exist now" would have quietly undone it.
>
> **What was deliberately not touched.** Historical release rows in
> `architecture/README.md` keep their contemporaneous numbers ("Suite: 240+ tests" in the 0.6.0
> row), on the same principle that pins
> [`LINKS_V7`](architecture/s13-decision-register.md#d-032) as text: **a release row is a
> statement about the past.** Only claims about the *present* were corrected, plus outright
> factual errors about the schema wherever they appeared.
>
> Also fixed: "the 337-test suite" → 344 in `python.yml:4`, `wheels.yml:166` and `s14:515`; the
> decision range in `architecture/README.md:62` → D-001…D-111. Gates green (`doc_link_tests`,
> `doc_sync_tests`, `packaging_tests`, `index_plan_tests`).
>
> **No register entry.** A3 corrects statements; it decides nothing, and a decision entry for
> "the documents now say what is true" would dilute the register. The *class* of failure it
> belongs to is [A5](#a5--a-decision-that-names-a-release-must-not-outlive-it)'s, and that is
> where the tripwire and the decision go — none of these could have gone red, which is the
> finding, not the fix.

### A4 · The two CI gaps that are not about R15

**macOS never runs the Rust suite.** `ci.yml`'s matrix is `[ubuntu-latest, windows-latest]`;
`README.md`'s second row promises macOS. `python.yml` added `macos-latest` at P7, so the only
macOS evidence this project holds arrives *through pyo3* — a strange shape for a crate that is
the product. Four lines, and the R15 rate on Apple silicon is unknown, which is worth knowing.

**`actions/checkout@v4` runs on deprecated Node 20**, force-migrated to Node 24, annotating
every job of every run across all four workflows. Bump to `@v5`.

Not proposed: `cargo-deny` / `cargo-audit`. Ten direct dependencies, all mainstream; a
supply-chain gate is a policy decision rather than a fix for anything observed. Recorded so its
absence reads as a choice.

**Documents.** `README.md` (platform claim now evidenced); `s6-s10` §8.

### A5 · A decision that names a release must not outlive it

**The tripwire this plan exists because of.** D-089 and D-087 both read *"Scheduled for
0.7.0"*. 0.7.0 shipped as the Python bindings release. Nothing anywhere went red, because a
scheduled decision is prose and prose is not executed. `doc_link_tests` was satisfied: the
anchors resolved.

**Deliverable.** A test in `tests/doc_sync_tests.rs` that scans `s13-decision-register.md` for
scheduling language naming a version (`Scheduled for X`, `Deferred to X`, `revisit at X`),
compares against `CARGO_PKG_VERSION`, and fails when a named version is **at or below** the
current one unless the entry also carries a delivery marker (`— DELIVERED`, or a successor
entry linking back to it).

**This is a small test with a wide blast radius**, so it needs the discipline
[D-088](architecture/s13-decision-register.md#d-088) applied to its own tripwire: match a
backticked or explicit version pattern, not the bare word, or it fires on prose and gets
disabled. `every_performance_decision_names_its_fixture` learned this the same way.

**Exit gate.** Red on today's register (D-087 and D-089 both name 0.7.0, both undelivered),
green once B1–B4 land and their entries carry successors. Verified by injection: a fabricated
`Scheduled for 0.7.0` entry fails; the same text with a delivery marker passes.

**Documents.** `s13` (D-112); `s6-s10` §8.

---

## Track B — 0.8.0, the break taken once

**The rule for this release: `Subgraph` moves once.** B1 → B2 → B3 in that order, in one
release. Three releases each moving it a little is the failure mode, and ordering is what
avoids it.

### B1 · `Subgraph`, `NodeData`, `EdgeRef`: fields private, accessors public — *the break*

**Measured blast radius** (this tree, today):

```
.nodes    55 sites     .out_adj   11 sites     .in_adj    9 sites
struct literals of the three types: 30       files touched: 14 — all in this repository
src/graph/algorithms.rs  →  .out_adj / .in_adj:  0 sites
```

**D-087 overstates the cost and the correction belongs in its successor entry.** The register
says the three types are *"read directly by every algorithm"*. They are not: `algorithms.rs`
reaches adjacency exclusively through `out_edges()` / `in_edges()`, which return borrowed
slices. Its entire field coupling is `.nodes`, at ten sites, every one `contains_key`, `keys()`
or `len()`.

**The Python surface does not move.** [D-101](architecture/s13-decision-register.md#d-101) made
`Subgraph` opaque at P4.2 for an unrelated reason (peak memory on conversion), and
`bindings/python/src/graph.rs` already presents `out_edges`, `node`, `__len__`, `__contains__`,
`node_ids`, `to_dict`. **The opacity decision pre-paid for this break.**

**Steps.**
1. Add `contains_node`, `node_ids`, `node_count`, `node(id) -> Option<&NodeData>`. Existing
   accessors stay.
2. Make the fields of all three types private. **Representation stays `String`.**
3. Fix the 14 files. `algorithms.rs` is ten mechanical substitutions.
4. `write_back_annotations`'s `self.nodes.keys()` becomes `self.node_ids()`.

**Why B1 is separate from B2.** The break lands with the representation unchanged, so anything
depending on the old shape in a way the compiler cannot express shows up against code that
still behaves identically. B2 then has no API consequences to reason about.

**Exit gate.** Suite green with **zero** behavioural diff — same partitions, same distances,
same `estimated_bytes()` on all four fixtures. `algorithms.rs` unchanged apart from the ten
sites. `doc_sync_tests` extended to compare `Subgraph`'s public method surface against Appendix
A, since two documents quote the field list this item deletes.

**Documents.** `appendices.md` Appendix A (the `Subgraph` surface); `s5-modules.md` §5.4;
`quickref.md:296` (the struct is reproduced verbatim there); `s13` (D-113); `s14` §14.10 if any
wording implies field access.

### B2 · Intern the keys

`EdgeRef` becomes `{u32, u32, f64, u32, u32}` — 24 bytes, no heap payload — against 104 bytes
of struct plus payload today. From [D-087](architecture/s13-decision-register.md#d-087):

| id length | bytes/edge now | interned | edges per MiB now | interned | ratio |
|---|---|---|---|---|---|
| 8 | 342 | 48 | 3,066 | 21,845 | **7.1×** |
| 26 (ULID) | 378 | 48 | 2,774 | 21,845 | **7.9×** |
| 64 | 454 | 48 | 2,309 | 21,845 | **9.5×** |

A **reachability** improvement in [D-073](architecture/s13-decision-register.md#d-073)'s
category — graphs that do not fit the byte budget start fitting — not a speed one.
[D-063](architecture/s13-decision-register.md#d-063)'s CPU finding is not disputed and this does
not rest on it.

**Two design points, both pre-registered in the register, both answered in code.**

*The id table must not cancel the win.* D-063 warns the original proposal's `ulid_to_idx`
"stores every id a second time, partly cancelling the memory win." The arithmetic answers it at
any realistic density: the duplication is per **node** (~26 bytes plus map overhead for a ULID)
and the saving is per **edge** (~330 bytes). At 20 edges per node that is tens of bytes spent
against thousands saved. It fails only on a subgraph with fewer edges than nodes, which nobody
loads. **Measure it — `estimated_bytes()` is the instrument and it already exists.**

*Determinism stops being structural and becomes procedural.* D-063 names this as the real cost
and pre-registers the test: **"a test that loads one graph under two different SQL orderings and
asserts identical index assignment — not a comment saying to sort."** The failure is silent in
the worst way: a reordered `ORDER BY` would not error, Louvain would return a different
partition, and §8's oracle would still pass, *because an upper bound on modularity cannot detect
a different valid answer*. **This test is a gate on B2, not a follow-up.** Related, same source:
sorting by `recorded_at` instead of id is not a free choice — it is not a total order over nodes
and its ties are unspecified.

**Exit gate.** The three-row table reproduced by `examples/budget_density_diag.rs` **on the real
type**, not on `size_of` arithmetic. The two-orderings determinism test. Every algorithm
bit-identical on all four fixtures. `to_dict()` round-trips through Python unchanged — asserted
by the existing Python suite, which at **this** item should need **no edits at all**; *if B2
alone needs Python edits, B1 did not privatise enough.* (B3 does change the Python surface, and
[B7](#b7--the-binding-surface-the-stub-and-the-wheel) is where that is done — the two must not be
confused, because "the Python suite went red" means opposite things for B2 and B3.)

**Documents.** `s5-modules.md` §5.4 (the representation and the determinism argument);
`s6-s10` §9 (edges-per-budget is a performance claim and needs its fixture named —
[D-088](architecture/s13-decision-register.md#d-088)); `s13` (D-113, superseding D-087);
`quickref.md`.

### B3 · `content` leaves the default load

`load_subgraph_with` **always** hydrates `content` — its rustdoc says so (`subgraph.rs:360`) —
and **no algorithm reads it**. `dijkstra`, `astar`, `louvain`, `k_core`, `scc` and `modularity`
touch topology and weights only. At 20 edges per node:

| content bytes/concept | node bytes | edge bytes | edges' share of budget |
|---|---|---|---|
| 0 | 37,800 | 1,368,000 | 97% |
| 2,000 | 437,800 | 1,368,000 | 76% |
| 20,000 | 4,037,800 | 1,368,000 | 25% |

At 20 KB per concept, **three quarters of the byte budget is document text an algorithm will not
look at.** Complementary to B2 rather than an alternative — interning wins when edges dominate,
this wins when nodes do — which is why they belong in one release.

**Decided inside the break.** D-087's rejection list says the fix is "a load option, not a
silent change of what the type contains", which was right for an additive release. Since
`NodeData` is being reshaped anyway, `content` becomes **`Option<String>`, populated on request,
default off**, so a caller who did not ask cannot mistake an empty string for an empty document.
The default serves the six algorithms that are the reason `Subgraph` exists.

**Exit gate.** Edges-per-budget at fixed content size on `dense_small` and `clustered`, before
and after. The six algorithms identical with content off — the claim, and the one thing here a
test settles outright. `to_dict()`'s Python shape stated explicitly for the absent case.

**Documents.** `s5-modules.md` §5.4; `appendices.md` Appendix A; `s13` (D-114); `quickref.md`.
**The Python half is [B7](#b7--the-binding-surface-the-stub-and-the-wheel)** — this item stops at
the Rust boundary, and B7 carries `s14`, the stub and the README example.

**This is a behaviour change without a compile break** *in Rust*, so it belongs in the release
note by name: which field stopped being populated by default. In Python it **is** a break, and
B7 says why that is the right shape.

### B4 · Schema v8, the last cheap rung

Two changes, one rung, one snapshot invalidation.

**(a) Drop the two indices with no reader.**
[D-089](architecture/s13-decision-register.md#d-089) found `idx_annotations_label` and
`idx_lc_tgt_active`: an index write per insert, forever, read by nothing, one of them on the
crate's hottest write path. Recorded as *"Scheduled for 0.7.0 alongside the other schema work"* —
and 0.7.0 had no schema work.

**(b) `concepts` gains `rowid_pk`, and the FTS index gains its third trigger.**
`concepts_fts` is external-content with `content_rowid='rowid'`, and `concepts` has
`id TEXT PRIMARY KEY`, so its rowid is **implicit** — which `VACUUM` renumbers.
[D-071](architecture/s13-decision-register.md#d-071) proved the hazard unreachable today only
because the delete guard is unconditional, so rowids are dense and `VACUUM`'s renumbering is the
identity map. **0.9.0's archival makes them sparse and makes the hazard real.** D-071 says so
itself: *"either one makes `concepts.rowid` sparse and makes this hazard real."*

> **Confirmed by probe** (`examples/concepts_rebuild_probe.rs`, §5): an explicit
> `rowid_pk INTEGER PRIMARY KEY` survives `VACUUM` **including when sparse** — `1:c0 3:c2 5:c4
> 6:c5` before and after. The mechanism D-084 proposes is sound.

**The rung must be taken now or never.** `rowid_pk INTEGER PRIMARY KEY` means
`concepts` becomes `rowid_pk INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE` — SQLite permits one
primary key per table, so **this is a primary-key change**, which
[D-036](architecture/s13-decision-register.md#d-036) forbids outright after 1.0 and names as
requiring "a major version with an explicit ETL migration path". Pre-1.0,
[D-032](architecture/s13-decision-register.md#d-032) makes it a baseline re-issue.

#### The probe refuted D-084's migration shape, and this is the finding

D-084 specifies the rung as a `links`-style rebuild. `links` has **no inbound foreign keys**;
`concepts` has two (`links.source_id`, `links.target_id`). `examples/concepts_rebuild_probe.rs`
was written to test whether the rung is writable at all. **Four approaches, measured on libSQL
0.9.30:**

| # | approach | result |
|---|---|---|
| 1 | `PRAGMA foreign_keys = OFF` inside the rung's transaction | **silently ignored** — `execute` returns Ok, reads back `1`. The pragma is a no-op inside a transaction and `apply_step` wraps every rung in `BEGIN IMMEDIATE` |
| 2 | `DROP TABLE concepts` inside the tx, FKs on | `FOREIGN KEY constraint failed` — **with or without** the delete guard. The guard is not even the obstacle |
| 3 | `PRAGMA defer_foreign_keys = ON` (per-transaction, designed for this) | every statement succeeds, `foreign_key_check` reports **0 violations** — and **COMMIT fails**. SQLite counts deferred violation *events*; re-adding an equivalent parent does not decrement the counter |
| 4 | rename-around (`concepts` → `concepts_old`, new one in, drop the orphan), with `legacy_alter_table` both ON and OFF | `FOREIGN KEY constraint failed` on the drop, both ways |
| **5** | **`PRAGMA foreign_keys = OFF` *outside* the transaction, rebuild inside, `foreign_key_check`, COMMIT, pragma back ON** | **works** — 2 concepts, 1 link, `rowid_pk` assigned, 0 violations, FK still enforced afterwards, delete guard still refusing |

**So the rung costs a ladder-mechanism change**, and that is new scope this plan is naming
before implementation rather than discovering during it. `Step` gains a flag — call it
`suspends_foreign_keys` — and `apply_step` toggles the pragma **around** the transaction for
rungs that set it, running `PRAGMA foreign_key_check` inside the transaction before commit so
the suspension cannot hide a real violation.

**Atomicity is not weakened**: the transaction is still one commit with the version stamp inside
it ([D-032](architecture/s13-decision-register.md#d-032)'s property). The pragma is
per-connection, and the migration connection is created in `open()` and discarded on failure, so
a crash between the toggle and the reset cannot leave a long-lived connection with FKs off. That
argument must be **written into the rung's rustdoc**, not left here.

**Steps.**
1. `Step` gains `suspends_foreign_keys: bool`; `apply_step` honours it and runs
   `PRAGMA foreign_key_check` inside the transaction, failing the rung on any row.
2. `SCHEMA_VERSION = 8`; `Step { from: 7, to: 8, name: "concepts-rowid-pk-and-unread-indices",
   suspends_foreign_keys: true }`.
3. Pin `CONCEPTS_V8` **as text**, for the reason `LINKS_V7` states (`migrations.rs:325` — a rung
   is a statement about the past).
4. Rung body, in order: `DROP INDEX` ×2 → create `concepts_v8` → copy `ORDER BY rowid` (so
   existing dense numbering is preserved) → drop the six `concepts` triggers → `DROP TABLE
   concepts` → rename → recreate triggers → drop and recreate `concepts_fts` with
   `content_rowid='rowid_pk'` → `REBUILD_CONCEPTS_FTS`.
5. `ddl.rs`: `CREATE_CONCEPTS_TABLE` gains `rowid_pk`; `CREATE_INDICES` loses two entries; the
   two FTS triggers use `NEW.rowid_pk` / `OLD.rowid_pk`; **add `trg_concepts_fts_delete`** —
   §4.6's named missing third trigger, inert while the delete guard is unconditional and
   required the moment 0.9.0 makes it conditional.
6. Callers: `tests/shadow_rebuild_tests.rs:219`, `examples/shadow_probe.rs:131`,
   `tests/migration_tests.rs:260`, `examples/chunk_diag.rs:408`.
7. `index_plan_tests`: `the_unread_indices_are_the_two_already_known` becomes **the unread set is
   empty**. That converts "these two are known bad" into "an index with no reader is a red
   test", which is what D-089 was arguing for.
8. `tests/wave1_regression_tests.rs:1233`'s rowid-density comment becomes historical — the
   density argument is replaced by an explicit column.

**`trg_concepts_guard_delete` stays unconditional in 0.8.0.** The rung installs the *capability*;
0.9.0's archive session is what makes the guard conditional. `vacuum_does_not_disturb_the_fts_index`
keeps passing throughout and becomes a genuine test rather than a tautology the moment rowids can
go sparse.

**Snapshot cost is already paid.** [D-043](architecture/s13-decision-register.md#d-043) makes a
`SCHEMA_VERSION` bump invalidate every snapshot on disk; Wave 4.4 made `open` re-anchor after a
migration precisely so the first `reconstruct` after an upgrade does not fold from genesis
(`connection.rs:626`).

**Exit gate.** `SCHEMA_VERSION == 8`. A v7 database opens, migrates and reads back identically,
with `PRAGMA foreign_key_check` clean after. `every_index_is_justified` green with an empty
`NoReader` set. `migration_tests` covers `v7 → v8` on the same ladder as every other rung, plus
a case with `links` rows present, because that is the condition the probe showed breaks the naive
shape. `assert_edge` measured on `star_of_stars` before and after — the argument is a per-insert
cost and this project does not ship an unmeasured performance claim
([D-088](architecture/s13-decision-register.md#d-088)). `vacuum_does_not_disturb_the_fts_index`
extended to a **sparse** `rowid_pk`.

**Documents.** `s4-schema.md` §4.1 (the `concepts` DDL), §4.2, §4.5, §4.6 (the third trigger
arrives — rewrite the "no delete trigger, by consequence rather than by choice" paragraph),
§4.7 if any row moves; `s5-modules.md` §5.1 (the ladder gains a rung kind); `s13` (D-115, D-116,
both superseding/completing D-089 and D-084); `s11-s12` R-row if the rebuild introduces one;
`README.md` schema-version table; `quickref.md:171` and the schema ladder.

### B5 · `reconstruct` below the log floor is not a corruption

**Reproduced against 0.7.0 as published**, through the binding:

| ledger | `reconstruct("2020-01-01…")` |
|---|---|
| empty database | `MaterializedState` — 0 concepts, 0 edges ✅ |
| one concept, one edge, never archived | `ReplayCorruptError: archive database file "…\kb_archive.db" does not exist` |

**The defect is the transition.** Before the first write the question has a correct answer;
after it the same question raises the class meaning *the ledger is damaged*, naming a file the
caller never created — the binding derives `kb_archive.db` from the database path. Asking what
was believed before your data existed is ordinary, and "empty" is the ordinary answer.

**Mechanism.** `hot_log_is_complete` (`replay.rs:512`) returns `false` when `ts` precedes
`MIN(recorded_at)`, and the branch below assumes the delta must be in cold storage. The hot file
records nothing about whether it was ever archived — the horizon lives in `cold.archive_horizon`
(`archive.rs:60`), inside the file whose existence is the question.

**Fix, needing no rung.** Return the empty state, and carry a flag on `MaterializedState` saying
the fold had no history to fold, so a caller who *does* have an archive can distinguish "nothing
was believed yet" from "I asked the wrong file". Where an archive path **was** given and the
file is missing, that stays an error — R14 is about it.

**The alternative is recorded rather than dropped**: a hot-side marker written by `archive()`
recording *archived at* and *horizon*, which would let the error say *"this database was archived
on 2026-06-01; pass the archive path"*. Rejected for 0.8.0 as a second rung in a release already
taking one — **and note it becomes cheap in 0.9.0**, where the archive path is being touched
anyway. If 0.9.0 takes it, this decision is revisited there rather than reopened.

**Exit gate.** The table above with the second row answering as the first does.
`tests/temporal_tests.rs:257` — which currently pins the present behaviour as correct — rewritten,
and a second test keeping the *named-archive-missing* path red. A property test asserting
`reconstruct(t) == empty` for every `t` below the log floor on a never-archived ledger, since the
claim is universally quantified over `t`.

**Documents.** `s5-modules.md` §5.5; `s6-s10` §7 (an error class stops being raised on a path);
`appendices.md` Appendix A if `MaterializedState` gains a field; `s14` §14.9 (this is the "rough
edge recorded for the crate" — mark it closed); `s13` (D-118).

**Behaviour change without a compile break** — release note by name: which error class stopped
being raised.

### B6 · Louvain's aggregation phase — *conditional, gated on B2*

**Louvain is fully implemented and deliberately phase-one-only.** It performs local moving —
nodes move greedily to whichever neighbouring community most increases modularity, up to
`LOUVAIN_MAX_SWEEPS = 100`, with `LOUVAIN_MIN_GAIN = 1e-12` so float noise cannot drive a sweep.
It does **not** aggregate communities into super-nodes and recurse. The rustdoc states this and
justifies it: *"For subgraph-sized analytics the local moving phase is the part that carries the
signal; the aggregation phase would matter on graphs far larger than the byte budget admits."*
Measured at 1.99 / 13.9 / 28.3 ms across sizes on `star_of_stars`
([D-063](architecture/s13-decision-register.md#d-063)).

**B2 weakens that justification by nearly an order of magnitude**, since the stated reason is
that the byte budget bounds graph size and interning raises what fits by 7–9×. The scope limit
was correct *for the old ceiling* and has to be re-argued against the new one.

**Gate, and skip if unmet** — the discipline T3.3 imposed on interning itself. After B2, run
phase-one-only against a full two-phase implementation on `clustered` at the new maximum size and
compare **Q**, not runtime. Take aggregation only if Q differs materially. If it does not,
**record the measurement and close the question** rather than leaving it open, which D-063 calls
"the worst of the three states".

**Two things not to disturb.** The oracle is an **upper** bound — modularity must not exceed the
enumerated optimum over all set partitions, via restricted growth strings — and *not* a lower
one, because "modularity is non-decreasing against the singleton partition" is satisfied by
construction by the all-singletons stub this algorithm replaced
([D-039](architecture/s13-decision-register.md#d-039)). And ties resolve to the lowest community
index by `BTreeMap` scan order; anything reintroducing a `HashMap` gives a different partition per
process.

**Documents.** `s5-modules.md` §5.4; `s6-s10` §8 (the oracle's direction, if aggregation lands)
and §9; `s13` (D-119); `quickref.md`.

### B7 · The binding surface, the stub, and the wheel

**The item that closes the gap between "the crate changed" and "the wheel is correct".** B1–B6
stop at the Rust boundary. Three of them are visible from Python, and one of those is a break —
and none of it happens by itself, because a `.pyi` is not generated
([D-109](architecture/s13-decision-register.md#d-109)) and `pyproject.toml` declares
`dynamic = ["version"]` but the manifests do not bump themselves.

#### What actually crosses

| From | Python-visible effect |
|---|---|
| **B2** interning | **nothing.** `graph.rs` reads `inner.nodes` / `inner.out_adj` today and moves to the accessors; every `#[pymethods]` signature is unchanged. This is [D-101](architecture/s13-decision-register.md#d-101) paying off |
| **B3** `content` optional | **a break.** `PyNodeData::content` is `#[getter] fn content(&self) -> &str` (`graph.rs:85`) and becomes `Option<&str>` → `str \| None`. `to_dict()`'s node entries change shape when content is absent. `Database.load_subgraph` gains a keyword |
| **B5** pre-genesis reconstruct | `MaterializedState` gains the "no history to fold" flag; `ReplayCorruptError` stops being raised on that path |
| **B4** schema v8 | nothing at the surface — but a wheel built against v7 opens a v8 database and vice versa, so the version bump is not cosmetic |

#### Why `str | None` rather than `""`

Returning an empty string for absent content is the failure
[D-096](architecture/s13-decision-register.md#d-096) refused for open intervals, restated: a
sentinel that is a *valid value of the type* cannot be distinguished from the real thing. A
concept whose `content` is genuinely empty and one that was not loaded are different facts, and
`""` conflates them at exactly the moment a caller is deciding whether to go back to the
database. `None` is the same answer this binding already gives for an open interval, and for the
same reason.

#### Steps

1. **`bindings/python/src/graph.rs`** — `PyNodeData::content` → `Option<&str>`; `to_dict()` emits
   `None` for absent content rather than omitting the key, because a missing key and a present
   `None` are different in Python and the omission is the one callers write `KeyError` bugs
   against; field reads move to B1's accessors.
2. **`bindings/python/src/database.rs`** — `load_subgraph` gains `content: bool = False`,
   keyword-only, matching the existing `edge_types` / `min_weight` / `now` shape. `traverse` is
   unaffected: it hydrates by `AttributeMode`, which is a different question
   ([D-102](architecture/s13-decision-register.md#d-102)).
3. **`bindings/python/src/temporal.rs`** — `PyMaterializedState` exposes B5's flag as a property.
   Name it for what it is (`folded_from_genesis` / `had_no_history`), not for the branch it came
   from.
4. **`python/macrame/_macrame.pyi`** — `NodeData.content`, `Subgraph.to_dict`,
   `Database.load_subgraph`, `MaterializedState`. **`tests_py/test_stubs.py` is red until this is
   done**, in both directions and against `errors.rs`, which is the mechanism working as designed
   rather than a nuisance.
5. **`tests_py/`** — `test_read_path.py` and `test_types.py` assert on `content`; add the
   absent-content case and a test that the six algorithms agree with content on and off, which is
   [B3](#b3--content-leaves-the-default-load)'s claim asserted from the Python side too.
6. **Version** — `Cargo.toml:7`, `bindings/python/Cargo.toml:7`, `README.md:38`
   (`macrame-db = "0.8"`), `Cargo.lock`. `tests/packaging_tests.rs` asserts the binding version
   equals the root version and `tests_py/test_packaging.py` asserts `macrame.__version__` against
   the manifest, so a partial bump is caught — **but only if the wheel is rebuilt and
   reinstalled**, which is the step that was missed once already in 0.7.0.
7. **The README's Python example** — it calls `load_subgraph`. Run it verbatim before the tag;
   the 0.7.0 example raised `FOREIGN KEY constraint failed` when first checked, because it
   asserted an edge to a concept it never wrote.

#### Exit gate

- `tests_py/run_suite.py` green through the gate, on the rebuilt wheel — **not** on a stale
  `maturin develop` artifact.
- `mypy --strict python/macrame` clean.
- `test_stubs.py`'s five injections still caught: module name dropped, class member dropped,
  member invented, exception attribute forgotten, exception attribute invented.
- `python.yml`'s `abi3` job green — one wheel built on 3.10, whole suite run on 3.13 against it.
- A Python-side assertion that `dijkstra`, `astar`, `scc`, `k_core`, `louvain` and `modularity`
  return identical results with `content=True` and `content=False`.
- `macrame.__version__ == "0.8.0"` read out of an installed wheel.

#### Documents

`s14` §14.4 (the value types — `content` joins the list of things that cross as `None`), §14.9
(status table: the 0.7.0 "rough edge recorded for the crate" is closed by B5), §14.10 (the read
path and the `Subgraph` handle), §14.11 if `MaterializedState`'s shape is described there, §14.15
(the stub's conventions gain one); `appendices.md` Appendix A if the Rust signature moved;
`s13` (D-123); `README.md` — the Python section's table, the `macrame-db = "0.8"` line, and the
example itself; `quickref.md` §10.

#### Rejected

*Regenerating the stub with `pyo3-stub-gen`* — refused at P8 for reasons B3 makes stronger, not
weaker: a generator writes `Optional[Any]` exactly where the interesting distinction lives
([D-109](architecture/s13-decision-register.md#d-109)). *Keeping `content` as `str` and returning
`""`* — see above. *Defaulting `content=True` in Python only, so the binding does not break* — a
binding that disagrees with the crate about a default is a second source of truth, and the whole
argument for the opaque handle is that there is one.

---

## Track C — 0.9.0, concept archival

**No migration.** 0.8.0's B4 installed everything the schema needs. 0.9.0 is feature work on a
stable schema, which is the best possible shape for the release carrying the design work.

### C1 · The archivability predicate

A link has a closed interval, so `LINKS_ARCHIVABLE` (`archive.rs:74`) can say *"closed before the
cutoff, or superseded"*. **A concept is an entity and has no closed state**, so archivability must
be expressed as **reachability**:

> A concept is archivable when it is `retired`, its `valid_to` precedes the cutoff, **and no
> surviving row of hot `links` references it in either direction.**

That last clause is what keeps the foreign key from `links` satisfiable without `CASCADE`, and it
makes concept archival **strictly downstream of link archival**: concepts become eligible only
once the edges mentioning them have themselves gone cold.

**Exit gate.** A property test over generated histories: after `archive(cutoff)`, every surviving
hot link's endpoints are present in hot `concepts`, and every archived concept is unreferenced.
The model-based shape `integrity_property_tests` uses, not a fixture —
[D-030](architecture/s13-decision-register.md#d-030) is why.

**Documents.** `s4-schema.md` §4.1; `s5-modules.md` §5.7; `appendices.md` Appendix C (the design
moves from "deferred" to "delivered"); `s13` (D-120).

### C2 · `cold.concepts`, and the guard becomes conditional

**Steps.**
1. `cold.concepts`, trigger-free, alongside `cold.links` — the shape Appendix C already sketches.
2. `trg_concepts_guard_delete` becomes marker-gated, matching `trg_links_guard_delete` and
   `trg_txlog_guard_delete` exactly: `WHEN NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type =
   'table' AND name = 'macrame_archive_session')`. **One trigger, the existing pattern.**
3. `trg_concepts_fts_delete` (installed inert by B4) now fires, keeping the search index correct
   as concepts leave.
4. `reconstruct` folds cold concepts by the same last-writer-wins `seq_id` rule already used for
   the log.
5. **No embedding crosses.** [Doctrine VII](architecture/s0-s3-foundations.md#doctrine-vii) makes
   an embedding a derived artifact, so an archived concept needs enough content preserved to
   *recompute* one on rehydration — which is what dissolves both the FK-from-embeddings problem
   and the absence of `F32_BLOB`/DiskANN on the ATTACHed cold file.

**Exit gate.** `tests/integrity_tests.rs:188` — which pins the guard as unconditional — rewritten
to pin *conditional*, with an ad-hoc delete outside a session still refused.
`vacuum_does_not_disturb_the_fts_index` now runs against genuinely sparse rowids and is a real
test. A cold-roundtrip test: archive concepts, `reconstruct` across the boundary, compare against
the pre-archive answer.

**Documents.** `s4-schema.md` §4.1 and §4.6 (the third trigger becomes live); `s5-modules.md`
§5.7; `s0-s3` if Doctrine V's commentary needs the concept case named; `s13` (D-120);
`s11-s12` R3 (log growth) — its mitigation now covers concepts.

### C3 · Rehydration is a move back, not a write

**Decided, not open** ([§2.3](#23-concept-archival-is-the-sanctioned-exit-and-traceability-answers-its-open-question)).
Rehydration mints no transaction-time facts: it moves rows from `cold.concepts` to hot inside a
declared session and updates the horizon. A concept reacquires its identity because the
alternative makes the transaction-time axis lie about when the concept was learned.

**Exit gate.** Archive → rehydrate → `reconstruct(t)` for a `t` spanning both operations returns
**bit-identical** state to the never-archived control at the same `t`. That is the executable form
of "traceable through the bitemporal ledger" and it is the whole point of the release.

**Documents.** `s5-modules.md` §5.7; `appendices.md` Appendix C and the glossary (rehydration is
a new term); `s13` (D-121); `s14` if the binding exposes it.

### C4 · Measure rehydration, and decide the hot-side marker

Rehydration cost is unmeasured and Appendix C names it as one of the two reasons archival was
deferred. Measure it on the fixture matrix, at the sizes §9 uses.

**And revisit B5's deferred alternative here**, where it is cheap: a hot-side marker recording
*archived at* and *horizon* would let a pre-genesis `reconstruct` say *"this database was archived
on X; pass the archive path"* instead of returning empty. 0.8.0 declined it as a second rung;
0.9.0 is already writing archive metadata, so the marginal cost is small — but it is a hot-table
addition, so under [D-036](architecture/s13-decision-register.md#d-036) it must land pre-1.0 or
not at all.

**Documents.** `s6-s10` §9 (a new budget row, with its fixture named); `s13` (D-122 if the marker
lands, or an amendment to D-118 if it does not).

### C5 · The binding catches up with archival

**Decided here rather than left as "if the binding exposes it".** The binding already exposes
`archive`, `archive_windowed` and `verify_snapshot_chain`; archival that concepts participate in
but Python cannot drive would make the wheel a second-class door onto the same ledger, which
[§14](architecture/s14-python-bindings.md) explicitly says it is not.

**Steps.**
1. **`ArchiveReport` gains concept counts.** C1/C2 make `archive()` move concepts as well as
   links, so the report's shape changes and `PyArchiveReport` follows. Additive, but it is a
   Python-visible field and therefore a stub change.
2. **`db.rehydrate(...)`** — the counterpart to `archive`. Same synchronous shape, same GIL
   release, same typed errors. It is a write, so it queues through the actor like any other
   ([§5.1.8](architecture/s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028))
   and its latency contract is documented, not implied.
3. **The doctrine claim needs a Python test, because it is the release's whole point.** C3's gate
   — archive → rehydrate → `reconstruct(t)` bit-identical to the never-archived control — is
   asserted from Python too. Not duplication: the Rust test proves the ledger is traceable, the
   Python one proves the *boundary* does not lose the property, which is the same argument
   [§14.7](architecture/s14-python-bindings.md#147-r15-through-the-boundary) makes about R15
   reaching through rather than being absorbed.
4. **Errors.** Any new `DbError` variant from C1–C3 must map to its own exception class — the
   exhaustive `match` in `errors.rs` makes this a **compile** failure in `macrame-py` before a
   wheel is built ([D-099](architecture/s13-decision-register.md#d-099)), so this step cannot be
   forgotten, only done. The stub and `test_stubs.py`'s `errors.rs` comparison still have to
   follow by hand.
5. **Version** — `0.9.0` across the same four places as [B7](#b7--the-binding-surface-the-stub-and-the-wheel) step 6.

**Exit gate.** As B7's, plus the archive→rehydrate→reconstruct equality asserted from Python, and
`test_errors.py`'s exhaustiveness green against the new variants.

**Documents.** `s14` §14.9 (status), §14.11 (temporal — `ArchiveReport`, `rehydrate`), §14.3 if
the error tree gains a branch; `python/macrame/_macrame.pyi`; `appendices.md` Appendix A;
`s13` (D-124); `README.md` Python section; `quickref.md` §10.

---

## 5. Sequencing and dependencies

```
NOW      A1  gate classifier ........... main is red; nothing below is measurable until it lands
         A2  libSQL 0.10 probe ......... answer wanted while 0.8.0 is still open
         A5  decision-deadline tripwire . red today, by design; goes green as B1–B4 land

0.8.0    B1  privatise + accessors ..... ┐
         B2  intern behind them ........ ├ Subgraph moves once, in this order
         B3  content out of default .... ┘
         B4  schema v8 ................. independent; needs the apply_step change first
         B5  reconstruct below floor ... independent
         B6  Louvain aggregation ....... gated on B2 — skip and close if Q agrees
         B7  binding + stub + wheel .... LAST: needs B2, B3, B5 settled
         A3  doc truth pass ............ README rows before the tag
         A4  macOS row, checkout@v5

0.9.0    C1  archivability predicate ... needs B4's rowid_pk in the wild
         C2  cold.concepts + guard ..... needs C1
         C3  rehydration ............... needs C2
         C4  measure + marker decision . needs C3
         C5  binding + stub + wheel .... LAST: needs C2, C3, and C4's marker decision
```

**B7 and C5 are last in their release and cannot be started early.** Each is downstream of every
Rust surface its release moves, and a stub written against a half-settled surface is a stub
written twice. That is also why they are the items most at risk of being skipped: everything
before them is green when they begin, and `test_stubs.py` going red is the *only* thing that says
they have not been done.

| Item | Touches | Rung | Breaking |
|---|---|---|---|
| A1 | `ci.yml`, one new script | — | no |
| A2 | nothing shipped | — | no |
| A3 | README, quickref, s11, s14, 2 workflows | — | no |
| A4 | 4 workflows | — | no |
| A5 | `doc_sync_tests.rs` | — | no |
| B1 | `subgraph.rs`, 13 call-site files, Appendix A, §5.4, quickref | — | **yes — Rust only** |
| B2 | `subgraph.rs`, `graph.rs` internals, `budget_density_diag.rs` | — | no (after B1) |
| B3 | `subgraph.rs`, loader, `to_dict()` | — | behaviour |
| B4 | `migrations.rs` (+`Step`), `ddl.rs`, 4 callers, `index_plan_tests.rs` | **v7 → v8** | no |
| B5 | `replay.rs`, `temporal_tests.rs` | — | behaviour |
| B6 | `algorithms.rs` | — | no |
| B7 | `bindings/python/src/{graph,database,temporal}.rs`, `_macrame.pyi`, `tests_py/`, 3 manifests, `s14` | — | **yes — Python, one getter** |
| C1–C4 | `archive.rs`, `replay.rs`, `ddl.rs` (triggers only) | **none** | no |
| C5 | `bindings/python/src/{temporal,database,errors}.rs`, `_macrame.pyi`, `tests_py/`, 3 manifests, `s14` | — | no (additive) |

**Hard constraints.**
- **B1 before B2.** The break lands with the representation unchanged so the compiler and the
  suite answer one question at a time.
- **`apply_step`'s FK suspension before B4's rung.** Measured, not assumed — see the probe table.
- **B4 before C1.** 0.9.0 must not carry a migration.
- **One rung in 0.8.0.** Every change to `concepts` travels on it; a second visit is a second full
  rebuild.
- **B7 after B2, B3 and B5; C5 after C2, C3 and C4.** The binding tracks a settled surface, never
  a moving one.
- **The wheel is rebuilt and reinstalled before either exit gate is read.** A green
  `tests_py/run_suite.py` against a stale artifact tests the previous release.

---

## 6. Decision entries this plan creates

| | |
|---|---|
| **D-110** | The Rust suite is gated by a classifier, not a retry count; [D-107](architecture/s13-decision-register.md#d-107)'s four outcomes apply to Rust, and the budget was calibrated on the wrong step (A1) |
| **D-111** | R15 against the libSQL 0.10 pre-release: the measurement, either way (A2) |
| **D-112** | A decision entry naming a release is a claim with a deadline, and deadlines get tripwires (A5) |
| **D-113** | `Subgraph`'s fields are private and its keys interned; **supersedes [D-087](architecture/s13-decision-register.md#d-087)**, corrects its blast-radius estimate, carries [D-063](architecture/s13-decision-register.md#d-063)'s pre-registered two-orderings test (B1, B2) |
| **D-114** | `content` is not loaded by default; `Subgraph` serves the algorithms first (B3) |
| **D-115** | Schema v8 drops the two indices with no reader; the `NoReader` category becomes empty rather than known-bad. **Completes [D-089](architecture/s13-decision-register.md#d-089)** (B4a) |
| **D-116** | `concepts` gains `rowid_pk` and the FTS index its third trigger, taken pre-1.0 under [D-036](architecture/s13-decision-register.md#d-036)'s deadline. **Completes [D-084](architecture/s13-decision-register.md#d-084) and corrects its migration shape** — a rung rebuilding a table with inbound foreign keys needs FK enforcement suspended *around* the transaction, measured four ways (B4b) |
| **D-117** | **Erasure is refused on doctrine, not deferred**; the alternative is pseudonymous ids with identifying content held by the application. **Supersedes [D-022](architecture/s13-decision-register.md#d-022)'s open framing** ([§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)) |
| **D-118** | A `reconstruct` below the log floor on a never-archived ledger is the empty state, not a corruption; the hot-side marker is designed and deferred to C4 (B5) |
| **D-119** | Louvain's aggregation phase, taken or closed on a Q comparison at the post-interning ceiling (B6) |
| **D-120** | Concept archival: archivability is reachability, not expiry; the delete guard becomes marker-gated (C1, C2) |
| **D-121** | Rehydration is a physical move back and mints no transaction-time facts — **derived from the traceability requirement**, closing Appendix C's open Doctrine III question (C3) |
| **D-122** | The hot-side archive marker, taken or refused with C4's measurement |
| **D-123** | Absent `content` crosses to Python as `None`, not `""` — the same refusal of a valid-value sentinel that [D-096](architecture/s13-decision-register.md#d-096) made for open intervals; the stub stays hand-written and the interning is confirmed invisible at the boundary (B7) |
| **D-124** | Archival is drivable from Python: `rehydrate` is exposed, and the traceability equality is asserted **through the binding** as well as in the crate, because a boundary that silently loses a doctrine property is the failure [§14.7](architecture/s14-python-bindings.md#147-r15-through-the-boundary) measured rather than assumed (C5) |

---

## 7. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Interning silently changes a partition via loader ordering | Medium | **High** — §8's oracle cannot see it ([D-063](architecture/s13-decision-register.md#d-063)) | The two-orderings test is a gate on B2, not a follow-up |
| The `apply_step` FK suspension hides a real violation | Low | **High** | `PRAGMA foreign_key_check` runs **inside** the transaction and fails the rung; measured clean in the probe |
| A crash mid-rung leaves a connection with FKs off | Low | Low | Per-connection pragma; the migration connection is created in `open()` and discarded on failure. Argument goes in the rung's rustdoc |
| `concepts` rebuild on a large database | Certain | Medium | O(rows), ~2× peak disk, same as v6 → v7. Documented in the rung and the release note |
| v8 invalidates snapshots | Certain | Low | Already handled — `open` re-anchors after a migration (Wave 4.4, `connection.rs:626`) |
| The id table cancels the memory win | Low | Medium | Per-node cost against per-edge saving; measured by `estimated_bytes()` rather than argued |
| B3 surprises a caller reading `content` | Medium | Low | `Option<String>`, so absence cannot be mistaken for emptiness; named in the release note |
| A1's classifier masks a real property failure | Low | High | Verified by injection in both directions before it is trusted |
| 0.9.0 needs a rung after all | Low | **High** — forfeits the shared rebuild | B4 installs `rowid_pk` *and* the inert delete trigger, which is the full set C2 needs. Re-check before tagging 0.8.0 |
| **B7/C5 skipped** — everything before them is green when they start | **Medium** | High — a wheel whose stub describes the previous release | `test_stubs.py` goes red on the first surface change and stays red; it is the only signal, so it is in the per-release definition of done rather than only in the item |
| The suite is read against a stale wheel | Medium | Medium | Rebuild-and-reinstall is a named step in B7/C5 and a per-release gate. This was missed once in 0.7.0 |
| A new `DbError` variant reaches Python untyped | Low | Medium | Cannot happen silently — the exhaustive `match` in `errors.rs` fails to compile `macrame-py` ([D-099](architecture/s13-decision-register.md#d-099)). The stub still has to follow by hand |

---

## 8. Definition of done

**Per item:** code, tests (including the named injection or property test), **every document in
its Documents list**, and a register entry with its `Rejected:` list — in the same commit or the
same short series. An item whose documents lag is not done.

**Per release:**
- Suite green through the A1 classifier on all three platforms, three consecutive runs.
- **The wheel is rebuilt and reinstalled**, then `tests_py/run_suite.py` is green through the
  gate against *that* artifact — not a `maturin develop` build from earlier in the cycle.
- `mypy --strict python/macrame` clean; `test_stubs.py` green in both directions and against
  `errors.rs`, with its five injections still caught.
- `macrame.__version__` read out of the installed wheel equals the tag, and both manifests and
  `README.md`'s dependency line agree.
- `doc_link_tests`, `doc_sync_tests`, `fixture_matrix_tests` and A5's new tripwire green.
- `cargo publish --dry-run` clean; wheels build on all four targets; `python.yml`'s `abi3` job
  green (built on 3.10, whole suite run on 3.13 against it).
- The README's Python example run **verbatim** and its output checked — 0.7.0's raised a foreign
  key error the first time this was done.
- `README.md`, `quickref.md`, `s14` §14.9 and `s11-s12`'s status reflect the release **before**
  the tag.
- This document gains a delivery blockquote per item, in the house style of the v0.6.0 and
  v0.7.0 plans — including **what each item's reasoning got wrong**, which is the part those
  plans made most valuable.

---

*Proposed 2026-08-01, against v0.7.0 as published. The interning blast radius, the 0-download
premise and the five-way `concepts`-rebuild probe were measured for this document rather than
carried from the register. `examples/concepts_rebuild_probe.rs` is kept: four of its five
approaches fail, and they are exactly the four a later reader would otherwise re-propose.*
