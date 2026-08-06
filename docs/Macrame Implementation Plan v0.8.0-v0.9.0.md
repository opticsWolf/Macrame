# Macrame — Implementation Plan v0.8.0 → v0.9.0

**Status:** proposed, 2026-08-01. Supersedes the withdrawn v0.7.5 consolidation proposal and
the v0.8.0 outline that preceded it.
**Basis:** a read of the crate, the docs, the four workflows, the CI history and both
registries, plus direct reproduction or measurement of every claim below — including
`examples/concepts_rebuild_probe.rs`, written for this plan, which **refuted the shape D-084
specifies for its own migration** and established the one that works.

**Two releases, one theme.** 0.8.0 spends the last cheap API break and the last cheap schema
rung this project will get. 0.9.0 builds concept archival on top of them, ~~needing no migration
at all~~ **needing one cheap trigger-only rung after all — corrected 2026-08-06, see
[§1](#1-the-two-releases-at-a-glance) and [D-126](architecture/s13-decision-register.md#d-126)**.
Erasure is refused rather than deferred, and [§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)
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
| Schema rung | **v7 → v8** ([B4](#b4--schema-v8-the-last-cheap-rung)) | ~~**none**~~ **v8 → v9** — corrected 2026-08-06, see below ([D-126](architecture/s13-decision-register.md#d-126)) |
| Python API | **one narrow break** — `NodeData.content` becomes `str \| None` ([B7](#b7--the-binding-surface-the-stub-and-the-wheel)). The interning is invisible: [D-101](architecture/s13-decision-register.md#d-101) pre-paid for it | additive ([C5](#c5--the-binding-catches-up-with-archival)) |
| Doctrine | erasure refused, recorded ([§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)) | Doctrine V's archive path extended to concepts |

Track A lands on `main` continuously and ships *in* 0.8.0 — it simply does not wait for it.
A1 must land now because `main` is red today.

> **The "none" in the schema-rung column was wrong, and the pre-tag re-check [§7](#7-risks)
> asked for is what found it** ([D-126](architecture/s13-decision-register.md#d-126),
> 2026-08-06). B4 shipped [C2](#c2--the-delete-guard-becomes-conditional)'s steps 1 and 3 but not
> step 2: `trg_concepts_guard_delete` is still *unconditional* in v8 while `trg_links_guard_delete`
> is already marker-gated, and two measured facts make correcting it a rung rather than a baseline
> re-issue — `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the **old body**, and
> `verify()` compares `type, name` and never trigger bodies, so a stale guard passes verification
> in silence. **The correction is smaller than the [§7](#7-risks) row feared** — a `DROP TRIGGER` +
> `CREATE TRIGGER`, no table rebuild, no data movement — but it is not nothing, and it is the
> reason the next release is **0.9.0 and not 0.8.5**. Under Cargo's 0.x caret rule
> `macrame-db = "0.8"` accepts any `0.8.z` on a routine `cargo update`, and a v9 database is
> hard-refused by 0.8.0 code (`migrations::run`: *"will not operate on a schema it does not
> know"*). A dependency update must not be able to migrate a user's database irreversibly, so the
> rung takes the minor bump that makes the upgrade a deliberate manifest edit. The two "last cheap
> X" claims in the preamble stand for the **API break**; the "no migration at all" half does not,
> and is struck there.

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
indefinitely. [D-128](#6-decision-entries-this-plan-creates) replaces that with a refusal and a
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
([D-130](#6-decision-entries-this-plan-creates)).

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

> **Delivered 2026-08-02** — [D-112](architecture/s13-decision-register.md#d-112). `macos-latest`
> in the Rust matrix; twenty action pins bumped across all four workflows.
>
> **The plan's "bump to `@v5`" was written against a stale view of the ecosystem, and the
> correction is the substance of this item.** `actions/checkout` is at **v7**, not v5. More
> importantly, Node 24 arrived at a **different major for each affected action** — and which
> ones are affected was read out of each action's own `action.yml` rather than inferred from
> the deprecation banner:
>
> | action | was | declares | bumped to | first `node24` major |
> |---|---|---|---|---|
> | `actions/checkout` | v4 | `node20` | **v5** | v5 (newest: v7) |
> | `actions/setup-python` | v5 | `node20` | **v6** | v6 (newest: v7) |
> | `actions/upload-artifact` | v4 | `node20` | **v6** | v6 (newest: v7) |
> | `actions/download-artifact` | v4 | `node20` | **v7** | v7 (newest: v8) |
> | `Swatinem/rust-cache` | v2 | `node24` | — | already fine |
> | `PyO3/maturin-action` | v1 | `node24` | — | already fine |
> | `pypa/gh-action-pypi-publish` | release/v1 | composite | — | no Node runtime |
>
> Reading the manifests is what kept three untouched pins from being bumped for nothing, and
> it is why `setup-python` is in the table at all — **A1 introduced that dependency four hours
> ago, at `@v5`, which is `node20`.** A1 added a deprecated action while fixing something else.
>
> **Bumped to the first `node24` major, not the newest**, and that is a decision rather than
> laziness. The newest would additionally take an ESM migration, `download-artifact`'s new
> hash-mismatch enforcement, `upload-artifact`'s direct-upload semantics and `checkout`'s
> fork-PR blocking — none of which this repository uses, and **none of which can be tested
> anywhere but on CI itself**. The stated problem is a deprecated runtime; these four solve it
> exactly. The one skipped major with a real break was checked rather than waved past:
> `download-artifact` v5 changed the output path for single-artifact downloads **by ID**, and
> `wheels.yml` names no artifact — `path: dist` with `merge-multiple: true` — so it does not
> apply.
>
> **macOS is four characters and one unknown.** The matrix gains `macos-latest`, so the README's
> three-platform promise stops resting on the *binding's* CI. It also puts a number on
> something never measured: every R15 figure in `.cargo/config.toml` and the risk row comes
> from one Windows machine, and the fault not having been *seen* elsewhere is not the same as
> it being absent. A1's classifier is what makes that legible — a macOS fault will report
> `CRASH` and name its target rather than arriving as a smaller green.
>
> **`cargo-deny` / `cargo-audit` recorded as declined**, per this item's intent, in D-112's
> Rejected list along with SHA-pinning: both are supply-chain *policy*, and neither belongs
> smuggled in beside a runtime bump.
>
> **What cannot be verified from here.** All four workflows parse and every pin resolves, but
> whether the Rust suite is green on `macos-latest` is discovered on the first push — it has
> never run there. If it goes red, that is a finding, not a regression: A4 is the item that
> makes the platform claim testable, and the first result is the first evidence either way.

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

> **Delivered 2026-08-02** — [D-113](architecture/s13-decision-register.md#d-113), not D-112:
> A4 took that number. `no_decision_still_awaits_a_release_that_has_shipped` plus two guard
> tests in `tests/doc_sync_tests.rs`.
>
> **Red first, and it named exactly the two.** On the unmodified register: `D-087` and `D-089`,
> nothing else, with the guard tests green beside it. Then both injection arms against the real
> register — a fabricated `Scheduled for 0.6.0` entry fails; the same entry with
> `DELIVERED in 0.6.0` passes; injection reverted.
>
> **Three corrections the test forced on its own design, and they are the substance of this item.**
>
> *A per-entry marker disarms the tripwire permanently.* The plan specified a bare `— DELIVERED`.
> But an entry that missed 0.7.0 and now names 0.8.0 carries **two** claims, and one unqualified
> marker settles both — the mechanism built to catch a missed release would have waved through
> the next one. Markers are keyed to the version they close: `DELIVERED in 0.8.0`,
> `RESCHEDULED from 0.7.0`. Each entry now comes due again at each boundary by itself.
>
> *The original sentences must not be reworded — [Doctrine III](architecture/s0-s3-foundations.md#doctrine-iii)
> applied to this file.* The easy way to green the test was to edit *"Scheduled for 0.7.0"* out of
> D-087 and D-089. That is rewriting an assertion in place. This register is a ledger of belief and
> the doctrine governing `links` governs it too: **superseded, never overwritten.** Both sentences
> stand exactly as written; a later sentence in each entry records that the release passed without
> them. Greening the test by destroying the evidence it was ever wrong was the worst available
> outcome and the easiest one.
>
> *The test fired on D-113 itself*, which quotes *"Scheduled for 0.7.0"* to explain the failure.
> A register that records its own history will keep quoting the schedules it records, so this is a
> class: a phrase immediately preceded by a quotation mark is a citation, not a commitment. The
> rule is deliberately dumb, per [D-088](architecture/s13-decision-register.md#d-088) — no
> deciding *which* quotations are citations. The gap is stated in the source rather than hidden.
>
> **The exit gate said "green once B1–B4 land". It is green now, and that is the stronger
> outcome.** Leaving `main` red on purpose is what [D-110](architecture/s13-decision-register.md#d-110)
> had just finished fixing, and red-on-purpose is indistinguishable from red-by-accident to
> anyone who did not write it. Neither decision shipped, so neither carries `DELIVERED`: both
> record that they missed 0.7.0 and name **0.8.0** (B2, B4). That is true, and **it arms the
> tripwire for the next boundary** — when B7 bumps `CARGO_PKG_VERSION` to 0.8.0, both come due
> again and the build stops unless the work landed and the marker is written.
>
> Rescheduling D-089 turned up a second error beside it: that entry and `README.md` both
> described the two unread indices as *added in v8*. They are in the **v7 baseline**, in
> `ddl::CREATE_INDICES`. Corrected in both — the same error A3 found from the other end.

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

> **Delivered 2026-08-02** — [D-114](architecture/s13-decision-register.md#d-114), not D-113:
> A5 took that number. Suites **308** / **319** / **344**, clippy clean.
>
> **Zero behavioural diff, measured rather than asserted.** `fixture_matrix_diag` was run
> against the pre-B1 commit in a detached worktree and against this one: every structural
> column identical on all four fixtures, `estimated_bytes` at 339,638 / 2,168,375 / 324,653 /
> 30,744,680. The property suites are what cover *same partitions, same distances* — they
> compare against brute-force oracles, so a changed answer fails by construction.
>
> **The blast radius was right about `algorithms.rs` and wrong about the number.** This item's
> correction to [D-087](architecture/s13-decision-register.md#d-087) holds: adjacency is reached
> **only** through `out_edges()` / `in_edges()`, 0 sites on either map. But "ten mechanical
> substitutions" counted only `.nodes`. The file also reads `EdgeRef` fields off those borrowed
> slices at **15 further sites**, which move once `EdgeRef` is closed — **26, not 10**. All
> mechanical; none a design question.
>
> **`NodeData` and `EdgeRef` had to be closed here, and the step list implies but never says
> why.** Leaving their fields public would make B1 cheap and then break the API twice more:
> B2 changes `EdgeRef::node` to `u32`, B3 changes `NodeData::content` to `Option`. *The break
> taken once* is this item's premise, and a break taken once per field is what it exists to
> avoid.
>
> **"The Python surface does not move" was read as "the binding needs no work".** The first
> half is exactly right — [D-101](architecture/s13-decision-register.md#d-101)'s opacity means
> **not one `#[pymethods]` signature changes.** But ~20 lines *behind* those methods read the
> Rust fields directly, and they now call accessors. `to_dict` needed the two new adjacency
> iterators. Worth separating, because the plan recorded only the surface half.
>
> **`add_edge` is public now**, which the steps did not anticipate. Three fixtures, a property
> generator and a diagnostic each built adjacency by hand, each doing its own
> `back.node = source` — five copies of an invariant the type already had one function for.
> They call it. A real improvement fell out of a mechanical change.
>
> **Two new tests, both verified by injection**: no document may advertise a field this item
> made private, and every public `Subgraph` method must appear in the block `quickref.md`
> quotes. **The first version of the field check went red on correct documentation** — it
> searched the whole file for `pub title:`, `pub weight:` and friends, which `ConceptUpsert`
> and `EdgeAssertion` legitimately declare, sharing five names. Scoped to the three
> declarations. That is [D-088](architecture/s13-decision-register.md#d-088)'s lesson landing
> for the third time this release, inside a test whose own comment warns about it.
>
> **One environment hazard worth knowing.** `cargo build` reported `Finished` in 0.22 s
> **without rebuilding** after the first edit to `subgraph.rs`, and reported success on code
> that does not compile. The repo is owned by a different Windows user; touching the file
> forced the rebuild. A green build here is not by itself evidence that the build ran.

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

> **Delivered 2026-08-02** — [D-115](architecture/s13-decision-register.md#d-115), not D-113.
> Suites **311** / **322** / **344**, clippy clean. `size_of::<EdgeRef>()` is **24**, measured.
>
> **The projected win was optimistic, and the exit gate is why we know.** It required the table
> "on the real type, not on `size_of` arithmetic", and the two disagree:
>
> | id length | B/edge at 0.7.0 | projected | **measured, all-in** | projected | **measured** |
> |---|---|---|---|---|---|
> | 8 | 342 | 48 | **59** | 7.1× | **5.8×** |
> | 26 (ULID) | 378 | 48 | **62** | 7.9× | **6.1×** |
> | 64 | 454 | 48 | **67** | 9.5× | **6.8×** |
>
> 48 is `2 × size_of` and treats the pool as free. It is not. **[D-063](architecture/s13-decision-register.md#d-063)'s
> objection — that an id table "partly cancels the memory win" — is right, and the amount is
> about 20%.** The win survives comfortably; it is 5.8×–6.8×. On the four fixtures
> `estimated_bytes` falls 339,638 → 190,134, 2,168,375 → 432,791, 324,653 → 174,023 and
> 30,744,680 → **4,386,282 (7.0×)**, every structural column unchanged.
>
> **D-063's determinism warning does not land, and the gate was written first anyway.** Only
> the *edges* are interned; `nodes` stays a `BTreeMap`, so iteration order is still structural
> and pool indices are not observable. **The gate's first version was vacuous and its own
> vacuity guard caught it** — it varied edge *insertion* order, but the walk goes through
> `idx_lc_traversal_cover`, so rows arrive in index order regardless and both databases scanned
> identically. It would have passed against any implementation. It now varies hand-built
> construction order, where first-seen assignment would actually bite.
>
> **A quadratic loader was introduced and an existing test caught it.** Charging the marginal
> pool cost by calling `estimated_bytes()` before and after each edge is O(pool) per row —
> loading became O(E²), which is exactly the defect
> [D-047](architecture/s13-decision-register.md#d-047) fixed, re-introduced by the change meant
> to make loading cheaper. `loading_scales_linearly_in_the_number_of_edges` failed. `intern`
> now returns its marginal cost and `add_edge` returns the total, so the check is O(1).
>
> **The exit gate's Python test passed in the strong form: the suite needed no edits at all**,
> so B1 did privatise enough. `PyEdgeRef` now owns resolved strings rather than wrapping an
> `EdgeRef` — a `#[pyclass]` outlives the graph and cannot borrow from its pool — but no
> `#[pymethods]` signature moved.
>
> **Where interning stops helping, from the same diagnostic:** at 20 edges/node with 8-byte
> ids, edges are 80% of the budget with empty `content` and **5%** at 20 KB of document text.
> This is a win on topology-heavy graphs; the content-heavy case is B3's, not this one.

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

> **Delivered 2026-08-02** — [D-116](architecture/s13-decision-register.md#d-116). Suites
> **312** / **323** / **344**, clippy clean.
>
> `content` is `Option<String>`, off by default; `TraversalBuilder::content(true)` asks for it;
> `load_subgraph`, which has no builder, never fetches it. **The query selects `NULL` in place
> of the column** rather than fetching and discarding — otherwise the I/O stays, which is most
> of what this avoids.
>
> **The claim is settled by test rather than argued.**
> `content_is_absent_by_default_and_no_algorithm_notices` loads one fixture both ways and
> asserts all six algorithms return identical answers **and** that `estimated_bytes()` differs
> by more than 2×. The second half is what stops the first passing vacuously on a fixture whose
> content happened to be empty — the failure mode
> [D-088](architecture/s13-decision-register.md#d-088) keeps producing, and the same one B2's
> determinism gate walked into.
>
> **Measured split, from `budget_density_diag` at 20 edges/node and 8-byte ids:** edges are
> **80%** of the budget with empty content, **30%** at 2 KB per concept, **5%** at 20 KB.
> Against those components the default load now carries 238,802 bytes where it carried
> 4,238,802 at 20 KB — **17.8×** — and 638,802 → 238,802 at 2 KB. The plan's table used a
> different node count and read 97/76/25%; the shape of the claim is the same and these are the
> numbers this tree produces.
>
> **One thing this item leaves broken, named rather than left to be found.** The binding's
> getter had to change to compile (`str | None`), so **Python can no longer obtain content at
> all** — `load_subgraph` grows the `content=` keyword in
> [B7](#b7--the-binding-surface-the-stub-and-the-wheel). One Python test asserted content on a
> default load and now asserts `is None`, with the gap written into it; the stub is corrected
> for the same reason, since a knowingly-wrong stub is worse than an unfinished one. `s14`, the
> README example and the keyword itself remain B7's.

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

> **This paragraph is wrong, and B4's own exit gate is what found it** — see
> [D-120](architecture/s13-decision-register.md#d-120). `VACUUM` renumbers a sparse implicit
> rowid **only for a table with no index at all**, and `concepts` has always carried the `id`
> autoindex, so archival would not have made the hazard real either. The rung stays; its
> justification is that SQLite *permits* the renumbering, not that anything was observed to
> perform it.

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

> **DELIVERED 2026-08-02.** `SCHEMA_VERSION = 8`.
> [D-117](architecture/s13-decision-register.md#d-117) (mechanism),
> [D-118](architecture/s13-decision-register.md#d-118) (the indices),
> [D-119](architecture/s13-decision-register.md#d-119) (the rung),
> [D-120](architecture/s13-decision-register.md#d-120) (a correction the exit gate produced).
>
> **The decision numbers this plan projected were taken.** B4 was written down as D-115/D-116;
> B2 and B3 had consumed those by the time it landed, so the entries are D-118 and D-119. The
> cross-reference table in §6 is corrected in place.
>
> **Step 1 landed first and separately**, on 2026-08-02: the mechanism is the part the probe
> changed, it is testable on its own, and a schema rung is a bad place to be debugging the ladder
> underneath it.
>
> **Steps 2–8 as specified**, with two departures worth naming. The rung's order is *triggers and
> `concepts_fts` down before the table is touched*, not the plan's "recreate triggers, then drop
> and recreate the FTS table" — the plan's order binds the new triggers to an index about to be
> dropped, and drops a table triggers still name, which is the schema-reparse failure the `links`
> rung hit from the other direction. And `rowid` is copied into `rowid_pk` **by value** rather
> than relying on `ORDER BY rowid` to reproduce it: identical on today's dense numbering, correct
> on a file where it is not.
>
> **The exit gate found two defects in its own tests before it found none in the rung.**
> `a_v7_database_climbs_to_v8_and_gains_rowid_pk` needed a genuine v7 fixture — built from
> `ddl::` constants it would have been a v8 database wearing a v7 stamp, testing the rung against
> a table that had already had its change — so `CONCEPTS_V7` is hand-written, and
> `the_v8_rung_needs_the_suspension_and_links_rows_prove_it` pins that the `links` rows are what
> make the suspension necessary. Verified by injection: `suspends_foreign_keys: false` fails three
> tests.
>
> **The bigger finding is [D-120](architecture/s13-decision-register.md#d-120), and it corrects
> this plan.** `vacuum_preserves_a_sparse_rowid_pk` was given a control arm on the principle that
> a test asserting *nothing moved* proves an outcome and not a mechanism. The control — the v7
> shape, implicit rowid, same gaps — **did not move**. `examples/vacuum_rowid_probe.rs` measured
> six shapes: `VACUUM` renumbers a sparse implicit rowid **only for a table with no index at
> all**, and `concepts` has always carried the `id` autoindex. So the hazard this section calls
> *real* — *"0.9.0's archival makes them sparse and makes the hazard real"* — was never live.
> The rung is still right and its justification is now the honest one: SQLite *documents* the
> renumbering as permitted, and an index's correctness should not rest on an optimisation that
> happens to decline to exercise the permission. 1.0 closes the door either way.
>
> **The performance claim is measured** ([D-088](architecture/s13-decision-register.md#d-088)).
> `examples/tgt_index_cost_probe.rs`, `star_of_stars`, 3,999 `assert_edge` calls, five repeats,
> arms alternated: **1,010.3 ms with `idx_lc_tgt_active`, 931.0 ms without — −7.9%, 19.8 µs per
> assertion.**
>
> `index_plan_tests` asserts the unread set is **empty**, with the `NoReader` variant kept under
> `#[allow(dead_code)]` so a future unjustifiable index can still be recorded as one — which is
> how the test is made to fail — plus a vacuity guard that the registry is not itself empty.
> `compat_contract_tests` records the primary-key change explicitly, since post-1.0 that entry
> becomes a release blocker.
>
> Suites **321** / **332** / **344**, clippy clean, doc gates green.
>
> **What the rung COSTS was still unmeasured when this note was written, and is now
> ([D-125](architecture/s13-decision-register.md#d-125), 2026-08-03).** Every test above runs on
> four concepts, so none of them says anything about a rung that rewrites a ledger table under
> the write lock. `examples/v8_migration_scale_probe.rs`, four scales to 200k concepts / 600k
> links / 800k log rows (733 MiB): **~10–13 µs per concept — 2.7 s at 200k — peak disk 1.09× the
> starting file, settling to 1.00× after a checkpoint, and `foreign_key_check` at 13–17% of the
> rung.** The 1.09× is the number worth carrying: the reflex estimate for a copy-and-swap is 2×,
> which is right about the *table* and wrong about the *file*, because `concepts` is a small
> share of a database whose bulk is `links` and the log. `foreign_key_check` is flagged as the
> one part that scales with the whole database rather than with `concepts`. The v6 → v7 `links`
> rung's own `roughly 2× links` is **still an estimate** and is now labelled as one rather than
> borrowing authority from its measured neighbour. The pinned v7 fixture moved from
> `migration_tests.rs` to `tests/common/v7_schema.rs` so correctness and cost read the same pin —
> [D-124](architecture/s13-decision-register.md#d-124) applied the day it was written.

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

> **DELIVERED 2026-08-02.** [D-121](architecture/s13-decision-register.md#d-121) — not D-118,
> which B4 took. The reproduction table's second row now answers as the first does, verified
> through the binding.
>
> **The reproduction understated the defect and the third case is the one that matters.** The
> floor is `MIN(recorded_at)` — *transaction* time, crate-stamped at write — so on a database
> written today **every** `ts` before today was below it. A concept asserted
> `valid_from = 2026-01-01` and written on 2026-08-02 made `reconstruct("2026-06-01")` report a
> damaged ledger. That is not a pre-genesis curiosity, and `reconstructing_below_the_log_floor_is_not_a_corruption`
> pins the axis explicitly so the distinction cannot quietly be lost again.
>
> **The rejected alternative was not needed, and the reason is a better outcome than the plan
> expected.** This section defers a hot-side marker to 0.9.0 as the only way to tell *never
> archived* from *archive missing*. It is not: `transaction_log.seq_id` is `AUTOINCREMENT`, a
> rollback leaves no gap ([D-049](architecture/s13-decision-register.md#d-049), measured), and
> only an archive session may delete from the table — so `MIN(seq_id) = 1 AND COUNT(*) = MAX(seq_id)`
> holds **iff** nothing was ever removed. Exact in both directions, no marker, no rung, no schema
> change. **0.9.0 should now take the marker only if it wants the richer error *message*
> (*"archived on 2026-06-01; pass the archive path"*), not to make this decision correct.**
>
> `hot_log_is_complete` becomes `hot_log_reach` returning `Covers` / `PredatesRecordedHistory` /
> `NeedsArchive`. The `bool` was the defect's shape: two answers for three cases, so the missing
> one got attached to whichever neighbour was closest.
>
> **One existing test was pinning the defect.** `test_cold_db_reconstruct_missing_archive_error`
> built a *never-archived* database, handed it a path that had never existed, and required an
> error — so it went red on the fix, correctly. Replaced by
> `a_missing_archive_is_an_error_when_rows_were_actually_archived`, which supersedes a concept,
> archives, checks rows really moved, and *then* deletes the cold file. The Python test carrying
> the same assertion, and the `s14` §14.9 paragraph explaining why the binding declined to
> smooth it over, are both rewritten in place.
>
> **Every gate asserts the flag false somewhere**, or a `reconstruct` that always returned
> nothing would satisfy all of them. Verified by injection: restoring the old branch fails the
> unit gate and the property gate and leaves the missing-archive gate green — which is the
> correct pattern, since that one guards the half that did not change.
>
> `MaterializedState::predates_recorded_history` is `#[serde(default)]`, but that is not what
> makes it safe: [D-119](architecture/s13-decision-register.md#d-119)'s `SCHEMA_VERSION` bump had
> already refused every snapshot written before this release. The two items landing together is
> what made an additive field free.
>
> Suites **320** (`metrics`) / **345** Python, clippy clean, doc gates green.
>
> ~~**The `property-tests` gate is red, and it is not B5's.**~~ **Retracted 2026-08-03 —
> the gate was never red and there was no regression. See
> [D-124](architecture/s13-decision-register.md#d-124).** This note reported three full-gate runs
> dying 6/6 and read 5/8 for `doctrine_property_tests` against 1/8 for `integrity_property_tests`
> as a regression on committed code. It was wrong in three ways: the baseline it compared against
> was a `~3/25` comment stale since 0.5.4 that no other file believed; CI was green on all five
> 0.8.0 commits on all three platforms, so what was red was one developer machine; and eight runs
> cannot resolve the comparison regardless — re-measured at n = 20 interleaved, the same executable
> gave 75% and then 45%, and the doctrine binary built at `v0.7.0` faults at the same rate as
> HEAD's. What survives is B5's own decision: its property's count was cut from 16 to 8 because it
> had been chosen without the measurement the file's own note demands, and 8 is retained because
> nothing argues for raising it. Raising the *retry* budget would still be the laundering
> [D-110](architecture/s13-decision-register.md#d-110) exists to prevent, and nothing here asks for
> it. The Python getter and the
> `.pyi` entry landed here rather than waiting for [B7](#b7--the-binding-surface-the-stub-and-the-wheel),
> because the defect was reported through the binding and a fix that cannot be checked there is
> not checked. B7 still owns the version bumps and the stub sweep.

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
and §9; `s13` (D-122); `quickref.md`.

> **DELIVERED 2026-08-02 — the question is closed and aggregation is NOT taken.**
> [D-122](architecture/s13-decision-register.md#d-122).
>
> **This item's own gate would have given the wrong answer, and that is the finding.** B6 says
> *take aggregation only if Q differs materially*. Q differs materially and the difference grows
> with size — **+0.00181 at 6,144 nodes, +0.00417 at 24,576, +0.00491 at the 49,152-node
> ceiling** — so read literally the gate says ship. Shipping would have replaced an exactly
> correct answer with a wrong one.
>
> `examples/louvain_aggregation_probe.rs` reports what ΔQ cannot. On `clustered`, whose ground
> truth is one community per clique, **local moving recovers the truth exactly at every size**,
> and two-phase earns its higher Q by **merging whole cliques** — two per community at 512
> cliques, four at 4,096, never splitting one — scoring *above the ground truth itself*. That is
> the modularity resolution limit: past a certain size the objective prefers a partition coarser
> than the true one. **A Q comparison cannot be the criterion for a change whose entire effect is
> to optimise Q harder.**
>
> **The rustdoc's stated reason is measured false and was replaced, not patched.** It said the
> aggregation phase *"would matter on graphs far larger than the byte budget admits"*. Two-phase
> diverges at 6,144 nodes; the ceiling is 4,096 cliques / 49,152 nodes / 544,767 edges at
> **35.6 MiB, 68 B/edge** under the benches' 64 MiB budget. The scope limit stands for the
> opposite reason to the one recorded: not *it would change nothing*, but *it would change
> something, and the change is wrong*.
>
> **Two controls**, because the arms are confoundable. The probe's local-moving step is a
> transcription of `louvain`, so control 1 asserts at every size that it induces the same
> grouping the crate's `louvain` does — without it, ΔQ could be the reimplementation. Control 2
> is the fixture's known ground truth, which is what makes "merged" distinguishable from "found".
>
> **Closed as a test, not as a probe nobody re-runs** — [D-063](architecture/s13-decision-register.md#d-063)
> calls an unmeasured open question the worst of the three states.
> `modularity_prefers_a_merged_partition_over_the_true_one_at_scale` needs no two-phase code in
> the crate: it asserts `louvain` is exact on 512 cliques **and** that the merged partition
> outscores the truth, which is the fact the decision rests on.
>
> **What would reopen it:** a fixture with genuine hierarchy. `clustered` is a chain of uniform
> cliques and cannot exhibit coarse structure that is real rather than an artifact of Q. If one
> is added, the criterion should be agreement with ground truth, not ΔQ.
>
> Neither `louvain` nor any algorithm changed. Suites **321** / — / **345**, clippy clean.

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

> **DELIVERED 2026-08-02.** [D-123](architecture/s13-decision-register.md#d-123). Python **348
> passed** on a rebuilt `0.8.0` wheel, `mypy --strict` clean, Rust **321** (`metrics`), clippy
> clean, doc gates green.
>
> **The gap B3 opened is closed.** `load_subgraph(..., content=False)`, keyword-only.
> `test_content_is_returned_when_asked_for`, `test_content_is_none_not_empty_string` — which
> writes a concept with `content=""` and requires `""` when asked and `None` when not, since a
> flag that is only ever checked one way is decoration — and
> `test_the_algorithms_do_not_notice_content`, which is [B3](#b3--content-leaves-the-default-load)'s
> claim asserted from the Python side over all six algorithms with a guard that the fixture
> actually carries text.
>
> **Step 1's premise was wrong and is corrected in place.** It says *"`to_dict()`'s node entries
> change shape when content is absent"* and asks for an explicit `None` rather than an omitted
> key. `to_dict()` returns `{id: NodeData}`, not nested plain dicts, so absent content is a
> property returning `None` and the `KeyError` hazard does not exist. No code change was needed;
> the stub now states the shape, which the signature `dict[str, Any]` does not.
>
> **Step 3's naming suggestion was not taken, and the reason is the one it gave.** It proposed
> `folded_from_genesis` / `had_no_history`. The first names the branch — exactly what the step
> warns against — and the second is ambiguous with *the graph is empty*. B5 shipped
> `predates_recorded_history`, which names the fact.
>
> **What did not cross is the more interesting half.** [B2](#b2--edgeref-is-interned)'s interning
> re-represented the largest structure the crate materialises and **no `#[pymethods]` signature
> moved** — because [D-101](architecture/s13-decision-register.md#d-101) had already made
> `Subgraph` an opaque handle, so there is no converted copy whose layout had to follow. A 0.7.0
> decision taken for API-shape reasons paid for a 0.8.0 change nobody had planned when it was
> taken.
>
> **Exit gate, item by item.** Suite green on a **rebuilt** wheel (`Installed macrame-db-0.8.0`),
> not a stale artifact — the step 0.7.0 missed. `mypy --strict` clean. Stub injections
> re-verified rather than assumed: an invented member is caught
> (*`NodeData: in the stub only: ['invented_member']`*) and a dropped one is caught
> (*`MaterializedState: not in the stub: ['predates_recorded_history']`*). The six-algorithm
> equality asserted from Python. `macrame.__version__` read out of the installed extension:
> **`0.8.0`**. Version bumped in all four places, `Cargo.lock` included. **The README example run
> verbatim** from a clean directory — `{'entanglement': 1.0, 'quantum': 0.0}` — which is the
> check that caught a broken example in 0.7.0.
>
> ~~**Not verified here:** `python.yml`'s `abi3` job…~~ **Since verified.** B7 was committed as
> `55b7844` and CI run `30770381302` is green, `python.yml` included, on Windows, macOS and Ubuntu.
>
> ~~**Still open and not B7's:** the `property-tests` gate is red on committed code.~~
> **Retracted 2026-08-03 ([D-124](architecture/s13-decision-register.md#d-124)): it was not red,
> and 0.8.0 is not blocked on it.** The gate passed on all five 0.8.0 commits including this one;
> the redness was local to one Windows machine, and the "regression" was measured against a stale
> baseline with a sample too small to resolve it. R15 is unchanged, still carried, and still
> effectively Windows-only — 46 s on macOS and 58 s on Ubuntu against 4 m 30 s on Windows in that
> same run.

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
moves from "deferred" to "delivered"); `s13` (D-128 — **recorded as D-128, not the D-129 this plan
projected**; see [§6](#6-decision-entries-this-plan-creates)).

> **Delivered 2026-08-06 ([D-128](architecture/s13-decision-register.md#d-128)).**
> `CONCEPTS_ARCHIVABLE` and `temporal::archivable_concepts(conn, cutoff)` ship in
> [`archive.rs`](../src/temporal/archive.rs); nothing is archived yet, which is C2.
>
> **The predicate gained a clause this item did not specify.** It reads `recorded_at < :cutoff`
> as well as `valid_to`, mirroring `LINKS_ARCHIVABLE`. A concept retired with its `valid_to`
> behind the cutoff but *recorded* at or after it would otherwise go cold while the log entries
> describing it stayed hot — the same two-clock mismatch the `links_current` compensation carried
> until Wave 4.5, reached from the other side.
>
> **And it did *not* gain the two clauses the FK argument seems to demand.** `concepts` has four
> inbound foreign keys, not two: `analytics_annotations` and every `embeddings_*` table reference
> it as well. Neither blocks archivability, because both hold derived rows —
> [Doctrine VII](architecture/s0-s3-foundations.md#doctrine-vii) makes an embedding an artifact of
> a model applied to content. Blocking on a recomputable artifact would answer *"not yet"* forever
> for any concept that had ever been embedded. C2 disposes of them instead.
>
> **The exit gate passed before it could fail, and that is the item's real finding.** The property
> was written in the model-based shape this item asks for, and it was green on the first run. Five
> injected defects were then put through it and **it caught two**. The cause was the generator, not
> the predicate: with both link endpoints drawn from the same two-concept domain, every concept is
> referenced in nearly every case, `NOT EXISTS` is false throughout, and the three row-level
> clauses are never evaluated. It took **four** concepts — two ordinary, one that can only be a
> link *target*, one no generated edge may name at all — before all five injections fail, at 32
> cases and again at 512. A conjunction can only be tested clause by clause if the generator can
> reach each clause independently. The predicate was correct the whole time; the evidence for it
> was not, and this is the third gate this cycle that could not tell *nothing ran* from
> *everything passed* ([D-124](architecture/s13-decision-register.md#d-124),
> [D-127](architecture/s13-decision-register.md#d-127)).

### C2 · `cold.concepts`, and the guard becomes conditional

**Steps.**
1. `cold.concepts`, trigger-free, alongside `cold.links` — the shape Appendix C already sketches.
2. `trg_concepts_guard_delete` becomes marker-gated, matching `trg_links_guard_delete` and
   `trg_txlog_guard_delete` exactly: `WHEN NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type =
   'table' AND name = 'macrame_archive_session')`. **One trigger, the existing pattern.**
   **This needs a `v8 → v9` rung, and that is settled rather than open**
   ([D-126](architecture/s13-decision-register.md#d-126)): re-issuing the baseline does *not*
   update an existing database, because `CREATE TRIGGER IF NOT EXISTS` keeps the old body, and
   `verify()` would not notice because it checks trigger *names* only. The rung is
   `DROP TRIGGER trg_concepts_guard_delete` then the new `CREATE` — no table rebuild, no data
   movement, and `suspends_foreign_keys` is **not** wanted. Cheap, but it is not nothing, and the
   exit gate must include a genuine v8 fixture whose guard is asserted *conditional* afterwards —
   the same trap `a_v7_database_climbs_to_v8_and_gains_rowid_pk` exists to avoid, since a fixture
   built from today's `ddl::` constants would already have the change.
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

**Documents.** `s4-schema.md` §4.1 and §4.6 (the third trigger becomes live) **and §4.7 — the
ladder gains the `v8 → v9` row, which is not optional here**; `s5-modules.md`
§5.7; `s0-s3` if Doctrine V's commentary needs the concept case named; `s13` ([D-129](architecture/s13-decision-register.md#d-129) for the rung and
[D-130](architecture/s13-decision-register.md#d-130) for the partition — **two entries, because they are
different kinds of decision**: one closes a hole, the other says what crosses a boundary);
`s11-s12` R3 (log growth) — its mitigation now covers concepts. **`README.md` and the release
note carry the migration**, because a rung is the one change a user cannot undo.

### C3 · Rehydration is a move back, not a write

**Decided, not open** ([§2.3](#23-concept-archival-is-the-sanctioned-exit-and-traceability-answers-its-open-question)).
Rehydration mints no transaction-time facts: it moves rows from `cold.concepts` to hot inside a
declared session and updates the horizon. A concept reacquires its identity because the
alternative makes the transaction-time axis lie about when the concept was learned.

**Exit gate.** Archive → rehydrate → `reconstruct(t)` for a `t` spanning both operations returns
**bit-identical** state to the never-archived control at the same `t`. That is the executable form
of "traceable through the bitemporal ledger" and it is the whole point of the release.

**Documents.** `s5-modules.md` §5.7; `appendices.md` Appendix C and the glossary (rehydration is
a new term); `s13` ([D-131](architecture/s13-decision-register.md#d-131) — **one entry, not two**: the
`v9 → v10` rung is not correcting a defect, it *is* the mechanism that makes rehydration a move,
and split apart neither half would make sense); `s14` if the binding exposes it.

> **Delivered 2026-08-06 ([D-131](architecture/s13-decision-register.md#d-131)).**
> `Database::rehydrate(&[…])` ships, and this item cost a schema rung the plan did not project.
>
> **`trg_concepts_log_insert` had to become marker-gated, and the reason is a fold detail.**
> Putting a concept back is an *insert*, so an unconditional `AFTER INSERT` makes "this is a move,
> not a write" unimplementable. Worse than a spurious row: the fold takes
> `ROW_NUMBER() OVER (… ORDER BY seq_id DESC) = 1`, so precedence is by **sequence, not
> timestamp**. A rehydrated row carries its original `recorded_at` but its log row would take a
> new `seq_id` — outranking the later `'U'` that retired the concept, and returning it **alive**
> at every `ts` after its creation. Had the fold resolved by timestamp, no rung would have been
> needed.
>
> **`rowid_pk` has both exits, not just the hazard.** Reinstate when free; otherwise a fresh
> rowid and the stale `concepts_fts` entry deleted at the old one.
>
> **The exit gate became two tests.** `reconstruct` bit-identical is necessary and *not*
> sufficient — the fold never reads `concepts` ([D-130](architecture/s13-decision-register.md#d-130)),
> so it passes even against a row written back garbled. The second test is against the live
> tables. Three of its assertions were wrong before they were right, and the register entry keeps
> all three: `load_subgraph` cannot see a retired concept and so cannot be a reader;
> *"archivable_concepts no longer lists it"* is false, because the predicate says **eligible**
> and never **due**; and the column check passed vacuously until it was moved ahead of the
> `upsert_concept` that had been rewriting the columns it verified.

### C4 · Measure rehydration, and decide the hot-side marker

Rehydration cost is unmeasured and Appendix C names it as one of the two reasons archival was
deferred. Measure it on the fixture matrix, at the sizes §9 uses.

**And revisit B5's deferred alternative here**, where it is cheap: a hot-side marker recording
*archived at* and *horizon* would let a pre-genesis `reconstruct` say *"this database was archived
on X; pass the archive path"* instead of returning empty. 0.8.0 declined it as a second rung;
0.9.0 is already writing archive metadata, so the marginal cost is small — but it is a hot-table
addition, so under [D-036](architecture/s13-decision-register.md#d-036) it must land pre-1.0 or
not at all.

**Documents.** `s6-s10` §9 (a new budget row, with its fixture named); `s13` (~~D-131 if the marker
lands, or an amendment to [D-121](architecture/s13-decision-register.md#d-121) if it does not~~ —
**both, and the number is D-132; corrected 2026-08-06.** The either/or was wrong in two ways: D-131
went to C3, and the amendment is not an alternative to an entry but a consequence of one — D-121
left the door open in its own text, so closing it has to be visible *there*, struck in place, as
well as argued in full where the new facts live. That is the same shape as
[D-119](architecture/s13-decision-register.md#d-119)'s correction. The stale "D-118" written here
was the index-drop entry, corrected 2026-08-06).

> **Delivered 2026-08-06 ([D-132](architecture/s13-decision-register.md#d-132)).** One entry, and it
> is a **refusal** rather than the deferral this item allowed for. The marker's whole value was the
> richer error message, and the message turned out to be free: `MAX(seq_id) - COUNT(*)` says how many
> rows went and `MIN(seq_id)` says how far back what remains reaches, which are the two facts a caller
> facing an unreachable cold file needs — and the marker would have carried neither, only the archive
> timestamp, which answers nothing. `archive_hint` in `temporal/replay.rs` assembles them and both
> unreachable-archive errors carry it, so the message is shipped rather than promised. Refused rather
> than deferred because [D-036](architecture/s13-decision-register.md#d-036) makes those the same
> sentence, and only one of them is honest about it.
>
> **The measurement is in [§9](architecture/s6-s10-flows-to-dependencies.md#9-performance-budgets):**
> 3.71 ms fixed, ~74 µs per concept, and a departure from linearity above n=1,000 that the trigger-free
> control attributes to FTS5 index maintenance — 53% of the cost at n=10,000, against a row-movement
> path that stays linear to within 1%. The fixture matrix moves it by 5.8%, which is noise, and is
> measured anyway because the predicate severs the matrix's axis and an unmeasured "no difference" is
> still a claim ([D-088](architecture/s13-decision-register.md#d-088)).
>
> **This item's own Documents line said "a new budget row"; there are two, and neither is the measured
> number.** ≤ 300 µs per concept is the rate the existing `Archive, 100K closed intervals ≤ 30 s` row
> already implies, applied to the reverse direction — [D-055](architecture/s13-decision-register.md#d-055)
> is why a budget is not written to match the reading that prompted it.
>
> **Two things the probe found.** `archive_hint` was being computed eagerly on every cold fold, for a
> string almost every caller discards; a panic in its never-archived branch fired from a test that
> raises nothing from that code, which is how the eager call was caught. Made lazy, the branch proved
> dead at both use sites and was deleted. Separately, and **not fixed here**: a file that exists at the
> archive path but is not a cold database still reports `no such table: cold.transaction_log`, a raw
> engine message where [D-069](architecture/s13-decision-register.md#d-069) wants the caller told the
> file is not an archive. Pre-existing, on a different path, and recorded rather than folded in.

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
`s13` (D-132); `README.md` Python section; `quickref.md` §10.

> **Delivered 2026-08-07 ([D-133](architecture/s13-decision-register.md#d-133)).** `ArchiveReport.concepts_archived`,
> `db.rehydrate(ids)` and `RehydrateReport` cross; version `0.9.0` in all four places; the wheel
> rebuilt and reinstalled **before** either gate was read. 353 Python tests, `mypy --strict` clean,
> `test_stubs.py` verified by injection — removing `rehydrate` from the stub fails it by name.
>
> **Step 1 was already done** (C2 shipped the getter and the stub property), and **step 4 was a
> no-op**: C1–C3 added no `DbError` variant, so the exhaustive `match` [D-099](architecture/s13-decision-register.md#d-099)
> put in `errors.rs` had nothing to catch. Recorded rather than passed over, because *no error work
> was needed* and *the error work was forgotten* look identical from a green build.
>
> **Half of step 3 does not cross, and the reason is structural rather than an omission.**
> `reconstruct` bit-identical crosses cleanly, against a second database seeded identically and never
> archived. The live-table half largely does not: `keyword_search` and `load_subgraph` both filter
> `retired = 0`, and an archivable concept is retired by definition, so neither can see the concept on
> *either* side of the round trip — C3's `load_subgraph` discovery, met again from Python through a
> different reader, which makes it a property of the predicate rather than of one API. Python has no
> raw SQL by design, so the FTS half stays in the Rust suite. What the Python test asserts instead
> tests more than presence: archiving the same concept a **second** time succeeds only if the row is
> back in the hot table with the column values the predicate reads, and a second rehydration moving
> nothing is what says it is hot rather than still cold.
>
> **This item's exit gate found that three previous items' gates were reporting on a smaller suite
> than they sounded like.** `archiving_links_only_enlarges_the_archivable_set` sits behind the
> `property-tests` feature; C2, C3 and C4 each ran the classifier on the default feature set and
> called it green. The property had been **false since C2** — `archive()` began moving archivable
> concepts out of the hot table, so a concept archivable before a session is legitimately absent
> afterwards, and the test called that a withdrawal. Restated as `before ⊆ after ∪ cold` and renamed
> `a_concept_leaves_the_archivable_set_only_by_being_archived`, which keeps the monotonicity and
> gains the half C1 recorded as *"cannot be asserted until C2 archives one"* — a docstring that named
> its own revision and was never revisited, because nothing was watching. **An exit gate that does
> not name its feature set has not said what it ran.**
>
> **And the feature set is now known not to complete here.** Under `--features "metrics property-tests"`
> the classifier exhausted its three retries on nine consecutive attempts, every one R15's shape.
> Run alone the binary is ~50/50 and green when it finishes — measured against a stashed baseline at
> the same rate, so C5's added `ATTACH` is not the cause. It is `integrity_property_tests` needing a
> database per case, contending with 27 other targets. Recorded in `README.md` beside R15, and it is
> why this release's counts are `330 · 339 with metrics · +7 property-tests run separately` rather
> than one number.

---

## 5. Sequencing and dependencies

```
NOW      A1  gate classifier ........... main is red; nothing below is measurable until it lands
         A2  libSQL 0.10 probe ......... answer wanted while 0.8.0 is still open
         A5  decision-deadline tripwire . red today, by design; goes green as B1–B4 land

0.8.0    B1  privatise + accessors ..... ┐
         B2  intern behind them ........ ├ Subgraph moves once, in this order
         B3  content out of default .... ┘
         B4  schema v8 ................. DELIVERED (v7 -> v8, D-117/118/119/120)
         B5  reconstruct below floor ... DELIVERED (D-121; no marker needed)
         B6  Louvain aggregation ....... CLOSED, not taken (D-122; Q was the wrong gate)
         B7  binding + stub + wheel .... DELIVERED (D-123; 0.8.0 wheel, stub, README)
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
| B4 | `migrations.rs` (+`Step`), `ddl.rs`, 4 callers, `index_plan_tests.rs` | **v7 → v8** — delivered | no |
| B5 | `replay.rs`, `temporal_tests.rs`, `temporal.rs` (binding), `_macrame.pyi` | — | behaviour — delivered |
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

> **The projected numbers drifted, and this table is corrected in place rather than reprinted
> from the plan's guess.** Two causes, both worth naming. One item was projected as a single
> entry and needed two (B1 and B2 are separate decisions about separate things). And B4's exit
> gate produced an entry nobody planned — [D-120](architecture/s13-decision-register.md#d-120),
> a measurement that says one of this plan's own premises is false. Everything after that shifts.
> The **Recorded as** column keeps the projection visible, since a plan that quietly renumbers
> itself is a plan whose predictions cannot be scored.

**Landed.**

| | recorded as | |
|---|---|---|
| **D-110** | D-110 | The Rust suite is gated by a classifier, not a retry count; [D-107](architecture/s13-decision-register.md#d-107)'s four outcomes apply to Rust, and the budget was calibrated on the wrong step (A1) |
| **D-111** | D-111 | R15 against the libSQL 0.10 pre-release: the measurement, either way (A2) |
| **D-112** | — | macOS joins the Rust matrix; the Node 20 actions go to their **first** `node24` major, not the newest (A4). Unprojected: A4 was scoped as maintenance |
| **D-113** | D-112 | A decision entry naming a release is a claim with a deadline, and deadlines get tripwires (A5) |
| **D-114** | D-113 | `Subgraph`, `NodeData` and `EdgeRef` have private fields and a public accessor surface; **supersedes [D-087](architecture/s13-decision-register.md#d-087)** and corrects its blast-radius estimate (B1) |
| **D-115** | D-113 | `EdgeRef` is interned to 24 bytes; the win is **5.8×–6.8×**, not the 7.1×–9.5× projected, and [D-063](architecture/s13-decision-register.md#d-063)'s objection is right by ~20% (B2). *Projected as half of one entry with B1* |
| **D-116** | D-114 | `content` is not loaded by default; `None` means *not loaded*, never *empty* (B3) |
| **D-117** | — | The ladder gains a rung kind: `suspends_foreign_keys`, with `foreign_key_check` inside the transaction (B4 step 1). Projected as part of B4b; landed separately and first |
| **D-118** | D-115 | Schema v8 drops the two indices with no reader; the unread set becomes **empty** rather than known-bad, measured at −7.9% off `assert_edge`. **Completes [D-089](architecture/s13-decision-register.md#d-089)** (B4a) |
| **D-119** | D-116 | `concepts` gains `rowid_pk` and the FTS index its third trigger, taken pre-1.0 under [D-036](architecture/s13-decision-register.md#d-036)'s deadline. **Completes [D-084](architecture/s13-decision-register.md#d-084) and corrects its migration shape** (B4b) |
| **D-120** | — | **Unprojected, and produced by B4's own exit gate.** `VACUUM` renumbers a sparse implicit rowid **only for a table with no index at all**, so [D-071](architecture/s13-decision-register.md#d-071)'s hazard was never live and this plan's "makes the hazard real" is wrong. Does not change the rung; changes why it is right |
| **D-121** | D-118 | A `reconstruct` below the log floor on a never-archived ledger is the empty state, not a corruption — and the hot-side marker is **not** needed to get there, because `seq_id` already answers it (B5) |
| **D-122** | D-119 | Louvain's aggregation phase **closed, not taken** — and the Q criterion this plan specified is what the measurement refuted: ΔQ is positive and growing, but it is the resolution limit merging true communities, not structure found (B6) |
| **D-123** | D-123 | Absent `content` crosses to Python as `None`, not `""`, and `load_subgraph` gains the `content=` keyword that makes the default overridable; the stub stays hand-written and the interning is **confirmed** invisible at the boundary — the one entry this plan projected exactly, number included (B7) |

**Recorded so far**, with the number this plan projected for each alongside it:

| | projected as | |
|---|---|---|
| **D-128** | D-129 | Concept archivability is **reachability**, not expiry, and it reads **both** clocks; the two derived-row foreign keys are deliberately not clauses. Landed as C1 alone rather than jointly with C2, and took the number the erasure entry below had been projected for (C1) |
| **D-129** | D-133 | **Corrective.** Schema v9: the concepts delete guard becomes marker-gated, and `verify` starts checking the delete guards' **bodies** rather than only their names — the second half of the hole [D-126](architecture/s13-decision-register.md#d-126) found (C2 step 2) |
| **D-131** | D-130 | Rehydration is a physical move back and mints no transaction-time facts — and the `v9 → v10` rung that makes it so, because the fold's tie-break is `seq_id` and not `recorded_at`. **One entry: the rung is the mechanism, not a correction** (C3) |
| **D-130** | — | **Architectural, and unprojected.** What crosses the archive boundary: the concept row moves column for column with its `content`; `analytics_annotations` and `embeddings_*` are disposed of. Also records that **C2's step 4 was a no-op** — `reconstruct` folds the log and never reads `concepts` (C2 steps 1, 3–5) |
| **D-132** | D-131 | Rehydration measured, and the hot-side marker **refused outright rather than deferred**: the richer message it was wanted for is strictly weaker than what the hot log already carries, so `archive_hint` ships it with no rung. Also the superlinearity above n=1,000 and its cause (C4) |
| **D-133** | D-128 | Archival is drivable from Python: `rehydrate` is exposed, and the traceability equality is asserted **through the binding** as well as in the crate, because a boundary that silently loses a doctrine property is the failure [§14.7](architecture/s14-python-bindings.md#147-r15-through-the-boundary) measured rather than assumed. Also: step 4 was a no-op; half of C3's gate does not cross, because every Python reader of a concept filters `retired = 0`; and the exit gate found `property-tests` had been red since C2 while three items reported green (C5) |

**Still projected**, and the numbers below are estimates a third time, so they are written as *next free* rather than as claims:

> **Renumbered 2026-08-06, and the reason is the hazard this table was already warning about.**
> These five were projected as D-124 … D-128. **All five of those numbers have since been taken
> by real entries** — [D-124](architecture/s13-decision-register.md#d-124) (the R15 retraction),
> [D-125](architecture/s13-decision-register.md#d-125) (the v7 → v8 rung measured),
> [D-126](architecture/s13-decision-register.md#d-126) (0.9.0 needs a rung),
> [D-127](architecture/s13-decision-register.md#d-127) (the §9 budgets re-measured) — none of
> which this plan projected, because three of the four were produced by exit gates rather than by
> items. So the projections below move to **D-128 … D-132**, and the register is the authority the
> moment an entry is written. A projected number is a **guess about ordering**, never a
> reservation: it is cited as `#6-decision-entries-this-plan-creates` (this table) while it is a
> guess, and only as `s13-decision-register.md#d-1NN` once it exists. Where this plan's prose
> cited a projected number, that citation moved with the row.
>
> **And it happened again immediately, which is the point.** C1 landed before the erasure entry
> was written, so it took **D-128** — the number projected here for erasure — and the joint
> C1/C2 row split in two. The table above records what was written; the table below is still a
> guess, and will be wrong again in the same way. That is not a defect in the plan: it is what
> "the register is the authority the moment an entry is written" means in practice.

| | projected as | |
|---|---|---|
| **D-129** | D-117 | **Erasure is refused on doctrine, not deferred**; the alternative is pseudonymous ids with identifying content held by the application. **Supersedes [D-022](architecture/s13-decision-register.md#d-022)'s open framing** ([§2](#2-doctrine-position-erasure-is-refused-archival-is-sanctioned)) |
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
| 0.9.0 needs a rung after all | ~~Low~~ **Certain** | ~~**High** — forfeits the shared rebuild~~ Low | **Re-checked before tagging 0.8.0, as this row asked, and the answer is that it DOES** ([D-126](architecture/s13-decision-register.md#d-126)). B4 installed `rowid_pk` and the inert FTS delete trigger — C2 steps 1 and 3 — but **not** step 2: `trg_concepts_guard_delete` is still *unconditional* in v8 ([`ddl.rs`](../../src/schema/ddl.rs)) while `trg_links_guard_delete` is already marker-gated. Two measured facts make that a rung and not a baseline re-issue: `CREATE TRIGGER IF NOT EXISTS` on an existing name **keeps the old body** (verified — `sqlite_master` still held the unconditional guard after the conditional one was re-issued), and `verify()` compares `type, name` and **never trigger bodies**, so the stale guard passes migration verification silently. A 0.8.0 database opened by 0.9.0 code would refuse concept archival at the trigger. **Impact is lower than this row assumed**, though: the rung is a `DROP TRIGGER` + `CREATE TRIGGER`, no table rebuild and no data movement, so there is no "shared rebuild" to forfeit. Deliberately **not** folded into v8 — the marker table exists during *links* archive sessions, so a conditional concepts guard shipped in 0.8.0 would leave concepts deletable during them, weakening [Doctrine V](architecture/s0-s3-foundations.md#doctrine-v) for a whole release to save a cheap rung |
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
