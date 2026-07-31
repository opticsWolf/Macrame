# Macrame — Python Bindings Plan (toward v0.7.0)

**Status:** proposed, 2026-07-31. Sequenced after v0.6.0 (Tier 4 / Tier 5 complete).
**Basis:** a read of the v0.6.0 public surface, plus direct measurement of the Cargo,
pyo3 and registry facts each structural choice depends on.

**Measurement note, stated once.** Everything marked **measured** below was run in this
repository or against a live registry API on 2026-07-31, on `rustc 1.97.1`, `cargo 1.97.1`,
`Python 3.13.14`, `maturin 1.14.1`. Everything marked **assumed** is a claim this plan
depends on that has *not* been reproduced here, and each one names the probe that has to
run before the item it supports lands. This project's standard is that a number is not a
number until it is measured on its own harness; the same applies to a build property.

---

## 0. The structural decision, and why it is not the one originally asked for

The brief was **one `Cargo.toml`, with the Rust crate still buildable separately and
cleanly from the wheel**. The second half of that is the requirement. The first half was
the means, and it does not survive contact with three facts.

**Fact 1 — `crate-type` is not feature-conditional.** Cargo offers no way to say "add
`cdylib` when feature `python` is on". A single manifest must therefore declare
`crate-type = ["rlib", "cdylib"]` permanently, so every `cargo build --release` links a
second artifact that drags the statically-linked `libsql-ffi` amalgamation through the
linker again — and `cargo publish` ships a crate advertising a cdylib to every consumer
of `macrame-db`, forever.

**Fact 2 — `pyo3/extension-module` must be on for the wheel and off for `cargo test`.**
With it on, the test binary links against Python symbols that are only supplied by the
interpreter that loads the extension. In one manifest that is a feature nothing may ever
unify on — and [`.github/workflows/ci.yml:66`](../.github/workflows/ci.yml) runs
`cargo check --all-features --all-targets`, which turns on every feature by definition.
The msrv job would have to grow a special case for a flag it cannot see the point of.

**Fact 3 — `[lib] name = "macrame"` is already load-bearing.** It is what keeps
`use macrame::…` working while the package is `macrame-db`. The wheel's cdylib has to be
named for the Python module. One manifest cannot hold both names.

### But the split must *not* be the usual `crates/macrame/` move

This is the part worth writing down, because it is specific to this repository and it
inverts the standard advice.

[`tests/fixture_matrix_tests.rs:247`](../tests/fixture_matrix_tests.rs) does
`include_str!("../docs/architecture/s13-decision-register.md")`, and
[`tests/index_plan_tests.rs:92`](../tests/index_plan_tests.rs) reads back
`src/temporal/replay.rs` and `src/temporal/archive.rs` to check that the SQL it pins still
exists at its source. Those resolve today **only because the package root is the repo
root**, which is what puts `docs/` inside the `.crate` tarball. Move the crate down one
level and `docs/` falls outside the package: `cargo publish`'s verify build unpacks the
tarball, the file is not there, and D-088's rule-enforcement test stops compiling. The
cost of the conventional layout is either deleting that test or moving the architecture
docs inside the crate, and both are worse than the problem they solve.

### The shape this plan adopts

**Root package unchanged; one leaf member added.** The root `Cargo.toml` becomes a
workspace root *and stays a package*.

> **Measured.** In a scratch workspace of exactly this shape, `cargo metadata` reports
> `workspace_default_members` as the **root package alone**; a bare `cargo build`
> compiled only the root and never touched the leaf; a bare `cargo test` likewise; and
> `cargo build -p leafpkg` was required to build the cdylib at all. This is documented
> Cargo behaviour — when a workspace root is itself a package, unspecified
> `default-members` is that package — and it was checked rather than trusted.

The consequence is the requirement, met more strictly than the single manifest could have
met it: **pyo3 is compiled only when something names it.** `cargo test`, `cargo clippy
--all-targets`, `cargo check --all-features --all-targets` and `cargo publish` at the root
behave exactly as they do today. No existing CI command uses `--workspace`, so **the
existing workflow needs no edit at all** — verified by reading every `cargo` invocation in
`ci.yml`.

---

## 1. Layout

```
Cargo.toml                      # [package] macrame-db  +  [workspace] members = [...]
Cargo.lock                      # now covers both; still one lockfile
.cargo/config.toml              # unchanged (RUST_TEST_THREADS = "1", R15)
src/  tests/  benches/  examples/  docs/        <-- NOTHING MOVES

bindings/python/
    Cargo.toml                  # macrame-py, publish = false, crate-type = ["cdylib"]
    src/
        lib.rs                  # #[pymodule] and nothing else
        runtime.rs              # the tokio boundary (§3)
        errors.rs               # DbError -> exception hierarchy (§4)
        types.rs                # value types: EdgeAssertion, ConceptUpsert, ...
        database.rs             # the Database facade (§6)
        graph.rs                # Subgraph, traversal, analytics (§8)
        temporal.rs             # reconstruct, archive, snapshot chain (§9)
        vector.rs               # embeddings, hybrid search (§10)

pyproject.toml                  # maturin, manifest-path = bindings/python/Cargo.toml
python/
    macrame/
        __init__.py             # re-exports from ._macrame, plus pure-Python sugar
        _macrame.pyi            # type stubs
        py.typed                # PEP 561 marker
tests_py/
    conftest.py
    test_*.py
```

`python/` is a *mixed* maturin project (`python-source = "python"`). It exists so there is
somewhere to put the `.pyi`, the `py.typed` marker, and the small amount of Python that is
better written in Python than in `#[pyfunction]` — the context manager, the `datetime`
coercion helpers, and `__all__`.

`tests_py/` sits outside `python/` deliberately, so the test suite is not shipped inside
the wheel.

---

## 2. The manifests

### Root `Cargo.toml` — one block added, nothing changed

```toml
# Added for the Python bindings (v0.7.0). The root stays a package, so this is
# additive: `default-members` is deliberately absent because Cargo's default for
# a workspace root that is itself a package is *that package alone*. Verified
# with `cargo metadata`, not assumed — the whole point of the layout is that
# `cargo test` / `cargo publish` here are byte-for-byte the commands they were
# before, and that property is worth a check rather than a belief.
#
# `bindings/python` is `publish = false`, so `cargo publish` still uploads
# exactly one package.
[workspace]
members = ["bindings/python"]
```

