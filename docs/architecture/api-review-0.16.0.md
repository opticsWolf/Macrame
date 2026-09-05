# The public API from 0.15.0 to 0.16.0, reviewed item by item

**Cycle in progress.** This covers 0.15.0 through **0.15.13** and is
regenerated at each release that moves the surface, and finally at 0.16.0. It
is opened here rather than at the end because [D-212](s13-decision-register.md#d-212)'s
finding was that nobody had read the *cycle's* diff — thirty-eight consecutive
per-release diffs are not a review of the cycle — and the cheapest time to read
an item is the release that added it.

Regenerate with:

```text
python scripts/api_review.py v0.15.0 --out docs/architecture/api-review-0.16.0.md
```

then re-attach this prose. The script is checked in, which
[`api-review-0.14.0.md`](api-review-0.14.0.md)'s was not: that file says
*"Regenerate with the script recorded in D-212"*, D-212 records no script, and
its own header points at `scripts/../` — a path nobody filled in. So the
document that quoted [D-205](s13-decision-register.md#d-205)'s rule — *a review
nobody can re-run is a review nobody can check* — was itself unre-runnable for
eighteen releases. Closed at 0.15.13 ([D-255](s13-decision-register.md#d-255)).

It also no longer needs a worktree or a nightly toolchain. The 0.14.0 review
had to build 0.13.0 from a tag because the baseline did not exist yet; it is
checked in now, so both sides of this diff are
`git show <rev>:docs/architecture/public-api.txt` and the review is a text
operation on two files.

## How to read this

`cargo-public-api` reports one line per *path*, so an item reachable at three
paths is three lines. Three collapses turn the raw diff into a review, applied
to both releases so neither is privileged, and each reported separately so a
category is never silently absorbed into another:

1. **`pub mod` lines are namespace, not item** ([D-208](s13-decision-register.md#d-208)),
   and are counted apart.
2. **Paths collapse to identities.** `macrame::connection::Tuning` and
   `macrame::prelude::Tuning` are one item. The item's own path keeps
   everything from its first type-like segment — `Tuning::cadence`, not
   `cadence`, because two types may both have a `new` — and crate paths inside
   a signature reduce to their last name. `core::time::Duration` is left whole:
   a change in one of those is a change.
3. **`#[non_exhaustive]` is stripped into a flag**, because it renders inline
   on the item's own line and is a decision worth counting on its own.

## What this instrument cannot see, which is most of what 0.15.13 did

**The listing below reports zero removals, and 0.15.13 is a breaking release.**
Both are true, and the gap between them is the thing to carry forward.
`#[non_exhaustive]` removes no item, no path and no signature. What it removes
is a *construction form* — the struct literal, outside the defining crate —
which does not appear in this diff at all, because `cargo-public-api` reports
what an API *has* and not what a caller is allowed to write. An item-level
review of this cycle, read alone, would say the surface is purely additive.

That is not a defect in the tool; it is the boundary of the question it
answers, and the reason [Appendix D](appendices.md)'s first promise is worded
about items and paths rather than about compilation. The compensating
instruments are named here so the pairing is on the record:

| what moved | seen by |
|---|---|
| items, paths, signatures | this file, `scripts/check_public_api.py`, `tests/public_path_tests.rs` |
| `#[non_exhaustive]` on structs that have public fields | `tests/api_growth_tests.rs` |
| a type nobody outside the crate can construct | `tests/api_growth_tests.rs` |
| a setter that assigns the wrong field, or none | `tests/api_growth_tests.rs` |
| the attribute deleted from `src/` before the next blessing | `tests/api_growth_tests.rs`, `scripts/check_public_api.py` |
| the Python surface | `tests/binding_parity_tests.rs`, `tests_py/` |

## The generated diff

```text
v0.15.0 : 1633 lines, 15 modules, 703 distinct items
working : 1739 lines, 16 modules, 741 distinct items
net lines: +106   net items: +38

--- modules (15 -> 16) ---
demoted: 0   new: 1
  + plan

--- non_exhaustive (21 -> 44) ---
  + pub enum WalkOutcome
  + pub struct Annotation
  + pub struct ArchiveReport
  + pub struct AsOf
  + pub struct BulkInterrupted
  + pub struct BulkProgress
  + pub struct ChainCheck
  + pub struct CheckpointReport
  + pub struct CostEstimate
  + pub struct CostEstimator
  + pub struct HybridHit
  + pub struct Interval
  + pub struct MaterializedState
  + pub struct MigrationOutcome
  + pub struct NodeAttributes
  + pub struct Overlap
  + pub struct ReadPlan
  + pub struct RebuildReport
  + pub struct RehydrateReport
  + pub struct SnapshotCadence
  + pub struct TraversalBuilder
  + pub struct Tuning
  + pub struct VectorSearchResult

--- surplus paths on items present in both: 782 -> 836 ---

=== REMOVED FROM THE SURFACE: 0 ===

=== ADDED TO THE SURFACE: 38 ===
  pub DbError::BranchArchived
  pub DbError::BranchArchived::branch: alloc::string::String
  pub DbError::BranchArchived::concept: alloc::string::String
  pub ReadPlan::branch: core::option::Option<BranchId>
  pub ReadPlan::limit: core::option::Option<usize>
  pub ReadPlan::recorded: core::option::Option<alloc::string::String>
  pub ReadPlan::valid: core::option::Option<alloc::string::String>
  pub TraversalBuilder::limit: core::option::Option<usize>
  pub WalkOutcome::Complete
  pub WalkOutcome::LimitReached
  pub async fn Database::edges(&self, ReadPlan) -> Result<alloc::vec::Vec<EdgeBelief>>
  pub async fn TraversalBuilder::execute_ids_explained(&self, &libsql::connection::Connection, &str) -> Result<(alloc::vec::Vec<alloc::string::String>, WalkOutcome)>
  pub const CREATE_LOG_INTEGRITY_TABLE: &str
  pub const CREATE_TXLOG_MARK_GAP: &str
  pub const SEED_LOG_INTEGRITY: &str
  pub enum WalkOutcome
  pub fn MaterializedState::empty(&str) -> Self
  pub fn NodeAttributes::embedding_model(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn NodeAttributes::new(impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn Overlap::new(impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>, (impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>), (impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>), bool) -> Self
  pub fn ReadPlan::limit(self, usize) -> Self
  pub fn ReadPlan::new() -> Self
  pub fn ReadPlan::on(self, BranchId) -> Self
  pub fn ReadPlan::recorded_at(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn ReadPlan::valid_at(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn SnapshotCadence::every_entries(self, i64) -> Self
  pub fn SnapshotCadence::poll_interval(self, core::time::Duration) -> Self
  pub fn TraversalBuilder::limit(self, usize) -> Self
  pub fn TraversalBuilder::plan(self, ReadPlan) -> Self
  pub fn TraversalBuilder::read_plan(&self) -> Result<ReadPlan>
  pub fn Tuning::cadence(self, CadencePolicy) -> Self
  pub fn Tuning::clock(self, alloc::sync::Arc<dyn Clock>) -> Self
  pub fn Tuning::future_stamps(self, FutureStampPolicy) -> Self
  pub fn Tuning::reader_cache_size(self, i32) -> Self
  pub fn Tuning::wal_autocheckpoint(self, WalCheckpointPolicy) -> Self
  pub fn Tuning::writer_cache_size(self, i32) -> Self
  pub fn WalkOutcome::hit_limit(self) -> bool
  pub struct ReadPlan
```

## The additions, read

**Nothing was removed**, so the review is a review of arrivals. They fall into
five groups and no item is in two of them.

**1. The read plan and its lowering (12 items, 0.15.9 and 0.15.10, W13.4/W13.5,
[D-251](s13-decision-register.md#d-251), [D-252](s13-decision-register.md#d-252)).**
`ReadPlan` with its four fields and five methods, `Database::edges`,
`TraversalBuilder::plan` and `read_plan`, and the `plan` module that holds
them. This is the largest single addition of the cycle and the one F-34 asked
for: three qualifiers stated once and composed. `ReadPlan` arrived
`#[non_exhaustive]` and gained a fourth qualifier one release later, which is
the attribute doing exactly the job the rest of this release is about.

**2. The walk's ceiling (5 items, 0.15.10, W13.5, C-8,
[D-252](s13-decision-register.md#d-252)).** `WalkOutcome` with two variants and
`hit_limit`, `TraversalBuilder::limit` and its field, and
`execute_ids_explained`. A limit that cannot say whether it bit is a limit
whose result cannot be interpreted, which is why the outcome type ships with
the ceiling rather than after it.

**3. Three DDL constants (0.15.7, W14.5,
[D-249](s13-decision-register.md#d-249)).** `CREATE_LOG_INTEGRITY_TABLE`,
`SEED_LOG_INTEGRITY`, `CREATE_TXLOG_MARK_GAP`. `schema::ddl` is public and its
constants are the schema; a rung that adds a table adds a constant. No
behaviour is public here that was not already.

**4. One error variant (3 items, 0.15.11, W15.1,
[D-253](s13-decision-register.md#d-253)).** `DbError::BranchArchived` and its
two fields. `DbError` is `#[non_exhaustive]` since
[D-207](s13-decision-register.md#d-207), so a variant is additive; the two
fields are what makes the refusal say *which* concept and *which* lineage
rather than that something was archived.

**5. Constructors and setters that make `#[non_exhaustive]` payable (18 items,
0.15.13, W15.3, C-11, [D-255](s13-decision-register.md#d-255)).** Six on
`Tuning`, two on `SnapshotCadence`, two on `NodeAttributes`, one each for
`Overlap::new` and `MaterializedState::empty`. Every one of these exists
because the same release forbade the struct literal, and **five of the six
`Tuning` setters are the only way an external caller can now set the field they
name.** `Tuning::default` is not in the listing and never was: it is derived,
and `check_public_api.py` omits auto-derived impls for
[D-205](s13-decision-register.md#d-205)'s reason. So the surface as recorded
shows six setters on a struct with no visible constructor at all, which is a
gap in the baseline rather than in the crate — recorded here, and asserted
against the source in `tests/api_growth_tests.rs`.

## The twenty-three types that gained the attribute

`#[non_exhaustive]` went from **21** types to **44**. One is an enum
(`WalkOutcome`, born with it); the other twenty-two are structs, and after this
release **every public struct in the crate that has public fields carries it**.
The list is in `tests/api_growth_tests.rs`, one entry per type, each saying
whether a caller ever builds one and how — because the attribute's own failure
mode is a type nobody outside the crate can construct, and that failure is
silent from inside.

## Against the stability contract

[Appendix D](appendices.md)'s first promise is *no item removed, no path stops
resolving, no signature narrows*. The listing above shows **0 removed** and no
signature change on an item present in both, so the promise holds item for item
across the cycle so far.

It is worth being exact about what that does and does not license. The contract
is a 1.0 promise and this is the pre-1.0 window in which C-11's breaking change
is allowed to land; the surface being additive is not the reason 0.15.13 is
acceptable, it is a separate fact that happens also to be true. The reason is
the deadline: after 1.0 the same change is a major version, and the struct it
is about is the one whose documented purpose is to keep acquiring fields.
