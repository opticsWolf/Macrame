"""Run the Rust suite so that an R15 crash cannot be read as a test failure.

Not a test. Run it instead of bare ``cargo test`` wherever the answer gates
something::

    python scripts/run_rust_suite.py --features metrics
    python scripts/run_rust_suite.py --features property-tests --attempts 6

Exits 0 only when the suite genuinely passed, and otherwise says which of the
distinguishable failures it was. Retries **only** the one that is noise.

Why this exists
---------------
`.cargo/config.toml` records R15: an intermittent libSQL access violation
(0xC0000005) on concurrent open. It does not raise, it kills the process.

`ci.yml` used to answer this with ``for attempt in 1 2 3``, twice, which counts
failures without reading them. Two things were wrong with that:

1. **A count is not a diagnosis.** Three failures produce four lines of log --
   attempt 1 failed, attempt 2 failed, attempt 3 failed, failed three times --
   and nothing in them says whether libSQL died or a property found a real
   defect. Re-running a genuine failure three times just takes longer to report
   the same thing, and re-running it *until it passes* is how a flaky assertion
   becomes permanent.
2. **The budget was calibrated on the wrong step.** "R15 has always passed on
   re-run" is true of the main suite, where ``RUST_TEST_THREADS = "1"`` applies.
   The quarantined binaries are quarantined precisely because serialising does
   not save them.

`tests_py/run_suite.py` already solved this for Python under D-107. This is the
same idea brought back to Rust, and the two files are deliberately shaped alike.
They cannot share an implementation: pytest is one process with one summary,
cargo is one process per target with one summary each, and D-107 is the decision
that they therefore need different checks.

How a crash is told apart from a failure
----------------------------------------
Measured on this repo (libsql 0.9.30, Windows, 26 test targets + doctests):

* cargo announces each target on **stderr** -- ``Running tests\\foo.rs (...)``,
  ``Running unittests src\\lib.rs (...)``, ``Doc-tests macrame``.
* each target's own output goes to **stdout**, beginning ``running N tests`` and
  ending ``test result: ok. N passed; ...``.

The two streams cannot be interleaved after the fact, and they do not need to
be. The load-bearing fact is that **a target killed mid-run has already printed
its ``running N tests`` line**: libtest emits it before the first test, and the
config file's own account of the hazard -- tests before the fault reported,
tests behind it silent -- says the stream survives up to the fault. So a crash
removes a target's *summary* without removing its *section*, sections stay in
step with the announced targets positionally, and the crashed target can still
be named.

That is also exactly the hazard: with ``--no-fail-fast`` the missing summary
makes the run come back with a **smaller pass count and zero failures**, which
reads as green to anything summing passes. It was observed at 0.7.0 as
"308 passed, 0 failed" with eight tests silently absent. Keying on the absence
of a per-target result line is what `.cargo/config.toml` instructs anything
gating on this suite to do, and is what this file does.

``--no-fail-fast`` is not optional and is added here rather than asked for:
without it, every target alphabetically behind a fault is skipped too, and the
distinction this file exists to draw is destroyed before it can be read.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# `ci.yml` sets `CARGO_TERM_COLOR: always`, so on CI every cargo status line
# arrives wrapped in SGR escapes:
#
#     '\x1b[1m\x1b[92m     Running\x1b[0m tests/foo.rs (target/debug/...)'
#
# The first version of this file matched `^\s*Running` and therefore matched
# nothing on CI, found zero targets, and reported `BUILD` on a run in which all
# 27 targets had passed. Locally it was invisible: the variable is set in the
# workflow, and cargo turns colour off for a non-tty by default.
#
# Stripped rather than tolerated in each pattern, because "every regex in this
# file must also allow for colour" is a rule that holds until someone adds the
# next regex.
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")

# Announcements on stderr. `Doc-tests` is not a `Running` line and is easy to
# drop; it is a target with a summary like any other and losing it would put
# every later section one out of step.
RUNNING = re.compile(r"^\s*Running (?:unittests )?(\S+)")
DOCTESTS = re.compile(r"^\s*Doc-tests (\S+)")

# Section boundaries on stdout.
SECTION = re.compile(r"^running (\d+) tests?$")
RESULT = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored"
)
CASE_FAILED = re.compile(r"^test (\S+) \.\.\. FAILED")


class Outcome:
    """What a single attempt was. Only CRASH is retried."""

    def __init__(self, kind: str, detail: str, annotations: list[str] | None = None):
        self.kind = kind
        self.detail = detail
        self.annotations = annotations or []

    @property
    def passed(self) -> bool:
        return self.kind == "PASSED"


def targets_from(stderr: str) -> list[str]:
    """Announced targets, in the order cargo ran them."""
    names = []
    for line in ANSI.sub("", stderr).splitlines():
        m = RUNNING.match(line)
        if m:
            names.append(Path(m.group(1)).stem)
            continue
        m = DOCTESTS.match(line)
        if m:
            names.append(f"doctests({m.group(1)})")
    return names


def sections_from(stdout: str) -> list[dict]:
    """One entry per `running N tests` block, whether or not it finished."""
    sections: list[dict] = []
    for line in ANSI.sub("", stdout).splitlines():
        m = SECTION.match(line)
        if m:
            sections.append(
                {"announced": int(m.group(1)), "result": None, "failed_cases": []}
            )
            continue
        if not sections:
            continue
        current = sections[-1]
        m = CASE_FAILED.match(line)
        if m:
            current["failed_cases"].append(m.group(1))
            continue
        m = RESULT.match(line)
        if m:
            current["result"] = {
                "ok": m.group(1) == "ok",
                "passed": int(m.group(2)),
                "failed": int(m.group(3)),
            }
    return sections


def classify(proc: subprocess.CompletedProcess) -> Outcome:
    targets = targets_from(proc.stderr)
    sections = sections_from(proc.stdout)

    # Positional pairing, and it degrades instead of lying. If the stderr parse
    # ever comes up short again -- a new cargo status wording, a colour scheme
    # this file has not met -- the sections on stdout are still the evidence of
    # what ran, and a target nobody could name is better reported by position
    # than not reported at all.
    def name(i: int) -> str:
        return targets[i] if i < len(targets) else f"target#{i + 1}"

    # 0. Nothing ran at all: no announcement AND no test output. A compile or
    #    link error, which is a real answer available on attempt 1, given its
    #    own name rather than squeezed into the nearest one.
    #
    #    **Both halves are required, and the first version required only the
    #    first.** It read `if not targets` and reported `BUILD` on a CI run
    #    where every target had passed, because `CARGO_TERM_COLOR: always` had
    #    defeated the stderr parse. The stdout sections were sitting right
    #    there. An empty target list is not evidence that nothing ran; it is
    #    evidence that nothing was *parsed*, and those differ exactly when this
    #    file has a bug.
    if not targets and not sections:
        if proc.returncode == 0:
            # No targets, no output, and cargo is happy. Whatever this is, it
            # is not a passing suite, and it must not be reported as one.
            return Outcome(
                "INCOMPLETE",
                "cargo exited 0 having produced no test targets and no test "
                "output. Nothing ran, and nothing said why.",
            )
        return Outcome(
            "BUILD",
            f"cargo produced no test targets and no test output "
            f"(exit {proc.returncode}). The suite did not run; this is a "
            f"build failure.",
        )

    # 1. A target announced but with no section at all: it never reached its
    #    first test. Distinct from a crash mid-run and not retried, because
    #    nothing about it looks like R15.
    if targets and len(sections) < len(targets):
        missing = targets[len(sections):] or ["(position not recoverable)"]
        return Outcome(
            "INCOMPLETE",
            f"cargo announced {len(targets)} targets but only {len(sections)} "
            f"started running. First unaccounted for: {missing[0]}",
            [f"target {missing[0]} was announced but produced no test output"],
        )

    # 2. Genuine failures. Reported on attempt 1 with the test named, which is
    #    the whole point of the exercise: `for attempt in 1 2 3` would have
    #    spent two more full suite runs to print the same thing less clearly.
    failing = [
        (name(i), s) for i, s in enumerate(sections)
        if s["result"] and not s["result"]["ok"]
    ]
    if failing:
        annotations = []
        total = 0
        for target, section in failing:
            total += section["result"]["failed"]
            for case in section["failed_cases"] or ["(name not printed)"]:
                annotations.append(f"{target}: {case} FAILED")
        return Outcome(
            "FAILED",
            f"{total} test(s) failed across {len(failing)} target(s). "
            f"Not retried -- this is a result, not noise.",
            annotations,
        )

    # 3. R15's signature: a section that started and never printed a summary.
    #    The one outcome worth retrying, and the one that a pass-count sum
    #    reports as a slightly smaller green.
    crashed = [
        (name(i), s) for i, s in enumerate(sections) if s["result"] is None
    ]
    if crashed:
        names = ", ".join(t for t, _ in crashed)
        lost = sum(s["announced"] - len(s["failed_cases"]) for _, s in crashed)
        return Outcome(
            "CRASH",
            f"{len(crashed)} target(s) started and printed no summary: {names}. "
            f"Up to {lost} test(s) silently absent. This is R15's shape.",
        )

    # 4. Every target green, every summary present, and cargo still unhappy.
    #    The inverse arrangement, and the one where reading only the exit code
    #    is right by accident while reading only the summaries is wrong. Not
    #    retried and not green: the tests are fine and the process is not.
    if proc.returncode != 0:
        return Outcome(
            "TEARDOWN",
            f"all {len(sections)} targets reported ok and cargo exited "
            f"{proc.returncode}. Look at teardown, not at the assertions.",
        )

    total = sum(s["result"]["passed"] for s in sections)
    return Outcome("PASSED", f"{total} passed across {len(sections)} targets")


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------
#
# Why this exists, and why it runs in CI as its own step.
#
# The classifier was verified by injection before it shipped -- a real `panic!`
# and a real `std::process::abort()`, both reported correctly -- and it still
# went wrong on its first CI run, because the injections were run *locally* and
# the defect was an environment difference: `CARGO_TERM_COLOR: always`.
# Injection proves the classifier reads a real cargo run; it cannot prove it
# reads a cargo run *shaped the way CI shapes it*.
#
# So the shapes are pinned as fixtures, including the coloured one, and the
# step costs no compile. A gate that can only be tested by the thing it gates
# is a gate that gets tested once.

GREEN_STDOUT = (
    "\nrunning 2 tests\ntest a ... ok\ntest b ... ok\n\n"
    "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; "
    "0 filtered out; finished in 0.01s\n\n"
    "\nrunning 1 test\ntest c ... ok\n\n"
    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
    "0 filtered out; finished in 0.01s\n"
)
PLAIN_STDERR = (
    "   Compiling macrame-db v0.7.0 (/x)\n"
    "    Finished `test` profile [unoptimized + debuginfo] target(s) in 1s\n"
    "     Running unittests src/lib.rs (target/debug/deps/macrame-abc)\n"
    "   Doc-tests macrame\n"
)
# The real shape from CI, reproduced locally with CARGO_TERM_COLOR=always.
COLOUR_STDERR = (
    "\x1b[1m\x1b[92m   Compiling\x1b[0m macrame-db v0.7.0 (/x)\n"
    "\x1b[1m\x1b[92m    Finished\x1b[0m `test` profile target(s) in 1s\n"
    "\x1b[1m\x1b[92m     Running\x1b[0m unittests src/lib.rs "
    "(target/debug/deps/macrame-abc)\n"
    "\x1b[1m\x1b[92m   Doc-tests\x1b[0m macrame\n"
)


def _self_test() -> int:
    def proc(out: str, err: str, code: int) -> subprocess.CompletedProcess:
        return subprocess.CompletedProcess(["cargo"], code, out, err)

    crashed = GREEN_STDOUT[: GREEN_STDOUT.rindex("test result:")]
    red = GREEN_STDOUT.replace(
        "test c ... ok\n\ntest result: ok. 1 passed; 0 failed",
        "test c ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed",
    )

    cases = [
        # The regression this exists for: identical run, colour on and off.
        ("green, plain stderr", proc(GREEN_STDOUT, PLAIN_STDERR, 0), "PASSED"),
        ("green, COLOURED stderr", proc(GREEN_STDOUT, COLOUR_STDERR, 0), "PASSED"),
        ("crash, coloured", proc(crashed, COLOUR_STDERR, 101), "CRASH"),
        ("failure, coloured", proc(red, COLOUR_STDERR, 101), "FAILED"),
        ("green summaries, bad exit", proc(GREEN_STDOUT, PLAIN_STDERR, 101), "TEARDOWN"),
        # A target announced that never started.
        (
            "one section missing",
            proc(GREEN_STDOUT[: GREEN_STDOUT.index("\nrunning 1 test")], PLAIN_STDERR, 101),
            "INCOMPLETE",
        ),
        # Nothing at all, both ways round.
        ("build failure", proc("", "error: could not compile\n", 101), "BUILD"),
        ("silent success", proc("", "", 0), "INCOMPLETE"),
        # Degradation: stdout intact, stderr unparseable. Must NOT be BUILD.
        ("stderr unreadable", proc(GREEN_STDOUT, "<<garbage>>\n", 0), "PASSED"),
    ]

    bad = 0
    for label, p, expected in cases:
        got = classify(p)
        ok = got.kind == expected
        bad += not ok
        print(f"  {'ok  ' if ok else 'FAIL'} {label:<26} expected {expected:<10} got {got.kind}")
        if not ok:
            print(f"       {got.detail}")

    # The colour pair is the whole point: assert they agree, not merely that
    # each lands somewhere.
    plain = classify(proc(GREEN_STDOUT, PLAIN_STDERR, 0))
    colour = classify(proc(GREEN_STDOUT, COLOUR_STDERR, 0))
    if plain.detail != colour.detail:
        bad += 1
        print(f"  FAIL colour changes the answer:\n       {plain.detail}\n       {colour.detail}")
    else:
        print("  ok   colour makes no difference to the detail line")

    print("self-test FAILED" if bad else "self-test passed")
    return 1 if bad else 0


def run_once(cargo_args: list[str]) -> Outcome:
    proc = subprocess.run(
        ["cargo", "test", *cargo_args, "--no-fail-fast"],
        cwd=REPO,
        capture_output=True,
        text=True,
        errors="replace",
    )
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    return classify(proc)


def run_docs(features: str) -> int:
    """`ci.yml`'s rustdoc gate, runnable locally.

    It is here rather than in a test because rustdoc's checks are not tests:
    a broken intra-doc link is a *warning* that `RUSTDOCFLAGS: -D warnings`
    escalates, so it is invisible to `cargo test` and to any assertion this
    project could write. That gap shipped one: `DbError`'s rustdoc linked to
    `schema::migrations::verify`, which is private and therefore unresolvable,
    and 0.10.0 was tagged and released before CI said so.

    **Nothing is retried.** The R15 budget exists because a test binary opens
    databases; rustdoc opens none, so a failure here is always real.
    """
    cmd = ["cargo", "doc", "--no-deps"]
    if features:
        cmd += ["--features", features]
    print(f"::group::{' '.join(cmd)}  (RUSTDOCFLAGS=-D warnings)")
    proc = subprocess.run(cmd, cwd=REPO, env={**os.environ, "RUSTDOCFLAGS": "-D warnings"})
    print("::endgroup::")
    if proc.returncode == 0:
        print("PASSED: rustdoc is clean with -D warnings")
        return 0
    print("::error::DOCS: rustdoc failed under -D warnings. A broken intra-doc "
          "link is the usual cause, and a link to a private item is the usual "
          "broken link -- rustdoc documents public items only.")
    return 1


def verdict(state: str, detail: str) -> None:
    """Say, in one line a human will actually meet, what this run was.

    `ci.yml` runs the quarantined step with `continue-on-error`, so its exit
    code no longer reaches anybody: the job is green either way and the reader
    would have to open the log to find out which way (D-236). A green job with a
    silently red step inside it is the same defect as a counter that cannot be
    zero -- an instrument with no contrast between its healthy and unhealthy
    output is decoration.

    Four states and not three, because folding the fourth in would be a lie:

    * `completed`      -- the suite ran and passed.
    * `crashed-R15`    -- every attempt died without a summary, R15's shape,
                          and **no test was named as failing**. That last clause
                          is the receipt: R15 kills at teardown, after the
                          assertions have reported, so a crashed attempt still
                          usually carries the verdict.
    * `named failures` -- a test failed. Not noise, and never retried.
    * `did not run`    -- BUILD, INCOMPLETE or TEARDOWN: the suite did not
                          produce an answer at all, which is neither a pass nor
                          a failing test and must not print as either.

    Written to `$GITHUB_STEP_SUMMARY` when it exists, which puts the line on the
    run's own page rather than inside a collapsed group.
    """
    line = f"VERDICT: {state} -- {detail}"
    print(line)
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    try:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(f"**{state}** — {detail}\n\n")
    except OSError as e:
        # A summary that cannot be written is not a reason to fail a suite that
        # ran. Say so and carry on.
        print(f"::warning::could not write the job summary: {e}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--features",
        default="",
        help="passed through to cargo test; empty means default features",
    )
    parser.add_argument(
        "--attempts",
        type=int,
        default=3,
        # Three for the main suite, matching what ci.yml has always claimed and
        # what tests_py/run_suite.py uses. Raisable per step now that only a
        # crash is ever retried: more attempts can no longer launder a real
        # failure into a pass, which was the objection to raising it before.
        help="how many times to retry a CRASH (nothing else is retried)",
    )
    parser.add_argument(
        "--docs",
        action="store_true",
        help="run ci.yml's rustdoc gate instead of the test suite; not retried",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="classify fixed fixtures and exit; runs no cargo, compiles nothing",
    )
    # Anything unrecognised goes straight to cargo. This is what lets the
    # injection checks in the exit gate run one target instead of rebuilding
    # the world: `--test bench_control_tests`.
    args, passthrough = parser.parse_known_args()

    if args.self_test:
        return _self_test()

    if args.docs:
        return run_docs(args.features)

    cargo_args = ["--features", args.features] if args.features else []
    cargo_args += passthrough

    for attempt in range(1, args.attempts + 1):
        print(f"::group::cargo test{' ' + args.features if args.features else ''}, "
              f"attempt {attempt}/{args.attempts}")
        outcome = run_once(cargo_args)
        print("::endgroup::")

        if outcome.passed:
            print(f"{outcome.kind}: {outcome.detail} (attempt {attempt}/{args.attempts})")
            verdict("completed", f"{outcome.detail} (attempt {attempt}/{args.attempts})")
            return 0

        for annotation in outcome.annotations:
            print(f"::error::{annotation}")

        if outcome.kind != "CRASH":
            print(f"::error::{outcome.kind}: {outcome.detail}")
            verdict(
                "named failures" if outcome.kind == "FAILED" else "did not run",
                f"{outcome.kind}: {outcome.detail}",
            )
            return 1

        print(f"::warning::CRASH on attempt {attempt}/{args.attempts}: {outcome.detail}")

    # **Deliberately not "this is more than R15 has needed".** That is what this
    # message used to say, and it was measured false the first time it fired:
    # six consecutive crashes on the quarantined step, and the binary that died
    # -- integrity_property_tests -- then faulted 1 in 6 runs *in isolation*
    # with exit 0xC0000005. At p ~ 0.6 a run of six has about a 5% chance, so a
    # budget of six will produce this message roughly one run in twenty with
    # nothing wrong. Telling the reader it must be real would train them to
    # disbelieve it.
    #
    # **And 5% was optimistic by an order of magnitude** (0.12.0, D-147). The
    # step itself was measured at 93% per attempt, n = 100, which puts six in a
    # row at 65%. The reasoning above stands and its input did not: p ~ 0.6 came
    # from one binary in one session at n = 15. The rate lives in
    # .cargo/config.toml, including the caveats, and is not restated here.
    #
    # So it says what to do instead, which is what .cargo/config.toml has always
    # advised: a binary that fails alone is a real failure, one that only fails
    # in the suite is almost certainly R15.
    print(
        f"::error::CRASH: {args.attempts} consecutive attempts died without a "
        f"summary, every one of them R15's shape. On the quarantined step this "
        f"is the EXPECTED outcome more often than not -- measured at 93% per "
        f"attempt, six in a row is about 65% of runs (.cargo/config.toml, "
        f"D-147). This message said 'roughly 1 run in 20' until 0.12.0, from a "
        f"rate nobody had measured on this step. Before treating it as real, "
        f"run the named binary on its own a few times: alone it should be "
        f"clean, and exit 0xC0000005 there is still R15."
    )
    verdict(
        "crashed-R15",
        f"{args.attempts} attempts, none with a summary, zero named failures. "
        f"Read before trusting: a binary that fails ALONE is a real failure "
        f"(.cargo/config.toml, D-147).",
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