### `bindings/python/Cargo.toml`

```toml
[package]
name = "macrame-py"
version = "0.7.0"          # tracks the wheel, which tracks macrame-db
edition = "2021"
# Not the root's floor restated: pyo3 0.29 declares 1.83 (measured, crates.io
# API), which is under macrame-db's 1.88, so the binding adds no MSRV pressure
# and the real floor is still `home@0.5.12` reached through libsql-ffi.
rust-version = "1.88"
license = "MIT OR Apache-2.0"
description = "Python bindings for Macrame"
# Never goes to crates.io. The artifact is a wheel; a source crate for this
# would be a second way to build the same thing, out of step with pyproject.toml.
publish = false

[lib]
# The Python module's import name. maturin's `module-name` must agree.
name = "_macrame"
crate-type = ["cdylib"]

[dependencies]
macrame-db = { path = "../..", features = ["metrics"] }
pyo3 = { version = "0.29", features = ["abi3-py310", "chrono"] }
tokio = { version = "1", features = ["rt-multi-thread"] }

[features]
# ON for the wheel, OFF for `cargo test -p macrame-py`. Never a default: with it
# on, a test binary links against Python symbols only the interpreter supplies.
# maturin turns it on itself, so nothing here has to remember to.
extension-module = ["pyo3/extension-module"]
```

Two choices in that file are arguments rather than defaults, and both are recorded as
decisions:

**`features = ["metrics"]` on the dependency (proposed D-093).** `metrics` is off by
default in Rust because a Rust consumer can turn it on. **A Python consumer cannot** —
feature flags do not survive into a binary wheel, so anything the wheel does not enable is
permanently unavailable downstream. T1.4's whole argument (D-079) was that a `CHUNK_BUDGET`
which cannot be checked in situ is an aspiration; shipping the one binding that *can't*
opt in without the counters would give Python users exactly the unobservable build the
tier was written against. The cost is the one D-079 already priced: a clock read per actor
turn.

**`abi3-py310` (proposed D-094).** One wheel per platform instead of one per Python minor
version. The build cost here is dominated by `libsql-ffi` compiling the SQLite
amalgamation from source on every target, so abi3 cuts the wheel matrix by roughly 5× at
the price of the limited C API — which these bindings do not touch. `py310` rather than
`py38`/`py39` because those are EOL or nearly so and each older floor forgoes API for
users who are not there.

### `pyproject.toml`

```toml
[build-system]
requires = ["maturin>=1.9,<2.0"]
build-backend = "maturin"

[project]
# `macrame` is taken on PyPI (measured: 0.0.1, 2021, "Utility to build
# Assembly/C/C++ projects"), exactly as it is taken on crates.io. This mirrors
# the resolution already recorded for the crate: distribution `macrame-db`,
# import path `macrame`. See §11 for the one way that analogy is imperfect.
name = "macrame-db"
requires-python = ">=3.10"
description = "A Bitemporal Graph Ledger on libSQL · Embedded knowledge database"
license = { text = "MIT OR Apache-2.0" }
authors = [{ name = "opticsWolf" }]
readme = "README.md"
classifiers = [
    "Programming Language :: Rust",
    "Topic :: Database :: Database Engines/Servers",
    "Typing :: Typed",
]
dynamic = ["version"]

[project.urls]
Repository = "https://github.com/opticsWolf/Macrame"

[tool.maturin]
manifest-path = "bindings/python/Cargo.toml"
module-name = "macrame._macrame"
python-source = "python"
features = ["extension-module"]
```

### P0 — ✅ **DELIVERED 2026-07-31**

> Shipped: `[workspace]` + `exclude` on the root manifest, `bindings/python/{Cargo.toml,
> src/lib.rs}`, `pyproject.toml`, `python/macrame/{__init__.py,py.typed}`,
> `tests/packaging_tests.rs` (4), `tests_py/test_packaging.py` (9), `.gitignore`.
> **The manifests as committed are authoritative; the sketches above are the argument
> for them.**
>
> **Measured, after.** `workspace_default_members` is `macrame-db` alone. The tarball went
> 105 → 106 files — the one addition is `tests/packaging_tests.rs` itself — with no
> `python/`, `bindings/`, `tests_py/` or `pyproject.toml` in it. Full suite: **25 result
> lines, 300 passed, 0 failed**, no R15 truncation. Clippy clean under `-D warnings` on
> both packages. `cargo publish --dry-run` clean at 106 files / 2.0 MiB. Wheel:
> `macrame_db-0.6.0-cp310-abi3-win_amd64.whl`, installed and imported, 9/9 Python tests.
>
> **Three things in this plan were wrong or incomplete, and are corrected here.**
>
> 1. **The `exclude` list was missed entirely.** §0 argued the include/exclude burden as a
>    cost of the *single-manifest* approach and let the workspace split appear to escape
>    it. It does not: the root package is still the repo root — that is the whole point of
>    the layout — so `pyproject.toml`, `python/` and `tests_py/` are packaged unless
>    named. Shipping them is not an error and raises no warning, so this would have gone
>    to crates.io silently. `bindings/` needs no entry, which is the one half of the
>    reasoning that held: Cargo skips a subdirectory carrying its own `Cargo.toml`.
> 2. **The binding manifest sketch listed pyo3's `chrono` feature.** Not needed and
>    dropped. Macrame's timestamps are `String`s and `PyDateTime` is in pyo3 core; the
>    feature only matters for converting *chrono* types, which nothing here has.
> 3. **"`maturin develop` imports an empty module" is the wrong acceptance test.** An
>    extension exporting only constants links clean without a byte of libSQL in it —
>    constants fold — so it would have proved nothing about the question P0 exists to
>    answer. The module ships `engine_linked()`, which takes the address of a function
>    reaching `Database::open` and so forces the amalgamation into the link.
>
> One trap found while writing the tests, recorded because it is not obvious: **`cargo
> publish` verify would not catch a bad `include_str!` in `tests/`.** It builds the
> unpacked tarball, it does not test it. A test reading a file the tarball omits passes
> `--dry-run` and breaks in a consumer's `cargo test`. That is why the cross-manifest
> checks live in `tests_py/` and not in `tests/packaging_tests.rs`.

