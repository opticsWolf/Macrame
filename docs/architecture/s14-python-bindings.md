<a id="1416-parity-with-0130"></a>
### 14.16 Parity with 0.13.0 (W6)

The binding's gaps were never features declined — §14.6 is the list of those, and it is
short and argued. These were omissions, which is a different failure: nothing records
them, so nothing goes red, and the surface diverges one release at a time.

**The three missing constants** (W6.1, 0.12.18). `MAX_ARCHIVE_SESSIONS` and the four
`chunk_rows` ceilings, following `BULK_ATOMIC_WARN_HOLD`'s precedent — a constant the
caller must reason *against* belongs on the caller's side of the boundary. The archive
ceiling is a refusal rather than a clamp ([D-080](s13-decision-register.md#d-080)), so
without it a caller computing a window from a span learns the limit by catching
`ArchiveWindowError`; with it the check is arithmetic done before the call.

**The `chunk_rows` docstrings carry a correction, not just a number.** Since 0.12.0 they
are **ceilings rather than sizes** ([D-143](s13-decision-register.md#d-143),
[D-146](s13-decision-register.md#d-146)): the bulk loops time each chunk and size the next
from its measured hold, so a populated database converges *below* the constant and a
caller dividing a batch by one of them to predict transaction count is reading a 0.11.0
fact. Exposing the number without that sentence would have shipped the stale reading
rather than merely leaving it unavailable — which is why this item is four lines of
registration and a paragraph of prose.

They are flat names — `CHUNK_ROWS_EDGES` and its three siblings — rather than a namespace
object or a dict. Rust's `chunk_rows` module has no Python equivalent that a type checker
reads: a submodule would need its own `.pyi`, and a dict loses `Final[int]` and with it
the only mechanism (`test_stubs.py`) that keeps the surface honest.

**The vector registry became readable** (W6.2, 0.12.19). `registered_models()` and
`declared_dimension()` close a write-without-read asymmetry: Python could create a model's
table and could not enumerate what existed, so *is this model already set up?* was
answerable only by calling `register_model` again and reading whether it raised — a write
issued to ask a question, and one that succeeds silently in the case you were checking for.

Both read the schema rather than a registry ([D-037](s13-decision-register.md#d-037)), so
neither can drift: `sqlite_master` for the set, `PRAGMA table_info` for the width, with
`F32_BLOB(n)` in the column type as the declaration itself. That is why the dimension is
worth exposing separately from the list — membership is a list, and the number a caller
sizes a buffer against is the one storage enforces, not a copy of it.

`declared_dimension` **raises** on an unregistered model rather than returning `None`,
matching the Rust side. The Python-specific reason is sharper than symmetry: the common
use is allocating a buffer of that many floats, and `None` there produces a zero-length
one without an error.

**The transaction-time axis became assertable** (W6.3, 0.12.20). `tests_py` could not
influence `recorded_at`, so every Python assertion about it was *this is a timestamp* and
*this one is after that one* — defect K's shape on the half that never received
[D-062](s13-decision-register.md#d-062)'s fix. `macrame._macrame._FakeClock` and
`Database._open_with_clock` close it on this module's existing terms: underscore-prefixed,
absent from `__all__` and from the stub's public surface, shipped in the wheel because a
`testing` feature would mean the tested wheel is not the published one
([§14.8](s14-python-bindings.md#148-testing-topology)).

**§14.6's entry against `open_with_clock` is qualified rather than reversed**, and the
distinction is what is accepted: a `_FakeClock`, not a `Clock` implementation. A caller
cannot supply their own, so *arbitrary time injected into a production ledger* — the thing
that row objects to — has no expression here. `test_clock.py` asserts the seam stays
private, so a later widening has to argue rather than inherit.

**It is not fully deterministic, and the test suite says so out loud.** On a populated
file `open_tuned` raises the clock to the newest stored `recorded_at` before the actor
starts, because a fake set behind the ledger aborts the first write on
`trg_concepts_monotonic_ra`. A test wanting exact stamps starts from an empty file.

**One defect found by writing the tests, in shared code.** `to_duration` — which
`archive_windowed`'s window also goes through — reported a *`TypeError` naming
`timedelta`* to a caller holding a negative `timedelta`: `Duration` cannot represent one,
so the extraction failed and the fallback then failed to read a `timedelta` as a float.
`timedelta(0)` was the inverse and worse, since `Duration` *can* hold zero: it passed as a
`timedelta` and was refused as the number `0`, one rule with two answers depending on how
the caller typed it. Both now raise `ValueError` about the sign. The existing window test
covered only the numeric spellings, which is why neither showed.

**0.13.0's own additions crossed in the release that made them** (W6.4, 0.12.21).
`analyze()`, `optimize()`, `checkpoint()` with a `CheckpointReport`, and the three tuning
knobs. The argument for doing it here rather than in 0.14.0 is that a gap opened in the
release that created the feature is the one that never becomes a convention — the earlier
constants (§14.16's first item) had been missing for eight releases because nothing made
them anyone's next task.

**`Tuning` does not cross as a type.** Rust needs the struct because a growing set of
options cannot be added to a function signature without breaking callers; Python has
keyword arguments for exactly that, so the knobs arrive as keywords on `open` and there is
one fewer name to import and construct. What does cross is the *shape of the defaults*:
each knob's absent state leaves the mechanism alone, and none of them spells that as
`None`-means-off — [D-155](s13-decision-register.md#d-155)'s lesson, which was learned
twice on the Rust side and is easier to get wrong here, where a keyword's default is
invisible at the call site.

`wal_autocheckpoint` takes `None`, `"disabled"`, or a positive page count, and **refuses
`0`** rather than inheriting SQLite's disable overload
([D-157](s13-decision-register.md#d-157)). A string for the third state rather than a
sentinel integer is the Python spelling of the tri-state enum: it cannot be produced by
arithmetic, which is the failure mode being refused.

**Two of the three knobs are not observable through this binding, and the tests say so
rather than pretending otherwise.** `wal_autocheckpoint` and `writer_cache_size` are
applied to the write connection, which no Python caller can reach; `diagnostic_conn` opens
its own and reports SQLite's defaults regardless. The test that would read `== 64` there
passes for the wrong reason on any release that stops applying the pragma at all, so it
asserts the default it actually sees and names `tests/wal_policy_tests.rs` as the holder
of the positive half. `reader_cache_size` *is* visible, because
[D-159](s13-decision-register.md#d-159) put `cache_size` in the read-only half of
`configure`.

**Verified rather than re-added**: the new `CommandKind` variants and the starvation
counter's getters already crossed in W4.3/W4.4. `KindMetrics.kind` reads
`CommandKind::as_str()` through the crate, so a new variant arrives with its own string
without a binding change — which is a property worth an assertion, and
`test_maintenance.py` makes one for `"checkpoint"`.

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

`DbError` has **33 variants** — the count was published as 24, then 27, and
[D-207](s13-decision-register.md#d-207) is where it was finally read off the enum — and every one maps to its own Python class, with its
structured fields set as attributes and `str(e)` still the `#[error]` rendering verbatim
([D-099](s13-decision-register.md#d-099)).

```text
MacrameError
├── EngineError, MigrationError, NotFoundError, DiagnosticConnError, MacrameClosedError
├── IntegrityError    overlaps, drift, rebuild, recorded_at, weights, leaked archive session
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
possible moment. The eight pairs above are pinned as non-inheriting classes, because
collapsing one is a regression no functional test would notice: both sides still raise.


**`SnapshotCorruptError` is the third name in a family that had two (0.13.12, W8.2, [D-185](s13-decision-register.md#d-185)).** `SnapshotIncompatibleError` means another build wrote the file, `ReplayCorruptError` means the ledger itself is damaged, and the new one means the *cache* is damaged and the ledger is not — three subjects, three responses, and the middle one is the worst thing this library can say about a database. Until v3 every failure of `load_snapshot` arrived as `ReplayCorruptError` with `seq = 0`, so a Python caller writing `except ReplayCorruptError` to catch a damaged ledger was also catching a snapshot file that could simply be deleted. Nothing on the ordinary path raises the new class — a damaged snapshot is skipped by the scan and the fold runs from the log — which is exactly why it needed the separate name: the case where a caller *does* see it is the case where they were about to act on the wrong diagnosis.


**Completeness was enforced by the compiler until 0.13.33, and is enforced by a test from
0.13.34 ([D-207](s13-decision-register.md#d-207)).** The mapping was a `match` over
`DbError` with no wildcard arm, so a variant added upstream failed to build `macrame-py`
at the line that needed a decision — before any wheel existed. That was the stronger
mechanism, and it was traded away on purpose: `DbError` is `#[non_exhaustive]` from
0.13.34, because a crate that will certainly add error variants after 1.0 cannot have
each addition be a major version, and the price of `#[non_exhaustive]` is that the
wildcard arm becomes mandatory.

`tests/binding_parity_tests.rs` is the replacement, and it lives in the crate that
*defines* `DbError` rather than in the binding — so it fails for the person adding the
variant, in the command they were already running, with no wheel built. It is weaker in
two stated ways (it runs rather than compiles, and it reads text, so an unreachable arm
would satisfy it) and wider in one: the compiler checked only that a variant had an arm,
while this also pins that every variant is sampled and that no two share a class. Six
tests, each verified by mutation rather than assumed — dropping an arm, pointing two
variants at one class, adding a variant upstream, and deleting one direction of a domain
enum's conversion each produce the failure they are supposed to.

The Python suite checks the half neither can — that each `setattr` used the right
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

**Absent `content` crosses as `None` too, and for the same reason** (0.8.0,
[D-123](s13-decision-register.md#d-123)). `NodeData.content` is `str | None`: since
[D-116](s13-decision-register.md#d-116) a `Subgraph` does not carry document text unless
`load_subgraph(..., content=True)` asks for it, and `""` cannot be the marker for *not
loaded* because it is a valid value of the type — a concept whose text really is empty and
one whose text was not fetched would be indistinguishable exactly when a caller is deciding
whether to go back to the database. Same refusal, same shape, one layer along from the
interval sentinel below.

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
| `Database::shadow_step` | A judgement rather than an invariant ([D-165](s13-decision-register.md#d-165)). It is public and safe in Rust; what does not cross is its *obligation* — the `epoch` from `ShadowOutcome::Started` must return to `ShadowStep::Swap`, and losing it swaps a stale projection over a live one, silently, because the swap succeeds. Two types the Rust caller cannot fabricate would become two `#[pyclass]`es whose only job is to be handed back correctly: the obligation restated as a convention instead of enforced. `rebuild_current_chunked` is the loop, is exposed, and cannot get the epoch wrong. |
| `open_with_clock` | `FakeClock` is a test seam, and `recorded_at` is the transaction-time axis. Exposing it invites injecting a clock into a production ledger. **Qualified in 0.12.20** ([D-163](s13-decision-register.md#d-163)): the *supported* surface still has no clock parameter, and `Database._open_with_clock` takes a `_FakeClock` rather than a `Clock`, so what this row objects to — arbitrary time injected into a real ledger — is still not reachable. |

**`diagnostic_conn()` is exposed as queries, never as a connection.** `diagnostic_query()`
and `explain()` each open the file `SQLITE_OPEN_READ_ONLY`, run the statement, and drop
the connection. The capability [T5.1](s13-decision-register.md#d-091) wanted — an
OS-level boundary rather than a reversible `PRAGMA` — is preserved; the object that would
let a caller keep it and do something else with it is not.

**Opening per call was called "the R15-safe shape" here, and that was half right**
([D-138](s13-decision-register.md#d-138), 0.10.0). Sequential opens are safe — 500 in one
process measured clean, and that is what the sentence was reasoning from. But the fault
counts *concurrent* opens, and per-call opening is what puts them on this path: `block_on`
releases the GIL, so *N* threads inside `diagnostic_query` are *N* concurrent opens,
reached by sharing one handle rather than by opening many. Measured at width 48: **7 bad
runs in 18**, two of them `0xC0000005` and five *returned* SQLite errors. These two calls
now take a mutex that bounds the path to one outstanding open — the only serialised
methods on the surface — and `tests_py/probes/r15_diagnostic_path.py` is the measurement.

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

**A third thing follows, and 0.7.0 did not see it.** "Handles opened once" is necessary
and was assumed sufficient, because `open` looked like the only place the file gets
opened. It is not: `diagnostic_conn()` opens per call, so a *correctly* held single handle
shared across threads still reaches R15 through `diagnostic_query` and `explain`. That
path is now serialised in the binding ([D-138](s13-decision-register.md#d-138)); the Rust
`Database::diagnostic_conn` is documented rather than locked, and its rustdoc carries the
same numbers.

**The reporting hazard does *not* carry across intact, and P6 measured the difference.**
This section said it did, on the strength of the mechanism being the same fault. The
mechanism is; the reporting is not, because the two suites have different process
topologies.

`cargo test` runs **one process per test binary**. A crash removes that binary's tests
from the aggregate while every other binary still prints its summary, so the run reads as
a *smaller pass count with no failures* — green, and wrong. That is the hazard as
originally stated, and it is real for the Rust side.

pytest runs **one process**. When it dies, everything dies. Measured with a deliberate
uncatchable fault mid-suite: **exit code 3, and no summary line at all**. So for pytest an
exit-code check would in fact catch a mid-run crash.

What it would not catch is the inverse, which is specific to this extension. `Drop for
PyDatabase` enters the tokio runtime, so a fault during interpreter teardown lands
*after* pytest has printed `325 passed` — a **green summary with a non-zero exit**. There,
reading only the exit code is right by accident and reading only the summary is wrong.

`tests_py/run_suite.py` is the gate that follows from this rather than from the assumption:
it requires the summary line to be present, the failure count to be zero, the collected
count to match, and the exit code to agree with all three — and it distinguishes the four
outcomes by name, retrying only the crash. Verified by injection in both directions: an
injected `faulthandler._sigsegv()` retries three times and reports `CRASH`; an injected
failing assertion reports `FAILED` on the first attempt and does not retry, because
re-running until green is how a flaky assertion becomes permanent.

---

<a id="148-testing"></a>
### 14.8 Testing topology

**The Python suite tests the binding, not the ledger.** The ledger has **27 Rust test
binaries and 330 tests** by default, 339 with `metrics`, and **362 with `--all-features`**
(measured 2026-08-07) — the difference being one `metrics` binary and the **three**
quarantined property binaries of [R15](s11-s12-milestones-and-risks.md#12-risks),
which carry 7 + 9 + 7 = 23 tests between them and are run as their own step. Re-asserting bitemporal semantics through Python would be a
second, weaker copy free to drift. What is genuinely new at this boundary, and therefore what is
covered:

| | |
|---|---|
| Packaging | the workspace shape, the tarball, module-name ↔ `[lib] name`, the abi3 floor ↔ `requires-python`, and the four CI invariants of [§14.14](s14-python-bindings.md#1414-ci) |
| Lifecycle | open/close round trip, the context manager, use-after-close, the `ResourceWarning`, GIL release under contention |
| Errors | every variant, its class, its base, its attributes — plus the six deliberately separated pairs |
| Coercion | `datetime` ↔ canonical string both ways, the `None` sentinel, naive rejection, embeddings |
| Write path | that values built in Python reach the actor intact, and that the ledger's typed errors arrive populated |
| Read path | the `as_of` / `attribute_mode` pairing, the `OMIT` refusal, the `min_weight` default, and that a `Subgraph` answers without being copied |
| Temporal | that a window is refused rather than clamped, and that `ChainCheck` surfaces `diverged()` rather than two comparable anchors |
| Vector | that a model name is refused where it is named, that both embedding forms agree, and that each search's scores arrive sorted in *its own* direction |
| Analytics | `astar`'s callback: a raising heuristic, a `NaN`, a non-number — none of which the callback signature can report |
| Metrics | that the counters are **real** in the wheel, which is the whole of D-093 |
| End to end | the *seams*: a `Subgraph` outliving an archive, search agreeing with the traversal it filtered by, counters covering a whole session, a reopened ledger answering the same questions |

`tests_py/probes/` holds diagnostics that are deliberately **not** tests — R15's reproducers
crash the interpreter when they succeed, and a suite that dies is not a suite that
reports. This mirrors `examples/*_diag.rs` on the Rust side. Two live there:
`r15_concurrent_open.py` (many opens) and `r15_diagnostic_path.py` (one handle, many
diagnostic calls), the second added in 0.10.0 with a before/after arm.

**Run `python tests_py/run_suite.py`, not bare pytest, wherever the answer gates something**
([D-107](s13-decision-register.md#d-107)). It is not a wrapper for tidiness: the four ways
this suite can fail are distinguishable and only one of them is worth retrying, and neither
the summary line nor the exit code is sufficient alone — see [§14.7](s14-python-bindings.md#147-r15-reaches-through-the-binding)
for which is wrong in which direction.

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
| P4.2 | traversals, `load_subgraph`, the `Subgraph` handle | ✅ |
| P4.3 | reconstruct, archive, the chain check | ✅ |
| P4.4 | embeddings, vector/keyword/hybrid/filtered search | ✅ |
| P4.5 | audit and the two rebuilds | ✅ |
| P4.6 | `metrics()` and the handle's introspection | ✅ |
| P4.7 | six algorithms on `Subgraph`, `astar` included | ✅ |
| P5 | wheels, sdist, the naming resolution | ✅ |
| P6 | the suite, its gate, and the end-to-end seams | ✅ |
| P7 | CI: the binding crate compiled, the suite gated, abi3 tested | ✅ |
| P8 | the stub, `py.typed`, and the docstrings that carry reasons | ✅ |
| B7 (0.8.0) | the surface tracks the crate's 0.8.0 changes: `content=` on `load_subgraph`, `NodeData.content` as `str \| None`, `MaterializedState.predates_recorded_history`, version to 0.8.0 | ✅ |
| C5 (0.9.0) | archival that concepts participate in: `ArchiveReport.concepts_archived`, `db.rehydrate(ids)` and `RehydrateReport`, the round-trip equality asserted from Python, version to 0.9.0 | ✅ |
| W (0.10.0) | `ArchiveSessionLeakedError` under `IntegrityError`; the diagnostic path serialised behind a mutex; version to 0.10.0 | ✅ |

**0.8.0 crossed less than expected, and the reason is a decision paying off.** [B2](../Macrame%20Implementation%20Plan%20v0.8.0-v0.9.0.md)'s interning of `EdgeRef` changed the crate's representation and **nothing** in `#[pymethods]`, because [D-101](s13-decision-register.md#d-101) had already made `Subgraph` an opaque handle rather than a converted dict — the copy that would have had to track the layout is one the binding declines to make. [B4](../Macrame%20Implementation%20Plan%20v0.8.0-v0.9.0.md)'s schema v8 is likewise invisible at the surface, but the version bump is not cosmetic: a wheel built against v7 opens a v8 database and refuses it by design, which is the whole point of `user_version`.

**0.9.0 crossed a whole subsystem and cost the error tree nothing** ([D-133](s13-decision-register.md#d-133), C5). Concept archival, rehydration and two schema rungs added **no** `DbError` variant, so `errors.rs`'s exhaustive `match` — the mechanism [D-099](s13-decision-register.md#d-099) put there precisely so a new variant cannot reach a wheel unmapped — had nothing to catch. That is the design working rather than the step being skipped: the operations are new, the *ways they fail* are the ones already named. `rehydrate` raises `ArchiveViolationError` when a guard refuses a delete inside a session and `MacrameClosedError` on a closed handle, both from the same classifier every other write goes through.

**0.10.0 is the first release in three where the error tree did move, and the mechanism fired exactly as specified** ([D-135](s13-decision-register.md#d-135), [D-138](s13-decision-register.md#d-138)). `DbError::ArchiveSessionLeaked` made `errors.rs`'s wildcard-free `match` a **compile error** — [D-099](s13-decision-register.md#d-099)'s whole purpose, and the paragraph above says 0.9.0 gave it nothing to catch. It surfaces as `ArchiveSessionLeakedError` under `IntegrityError` rather than beside `ArchiveViolationError` under `TemporalError`: the latter is a guard *refusing* a delete, which is the invariants holding; this is the invariants being unenforced.

**The other crossing is a behaviour change with no surface change, which is the harder kind to notice.** `diagnostic_query` and `explain` now take a mutex, so calls on that path serialise across threads while the typed surface stays concurrent. Nothing in the stub moves and nothing is renamed — but `diagnostic_conn()` opens the file per call, and `block_on` releases the GIL, so threads sharing **one** handle were reaching R15 without ever calling `open` twice. Measured at width 48: 7 bad runs in 18 before, 0 in 18 after. §14.6's claim that per-call opening is "the R15-safe shape" was reasoning from sequential opens and is corrected there. The Rust `Database::diagnostic_conn` is documented rather than locked, because its rustdoc promises the connection is the caller's own.

**What the boundary could not be made to assert, and why it is the crate's job.** C3's exit gate is two tests: `reconstruct` bit-identical, and the concept usable in the *live* tables. Only the first crosses cleanly. The obvious live-side readers are unavailable here — `keyword_search` and `load_subgraph` both filter `retired = 0`, and an archivable concept is retired by definition ([D-128](s13-decision-register.md#d-128)), so neither can see the concept on either side of the round trip. Python has no raw SQL by design ([§14.6](s14-python-bindings.md#146-what-the-binding-deliberately-does-not-expose)), so the FTS-index half of that gate is **not assertable across this boundary at all** and the Rust suite keeps it. What the Python test asserts instead is stronger than a consolation: archiving the same concept a second time succeeds only if the row is back in the hot table *with the column values the predicate reads*, and a second rehydration moving nothing is what says it is hot rather than still cold.

**Not yet settled**, and each changes what gets built:

- The import name is `macrame`, mirroring the crate's `macrame-db`/`macrame` split. Unlike
  Rust's per-build-graph `[lib] name`, `site-packages` is flat, so a distribution also
  installing a top-level `macrame/` would contend. The PyPI package of that name is a dead
  2021 build tool and `pip` warns on file conflicts, so this is a known, non-silent risk.
- ~~`Subgraph` as an opaque handle~~ — delivered in P4.2
  ([D-101](s13-decision-register.md#d-101)).
- ~~`astar`'s heuristic~~ — resolved in P4.7 by not releasing the GIL for that one method
  ([D-104](s13-decision-register.md#d-104)). The proposed fallback, a fixed heuristic set
  instead of a callable, was not needed.
- ~~**A rough edge in the crate, found through the binding**~~ — **closed in 0.8.0**
  ([D-121](s13-decision-register.md#d-121)). `reconstruct` at an instant older than
  anything on hand sent the fold to cold storage, so on a ledger that had never been
  archived it raised `ReplayCorrupt` naming an archive file the caller never made. The
  objection recorded here against smoothing it over in the binding — *translating it into
  an empty state would mean claiming a real missing archive is also nothing to worry
  about* — was correct, and it was a reason to answer the question rather than to keep the
  behaviour. `transaction_log.seq_id` is `AUTOINCREMENT` and only an archive session may
  delete from the table, so a log whose ids are exactly `1..MAX` has provably never been
  archived from. *Before recorded history* and *the cold file is gone* are now different
  states, and only the second raises. `MaterializedState` gains
  `predates_recorded_history` so an empty answer says which kind of empty it is.

---

<a id="1410-read-path"></a>
### 14.10 The read path, and the `Subgraph` handle

A traversal is a **call**, not an object. [§14.4](s14-python-bindings.md#144-timestamps-and-value-types) settled that these bindings ship no chained setters, and `TraversalBuilder` is the type that argument applies to most directly: `db.traverse(start, max_depth=3, edge_types=["CITES"])` assembles it inside the method. The cost is that a Rust caller can reuse a builder across ten instants and a Python caller passes the arguments ten times; the alternative is exporting a mutable builder across the GIL boundary, which is the shape [§14.2](s14-python-bindings.md#142-the-asyncsync-boundary) rejected for `Database` for the same reason.

Three methods, and the split between them is the whole design:

| | |
|---|---|
| `traverse_ids(start, …)` | node ids. Cannot raise `AttributeModeUnstated` — topology at an instant is unambiguous |
| `traverse(start, …)` | ids **and** hydrated text, under a stated `attribute_mode` |
| `load_subgraph(start, max_hops, byte_budget, …, content=False)` | the neighbourhood as a `Subgraph`, bounded. `content=True` fetches document text; without it `NodeData.content` is `None` ([D-116](s13-decision-register.md#d-116), [D-123](s13-decision-register.md#d-123)) |

**The four chunked writes take `progress=` and `cancel=` as of 0.13.8 ([D-181](s13-decision-register.md#d-181)).** `bulk_import`, `write_concepts`, `upsert_embeddings` and `write_analytics_annotations` are keyword-only on both, and every exception they raise now carries `written` — the rows the chunks before the stop committed, which are still committed. The exception *class* is unchanged and still chosen by the cause, so `except NotFoundError` catches the same thing it always did.

Two facts here are the binding's rather than the ledger's. First, `cancel` has to be a `CancelToken` and not a `bool` a caller flips, because these calls run with the GIL released for their whole duration: the thread that called one cannot reach in, so the cancelling thread is by construction a different one, and the flag it sets has to be safe to set from there. Second, `progress` runs on the ledger's thread with the GIL re-acquired — one acquire per chunk boundary, with the import stalled for as long as the callable runs — so the docstrings say to update a counter rather than write a file. A callback that raises cancels the write and propagates in place of the `BulkCancelledError` it caused; swallowing it would turn a broken progress bar into a silently truncated import.

**`AttributeModeUnstatedError` carries `as_of_valid` and `as_of_recorded` as of 0.13.10 ([D-183](s13-decision-register.md#d-183)).** It carried `as_of` — the keyword the binding stopped accepting in 0.13.2, so a caller who passed `as_of_recorded=` got their instant back under a name they could not have typed. Both attributes are always present and the axis that was not stated is `None`, matching how the two keywords read on the call that raised it. The message names them too, so a caller who wanted the other axis can read what to pass from the error they are holding.

**Every Macrame exception carries `written` as of 0.13.9 ([D-182](s13-decision-register.md#d-182)).** `int | None`: the number of rows committed before the failure on the four chunked paths, and `None` everywhere else — reads, single writes, `write_bulk_atomic`, a closed handle — meaning *partial application is not a concept here*. `e.written is not None` is the test for "did this leave the database partially written", and `0` is reserved for its real meaning on a chunked path, which is that the first chunk failed. Uniform because the Rust surface carries this distinction in its return types and Python has none: without it, `except MacrameError as e: log(e.written)` raises an `AttributeError` from inside the handler for every non-chunked failure. Set at the two construction sites in `errors.rs` rather than per-arm, so it cannot be forgotten by a variant added later.

**`OverlappingIntervalError` carries `within_batch` as of 0.13.7 ([D-180](s13-decision-register.md#d-180)).** `False` is the ordinary case and the other interval is a committed row a caller can query for. `True` means `write_bulk_atomic` was handed a batch contradicting itself, refused before the transaction opened — the interval named is another edge in the same call, and querying for it finds nothing. The message says the same thing in words; the attribute exists so a caller can branch on it without parsing them.

**`Database.open` takes `future_stamps` as of 0.13.5 ([D-178](s13-decision-register.md#d-178)).** `None` refuses a stored `recorded_at` more than a day ahead of the wall clock, a number is a tolerance in seconds, `"allow"` waives it. Same three-state shape as `wal_autocheckpoint` and for the same reason — the absent state leaves the guard *on*. The refusal arrives as `FutureRecordedAtError` under `IntegrityError`, carrying `stamp` and `limit`. Its message names the knob rather than the Rust spelling of it, because the message crosses verbatim and a Python caller cannot write a `Tuning` literal; `future_stamps` and `allow` are the two words that mean the same thing on both surfaces.

**All three take `as_of_valid` and `as_of_recorded` as of 0.13.2, and `load_subgraph` took neither before ([D-175](s13-decision-register.md#d-175)).** The keyword `as_of` is gone from the binding surface along with the Rust method: it named the valid-time axis for topology and the transaction-time axis for attributes, so one keyword asked two questions. `as_of_valid` is *what was true*, `as_of_recorded` is *what we believed*, both together is the bitemporal cell, and `AttributeModeUnstatedError` now fires on either rather than on `as_of` alone. `load_subgraph` gaining them in the same release is W6's finding applied: a binding gap opened in the release that created the feature never becomes a convention — and here the gap was worse than absence, because the Rust loader accepted an instant on the builder and discarded it.

**`byte_budget` has no default because the crate has none.** How much memory a process may spend materialising one graph is not a question a binding can answer, and `SubgraphTooLarge` is the refusal it produces.

**`Subgraph` is opaque** ([D-101](s13-decision-register.md#d-101)). The accessors are forwarded and `to_dict()` is the caller's explicit purchase of a copy — because converting on return would double the peak memory of the one operation that already has a budget, eagerly, whether or not the caller reads more than `degree()`. It is a *value*, not a cursor: it outlives the handle that loaded it, and the algorithms of [§14.12](s14-python-bindings.md#1412-the-algorithms-and-astars-inversion) run on it after `close()`.

Two boundary decisions that are not merely translation:

- **`traverse(attribute_mode=OMIT)` is refused** ([D-102](s13-decision-register.md#d-102)), naming `traverse_ids`. It is the single place the binding narrows the library, and the reason is that `execute` under `Omit` returns an empty list its own rustdoc calls indistinguishable from a traversal that reached nothing.
- **An unstated `min_weight` is `-inf`** ([D-103](s13-decision-register.md#d-103)), not the builder's `0.0`, so a negative weight reaches `NegativeEdgeWeight` rather than being silently dropped. A *stated* floor filters, because stating one is asking to exclude.

The pairing rule survives intact and is visible from Python in one test: at an instant before anything was recorded, `traverse_ids(as_of=t)` finds the topology, `AttributeMode.CURRENT` hydrates it, and `AttributeMode.AT_TIME` returns nothing — because on that date nobody had written it down. Two calls differing by one keyword, disagreeing completely, which is the concrete case [D-085](s13-decision-register.md#d-085) refuses to default.

---

<a id="1411-temporal-vector-counters"></a>
### 14.11 Temporal, vector, and the counters

**Shape follows meaning, not consistency for its own sake** ([D-105](s13-decision-register.md#d-105)). Edges stay `(source, target, edge_type, valid_from, valid_to)` tuples, with the two timestamps rendered as aware `datetime`s and the open sentinel as `None` — [§14.4](s14-python-bindings.md#144-timestamps-and-value-types)'s rule is about the boundary, not about which container a value arrives in. `ArchiveReport`, `ChainCheck`, `CostEstimate`, `RebuildReport`, `MetricsSnapshot` and `VectorHit` are classes, because each carries either more fields than a reader can index safely or a *relationship between fields* that a position cannot state.

`ChainCheck` is the clearest case. `composed_anchor` and `folded_anchor` **may legitimately differ and must never be compared** — the composed answer anchors at the snapshot it started from plus its delta, the fold at the newest row it saw — so an equality check reports divergence that is not there, which is worse than no check because it is a check. `diverged()` is the method, the anchors are diagnostic, and a tuple would put them at index 1 and 2 next to each other.

**`RehydrateReport` is a class with two fields, and the rule above does not reach it** (0.9.0, C5). By arity it should be a tuple. It is not, because it is the **counterpart of `ArchiveReport`** — the pair is the unit a caller thinks in, so a caller who reads `report.concepts_archived` going out and `report[0]` coming back has to learn which direction returns which shape — and because `rowids_reassigned` is exactly the field a position gets wrong: two `int`s where one is *work done* and the other is *something unusual happened* invites reading `[0]` and meaning `[1]`. The arity rule is a proxy for *is there anything here to get wrong*; here there is, at arity two.

`db.rehydrate(ids)` is a **write**, so it queues through the actor and its latency contract is stated rather than implied ([§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028)): the wait is a channel wait in Rust that `busy_timeout` does not bound. Ids absent from the cold file are **skipped, not refused** — the caller's list generally comes from an earlier cold-side query, so partial staleness is the normal case rather than an error, and `concepts_rehydrated` is how many actually moved. It takes one transaction with no window boundaries and gains no `rehydrate_windowed` twin, which is the one place archival and its inverse deliberately differ in shape ([D-132](s13-decision-register.md#d-132)).

`archive_windowed` takes a `timedelta` or a number of seconds, and **refuses rather than clamps**: zero, negative and non-finite windows are rejected at the boundary. Passed through, a zero window reaches `ArchiveWindow` with a message about session counts — a true statement about the wrong problem.

On the vector side, a model name is the one string in this crate that cannot be bound as a parameter, because `ModelName::table()` builds an SQL *identifier*. So every method takes a `str` and constructs the `ModelName` at the boundary: an invalid name is `InvalidModelNameError` from the call that used it, not a syntax error from underneath. Embeddings accept either coercion form from [§14.5](s14-python-bindings.md#145-embeddings-and-what-abi3-costs); `search_filtered` returns `(hits, plan)` because [D-007](s13-decision-register.md#d-007)'s requirement is empirical tuning, and that needs the estimate next to the outcome rather than in a log.

**The counters are on in the wheel** ([D-093](s13-decision-register.md#d-093)), and that is a different decision for a different audience rather than a contradiction of [§5.10](s5-modules.md#510-metricsrs--what-the-actor-holds-the-lock-for)'s default. A Rust caller who wants them adds a feature flag to a `Cargo.toml` they own. A Python caller cannot rebuild the extension, so shipping the feature off would mean shipping `metrics()` as a method that exists and always answers zero — or not shipping it, leaving `chunk_budget_ms()` a number in the docs with no way to check it *in situ*, which is what T1.4 existed to fix. `MetricsSnapshot.violations()` comes first because it is the question; the per-kind histogram is the evidence.

---

<a id="1412-algorithms"></a>
**Two edge shapes from 0.14.5, and the split is resolved against unresolved ([D-222](s13-decision-register.md#d-222)).** `query_as_of_edges` answers `(source, target, edge_type, valid_from, valid_to)`; `MaterializedState.edges` answers the same with a sixth field, the lineage holding the belief. The reader that *resolved* answers for one lineage — the caller's, or the trunk when they named none — so a label would repeat what the caller already said. The reader that *folds the whole ledger* is answering a question a forked ledger answers with two beliefs about one edge key, and without the label those are two indistinguishable rows. The stub carries both as named aliases, `Edge` and `EdgeBelief`, so the difference is visible at a type checker rather than only at a length.

**Six fields, against the rule above that more than five should be a class.** The rule is a proxy for *is there anything here to get wrong*, and here there is not: `branch` is a `str` appended after a `datetime | None`, so a misindex fails on type rather than returning a plausible wrong value, and it carries no relationship to another field and no derived question. What six does cost is that a seventh field breaks unpacking again — which `#[non_exhaustive]` spares the Rust `EdgeBelief` and Python has no counterpart for. Recorded as the price of the shape rather than argued away: a seventh field is when this becomes a class, and that is a trigger rather than a preference.

### 14.12 The algorithms, and `astar`'s inversion

`dijkstra`, `scc`, `k_core`, `modularity` and `louvain` are methods on `Subgraph` rather than free functions, because `g.louvain()` is where a Python caller looks and there is no second kind of graph they could apply to. **All of them release the GIL**: pure CPU over Rust-owned data with no Python object in reach, and `louvain` on a budget-sized graph is long enough that holding it would stall every other thread for no reason.

`astar` is the exception, and it was the plan's one flagged unknown ([D-104](s13-decision-register.md#d-104)). An arbitrary Python heuristic means calling **into** Python from Rust, inverting [§14.2](s14-python-bindings.md#142-the-asyncsync-boundary)'s arrangement. The resolution is that it **does not release the GIL at all** — re-attaching per expansion would pay two GIL transitions per node to hold it for the arithmetic in between, which is strictly worse than never releasing it. The cost is isolated to the one method that earns it, and `heuristic=None` takes the detaching path.

The plan's fallback — a fixed heuristic set instead of a callable — was not needed. What was needed is two guards, because `Fn(&str, &str) -> f64` cannot report failure: a raising heuristic is captured and re-raised after the search, and a `NaN` is refused by name rather than reaching a priority queue whose comparison would then panic inside a callback across an FFI boundary. `0.0` stands in meanwhile, which is admissible on every graph, so the search stays well-defined rather than running on a poisoned ordering.

`modularity` is exposed separately from `louvain` on purpose, and the reason is the same one it exists for in Rust: a detector returning one node per community satisfies "modularity did not decrease from the singleton partition" *by being* that partition, and measuring Q is what tells the two apart.

---

<a id="1413-packaging"></a>
### 14.13 Packaging and distribution

Four wheels and an sdist, built by `.github/workflows/wheels.yml` ([D-106](s13-decision-register.md#d-106)):

| | | |
|---|---|---|
| `manylinux_2_28` x86_64 | built and smoke-tested on the runner | |
| `manylinux_2_28` aarch64 | cross-built under emulation | **not** smoke-tested — an x86_64 runner cannot import it |
| macOS `universal2` | built and smoke-tested | |
| Windows x86_64 | built and smoke-tested | |
| sdist | built, then installed `--no-binary :all:` | the fallback for everything above's gaps |

**One wheel per platform, not per interpreter**, because [D-094](s13-decision-register.md#d-094) chose abi3. That decision's cost was measured in µs per embedding; this is where it is repaid — the alternative is a 4 × 5 matrix of a crate that rebuilds the SQLite amalgamation on every cell.

**What the smoke test asserts is the failure this file exists to prevent**: not that the wheel imports, but that it has an *engine* in it and the counters are *real*. `engine_linked()` is P0's tautology-resistant link check, kept precisely for this; `metrics().turns > 0` is [D-093](s13-decision-register.md#d-093) — a wheel built without `--features metrics` still imports, still answers `metrics()`, and answers zero forever.

**Measured, probe P5-a** (2026-08-01, Windows x86_64 native, `cargo clean` first):

| | |
|---|---|
| cold build | 54–62 s, 197 crates, `libsql-ffi` included |
| wheel | 4.3 MiB compressed · 11.0 MiB unpacked · 10.7 MiB of that the extension |
| sdist | 748 KiB, 124 members |
| `pip install --no-binary :all:` | 183 s, fresh virtualenv |

The build is cheap. **Only the native target is measured**, and the plan's named risk — aarch64 under QEMU, typically 5–15× — is what the first CI run answers.

**Uploads carry no token.** Trusted Publishing mints a short-lived OIDC credential for this repository and this workflow; there is no long-lived secret in the repo and none for anyone to handle. It is configured once on PyPI by the project owner. A packaging test asserts `wheels.yml` names no secret, because pasting a token in is the obvious fix for a failed upload and it reviews as a small change.

**The public surface is now pinned in both directions.** P4 added twelve classes and four constants across five Rust modules, each registered in `lib.rs` and each needing a *second*, hand-written entry in `python/macrame/__init__.py`. A missed one is invisible rather than broken: importable only as `macrame._macrame.Thing`, absent from `dir()`, from `import *`, and from the stub ([§14.15](s14-python-bindings.md#1415-the-stub-and-what-keeps-it-true)). `test_packaging.py` compares the extension's exports against `__all__` both ways, and checks every public class claims `module = "macrame"` rather than the private extension. Verified by injection — dropping `Subgraph` from `__all__` fails the test naming it.

### 14.14 CI

Three workflows, and the division between them is by *question*, not by language.

| | asks | runs on |
|---|---|---|
| `ci.yml` | is the crate correct | push, PR, and `release.yml` |
| `python.yml` | is the binding correct, on every platform and interpreter claimed | push, PR, and `wheels.yml` |
| `wheels.yml` | does it *package*, on four targets | tags, and by hand |

**`python.yml` does not gate on `ci.yml`** ([D-108](s13-decision-register.md#d-108)). The plan sketched it that way, from a draft where this file also built the wheels; P5 moved those out, and `workflow_call` does not deduplicate — gating would re-run the whole Rust matrix, R15 retries included, on every pull request that already ran it, and would report the Python answer late for no gain. The place the two must be green *together* is before an upload, so that is where the gate is: `wheels.yml`'s publish job calls `python.yml`, as `release.yml` calls `ci.yml`. Until P7 a tag could build four wheels, pass a six-line smoke test, and upload to PyPI with the 344-test suite never having run.

**Until this file existed, nothing in CI compiled the binding crate.** `bindings/python` is a workspace member but never a *default* member, because the root package is itself a member — deliberate, and the reason `cargo publish` is still a one-package operation ([D-098](s13-decision-register.md#d-098)). The consequence went unnoticed for three phases: `ci.yml`'s clippy is scoped to `macrame-db` by that same default, and the only thing that built `macrame-py` was `wheels.yml`, on tags. A pull request could break every file P1–P4 wrote and all four checks would go green. `cargo clippy -p macrame-py --all-targets` under `-D warnings` is now a job, and it passes today rather than arriving red.

What the matrix covers, and why each row is there:

| | |
|---|---|
| ubuntu · 3.13 | the baseline |
| ubuntu · **3.10** | the floor `pyproject.toml` declares. pip enforces `requires-python` against users and nothing enforced it against us |
| windows · 3.13 | where R15 was measured |
| **macOS** · 3.13 | never run before. `wheels.yml` builds a universal2 wheel and smoke-tests it in six lines; this is the first time the Python surface is exercised on Apple silicon |

Installation is `pip install .`, not `maturin develop`: it goes through the PEP 517 backend, so it reads `[tool.maturin]`, builds with `--features extension-module` and in release — the path a user actually takes. The suite then runs through **`python tests_py/run_suite.py`**, never bare pytest ([D-107](s13-decision-register.md#d-107)).

The `abi3` job is the only one that tests the claim funding the whole wheel matrix. Every other job builds and runs on one interpreter; this one builds on 3.10, asserts the artifact is tagged for the *ABI* rather than for a version, and runs the entire suite through 3.13 against that same file. Drop `abi3-py310` and every other job stays green while the matrix quietly becomes wrong by a factor of five.

Four packaging tests pin the arrangement, each guarding a failure with no local symptom, and all four verified by injection: that some job names `-p macrame-py`, that the gate is `run_suite.py`, that `publish` still needs the suite, and that the declared Python floor is a version some job runs.

### 14.15 The stub, and what keeps it true

`python/macrame/_macrame.pyi` is hand-written ([D-109](s13-decision-register.md#d-109)). `pyo3-stub-gen` would add a build step and generate `Optional[Any]` where the boundary actually says `str | datetime | None` — and that difference is the whole reason to ship a stub. Four conventions carry most of the value, none of which a generator could infer:

| | |
|---|---|
| a timestamp **in** | `str \| datetime`, aware only — naive is refused at runtime, which no annotation can say, so the stub says it in prose |
| a timestamp **out** | always an aware UTC `datetime` |
| an open interval | `None`, never a sentinel datetime — `OPEN` is the stored string, and `datetime` cannot carry it safely |
| `astar`'s heuristic | `Callable[[str, str], float]`, which pyo3 sees only as a `PyObject` |

**A stub is not executed, so nothing notices when it stops being true.** The defect is narrow: a method added in Rust and never stubbed works perfectly, and surfaces as a type error in somebody else's editor months later. `tests_py/test_stubs.py` therefore parses the `.pyi` and compares it against the live module in both directions, class by class and member by member.

It deliberately compares the *public* surface plus the dunders that change how an object is used — `__len__`, `__iter__`, `__contains__`, `__enter__`, `__exit__`. The six rich-comparison slots, `__int__` on the enums and `__new__` are pyo3's codegen rather than this library's surface; requiring them would make the test a transcript of pyo3 and would go red on an upgrade that changed nothing here.

Exception attributes need a different check, because they do not exist on the class: the mapping layer sets them on the raised instance, so `hasattr(NotFoundError, "id")` is correctly False. They are compared against `errors.rs`, and then a third test pushes every `DbError` variant through `_raise_db_error` and asserts the raised object carries what the stub promised — **stub → source → runtime**, rather than two documents agreeing with each other.

None of that can see a wrong *type*. `mypy --strict` over `python/macrame` runs in CI for exactly that half ([§14.14](s14-python-bindings.md#1414-ci)); a fifth test asserts the step is still there.

**Four docstrings had to survive the crossing in substance, not just in signature**, and they are the ones where the Python surface would otherwise mislead: `close()` (why it is not optional — the final snapshot *and* the write actor's exit status, which no other method can return), `AttributeMode` (that `None` is *unstated*, not `CURRENT`), `write_bulk_atomic` (the hold ceiling, with T1.3's measured numbers, and the pointer to `bulk_import` for callers who want the latency bound instead), and `diagnostic_query` (a boundary — `SQLITE_OPEN_READ_ONLY` on its own connection — not a guardrail, returning values as stored). They live in the Rust source, where they are the same text a `cargo doc` reader sees, and the load-bearing ones are repeated in the stub because editors read stubs first.

### 14.17 `BranchView` is a Python class, and the reason is that the guarantee does not cross (0.14.9, W12.9, [D-226](s13-decision-register.md#d-226))

Fifth holding of [W6's convention](#1416-parity-with-0130-w6) — the binding ships
in the release that creates the feature — and the first that is **not** a pyo3
class. Two facts make that the right shape rather than a shortcut.

`PyDatabase` holds an owned `Database` behind an `RwLock<Option<…>>`, not an
`Arc`, so a pyo3 `BranchView` could not wrap the Rust one. It would be a
**parallel implementation** of the same delegation, and the two would be free to
drift in exactly the way §14's whole apparatus exists to prevent.

More to the point, the property the Rust type is built on is unavailable here.
`Database::close` takes `self` by value and an `Arc` cannot surrender that while
a clone survives, so a Rust view *cannot* end the handle it reads through — the
restriction is structural. Python has no move semantics to build that out of:
`close()` is a method on the `Database` object the caller already holds, and no
wrapper can take it away. **The Python view therefore delivers the ergonomics
and not the guarantee, and its own docstring says so.** Writing it as a Python
class makes each method visibly one line that passes `branch=` through to the
binding, which is the strongest available statement that the two surfaces mean
the same thing — a pyo3 class would have implied a guarantee it could not keep.

That is also why there is no `db.view(...)` in Python. In Rust the method exists
to clone the `Arc`; here there is no `Arc` to clone, so `macrame.BranchView(db,
alt.id)` is the constructor. **Second deliberate asymmetry in the branch
surface**, after [`BranchId` having no Python
class](s13-decision-register.md#d-224), and recorded the same way — a language
difference stated rather than a parity gap.

What *did* have to cross into the extension is `on_branch` on `EdgeAssertion`
and `ConceptUpsert`, which is what the view stamps with. Rebuilding an assertion
from its getters would silently drop any field added later, so the stamp happens
on the Rust value, where a new field comes along for free — the reasoning
[D-225](s13-decision-register.md#d-225) used when it took `#[non_exhaustive]` on
both structs.

<!--nav-->
← [previous](s13-decision-register.md) · [index](README.md) · [next](appendices.md) →
<!--/nav-->
