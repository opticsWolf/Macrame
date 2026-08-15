# STATUS_ACCESS_VIOLATION opening many local databases in one process (Windows)

Upstream report for [tursodatabase/libsql](https://github.com/tursodatabase/libsql).
Reproduced against **0.9.30** and **0.10.0-pre.4**.

This supersedes the earlier report from 2026-07-31, which described the trigger
as *concurrent* open. **That was wrong**, and the correction is the reason this
document exists: the fault reproduces with no concurrency of any kind.

---

## Summary

A process that opens local databases in a loop dies with
`STATUS_ACCESS_VIOLATION` (`0xC0000005`) after a few thousand iterations. It is
not a panic and not a `libsql::Error` — there is nothing to catch. The process
is simply gone.

The trigger is **cumulative `connect()` against distinct database files**. It is
not a data race: it reproduces on a single-threaded runtime, in one task, with
no overlap anywhere.

## Environment

| | |
|---|---|
| libsql | 0.9.30 (also 0.10.0-pre.4) |
| OS | Windows 11 Pro 26200 |
| Toolchain | rustc 1.97.1, MSVC |
| Profile | release (also reproduces in debug) |
| Filesystem | NTFS, local disk |

Effectively Windows-only in practice. The same suite that dies regularly on
Windows takes 46 s on macOS and 58 s on Ubuntu without faulting.

## Minimal reproducer

No threads, no concurrency, one task on a current-thread runtime.

```rust
// Cargo.toml: libsql = "0.9.30", tokio = { version = "1", features = ["rt", "macros"] }
use libsql::Builder;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dir = std::env::temp_dir().join("libsql-av-repro");
    std::fs::create_dir_all(&dir).unwrap();

    for i in 0u64.. {
        let path = dir.join(format!("db-{i}.db"));
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        let _ = conn.query("SELECT 1", ()).await;
        // conn and db drop here

        if i % 500 == 0 {
            eprintln!("{i}");
        }
    }
}
```

**Expected:** runs indefinitely.
**Actual:** the process dies with `0xC0000005`, typically between 2,000 and
40,000 iterations. Exit code surfaces as `-1073741819`.

Because the fault kills the process, measuring it needs a parent that re-execs
this and reads the child's exit code. Ours is
[`examples/r15_soak.rs`](../examples/r15_soak.rs) in this repository.

## What we ruled out

Each row changes exactly one thing from the reproducer above. `n` is small — 6
to 10 — so read these as **eliminated / not eliminated**, never as a comparison
of rates.

| variant | faults | conclusion |
|---|---|---|
| never call `connect()`, only `build()` | **0/6** | **`build()` alone is not enough** — and this survived ~880,000 iterations per run, a 100× margin over every arm that dies |
| `connect()` but no query | 6/6 | the query is not required |
| `build()` serialised under a mutex | 6/6 | not a race in `build()` |
| `connect()` serialised under a mutex | 5/6 | not a race in `connect()` |
| all handles held, dropped one at a time at the end | 6/6 | not concurrent teardown |
| fully sequential, multi-thread runtime | 4/6 | not simultaneity |
| **fully sequential, current-thread runtime** | **2/10** | **not concurrency at all** — no overlap, no worker migration |

The last row is the one that matters: one task, one OS thread,
`build → connect → query → drop` in a loop. Nothing in the process is
concurrent with anything else, and it still faults.

## Distinct files vs. one file

Holding one path and reopening it, everything else identical (n = 8):

| | faults | how far runs got |
|---|---|---|
| a fresh file per iteration | 6/8 | faults from 2,272 iterations |
| **one file reopened** | **1/8** | 7 clean runs reached **40,352–43,360** |

So the dominant variable is **distinct databases**, with `connect()` as the call
that pays for them. Reopening one file is markedly safer but not immune.

## It is not a fixed threshold

We instrumented the loop to record its iteration count durably before each
batch, so a faulting run reports where it died. Pooled n = 20 at one
configuration:

```
faults (17):  2144  2272  2272  6544 10176 10928 17536 ×8  19520 33856 40416
clean  (3):  42416 44864 46384      <- all three stopped on a time limit, not cleanly
```

Fault positions span an order of magnitude, and the clean runs stopped only
because the harness ran out of time. This looks like a **per-`connect()`
probability** — very roughly 1 in 20,000–25,000 on this machine — rather than a
leak with a ceiling.

**One caution, because it nearly went into this report as a finding.** In the
first session 8 of 11 faults landed on exactly 17,536, which reads as a hard
threshold. Re-running the identical configuration produced that value once in
eight. **The cluster did not replicate**, and the number is left in the table
above only to document that this measurement can manufacture a convincing
constant.

## Why we are reporting it rather than working around it

We carry two mitigations, and this diagnosis says neither addresses the
mechanism:

- `RUST_TEST_THREADS = "1"` — serialises our test suite. Every arm above that
  serialises something still faults. It lowers the observed rate, we believe by
  lowering databases-created-per-run, not by removing a race.
- Quarantining our property-test binaries — they create a database per generated
  case, which this diagnosis identifies as the worst-exposed shape.

Application code that opens a bounded set of connections once and holds them
does not appear to be affected; we soak that shape and it is clean. The exposed
party is test suites and tooling that create many short-lived databases.

## What would help

Any of:

1. Confirmation of whether a per-`Database` or per-`Connection` resource is
   released on drop on Windows.
2. A bound we can respect — if there is a known ceiling on databases opened per
   process, we can document it.
3. Whether `Builder::new_local(...).build()` being clean at ~880,000 iterations
   while adding `connect()` dies within a few thousand points at a specific
   allocation.

We are happy to run further arms; the harness takes a new variable in a few
lines.