---

## 3. P1 — The runtime boundary *(the item everything else waits on)*

This is the hard part and it should be built and tested before a single ledger method is
exposed. Every interesting method on `Database` is `async`, and `Database` owns two
`tokio::task::JoinHandle`s (the write actor and the snapshot cadence) that must outlive
every call and be shut down in order.

### Sync facade, not coroutines

**Decision (proposed D-095): the binding is synchronous.** `pyo3-async-runtimes` and an
asyncio-native surface are rejected for v0.7.0, on the architecture's own grounds: the
Write Actor is the sole write connection and serialises every write through one channel,
so exposing `await` on the write path advertises concurrency the actor will not grant.
The read path is genuinely concurrent, but a mixed surface — some methods awaitable, some
not — is worse than either pure form. Revisit when there is a caller who has measured a
need.

The mechanism is `Python::detach` (pyo3 0.29's name for `allow_threads`) around `Runtime::block_on`, so the GIL is
released for the duration of every database call and other Python threads keep running:

```rust
// Sketch. `allow_threads` requires the closure and its captures to be Ungil,
// which for `&Database` means `Database: Sync`.
py.detach(|| runtime().block_on(async { db.write_concepts(v).await }))
```

> **Measured.** A compile probe (`fn assert_send_sync<T: Send + Sync>()`, since removed)
> confirmed `Database: Send + Sync`, and likewise `EdgeAssertion`, `ConceptUpsert`,
> `TraversalBuilder`, `Subgraph`, `MaterializedState`, `ArchiveReport`, `HybridHit`,
> `VectorSearchResult`, `RebuildReport`, `DbError` and `libsql::Connection`. **This is
> the fact the whole design rests on**: `#[pyclass]` requires `Send`, and without it the
> only option is `#[pyclass(unsendable)]`, which pins the object to the thread that made
> it and would make `allow_threads` unavailable. Re-run the probe if `Database` ever
> gains a field.

### One process-global runtime

A runtime per `Database` would mean N thread pools for N handles. One global
multi-threaded runtime behind a `OnceLock` is the right shape, and it also sidesteps the
sharpest failure mode: `tokio::runtime::Runtime::drop` panics when called from inside a
runtime context, and a per-handle runtime dropped by Python's GC has no guarantee about
where it is dropped from. A global runtime is never dropped.

**Hazard to document, not to fix (assumed, needs probe P1-b):** a `OnceLock<Runtime>` is
not fork-safe. On Linux, `multiprocessing`'s default start method is `fork`, and a child
inherits a runtime whose worker threads did not come with it — the first `block_on` in the
child deadlocks. The mitigation is documentation plus `os.register_at_fork` in
`__init__.py` to poison the handle with a clear error rather than hang. **Probe: fork a
child under `multiprocessing` on Linux and confirm the failure mode is the error and not
the hang.**

### `close()` consumes `self`, and Python cannot

[`Database::close`](../src/connection.rs) takes `self` by value; a `#[pymethods]` method
only ever gets `&mut self`. The facade is therefore:

```rust
#[pyclass(name = "Database", module = "macrame")]
pub struct PyDatabase {
    inner: Option<macrame::Database>,   // None once closed
}
```

with every method going through `self.inner.as_ref().ok_or_else(closed_error)?`. That
gives a real, typed "this database is closed" error instead of a panic, and it is what
makes the context manager below possible.

### The context manager is the answer to `Drop`

[`impl Drop for Database`](../src/connection.rs) warns rather than asserts, and its
rationale is explicit: dropping costs one final snapshot, plus the writer's `Result`,
which only `close()` can return. Python's GC is non-deterministic, so a Python user who
never calls `close()` gets that loss at an unpredictable time — and `tracing::warn!` is
invisible in any application that has not configured a subscriber, which is essentially
every Python application.

**So `__enter__`/`__exit__` are not sugar here, they are the supported path**, and the
docstring says why in the same terms the `Drop` impl does:

```python
with macrame.Database.open("kb.db") as db:
    db.write_concepts([...])
# close() ran: the final snapshot is written and the write actor's exit status was checked
```

`__del__` maps to the same warning the Rust `Drop` produces, raised through
`warnings.warn(..., ResourceWarning)` — visible under `-W default` and to pytest, which is
strictly more than `tracing::warn!` reaches today.

### Acceptance for P1

- `macrame.Database.open(path)` → `close()` round-trips, and the snapshot appears on disk.
- A second Python thread demonstrably makes progress while a long call is in flight
  (proves `allow_threads` is wired). **Probe: two threads, one running a large
  `bulk_import`, the other incrementing a counter.**
- Calling any method after `close()` raises `MacrameClosedError`, never panics.
- A dropped-without-close handle emits a `ResourceWarning`.

### P1 — ✅ **DELIVERED 2026-07-31**

> Shipped: `bindings/python/src/{runtime,errors,database}.rs`, `lib.rs` rewritten as a
> table of contents, `python/macrame/__init__.py` with the fork guard,
> `tests_py/{conftest.py,test_lifecycle.py}` (25 tests), `tests_py/probes/`.
> **34 Python tests pass**; clippy clean on `macrame-py` under `-D warnings`.
>
> **The plan's core design decision was wrong and is replaced.** §3 specified
> `PyDatabase { inner: Option<Database> }` with methods taking `&mut self`. That does not
> survive the GIL rule in the same section: a non-`frozen` `#[pyclass]` borrows through a
> runtime `RefCell`, and the borrow is live across the whole GIL-released call, so a
> second thread entering any method during that window gets `PyBorrowMutError` — an error
> about pyo3's internals raised for an ordinary concurrent read. Shipped instead:
> `#[pyclass(frozen)]` over `RwLock<Option<Database>>`, where ordinary calls take a read
> lock and run concurrently and `close()` takes the write lock and waits for them. This
> also states the architecture's actual concurrency (reads concurrent, writes serialised
> by the actor) rather than flattening it to "one at a time".
>
> A consequence worth its own sentence: **the lock must be acquired inside the
> GIL-released closure, not outside.** `close()` blocking on `inner.write()` while
> holding the GIL, against a reader holding the read lock inside `detach`, deadlocks —
> the reader needs the GIL back to finish and the closer will not yield it.
>
> **Other corrections.** pyo3 0.29 renamed `Python::allow_threads` → `detach` and
> `Python::with_gil` → `attach`; the plan (and this document above) used the old names.
> `Database::open` takes `impl AsRef<Path>`, which cannot be turbofished, so the link
> anchor needs a monomorphising wrapper — and two attempts at that anchor were written as
> null comparisons, which clippy correctly rejects as tautologies. It is `black_box` now,
> because the claim was never about a runtime value.
>
> **Probe P6-a resolved, and it did not go the hopeful way.** `tests_py/probes/
> r15_concurrent_open.py`: 48 concurrent opens from 48 Python threads on a barrier
> faulted **2 in 12 runs** — the same rate as the Rust control arm of `r15_soak.rs` at the
> same width. The argument that the boundary might *reduce* exposure (one shared runtime,
> GIL-serialised entry) is refuted, and by P1's own central feature: `block_on` releases
> the GIL, so the threads are genuinely concurrent inside `open`. §9's no-xdist rule is
> now measured rather than transferred.
>
> **Measured, not asserted:** the GIL test's discrimination. A ticker thread advanced
> **333 times during a 0.507 s blocked call**, against a test threshold of 25 — 13× of
> headroom, so it distinguishes "ran" from "did not run at all" without being a timing
> flake.

