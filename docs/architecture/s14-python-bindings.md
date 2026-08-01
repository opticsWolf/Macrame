<!--nav-->
← [previous](s13-decision-register.md) · [index](README.md) · [next](appendices.md) →
<!--/nav-->

<a id="14-python-bindings"></a>
## §14 Python bindings

**Added 0.7.0.** Everything here describes `bindings/python`, a workspace leaf that
depends on `macrame-db` by path and is published as the wheel `macrame-db`. Read
[§3](s0-s3-foundations.md#3-crate-layout) first for the crate layout this sits beside.

**The binding changed no line of `src/`.** That is the first fact about it and the one
every other section here is arranged to keep true. The ledger it wraps is the ledger
[§4](s4-schema.md#4-schema) specifies, with the same guards, the same actor and the same
errors; what follows is a translation layer and a build topology, not a second
implementation.

---

<a id="141-build-topology"></a>
### 14.1 Build topology, and why the crate did not move

The root `Cargo.toml` is **both a package and a workspace root**. `bindings/python` is its
only member.

```
Cargo.toml            [package] macrame-db  +  [workspace] members = ["bindings/python"]
src/ tests/ benches/ examples/ docs/        unchanged, in place
bindings/python/      macrame-py: publish = false, crate-type = ["cdylib"]
pyproject.toml        maturin, manifest-path = bindings/python/Cargo.toml
python/macrame/       __init__.py, py.typed
tests_py/             pytest; not shipped in the wheel
```

**The conventional layout — move the crate to `crates/macrame/` — is not available here,
and the reason is specific to this repository.**
[`tests/fixture_matrix_tests.rs`](../../tests/fixture_matrix_tests.rs) does
`include_str!("../docs/architecture/s13-decision-register.md")` to enforce
[D-088](s13-decision-register.md#d-088)'s rule that every performance decision names its
fixture, and [`tests/index_plan_tests.rs`](../../tests/index_plan_tests.rs) reads
`src/temporal/*.rs` back to check that the SQL it pins still exists at its source. Those
resolve only because the package root *is* the repo root, which is what puts `docs/`
inside the `.crate` tarball. One level down, `docs/` falls outside the package and both
tests stop compiling for anyone who gets the crate from crates.io.

So the root does not move, and the cost of that is an `exclude` list: everything at the
repo root ships to crates.io unless named. `bindings/` needs no entry — Cargo skips a
subdirectory carrying its own `Cargo.toml` — but `pyproject.toml`, `python/` and
`tests_py/` do. Shipping them would not be an error and would raise no warning, which is
why [`tests/packaging_tests.rs`](../../tests/packaging_tests.rs) asserts the list rather
than trusting it.

**What this buys: pyo3 is compiled only when something names it.** When a workspace root
is itself a package, Cargo's `default-members` defaults to that package alone — verified
with `cargo metadata`, not assumed — so `cargo test`, `cargo clippy --all-targets`,
`cargo check --all-features --all-targets` and `cargo publish` at the root are the
commands they were before. Building the extension requires `-p macrame-py` or maturin.
No command in `.github/workflows/ci.yml` uses `--workspace`, so the Rust CI needed no
edit. `packaging_tests.rs` pins the two ways this is one line from being lost: adding
`default-members`, and turning the root into a virtual manifest.

**Three properties of the binding manifest are decisions, not defaults**
([D-093](s13-decision-register.md#d-093), [D-094](s13-decision-register.md#d-094),
[D-098](s13-decision-register.md#d-098)):

| | |
|---|---|
| `crate-type = ["cdylib"]` on the *leaf* | `crate-type` is not feature-conditional in Cargo. A cdylib on the root package would relink the statically-bound `libsql-ffi` amalgamation on every `cargo build --release`, and would advertise a cdylib to every consumer of `macrame-db` forever. |
| `features = ["metrics"]` on the dependency | Off by default in Rust because a Rust consumer can turn it on. **A Python consumer cannot** — a feature flag does not survive into a binary wheel. [T1.4 / D-079](s13-decision-register.md#d-079) argued that an unobservable `CHUNK_BUDGET` is an aspiration; the wheel is the one binding that could not opt in later. |
| `extension-module` declared but never defaulted | It must be **on** for the wheel and **off** for `cargo test -p macrame-py`, or the object links against Python symbols only the loading interpreter supplies. `pyproject.toml` turns it on, so no human has to remember. |

---

<a id="142-the-async-sync-boundary"></a>
### 14.2 The async→sync boundary

Every interesting method on `Database` is `async`; Python is not. The translation is one
process-wide tokio runtime and one rule about the GIL
([D-095](s13-decision-register.md#d-095)).

**The binding is synchronous, and that follows from
[§5.1](s5-modules.md#51-connectionrs--the-handle-the-pragmas-and-the-write-actor) rather
than from convenience.** The Write Actor is the sole write connection and serialises every
write through one channel, so exposing `await` on the write path would advertise
concurrency the architecture does not grant. The read path is genuinely concurrent, but a
surface where some methods are awaitable and some are not is worse than either pure form.

**One runtime, never dropped.** A runtime per handle would mean N thread pools, and would
put `Runtime::drop`'s panic-inside-a-runtime-context in reach of Python's garbage
collector, which chooses when and where. A `OnceLock` in a static is never dropped, so the
question does not arise. The cost is idle threads outliving the last handle.

**Every call that reaches the engine releases the GIL.** Without that, one thread inside a
traversal stops the whole interpreter — for an embedded database, the difference between
a library and a global lock. This is what makes `Database: Send + Sync` load-bearing: it
is what `Python::detach` requires, and the alternative is `#[pyclass(unsendable)]`, which
pins the object to its creating thread. A compile-time assertion in `runtime.rs` fails
with an explanation if a future field on `Database` breaks it.

**Three consequences that are not obvious, all of them found by building it:**

1. **`close()` consumes `self` and Python cannot.** The type system makes call-after-close
   impossible in Rust. Python has no way to express that, so the handle holds an `Option`
   and `close()` takes it; every other method meets a `None` and raises `MacrameClosedError`.
2. **The class is `frozen` over an `RwLock`, not `&mut self`.** A non-`frozen`
   `#[pyclass]` borrows through a runtime `RefCell`, and the borrow is live across the
   whole GIL-released call — so a second thread entering any method during that window
   gets `PyBorrowMutError`, an error about pyo3's internals raised for an ordinary
   concurrent read. `RwLock` states the real thing instead: ordinary calls take a read
   lock and run concurrently, `close()` takes the write lock and waits for them.
3. **The lock is acquired *inside* the GIL-released closure.** `close()` blocking on
   `inner.write()` while holding the GIL, against a reader holding the read lock inside
   `detach`, deadlocks: the reader needs the GIL back to finish and the closer will not
   yield it.

**`fork()` is made loud rather than made to work.** A `OnceLock<Runtime>` is not
fork-safe: on Linux `multiprocessing` still defaults to `fork`, and a child inherits the
runtime as a struct whose worker threads did not come with it, so the first call there
waits forever. `__init__.py` registers an `os.register_at_fork` handler that poisons the
runtime, converting a silent hang — the worst available outcome — into an exception that
names the cause. The supported answer is the `spawn` start method. *This guard is written
and wired; it has not been exercised on a platform that has `fork`.*

**The context manager is the supported way to hold a handle, and it is not sugar.** The
Rust `Drop` impl notes a missed `close()` at `tracing::warn!`, which is invisible in any
application that has not configured a subscriber — essentially every Python application.
Python's collector is non-deterministic on top of that. So `__exit__` closes, and a handle
that is collected without closing raises `ResourceWarning`, which is what an unclosed file
object raises and what pytest surfaces.

---

<a id="143-errors"></a>
### 14.3 Errors

`DbError` has **27 variants** and every one maps to its own Python class, with its
structured fields set as attributes and `str(e)` still the `#[error]` rendering verbatim
([D-099](s13-decision-register.md#d-099)).

```text
MacrameError
├── EngineError, MigrationError, NotFoundError, DiagnosticConnError, MacrameClosedError
├── IntegrityError    overlaps, drift, rebuild, recorded_at, weights
├── ValidationError   edge types, ids, timestamps, model names, attribute mode
├── VectorError       dimensions, unregistered models
├── TemporalError     replay, snapshots, payload versions, archive
├── WriterError       the write actor
└── BudgetError       subgraph size
```

**Flattening was the risk, and it is the reason this is not one class with a string.**
[§7](s6-s10-flows-to-dependencies.md#7-errors) and
[D-069](s13-decision-register.md#d-069) record several releases spent making these errors
name the right subject — `DiagnosticConn` rather than `NotFound` because a file is not a
node, `InvalidId` rather than `NotFound` because refused is not missing, `InvalidTimestamp`
rather than `ReplayCorrupt` because bad caller input is not a damaged ledger,
`RebuildInterrupted` rather than `RebuildFailed` because *did not run* is not *did not
repair*. Rendering all of that onto `RuntimeError(str(e))` would discard it at the last
possible moment. The six pairs above are pinned as non-inheriting classes, because
collapsing one is a regression no functional test would notice: both sides still raise.

**Completeness is enforced by the compiler.** The mapping is a `match` over `DbError`
with no wildcard arm, so a variant added upstream fails to build `macrame-py` at the line
that needs a decision — before any wheel exists. That is stronger than the
rule-enforcement test this project would otherwise reach for, because a test can only run
after the thing exists and the failure being guarded against is a new variant falling
silently through to the base class, which a wildcard would hide. Verified by injection,
not assumed: a probe variant produced `error[E0004]: non-exhaustive patterns`.

The Python suite checks the half a compiler cannot — that each `setattr` used the right
name, that classes sit under the right bases, that they are reachable from `macrame` — and
closes the seam by parsing `src/error.rs` and comparing it against both the Rust sample
table and its own expectation table.

**`to_py` re-acquires the GIL.** It is called from inside `Python::detach` closures, so
building an exception object has to `attach`. One GIL acquire per raised error, nothing on
the success path.

---

<a id="144-timestamps-and-value-types"></a>
### 14.4 Timestamps and value types

[§4.1](s4-schema.md#41-concepts-and-per-model-embeddings) fixes every temporal column at
`YYYY-MM-DDTHH:MM:SS.ffffffZ` and `normalize` refuses anything else rather than guessing.
That is right, and it is the first thing a Python caller trips over, because they will pass
a `datetime` ([D-096](s13-decision-register.md#d-096)).

**`str` or aware `datetime` in; `datetime` out.** A naive datetime is **refused**, not
assumed to be UTC — [D-029](s13-decision-register.md#d-029)'s rule one layer out, since a
naive value does not say which instant it names and picking one is a wrong answer in a
temporal query later, shifted by an amount nothing records. A bare `date` is refused for
the same reason: which midnight, in which zone, is exactly what it does not answer.

**An open interval crosses as `None`, in both directions.** The sentinel
`9999-12-31T23:59:59.999999Z` is exactly `datetime.max`, so exposing it as a `datetime`
constant was the obvious design and it does not survive measurement:

```text
aware = datetime(9999,12,31,23,59,59,999999, tzinfo=utc)
  aware.astimezone(timezone(timedelta(hours=1)))  -> OverflowError
  aware.astimezone()          # local zone        -> OSError
  aware + timedelta(microseconds=1)               -> OverflowError
```

`astimezone()` raises for every zone east of UTC, and under
[Doctrine II](s0-s3-foundations.md#doctrine-ii) the open interval is *current belief* —
not a rare row, the common one. A landmine in the common path is worse than a less
convenient type. `macrame.OPEN` is the stored string for callers who need to name it. The
cost is stated rather than hidden: sorting a `valid_to` column needs
`key=lambda r: (r.valid_to is None, r.valid_to)`.

**Value types validate in their constructor** ([D-100](s13-decision-register.md#d-100)).
Rust's builders defer to `normalized()` at the point of use, which is right there and
wrong here: a Python caller builds a *list* and hands it to `write_bulk_atomic`, where a
failure reports "invalid edge type" for one of ten thousand edges with no index and a
traceback pointing at the write. In the constructor the traceback points at the line that
has to change — and an `EdgeAssertion` that exists is then one the ledger will accept.

`properties` stays a JSON **string**, matching the column. Accepting a dict would make the
binding decide key order and what happens to a `Decimal` for data the ledger never reads.

---

<a id="145-embeddings-and-abi3"></a>
### 14.5 Embeddings, and what abi3 costs

The wheel is built `abi3-py310`: one wheel per platform instead of one per Python minor
version, because `libsql-ffi` rebuilds the SQLite amalgamation for every target and the
matrix cost dominates ([D-094](s13-decision-register.md#d-094)).

**That decision was taken on a justification that turned out to be false, and the record
says so.** It was recorded as costing "the limited C API, which these bindings do not
touch". They touch it twice, and compiling found it: `PyDateAccess` / `PyTimeAccess` —
pyo3's `get_year()` / `get_hour()` — and `pyo3::buffer` are both compiled out under
`Py_LIMITED_API`.

- Timestamp fields are read with `getattr`: seven Python lookups instead of seven struct
  reads, on the coercion path only. `isoformat()` is the tempting one-call alternative and
  is a trap — it omits `.000000` when microseconds are zero, so every timestamp landing
  exactly on a second would render non-canonical. A test pins this.
- The buffer protocol is gone, so a numpy `float32` array cannot cross as a memory view.
  Replaced by an explicit packed-`bytes` path (`arr.astype("<f4").tobytes()`) plus
  sequence extraction for everything else.

**abi3 stands, now on measurement.** Coercing a 768-dimension vector: packed bytes
**60.8 µs**, numpy `float32` as a sequence **94.9 µs**, numpy `float64` 114.3 µs, Python
list 73.5 µs. The buffer protocol would have bought ~35 µs per vector — 1.6×, not the
order of magnitude implicitly assumed — against a 4–5× wheel matrix.

**Only `bytes` takes the packed path, and that is a correctness rule rather than
convenience.** An earlier draft accepted anything extracting as `Vec<u8>`, so `bytearray`
and `memoryview` would be fast too. That also swallows a `tuple` of small ints and
reinterprets it as float32 — a silent wrong answer producing a valid embedding of a
quarter the length, which the dimension check would then blame on the model.

---

<a id="146-what-is-not-exposed"></a>
### 14.6 What the binding deliberately does not expose

[§4.7](s4-schema.md#47-what-this-schema-does-not-enforce) invariant 2 lists the holes in
"all writes are serialised through one connection". **The binding adds no fourth hole**,
and that is a design constraint rather than an accident:

| Not exposed | Why |
|---|---|
| `Database::raw()` | `#[doc(hidden)]` since [D-091](s13-decision-register.md#d-091), and invariant 2's named hole. A Python escape hatch into `libsql::Database` would export it to a much larger audience with much less context. |
| `Database::read_conn()` | Hands back a *shared* connection, so a long reporting query would compete with every traversal and fold in the process. `diagnostic_conn()` exists precisely because that need is real and this is the wrong way to serve it. |
| the free `register_model` / `upsert_embedding` | Also `#[doc(hidden)]`, also invariant-2 holes. `Database::register_model` is the exposed path. |
| `open_with_clock` | `FakeClock` is a test seam, and `recorded_at` is the transaction-time axis. Exposing it invites injecting a clock into a production ledger. |

**`diagnostic_conn()` is exposed as queries, never as a connection.** `diagnostic_query()`
and `explain()` each open the file `SQLITE_OPEN_READ_ONLY`, run the statement, and drop
the connection. The capability [T5.1](s13-decision-register.md#d-091) wanted — an
OS-level boundary rather than a reversible `PRAGMA` — is preserved; the object that would
let a caller keep it and do something else with it is not. Opening per call is also the
[R15](s11-s12-milestones-and-risks.md#r15)-safe shape: the fault counts *concurrent*
opens, and 500 sequential opens in one process measured clean.

---

<a id="147-r15-through-the-boundary"></a>
### 14.7 R15 reaches through the binding

There was a plausible argument that the Python boundary would *reduce*
[R15](s11-s12-milestones-and-risks.md#r15) exposure — one shared runtime, entry serialised
by the GIL. **It does not.** `tests_py/probes/r15_concurrent_open.py`: 48 concurrent opens
from 48 Python threads on a barrier faulted **2 in 12 runs**, the same rate as the Rust
control arm of `examples/r15_soak.rs` at the same width.

The reason is §14.2's central feature: `block_on` releases the GIL, so the threads are
genuinely concurrent inside `open`, which is what the fault counts. The boundary is
transparent to R15.

Two things follow, and both are now measured rather than transferred:

- The pytest suite runs **single-process**. `pytest-xdist` opens a database per worker,
  which is this shape.
- Application guidance is unchanged from [D-092](s13-decision-register.md#d-092): a
  bounded set of handles opened once, not one per request.

The reporting hazard carries across intact. The fault kills the process, so a crashed run
comes back with a **smaller** pass count and no failures. Anything gating on either suite
must key on the summary line being present, not on the exit code alone.

---

<a id="148-testing"></a>
### 14.8 Testing topology

**The Python suite tests the binding, not the ledger.** The ledger has 25 Rust test
binaries and 300 tests; re-asserting bitemporal semantics through Python would be a second,
weaker copy free to drift. What is genuinely new at this boundary, and therefore what is
covered:

| | |
|---|---|
| Packaging | the workspace shape, the tarball, module-name ↔ `[lib] name`, the abi3 floor ↔ `requires-python` |
| Lifecycle | open/close round trip, the context manager, use-after-close, the `ResourceWarning`, GIL release under contention |
| Errors | every variant, its class, its base, its attributes — plus the six deliberately separated pairs |
| Coercion | `datetime` ↔ canonical string both ways, the `None` sentinel, naive rejection, embeddings |
| Write path | that values built in Python reach the actor intact, and that the ledger's typed errors arrive populated |

`tests_py/probes/` holds diagnostics that are deliberately **not** tests — R15's reproducer
crashes the interpreter when it succeeds, and a suite that dies is not a suite that
reports. This mirrors `examples/*_diag.rs` on the Rust side.

Two hooks ship *in the released wheel* rather than behind a Cargo feature: the error
sample table and the blocking probe used by the GIL test. A `testing` feature would mean
the wheel that is tested is not the wheel that is published, which is the one property a
packaging test exists to establish. Both are underscore-prefixed and absent from
`__all__`.

---

<a id="149-status"></a>
### 14.9 Status

Sequenced in `docs/Macrame Python Bindings Plan v0.7.0.md`, which carries the delivery
record and the corrections each phase made to it.

| Phase | | |
|---|---|---|
| P0 | workspace, packaging, skeleton | ✅ |
| P1 | runtime boundary, handle lifecycle | ✅ |
| P2 | the exception hierarchy | ✅ |
| P3 | timestamps, value types, embeddings | ✅ |
| P4.1 | the write path | ✅ |
| P4.2–P4.7 | read, temporal, vector, integrity, introspection, analytics | not started |
| P5–P8 | wheels, CI, stubs, docs | not started |

**Not yet settled**, and each changes what gets built:

- The import name is `macrame`, mirroring the crate's `macrame-db`/`macrame` split. Unlike
  Rust's per-build-graph `[lib] name`, `site-packages` is flat, so a distribution also
  installing a top-level `macrame/` would contend. The PyPI package of that name is a dead
  2021 build tool and `pip` warns on file conflicts, so this is a known, non-silent risk.
- `Subgraph` is to cross as an opaque handle rather than a dict
  ([D-097](s13-decision-register.md#d-097)); decided, lands with P4.2.
- `astar`'s heuristic — an arbitrary Python callable means calling *into* Python from
  Rust with the GIL released, which inverts §14.2's arrangement and needs its own design
  pass.

<!--nav-->
← [previous](s13-decision-register.md) · [index](README.md) · [next](appendices.md) →
<!--/nav-->
