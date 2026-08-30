"""Diff the crate's public API against the checked-in baseline.

Not a test, for the same reason `cargo-fuzz` is not one: it needs a nightly
toolchain, so it cannot run in the loop every other gate runs in::

    python scripts/check_public_api.py            # diff against the baseline
    python scripts/check_public_api.py --bless    # adopt the current surface

Exit codes are three-valued on purpose. **0** is "the surface is the baseline".
**1** is "the surface moved" and prints what moved, or the prose in appendix D
disagrees with the baseline. **2** is "this could not be measured" — nightly or
`cargo-public-api` is missing, or the appendix's anchor moved — which is a
different thing from a clean run and must not be reported as one.

All three go through `cannot_measure()` / `return`, never through
`sys.exit("...")`: passing a **string** to `sys.exit` exits 1 and prints it, so
until 0.14.17 every "exit 2" path in this file actually exited 1 and the third
value did not exist.

Why the baseline is checked in
------------------------------
W11.2 asks, of every public item, whether it is intended to be supported for
the life of 1.x. That question is answered once; what a file in the repository
adds is that the answer stays answered. Rust's semver rules make every `pub`
path a commitment, and the commitments this crate did not mean to make were
invisible until 0.13.32 generated this list — a public actor-command enum
carrying `tokio::sync::oneshot::Sender` in its variants, 33 exhaustive
`DbError` variants, and 39 public modules giving most types two to four
supported paths.

None of that is visible in a diff of the source. It is visible in a diff of
*this file*, which is why the baseline lands before the releases that change
the surface rather than after them: from 0.13.33 on, each of those ships the
output of this script in its commit message, and "did we remove more than we
meant to" is answered from the record instead of from memory (D-205).

Why `--all-features`
--------------------
`metrics` is on by default and `property-tests` adds no items, so
`--all-features` is the maximal surface and `--no-default-features` is a strict
subset of it (it drops `Database::metrics` and the `metrics` module's types).
Baselining the superset means a feature-gated item cannot enter unnoticed by
being gated; the subset needs no separate file because nothing is public *only*
without a feature.

Why the three `--omit` flags
----------------------------
Blanket, auto-trait and auto-derived impls are not this crate's API. Left in,
they add roughly a thousand lines that move when a *dependency* changes — the
first run of this tool reported `ppv_lite86::types::VZip` and
`zerocopy::pointer::invariant::Read` implemented for `Tuning`, neither of which
anyone here wrote or can affect. A baseline that churns for reasons outside the
repository is a baseline people learn to `--bless` without reading, which is
the failure mode this file exists to avoid.
"""

from __future__ import annotations

import argparse
import difflib
import re
import typing
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = ROOT / "docs" / "architecture" / "public-api.txt"
APPENDIX = ROOT / "docs" / "architecture" / "appendices.md"

# Appendix D states the frozen surface as a number, in prose, and prose does not
# recompute. `the_contract_names_the_surface_the_baseline_holds` already checks
# it -- but that test runs in `cargo test`, and blessing happens after the suite
# has run. At 0.14.15 the baseline went to 1,556 while the appendix still said
# 1,555, and the gate was RED AT HEAD THROUGH A SHIPPED RELEASE because the only
# thing that could see it had already passed. Checking it here closes that
# ordering: this script is what moves the number, so this script is what has to
# refuse.
APPENDIX_COUNT = re.compile(r"is the surface — \*\*([\d,]+) items\*\*")

ARGS = [
    "+nightly",
    "public-api",
    "--all-features",
    "--omit",
    "blanket-impls,auto-trait-impls,auto-derived-impls",
]

HEADER = [
    "# The public API of `macrame-db`, as `cargo-public-api` reports it.",
    "#",
    "# Generated. Do not hand-edit: `python scripts/check_public_api.py --bless`",
    "# rewrites it, and that script's docstring says why this file is checked in",
    "# (D-205). A change here is a change to what 1.x promises, so it belongs in",
    "# the same commit as the code that caused it, with the diff in the message.",
    "#",
    "#     cargo " + " ".join(ARGS),
    "#",
]


def cannot_measure(message: str) -> "typing.NoReturn":
    """Exit **2**: this could not be measured, which is not a pass and not a diff.

    Every path below used to be `sys.exit("... [exit 2]")`, and that exits
    **1**: `sys.exit` treats a string argument as a message to print, not as a
    status. So the three-valued contract in this module's docstring was
    two-valued in fact, exit 2 was unreachable, and `ci.yml`'s
    `if [ "$code" = "2" ]` branch could never be taken -- a missing nightly or a
    missing `cargo-public-api` was reported as "the surface moved" and failed
    the job, which is the exact collapse the docstring forbids.

    Found by mutation in 0.14.17, while testing the appendix check added in the
    same release: the message said `[exit 2]` and the process returned 1.
    """
    print(message, file=sys.stderr)
    raise SystemExit(2)