---

## 4. P2 — Errors

`DbError` has 24 variants, and the crate has spent several releases making them *specific*
— D-069's theme, restated in the source at least four times, is that an error naming the
wrong subject sends a caller to fix the wrong thing. `DiagnosticConn` exists rather than
`NotFound` for exactly that reason; so does `InvalidTimestamp`, so does `InvalidId`, so
does `RebuildInterrupted` against `RebuildFailed`.

**Flattening all of that into `RuntimeError(str(e))` would discard the most deliberate
work in the crate at the boundary.** The mapping is therefore structural:

```
MacrameError(Exception)                     # base; catch-all
├── EngineError                             # Engine
├── IntegrityError
│   ├── OverlappingIntervalError            # .overlap -> Overlap dataclass, 7 fields
│   ├── SingleOpenViolationError            # .source_id .target_id .edge_type
│   ├── CurrentDriftError                   # .n
│   ├── RebuildFailedError                  # .n
│   └── RebuildInterruptedError             # .reason   (distinct: repair did NOT run)
├── NotFoundError                           # .id
├── ValidationError
│   ├── InvalidEdgeTypeError                # .edge_type
│   ├── InvalidIdError                      # .id .reason
│   ├── InvalidTimestampError               # .value .reason
│   ├── InvalidModelNameError               # .model
│   └── AttributeModeUnstatedError          # .as_of        (T3.2 / D-085)
├── VectorError
│   ├── DimMismatchError                    # .got .expected .model
│   └── ModelNotRegisteredError             # .model .table
├── TemporalError
│   ├── ReplayCorruptError                  # .seq .reason
│   ├── SnapshotIncompatibleError           # .path .reason
│   ├── PayloadVersionError                 # .got .max
│   ├── ArchiveViolationError               # .table
│   └── ArchiveWindowError                  # .window (timedelta) .reason
├── WriterError                             # WriterUnavailable / DroppedResponder / Stopped
├── DiagnosticConnError                     # .path .reason
├── BudgetError                             # SubgraphTooLarge: .n .budget
└── MacrameClosedError                      # binding-only: method called after close()
```

Every variant's fields become exception attributes. `str(e)` stays exactly the `#[error]`
rendering, so nothing is lost for the caller who only wants the sentence.

**Two mappings are worth arguing rather than assuming:**

- **`AttributeModeUnstatedError` must not be softened.** T3.2 turned this from a
  `tracing::warn!` into a value the caller cannot miss, precisely because warnings are
  invisible. Python has an even stronger pull toward "just warn"; the answer is no. It
  raises.
- **`NegativeEdgeWeightError`** belongs under `IntegrityError`, not `ValidationError`: it
  is raised at *load* time about data already stored, and a caller who reads it as
  "your input was bad" will go looking in the wrong place.

**Acceptance:** a rule-enforcement test in the house style — `tests_py/test_errors.py`
parses `src/error.rs` for `#[error(` variants and asserts every one has a mapping and a
distinct Python class. This is the same shape as
`every_performance_decision_names_its_fixture`, and for the same reason: the failure mode
is a new variant silently falling through to the base class, which no ordinary test would
notice.

### P2 — ✅ **DELIVERED 2026-07-31**

> Shipped: `bindings/python/src/errors.rs` (35 exception classes), `testing.rs` (the
> sample table), `__init__.py` exporting the tree, `tests_py/test_errors.py`.
> **129 Python tests pass**; clippy clean under `-D warnings`.
>
> **The variant count in this section was wrong: there are 27, not 24.** The tree above
> also omitted `Migration` and `RecordedAtRegression` entirely, and folded the three
> `Writer*` variants into one class. All are mapped now — `MigrationError` directly under
> the base, `RecordedAtRegressionError` under `IntegrityError` (it is transaction-time
> monotonicity, not input validation), and `WriterUnavailableError` /
> `WriterDroppedResponderError` / `WriterStoppedError` under `WriterError`.
>
> **The acceptance test above is not the strongest available tripwire, and was replaced
> by a stronger one.** `errors::build` is a `match` over `DbError` with **no wildcard
> arm**, so a variant added upstream fails to compile `macrame-py` at the line that needs
> a decision. A test can only run after the thing exists; a compiler error arrives
> before a wheel is built. **Verified rather than asserted**: injecting a
> `ProbeVariantForP2` into `src/error.rs` produced
> `error[E0004]: non-exhaustive patterns … not covered` at `errors.rs:363`, and the file
> was restored byte-identical (0 CRs, empty `git diff`).
>
> The parsing test still exists, because a compiler cannot check that a `setattr` used
> the right *name*, that a class sits under the right base, or that it is reachable from
> `macrame`. It now covers the seam between the two mechanisms: `src/error.rs` is parsed
> and compared against both the Rust sample table and this test's own expectation table,
> so a variant that is mapped but never constructed fails.
>
> **Two design changes.**
>
> 1. **`Overlap` is flattened, not nested.** The plan proposed `.overlap` holding a
>    dataclass of seven fields. Shipped as seven attributes directly on the exception:
>    the names already carry the distinction the type existed to preserve — `valid_*` is
>    what the caller asserted, `existing_*` is what it collided with — so a wrapper adds
>    a hop without adding information, and `e.source_id` is what a Python caller reaches
>    for first.
> 2. **`to_py` re-acquires the GIL, which this plan did not anticipate.** It is called
>    from inside `Python::detach` closures — that is the whole point of P1's `with_db` —
>    so building an exception object, which needs a `Python` token, has to `attach`. One
>    GIL acquire per raised error, nothing on the success path.
>
> `libsql` is now a direct dependency of `macrame-py`. It is already in the graph through
> `macrame-db` so it costs nothing to build; it is there to be *nameable*, so
> `testing.rs` can construct a sample `DbError::Engine` and close the last gap in the
> variant table. P4.6's diagnostic query will need `libsql::Value` for the same reason.

