"""R15 through the Python boundary — the plan's probe P6-a. **Not a test.**

Deliberately not named ``test_*``: this crashes the interpreter when it
reproduces, which is the point, and a suite that dies is not a suite that
reports. Run it by hand::

    for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
        python tests_py/probes/r15_concurrent_open.py 48 || echo "run $i faulted"
    done

# What it answered

The binding plan listed this as *assumed*: R15 is a libSQL fault on concurrent
open of local databases in one process (see ``.cargo/config.toml`` for the
measurement, the refuted churn hypothesis, and the reporting hazard), and it
was not obvious whether the Python boundary changed the exposure. There was a
plausible argument that it would *reduce* it — one shared runtime, entry
serialised by the GIL.

**It does not.** Measured 2026-07-31, libSQL 0.9.30, Windows, 48 concurrent
opens from 48 Python threads on a ``threading.Barrier``: **2 faults in 12
runs**, the same rate as the Rust control arm of ``examples/r15_soak.rs``
(2/10 at the same width).

The reason the GIL does not save us is the reason P1 exists: ``block_on``
releases it. Every thread is genuinely inside a concurrent open, which is
precisely what the fault counts. The boundary is transparent to R15.

# What follows from it

- The pytest suite runs single-process. ``pytest-xdist`` opens a database per
  worker, which is this shape.
- Application code should hold a bounded set of handles opened once, not open
  one per request. That is the same claim soaked and defended for Rust in
  T5.2/D-092, and it now has Python evidence behind it rather than an
  assumption of transfer.
"""

from __future__ import annotations

import sys
import tempfile
import threading
from pathlib import Path

import macrame


def main(width: int) -> int:
    tmp = Path(tempfile.mkdtemp(prefix="r15_py_"))
    # A barrier rather than plain thread starts: without it the opens stagger
    # across thread startup and the concurrency never reaches `width`, which is
    # how a probe of this kind quietly measures nothing.
    barrier = threading.Barrier(width)
    errors: list[str] = []

    def worker(i: int) -> None:
        try:
            barrier.wait()
            db = macrame.Database.open(
                tmp / f"db{i}.db", snapshot_every_entries=None
            )
            db.close()
        except BaseException as exc:  # noqa: BLE001 - recording, not handling
            errors.append(repr(exc))

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(width)]
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
