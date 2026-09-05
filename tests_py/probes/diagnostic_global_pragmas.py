"""Pragmas through the side door that are not the connection's. **Not a test.**

Deliberately not named ``test_*``, for the reason
``tests_py/probes/r15_diagnostic_path.py`` gives: one arm here leaves the
process unable to open any database at all, and a suite that dies is not a
suite that reports. Run it by hand::

    PYTHONPATH=python python tests_py/probes/diagnostic_global_pragmas.py

Each case runs in its own subprocess, because the first version of this probe
ran them in one and the second case destroyed the four that followed it.

A child that exits ``0xC0000005`` faulted on R15 — unfixed upstream, unrelated
to anything set here — and is retried once. The label says so when it happens.

# The question

[D-257] scrubbed the shared diagnostic connection between callers and said the
residue it could not scrub — a pragma the crate does not itself set — "only
affects later diagnostic reads on the same handle; it cannot change any typed
answer". That is a claim about *per-connection* state, and it was made without
checking whether every pragma reachable here is per-connection.

Not all of them are. Some SQLite settings belong to the library rather than to
the connection, one per process, and `SQLITE_OPEN_READ_ONLY` does not stand in
their way because setting one is not a write to the database file.

# What it answered

Measured 2026-09-05, libSQL 0.9.30, Windows. Each row: an ordinary write, then
the pragma through ``diagnostic_query``, then an ordinary write, read,
``checkpoint()``, ``close()``, and finally an attempt to open a **different**
database in the same process.

=========================================  ==========================================
pragma through the side door               what ordinary use does afterwards
=========================================  ==========================================
``soft_heap_limit = 1``                    unaffected -- a hint to reclaim, not a wall
``hard_heap_limit = 1``                    **everything fails, `out of memory`** --
                                           writes, reads, checkpoint, close, and
                                           opening an unrelated database file
``locking_mode = EXCLUSIVE``               accepted; the writer kept working
``temp_store_directory = '.'``             no effect
``wal_checkpoint(TRUNCATE)``               refused (`disk I/O error`) -- read-only
``max_page_count = 1``                     clamped to the current size, per-connection
``case_sensitive_like = ON``               unaffected -- the control, and the residue
                                           D-257 documents
=========================================  ==========================================

So the claim holds for six of seven and fails for ``hard_heap_limit``, which is
process-wide and permanent for the life of the process.

# What it is *not*

It is not a consequence of the shared connection, and no amount of scrubbing
addresses it. Re-measured with the 0.15.13 shape — a fresh connection minted for
every side-door call, the whole hazard class D-257 exists to bound — the result
is identical: the process is dead either way. The setting is not stored on the
connection that was thrown away.

It is therefore in the same family as the ``ATTACH`` note in
``Database::diagnostic_conn``'s rustdoc: a property of handing a caller
arbitrary SQL, not of the flags the connection was opened with. See
[D-258](../../docs/architecture/s13-decision-register.md#d-258).

# Why it is documented rather than blocked

Blocking would mean refusing statements by inspecting the SQL for keywords,
which is guesswork wearing the costume of a guarantee: it would not survive
whitespace, comments, or a pragma spelled through a different surface, and it
would do nothing for a Rust caller holding the connection directly. The honest
mitigation is knowing that ``diagnostic_query`` is not a safe place to put
strings that came from somewhere else -- which was already true because of
``ATTACH``, and is now true more sharply.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import threading

T0 = "2026-01-01T00:00:00.000000Z"

CASES = [
    ("soft_heap_limit", "PRAGMA soft_heap_limit = 1"),
    ("hard_heap_limit", "PRAGMA hard_heap_limit = 1"),
    ("locking_mode", "PRAGMA locking_mode = EXCLUSIVE"),
    ("temp_store_directory", "PRAGMA temp_store_directory = '.'"),
    ("wal_checkpoint", "PRAGMA wal_checkpoint(TRUNCATE)"),
    ("max_page_count", "PRAGMA max_page_count = 1"),
    ("case_sensitive_like", "PRAGMA case_sensitive_like = ON"),
]


def timed_write(db, ident, seconds=8.0):
    """One ordinary write, with a ceiling.

    In a worker thread with a join timeout because "it refused" and "it never
    came back" are different findings and a probe that hangs reports neither.
    """
    import macrame

    out = {}

    def go():
        try:
            db.write_concepts([macrame.ConceptUpsert(ident, "X", valid_from=T0)])
            out["r"] = "ok"
        except Exception as e:  # noqa: BLE001
            out["r"] = f"ERROR {type(e).__name__}: {e}"

    t = threading.Thread(target=go, daemon=True)
    t.start()
    t.join(seconds)
    if t.is_alive():
        return f"HUNG (> {seconds:.0f}s)"
    return out.get("r", "?")


def run_one(label: str, sql: str) -> None:
    """The child half: one case, in a process it is allowed to destroy."""
    import macrame

    path = os.path.join(tempfile.mkdtemp(), "case.db")
    print(f"--- {label}:  {sql}")
    try:
        with macrame.Database.open(path, snapshot_every_entries=None) as db:
            print("  ordinary write before :", timed_write(db, "before"))
            try:
                got = repr(db.diagnostic_query(sql))[:60]
                print(f"  side door             : accepted {got}")
            except Exception as e:  # noqa: BLE001
                print(f"  side door             : refused: {str(e)[:60]}")
            print("  ordinary write after  :", timed_write(db, "after"))
            try:
                db.reconstruct(ts="2030-01-01T00:00:00.000000Z")
                print("  ordinary read after   : ok")
            except Exception as e:  # noqa: BLE001
                print(f"  ordinary read after   : ERROR {str(e)[:55]}")
            try:
                r = db.checkpoint()
                print(f"  checkpoint after      : ok ({r.checkpointed_frames} frames)")
            except Exception as e:  # noqa: BLE001
                print(f"  checkpoint after      : ERROR {str(e)[:55]}")
        print("  closing the handle    : ok")
    except Exception as e:  # noqa: BLE001
        print(f"  handle-level failure  : {type(e).__name__}: {str(e)[:60]}")

    # The question that separates "this handle is spoiled" from "this process
    # is spoiled": can a database that was never touched still be opened?
    try:
        other = os.path.join(tempfile.mkdtemp(), "unrelated.db")
        with macrame.Database.open(other, snapshot_every_entries=None) as db2:
            db2.write_concepts([macrame.ConceptUpsert("n", "N", valid_from=T0)])
        print("  an unrelated database : ok")
    except Exception as e:  # noqa: BLE001
        print(f"  an unrelated database : ERROR {str(e)[:60]}")


#: `STATUS_ACCESS_VIOLATION`. R15's signature, and unrelated to anything this
#: probe sets -- see `tests_py/probes/r15_concurrent_open.py`.
ACCESS_VIOLATION = -1073741819


def child(label: str, sql: str) -> subprocess.CompletedProcess:
    """One case, in its own process, unbuffered.

    ``-u`` because a child that dies mid-case must not take the lines it had
    already printed with it: the first run of this probe lost a whole case that
    way and reported it as silence.
    """
    return subprocess.run(
        [sys.executable, "-u", os.path.abspath(__file__), label, sql],
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )


def main() -> int:
    if len(sys.argv) == 3:
        run_one(sys.argv[1], sys.argv[2])
        return 0

    print("Each case in its own process. Ordinary write, then the pragma")
    print("through diagnostic_query, then ordinary use again.")
    print()
    for label, sql in CASES:
        proc = child(label, sql)
        # R15 is unfixed upstream and reachable from any of these children.
        # Retried once, because a case that faulted told us nothing about its
        # pragma -- and only for that fault, so a real crash still reports.
        if proc.returncode == ACCESS_VIOLATION:
            print(f"--- {label}: 0xC0000005 (R15), retrying once")
            proc = child(label, sql)
        print(proc.stdout.rstrip())
        if proc.returncode != 0:
            hexed = f"0x{proc.returncode & 0xFFFFFFFF:08X}"
            note = " -- R15, not this pragma" if proc.returncode == ACCESS_VIOLATION else ""
            print(f"  (child exited {proc.returncode} / {hexed}{note})")
            if proc.stderr.strip():
                print("  " + proc.stderr.strip().splitlines()[-1][:100])
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