---

## 5. P3 — Value types and coercion

### Builders become keyword constructors

The Rust builders consume `self` (`EdgeAssertion::new(..).valid_from(..).weight(..)`).
Chaining that in Python is possible but un-Pythonic. The constructor takes keywords and
the chained setters remain, returning a new object:

```python
macrame.EdgeAssertion("a", "b", "LINKS", valid_from=t0, valid_to=t1, weight=0.8)
macrame.ConceptUpsert("a", "A", content="body", valid_from=t0)
macrame.Traversal("a", max_depth=3, edge_types=["LINKS"], attribute_mode=AttributeMode.AT_TIME)
```

`normalized()` is called on the Rust side at the point of use, so validation errors surface
from the method that would have written the row — not from the constructor, where the
caller has no operation to associate them with.

### Timestamps (proposed D-096)

Every timestamp in the crate is a canonical RFC3339 microsecond UTC `&str`, and
`InvalidTimestamp` is what a caller gets for anything else. A Python user will pass a
`datetime` on their first attempt.

**Accept both, normalise at the boundary, never return anything but `datetime`.** Inbound:
`str` passes through to `timestamp::normalize` unchanged; `datetime` is converted (naive
is rejected, not assumed-UTC — the same principle §4.1 applies to timestamps, where a
silent repair becomes a wrong answer later). Outbound: always `datetime` with `tzinfo=utc`,
**except** the open sentinel `9999-12-31T23:59:59.999999Z`, which is exposed as
`macrame.OPEN` — a module-level `datetime` constant — so `valid_to == macrame.OPEN` reads
as intended and `Interval.is_open()` remains the supported check.

**Assumed, needs probe P3-a:** that `9999-12-31T23:59:59.999999` round-trips through
`datetime` on all target platforms. `datetime.max` is `9999-12-31 23:59:59.999999`, so
this is exactly representable — but it is one microsecond from overflow, and any
arithmetic on it raises. Probe before committing to the `datetime` sentinel; the fallback
is a distinct singleton `Open` object.

### Embeddings

`Vec<f32>` inbound accepts anything supporting the buffer protocol *or* a plain sequence.
This is the difference between a numpy array crossing as a memory view and 768 boxed
Python floats being unpacked one at a time. **No numpy build dependency** — `PyBuffer<f32>`
is in pyo3 itself, so numpy is supported without being required.

### P3 — ✅ **DELIVERED 2026-07-31**

> Shipped: `bindings/python/src/{timestamps,types}.rs`, `__init__.py` exporting the value
> types, `tests_py/test_types.py` (45 tests). **174 Python tests pass**; Rust suite
> unchanged; clippy clean under `-D warnings`.
>
> **Probe P3-a resolved, and it refutes this section.** The plan proposed `macrame.OPEN`
> as a module-level `datetime`. Measured on CPython 3.13:
>
> ```text
> aware = datetime(9999,12,31,23,59,59,999999, tzinfo=utc)
>   aware.astimezone(timezone(timedelta(hours=1)))  -> OverflowError
>   aware.astimezone()          # local zone        -> OSError
>   aware + timedelta(microseconds=1)               -> OverflowError
> ```
>
> `astimezone()` raises for every zone east of UTC, and in a bitemporal ledger the open
> interval is *current belief* — not a rare row, the common one. **An open interval is
> `None`**, in both directions; `macrame.OPEN` is the stored string, for callers who need
> to name it. The cost is stated rather than hidden: sorting a `valid_to` column needs
> `key=lambda r: (r.valid_to is None, r.valid_to)`. The probe survives as a test, so if a
> future CPython makes those work, the failure is the signal to revisit D-096.
>
> **D-094's justification for abi3 was wrong, and compiling found it.** It claimed the
> price was "the limited C API, which these bindings do not touch". They touch it twice:
> `PyDateAccess` / `PyTimeAccess` — pyo3's `get_year()` / `get_hour()` — and
> `pyo3::buffer` are both compiled out under `Py_LIMITED_API`.
>
> - Timestamp fields are read with `getattr`: seven Python lookups instead of seven
>   struct reads, on the coercion path only. `isoformat()` is the tempting one-call
>   alternative and is a trap — it omits `.000000` when microseconds are zero, producing
>   a non-canonical string for every timestamp landing exactly on a second. Pinned by a
>   test.
> - The buffer protocol is gone, so §5's `PyBuffer<f32>` does not exist. Replaced by an
>   explicit packed-`bytes` fast path (`arr.astype("<f4").tobytes()`) plus sequence
>   extraction for everything else.
>
> **abi3 stays, and now on evidence rather than assumption.** Coercing a 768-dim vector:
> packed bytes **60.8 µs**, numpy `float32` as a sequence **94.9 µs**, numpy `float64`
> 114.3 µs, Python list 73.5 µs. The buffer protocol would have bought ~35 µs per vector
> — 1.6×, not the order of magnitude the plan implicitly assumed — against a 4–5×
> wheel matrix on a crate that rebuilds the SQLite amalgamation per target.
>
> **Three further changes.**
>
> 1. **Validation happens in the constructor, not at the point of use.** §5 said
>    otherwise. The deciding case is bulk writes: validating in `write_bulk_atomic`
>    reports "invalid edge type" for a list of ten thousand edges with no indication
>    which one, from a traceback pointing at the write. In the constructor the traceback
>    points at the line that built it — and an `EdgeAssertion` that exists is then one the
>    ledger will accept.
> 2. **`properties` stays a JSON string**, not a dict. The crate documents the payload as
>    opaque; accepting a dict would make this binding decide key order and what happens to
>    a `Decimal` for data it never reads.
> 3. **No chained setters.** §5 proposed keeping them alongside the keyword constructor.
>    The constructor is complete, and a second way to build the same value is API surface
>    with no capability behind it.
>
> **A bug caught before it shipped.** The first draft of `coerce_embedding` took the
> packed path for anything extracting as `Vec<u8>`, so `bytearray` and `memoryview` would
> be fast too. That also swallows a `tuple` of small ints and reinterprets it as float32
> — a silent wrong answer giving a valid embedding of a quarter the length, which the
> dimension check would then blame on the model. Now `bytes` exactly; there is a test.
>
> **R15 fired during this phase**, exactly as `.cargo/config.toml` describes: one run of
> four came back **24 result lines / 294 passed / 0 failed** against 25 / 300. The other
> three were clean. The Rust crate is untouched since P0, so this is the documented
> upstream fault and not a regression — recorded because the whole point of that note is
> that a *smaller* green number is the symptom.