def measure() -> list[str]:
    """The current surface, or exit 2 saying what is missing."""
    if shutil.which("cargo") is None:
        cannot_measure("cargo is not on PATH; cannot measure the public API")

    try:
        proc = subprocess.run(
            ["cargo", *ARGS],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as e:  # pragma: no cover - environment failure
        cannot_measure(f"could not run cargo-public-api: {e}")

    if proc.returncode != 0:
        stderr = proc.stderr.strip()
        hint = ""
        if "no such command" in stderr or "public-api" in stderr and "not" in stderr:
            hint = "\n\n    cargo install cargo-public-api --locked"
        if "nightly" in stderr and "not installed" in stderr:
            hint = "\n\n    rustup toolchain install nightly --profile minimal"
        print(stderr, file=sys.stderr)
        cannot_measure(f"cargo-public-api failed{hint}")

    return [line.rstrip() for line in proc.stdout.splitlines() if line.strip()]


def is_header(line: str) -> bool:
    """A header comment, and not an item.

    The distinction is `#[`: twelve items in the current surface begin
    ``#[non_exhaustive]``, and the first version of this function dropped every
    one of them as a comment. It reported a twelve-line diff against a file it
    had just written, which is at least the right direction for a bug in a
    differ to fail in.
    """
    return line.startswith("#") and not line.startswith("#[")


def appendix_count() -> int:
    """What appendix D says the surface is, or exit 2 if it cannot be read.

    Exit 2 rather than 1 on an unreadable appendix, for the same reason
    `measure()` uses it: "the number moved" and "the number could not be found"
    are different answers, and collapsing them is how a gate becomes noise.
    """
    try:
        text = APPENDIX.read_text(encoding="utf-8")
    except OSError as e:
        cannot_measure(f"could not read {APPENDIX.relative_to(ROOT)}: {e}")

    found = APPENDIX_COUNT.findall(text)
    if len(found) != 1:
        cannot_measure(
            f"expected exactly one surface count in "
            f"{APPENDIX.relative_to(ROOT)}, found {len(found)}; the anchor "
            f"'is the surface — **N items**' moved"
        )
    return int(found[0].replace(",", ""))


def check_appendix(n: int) -> bool:
    """True when appendix D agrees with `n`; otherwise says what to change."""
    stated = appendix_count()
    if stated == n:
        return True
    print(
        f"\nappendix D says the surface is {stated:,} items; it is {n:,}. "
        f"Edit {APPENDIX.relative_to(ROOT)} to read '**{n:,} items**'.\n"
        "Note when reading the delta: `macrame::prelude` re-exports the flat "
        "aliases, so one new enum variant is TWO baseline items, not one. A "
        "one-variant change reads as +2 and that is correct.",
    )
    return False


def stored() -> list[str]:
    if not BASELINE.exists():
        return []
    lines = BASELINE.read_text(encoding="utf-8").splitlines()
    return [ln.rstrip() for ln in lines if ln.strip() and not is_header(ln)]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--bless",
        action="store_true",
        help="rewrite the baseline from the current surface",
    )
    args = ap.parse_args()

    current = measure()

    if args.bless:
        BASELINE.parent.mkdir(parents=True, exist_ok=True)
        BASELINE.write_text(
            "\n".join([*HEADER, *current]) + "\n", encoding="utf-8", newline="\n"
        )
        print(f"blessed {BASELINE.relative_to(ROOT)}: {len(current)} items")
        # Blessing is exactly the moment the appendix goes stale, so it is the
        # moment to refuse. Returning 1 here does not un-bless the file -- the
        # baseline is written and correct -- it refuses to call the job done
        # while the prose disagrees with it.
        return 0 if check_appendix(len(current)) else 1

    baseline = stored()
    if not baseline:
        cannot_measure(
            f"no baseline at {BASELINE.relative_to(ROOT)}; run with --bless"
        )

    if current == baseline:
        print(f"public API unchanged: {len(current)} items")
        # Unchanged since the last bless is not the same as consistent: this is
        # the state 0.14.15 shipped in.
        return 0 if check_appendix(len(current)) else 1

    diff = difflib.unified_diff(
        baseline,
        current,
        fromfile="baseline",
        tofile="current",
        lineterm="",
        n=0,
    )
    print("\n".join(diff))
    added = len([ln for ln in current if ln not in set(baseline)])
    removed = len([ln for ln in baseline if ln not in set(current)])
    print(
        f"\npublic API moved: +{added} -{removed} "
        f"({len(baseline)} -> {len(current)} items). "
        "If this is intended, re-run with --bless and put the diff above in the "
        "commit message."
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
