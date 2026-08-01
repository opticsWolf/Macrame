"""Run the Python suite so that an R15 crash cannot be read as success.

Not a test — pytest does not collect this, and it is not meant to be imported.
Run it instead of ``pytest`` wherever the answer gates something::

    python tests_py/run_suite.py

Exits 0 only when the suite genuinely passed. Exits 1 otherwise, having said
which of the several distinguishable failures it was.

Why this exists
---------------
`.cargo/config.toml` records R15: an intermittent libSQL access violation
(0xC0000005) on **concurrent open** of local databases. It does not raise, it
kills the process. `tests_py/probes/r15_concurrent_open.py` reproduced it
*through this binding* at 2 in 12 runs with 48 concurrent opens, so the boundary
does not protect us.

**The plan said the Rust reporting hazard "carries across unchanged". It does
not, and the difference decides how this file works.** `cargo test` runs one
process per test binary, so a crash removes one binary's tests from the total
while the others still print their summaries — a *smaller pass count with no
failures*, which reads as green. pytest runs one process. When it dies,
everything dies, and the measured result is:

    exit code    3 (non-zero)
    stdout       the faulthandler traceback, no summary line at all

So for pytest the exit code *would* catch a mid-run crash. What it would not
catch is the inverse, which is specific to this extension and is the reason the
summary check is here anyway: `PyDatabase::drop` enters the tokio runtime, and
a fault during interpreter teardown lands **after** pytest has printed
``325 passed``. That produces a green summary with a non-zero exit — the
opposite arrangement, and the one where reading only the exit code is right by
accident while reading only the summary is wrong.

Both are therefore checked, and they are checked against each other.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Three, matching `ci.yml`'s Rust retry and for the same reason: R15 has always
# passed on re-run, so a genuine failure still goes red after three, and the
# attempt count stays visible in the log rather than being hidden by a
# `continue-on-error`.
ATTEMPTS = 3

SUMMARY = re.compile(r"(?:(\d+) failed[, ]+)?(\d+) passed(?:[, ]+(\d+) (?:error|errors))?")
COLLECTED = re.compile(r"collected (\d+) items?")


def run_once() -> tuple[bool, str]:
    """Return (passed, reason). `reason` is empty when it passed."""
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", "tests_py", "-q", "--color=no"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    out = proc.stdout + proc.stderr
    print(out)

    summary = SUMMARY.search(out)

    # 1. No summary at all: the interpreter died mid-run. This is R15's shape,
    #    and it is the one worth retrying.
    if summary is None:
        return False, f"CRASH: no pytest summary line (exit {proc.returncode})"

    failed = int(summary.group(1) or 0)
    passed = int(summary.group(2))
    errors = int(summary.group(3) or 0)

    # 2. Real failures. Not retried — re-running until green is how a flaky
    #    assertion becomes permanent.
    if failed or errors:
        return False, f"FAILED: {failed} failed, {errors} error(s), {passed} passed"

    # 3. A green summary that does not account for everything collected. Cannot
    #    currently happen without xdist, and is checked because the failure it
    #    guards is silent: tests that vanish rather than fail.
    collected = COLLECTED.search(out)
    if collected and int(collected.group(1)) != passed:
        return False, (
            f"INCOMPLETE: collected {collected.group(1)} but only {passed} passed, "
            f"with nothing reported failed"
        )

    # 4. A green summary and a bad exit code: the teardown crash described in
    #    this module's docstring. Deliberately **not** retried and deliberately
    #    not green — the tests passed, and the process still died, which is a
    #    defect in the extension's shutdown path rather than in a test.
    if proc.returncode != 0:
        return False, (
            f"TEARDOWN: {passed} passed and pytest still exited {proc.returncode}. "
            f"The tests are fine and the process is not — look at Drop, not at "
            f"the assertions."
        )

    return True, ""


def main() -> int:
    for attempt in range(1, ATTEMPTS + 1):
        ok, reason = run_once()
        if ok:
            print(f"python suite passed on attempt {attempt}/{ATTEMPTS}")
            return 0
        print(f"attempt {attempt}/{ATTEMPTS}: {reason}", file=sys.stderr)
        # Only a crash is R15's signature. Everything else is a real result and
        # retrying it would just take longer to report the same thing.
        if not reason.startswith("CRASH"):
            return 1
        print("  (R15's shape — see .cargo/config.toml — retrying)", file=sys.stderr)

    print(
        f"{ATTEMPTS} consecutive runs crashed without a summary. That is more "
        f"than R15 has ever needed; treat it as real.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