### `Subgraph` stays opaque (proposed D-097)

`Subgraph` is three `BTreeMap`s and is bounded by an explicit `byte_budget` because it is
already the largest thing the crate materialises (D-047, and D-087 is scheduled to intern
its keys). Converting it to Python dicts on return **doubles the peak memory of the one
operation that already has a budget**, and does it eagerly whether or not the caller reads
more than `degree()`.

So it is a `#[pyclass]` wrapping the Rust value, with the accessors forwarded —
`out_edges(node)`, `in_edges(node)`, `degree(node)`, `weighted_degree(node)`,
`total_weight()`, `edge_count()`, `estimated_bytes()`, `is_closed()`, plus `__len__`,
`__contains__`, and iteration over node ids. An explicit `.to_dict()` is provided for
callers who want the copy and have decided to pay for it.

---

## 6. P4 — The `Database` facade

Exposed, in this order (each phase independently shippable and testable):

| Phase | Methods |
|---|---|
| **P4.1 write** | `upsert_concept`, `write_concepts`, `assert_edge`, `retire_edge`, `bulk_import`, `write_bulk_atomic`, `write_analytics_annotations` |
| **P4.2 read** | `traverse` (`TraversalBuilder::execute` / `execute_ids`), `load_subgraph`, `load_subgraph_with` |
| **P4.3 temporal** | `reconstruct`, `archive`, `archive_windowed`, `verify_snapshot_chain`, `query_as_of_edges` |
| **P4.4 vector** | `register_model`, `upsert_embeddings`, `search_vector`, `keyword_search`, `HybridSearch`, `FilteredVectorSearch`, `rebuild_fts` |
| **P4.5 integrity** | `rebuild_current`, `rebuild_current_chunked`, `audit_current` |
| **P4.6 introspection** | `path`, `schema_version`, `archive_path`, `snapshots_dir`, `metrics` |
| **P4.7 analytics** | `dijkstra`, `astar`, `scc`, `k_core`, `modularity`, `louvain` on `Subgraph` |

`write_bulk_atomic` carries `estimated_bulk_hold` and `BULK_ATOMIC_WARN_HOLD` across as a
`estimate_bulk_hold(edges) -> timedelta` free function, because T1.3's whole delivery was
making that ceiling predictable *before* the call, and a Python caller who cannot ask is
back where 0.5.x was.

### P4.1 — ✅ **DELIVERED 2026-07-31**

> Shipped: the seven write methods on `Database`, `estimate_bulk_hold` +
> `BULK_ATOMIC_WARN_HOLD` at module level, `bindings/python/src/rows.rs`,
> `tests_py/test_write_path.py` (32 tests). **206 Python tests pass**; Rust suite
> 25/300; clippy clean; tarball unchanged.
>
> **`diagnostic_query` and `explain` are pulled forward from P4.6.** P4.1's acceptance is
> "writes, and reads back", and without a read path there is no way to tell a write that
> landed from a method that returned a plausible count and did nothing. Every later
> phase's tests need the same thing. §7's constraint is kept exactly: they are *methods
> that run a query and return rows*, never a connection object, so the capability T5.1
> wanted survives and the object that would let a caller keep it does not.
>
> **P2's error mapping is now tested against the ledger rather than against a hook.**
> A real overlapping assertion raises `OverlappingIntervalError` carrying
> `valid_from="2026-03-01…"` (what was asserted) and `existing_from=T0` (what it hit);
> two open intervals raise `SingleOpenViolationError` with `source_id`. That is the loop
> P2 could only close synthetically.
>
> **D-041 has a test that would catch its regression.** `write_analytics_annotations`
> leaves `transaction_log` at exactly the count it had, while `write_concepts` raises it
> — measured 6 → 6 against 6 → more. Before D-041 these were one call, and analytics
> output overwrote concept content and versioned it. A test asserting only that
> annotations land would pass just as well if they landed in the log too.
>
> **Two things this phase got wrong, both mine and both in the tests.**
>
> 1. A test comment claimed "the edge tables do not enforce" referential integrity. They
>    do — `configure()` sets `PRAGMA foreign_keys = ON` on every connection — so two
>    tests wrote edges to concepts that did not exist and got `EngineError: FOREIGN KEY
>    constraint failed`. The binding was right and the test was wrong. Fixed, and the
>    behaviour now has a test of its own, because it is the first thing anyone hits who
>    writes edges before concepts and the error is the schema talking rather than a
>    binding defect.
> 2. `retire_edge(..., valid_to=None)` would have passed the open sentinel down, where
>    the ledger answers with a single-open violation about a row the caller did not think
>    they wrote. Now refused at the boundary with a sentence saying why.
>
> **Checked against T1.3's table**: `estimate_bulk_hold` for 500 spread edges returns
> 37 ms against the measured ~34 ms, and the pathological shape — many corrections to one
> relationship — estimates strictly higher than the same row count spread across distinct
> relationships, which is the 7× case the model exists for.

