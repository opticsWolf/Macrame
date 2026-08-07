"""R15 on the diagnostic path — the 0.10.0 W4.1 arm. **Not a test.**

Deliberately not named ``test_*``, for the reason
``tests_py/probes/r15_concurrent_open.py`` gives: the unmitigated arm crashes
the interpreter, and a suite that dies is not a suite that reports. Run it by
hand::

    for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
        python tests_py/probes/r15_diagnostic_path.py 48 || echo "run $i faulted"
    done

# The shape it probes

``r15_concurrent_open.py`` measured concurrent ``Database.open``. This measures
something a caller is far more likely to do by accident: **many threads sharing
one already-open handle**, which sounds safe and, on this one method, is not.

``diagnostic_query`` and ``explain`` call ``Database::diagnostic_conn()``, which
performs a fresh ``libsql::Builder::…build()`` per call — the only method on the
handle that opens the file after ``open`` returns. ``block_on`` releases the
GIL, so *N* threads inside it are *N* concurrent opens: R15's shape, reached
without ever calling ``open`` more than once.

# What it answered

Measured 2026-08-07, libSQL 0.9.30, Windows, 48 threads on a
``threading.Barrier``, 5 ``diagnostic_query`` + 5 ``explain`` calls each,
18 runs per arm. The unmitigated arm was produced by commenting the ``lock()``
out, rebuilding, and putting it back.

===================  =========  =====================================
arm                  18 runs    failures
===================  =========  =====================================
without the mutex    **7 bad**  2 x ``0xC0000005``; 4 x ``database is
                                locked``; 1 x ``bad parameter or other
                                API misuse``
with the mutex       0 bad      --
===================  =========  =====================================

That is the whole justification for the lock, and it is a measurement rather
than an argument — which, on this repository's history with R15, is the
difference between a mitigation and a hope.

**Note the failure mode, because it is not only R15.** Two runs took the access
violation, but most took a *returned SQLite error* instead. Those are the same
race arriving through a survivable path, and they are worse in one respect: an
``EngineError`` on a diagnostic query looks like a fact about the database, and
a caller debugging with this method would have believed it. The mutex removes
both.

# What it does not establish

That R15 is fixed. It is not, upstream or here. This bounds one path in one
binding to one outstanding open; ``Database.open`` from many threads is still
the shape ``r15_concurrent_open.py`` measures, and the Rust
``Database::diagnostic_conn`` is documented rather than locked — see its
rustdoc for why the crate does not take this lock on the caller's behalf.
"""

from __future__ import annotations

import sys
import tempfile
import threading
from pathlib import Path

import macrame


def main(width: int) -> int:
    tmp = Path(tempfile.mkdtemp(prefix="r15_diag_"))
    # A barrier for the same reason as the sibling probe: without it the calls
    # stagger across thread startup and the concurrency never reaches `width`.
    barrier = threading.Barrier(width)
    errors: list[str] = []

    with macrame.Database.open(tmp / "probe.db", snapshot_every_entries=None) as db:
        db.write_concepts(
            [
                macrame.ConceptUpsert(
                    id="a",
                    title="A",
                    content="body",
                    valid_from="2026-01-01T00:00:00.000000Z",
                )
            ]
        )

        def worker() -> None:
            try:
                barrier.wait()
                for _ in range(5):
                    rows = db.diagnostic_query("SELECT COUNT(*) FROM concepts")
                    if rows[0][0] != 1:
                        errors.append(f"wrong count: {rows[0][0]}")
                    db.explain("SELECT * FROM concepts")
            except BaseException as exc:  # noqa: BLE001 - recording, not handling
                errors.append(repr(exc))

        threads = [threading.Thread(target=worker) for _ in range(width)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

    if errors:
        print(f"ERRORS ({len(errors)}): {errors[:2]}")
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(int(sys.argv[1]) if len(sys.argv) > 1 else 48))