`astar` takes a heuristic callback, which means calling **into** Python from Rust while the
GIL is released. That inverts the P1 arrangement and is the one method that needs its own
design pass — it is last in the list for that reason, and the fallback is to ship `astar`
with a built-in heuristic set rather than an arbitrary callable.

---

## 7. What is deliberately **not** exposed

- **`Database::raw()`** — `#[doc(hidden)]` since T5.1/D-091, and §4.7 invariant 2's named
  hole. A Python escape hatch into `libsql::Database` would export the hole to a much
  larger audience with much less context.
- **`Database::read_conn()`** — hands back a *shared* connection; a long Python query on
  it competes with every traversal and fold in the process. `diagnostic_conn()` exists
  precisely because that need is real and this is the wrong way to serve it.
- **`vector::registry::register_model(conn, …)`** — the free function, also
  `#[doc(hidden)]`, also an invariant-2 hole. `Database::register_model` is exposed; the
  bare-connection form is not.
- **`open_with_clock`** — `FakeClock` is a test seam. Exposing it invites a Python caller
  to inject a clock into a production ledger, and `recorded_at` is the transaction-time
  axis. If Python-side temporal tests need it, they get a separate
  `macrame.testing` submodule, gated and documented as unsupported.

**`diagnostic_conn()` is exposed**, but as a *method that runs a query and returns rows* —
`db.explain(sql)` and `db.diagnostic_query(sql, params)` — not as a raw connection object.
The capability T5.1 wanted (a caller's own read-only connection, an OS-level boundary
rather than a reversible pragma) is preserved; the object that would let a caller keep it
and do something else with it is not.

---

## 8. P5 — Packaging, naming, wheels

### The name collision, and where the crates.io analogy breaks

> **Measured.** `macrame` on PyPI: version 0.0.1, released 2021-09-25, "Utility to build
> Assembly/C/C++ projects". The same situation as crates.io, and effectively abandoned.

The crate's resolution — publish as `macrame-db`, import as `macrame` — maps across, and
consistency is worth something. **But the analogy is imperfect in one way that has to be
stated:** Rust's `[lib] name` is namespaced per build graph, so `macrame-db` providing
`macrame` collides with nothing. Python's `site-packages` is flat. If the 2021 package
also installs a top-level `macrame/`, then `pip install macrame macrame-db` produces two
distributions fighting over one directory, and which one wins depends on install order.

This is unlikely (that package is dead and its users are not our users) and it is not
silent (`pip` warns on file conflicts). Recommendation: **import as `macrame`**, and say
so in the README next to the existing note about the crate name. The alternative —
importing as `macrame_db` — costs the symmetry and is available if the collision ever
turns out to matter.

### Wheels

- `manylinux_2_28` x86_64 + aarch64, macOS universal2, Windows x86_64.
- `sdist` published too, so a platform without a wheel can build from source — which
  works, since `libsql-ffi` compiles the amalgamation anyway and needs only a C compiler.
- **Assumed, needs probe P5-a:** wheel build time and size. `libsql-ffi` compiles SQLite
  from source per target; aarch64 under emulation is the risk. If a cross-build exceeds
  the runner budget, the fallback is native aarch64 runners rather than dropping the
  target. **Nothing in this plan should assume the matrix is cheap until one full build
  has been timed.**
- **`musllinux` is out of scope for 0.7.0** until someone asks — it doubles the Linux
  matrix for a `libsql-ffi` build that has not been checked against musl here.

---

## 9. P6 — Tests

`tests_py/`, pytest. **The suite tests the binding, not the ledger.** The ledger has 24
Rust test binaries; re-asserting bitemporal semantics through Python would be a second,
weaker copy that drifts. What is genuinely new at this boundary, and therefore what gets
tested:

1. **Type coercion** — `datetime` ↔ canonical string in both directions, the `OPEN`
   sentinel, buffer-protocol embeddings, and every rejection (naive datetime, wrong
   dimension, invalid model name).
2. **The error mapping**, exhaustively, via the `src/error.rs`-parsing rule test in §4.
3. **Lifecycle** — context manager, use-after-close, the `ResourceWarning`, and that
   `close()` actually wrote the snapshot.
4. **GIL release** — the two-thread progress test from P1.
5. **One end-to-end smoke test per phase**, proving the wiring, not the semantics.

### R15 applies here too, and pytest will trip it

The fault is **concurrent open**, and `pytest-xdist` opening a database per worker is
exactly the shape that reproduced 2/12 at 32 concurrent opens. `.cargo/config.toml`'s
`RUST_TEST_THREADS = "1"` does not reach pytest.

The Python suite therefore runs **single-process, no xdist**, and `tests_py/conftest.py`
carries a comment pointing at `.cargo/config.toml` rather than restating it — one copy of
that analysis, in the place that already has it right. The reporting hazard carries across
unchanged: the fault kills the process, so a crashed run comes back with a *smaller* pass
count and no failures. Any CI gate on this suite must key on the absence of the summary
line, not on the exit code alone.

**Assumed, needs probe P6-a:** that R15 reproduces through the Python boundary at all.
It should — the binding opens databases the same way — but the concurrency profile is
different (one global runtime, GIL-serialised entry) and it is possible the boundary
*reduces* exposure. Worth 20 minutes with the `r15_soak` shape before writing the
constraint into the docs as fact.

---

## 10. P7 — CI

A new `python.yml`, calling the existing `ci.yml` as a gate first — the same shape
`release.yml` already uses:

```yaml
jobs:
  rust:                       # the crate must be green before the wheel is built
    uses: ./.github/workflows/ci.yml
  wheels:
    needs: rust
    # PyO3/maturin-action, matrix over the targets in §8
  test:
    needs: wheels
    # install the built wheel, run tests_py/ single-process
```

`ci.yml` itself needs **no change** — verified by reading every `cargo` invocation in it;
none use `--workspace`, so all of them remain scoped to `macrame-db`.

Publishing to PyPI uses **Trusted Publishing (OIDC)**, not a stored token. This is the
one place the Python side is unambiguously better off than the crates.io side: PyPI's
OIDC support is mature, and it removes the "add a secret I will not touch" step entirely.
The crates.io job can follow later (it was already noted as optional after 0.6.0).

---

## 11. P8 — Stubs and docs

- **`python/macrame/_macrame.pyi`**, hand-written. `pyo3-stub-gen` is available but adds a
  build step and generates stubs that still need hand-editing for overloads.
- **A rule-enforcement test** asserting the stub names exactly the module's `dir()` — same
  house pattern as §4's error test, and it catches the real failure, which is a method
  added in Rust and never stubbed.
- **`py.typed`** so the stubs are actually consulted.
- **Docstrings carry the *reasons*, not just the signatures.** This crate's docs are its
  main asset and most of them are arguments. The four that must survive the crossing
  verbatim in substance: `close()` (why it is not optional), `AttributeMode` (T3.2 — what
  `as_of` does and does not fix), `write_bulk_atomic` (the hold ceiling), and
  `diagnostic_conn` (boundary vs guardrail).

---

## 12. Sequencing

| Item | Depends on | Deliverable | Acceptance |
|---|---|---|---|
| **P0** Workspace ✅ | — | root `[workspace]` + `exclude`, `bindings/python`, `pyproject.toml`, packaging tests | ✅ suite 300/300; tarball clean; wheel imports; `engine_linked()` true |
| **P1** Runtime ✅ | P0 | `runtime.rs`, `errors.rs`, `database.rs`, fork guard | ✅ 34 Python tests; GIL probe 333 vs 25; R15 probe P6-a resolved |
| **P2** Errors ✅ | P1 | `errors.rs` (35 classes), `testing.rs` sample table | ✅ 129 Python tests; exhaustive-match tripwire verified by injection |
| **P3** Types ✅ | P2 | `timestamps.rs`, `types.rs`, packed-bytes embeddings | ✅ 174 Python tests; probe P3-a resolved (refuted the datetime sentinel) |
| **P4.1** Write ✅ | P3 | 7 write methods, `estimate_bulk_hold`, `rows.rs`, diagnostic reads | ✅ 206 Python tests; D-041 pinned; P2 mapping verified against the ledger |
| **P4.2** Read | P3 | traversal, `Subgraph` pyclass | smoke; `AttributeModeUnstated` raises |
| **P4.3** Temporal | P4.1 | reconstruct/archive/chain | smoke; `ChainCheck` surfaces divergence |
| **P4.4** Vector | P3 | register/upsert/search/hybrid | dim-mismatch raises typed |
| **P4.5** Integrity | P4.1 | rebuild/audit | smoke |
| **P4.6** Introspection | P1 | `metrics()` etc. | counters non-zero after a write |
| **P4.7** Analytics | P4.2 | 6 algorithms; `astar` last | smoke per algorithm |
| **P5** Packaging | P4.1 | wheel matrix | probe P5-a: one full matrix timed |
| **P6** Tests | P4.x | `tests_py/` | probe P6-a: R15 exposure characterised |
| **P7** CI | P5, P6 | `python.yml`, Trusted Publishing | green on all targets |
| **P8** Stubs/docs | P4.x | `.pyi`, `py.typed`, docstrings | stub-coverage rule test green |

**P0–P3 is the real risk.** Once the runtime boundary, the error tree and the coercion
layer are right, P4.x is mechanical breadth — repetitive, but each phase is small and
independently verifiable. Anyone estimating this should weight the first four items far
above their line count.

---

## 13. Open decisions

These change what gets built and are not mine to settle:

1. **Import name** — `macrame` (symmetric with the crate; small flat-namespace collision
   risk, §8) or `macrame_db` (no risk; asymmetric). Recommendation: `macrame`.
2. **abi3 floor** — `py310` as proposed, or `py39` to reach older deployments.
3. **`metrics` in the wheel** — on, as argued in §2, or off to match the Rust default.
4. **Scope of v0.7.0** — the full P4.1–P4.7 surface, or ship P4.1–P4.3 (write, read,
   temporal) and leave vector/analytics to 0.7.1. The bitemporal ledger is the part with
   no Python equivalent; vector search has many.
5. **`astar` heuristic** — arbitrary Python callable (needs the re-entrancy design pass)
   or a fixed set of built-ins.

---

## 14. Proposed decision-register entries

To be written into `docs/architecture/s13-decision-register.md` as each lands. Numbering
continues from D-092.

| ID | Decision |
|---|---|
| D-093 | The wheel ships with `metrics` on: a feature flag does not survive into a binary artifact, so anything the wheel omits is unavailable to Python users permanently. |
| D-094 | `abi3-py310`: one wheel per platform, because `libsql-ffi` rebuilds SQLite per target and the matrix cost dominates. **Amended: it does cost limited-API surface** — the datetime accessors and `PyBuffer` are compiled out. Measured at ~35 µs per 768-dim vector against a 4–5× wheel matrix, so it stands. |
| D-095 | The binding is synchronous. The Write Actor serialises writes, so `await` on the write path advertises concurrency the architecture does not grant. |
| D-096 | Timestamps are accepted as `str` or aware `datetime` and always returned as `datetime`; naive datetimes are rejected rather than assumed UTC, per §4.1's rule against silent repair. **Amended by probe P3-a: an open interval crosses as `None`, not as a `datetime` at the sentinel** — `datetime.max` cannot survive `.astimezone()` east of UTC. |
| D-097 | `Subgraph` crosses as an opaque handle, not a dict. Converting eagerly doubles the peak memory of the one operation that already carries a byte budget. |
| D-099 | Every `DbError` variant maps to its own Python class with its fields as attributes, and completeness is enforced by an exhaustive `match` rather than a test — a wildcard arm would hide exactly the regression being guarded against. |
| D-098 | The bindings live in a workspace *leaf*; the root package does not move, because `tests/` `include_str!`s `docs/` and `src/`, and moving the crate would put those outside the published tarball. |
