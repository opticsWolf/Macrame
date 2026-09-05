"""Build the Python extension and put it where the suite reads it from.

    python scripts/build_python_ext.py

Two steps, and the second is the one this file exists for::

    cargo build --release -p macrame-py --features extension-module
    copy target/release/_macrame.<dll|so|dylib> -> python/macrame/_macrame.<pyd|so>

Exit 0 when `python/macrame/` holds a freshly built extension whose version is
the one `bindings/python/Cargo.toml` declares. Exit 1 otherwise, saying which
step failed.

Why a script rather than a line in the README
---------------------------------------------
It *was* a line in a README, and the failure it guards against has now happened
three times across two years of this repository.

`tests_py/run_suite.py` runs with `python/` on `PYTHONPATH`, so the suite
imports `python/macrame/__init__.py` and, beside it, `python/macrame/_macrame`
— a **build artifact in the source tree**. Nothing writes that file as a side
effect of anything. `cargo test` does not. `cargo build` does not: it writes
`target/release/`, which no import path looks at.

So the extension goes stale silently, and what the suite then measures is the
last build somebody remembered to copy. Recorded instances:

* 0.12.17 — `site-packages` held a non-editable `macrame-db 0.12.0` that won
  over the editable path entry, and the suite tested a five-release-old
  extension. `test_packaging`'s version check did not catch it, because it
  compared the installed wheel to the installed binding: both stale, and
  agreeing.
* 0.15.10 and 0.15.11 — the same class, reached the other way. The version bump
  landed in `bindings/python/Cargo.toml` and the extension was not rebuilt, so
  `test_packaging` went red with `assert '0.15.9' == '0.15.10'` — a failure that
  reads as a broken manifest and is in fact a stale binary.

The second pair went red, which is the good case. The first did not, and that
is the shape worth designing against: a stale extension whose version happens to
match is a **green suite measuring code that is not in the tree**.

Why not `maturin develop`
-------------------------
Because the obvious invocation is wrong in a way that succeeds. Run from
`bindings/python/`, maturin finds no `pyproject.toml` there, falls back to the
`Cargo.toml`, and builds a distribution called **`macrame_py`** that ships only
`_macrame` — no `macrame/` package, no stubs, and nothing written to
`python/macrame/`. It prints `🛠 Installed macrame-py-0.15.11` and exits 0. The
suite carries on reading whatever was in the source tree beforehand.

The root `pyproject.toml` is the maturin project ([`tool.maturin`] with
`python-source = "python"`), so maturin *from the repo root* would do the right
thing — but it wants a virtualenv, and the released path is `pip install .`
through the PEP 517 backend ([D-107], [§14](../docs/architecture/s14-python-bindings.md)),
which is what CI runs and what a user takes. Neither is a dev loop: one needs an
environment this repository does not create, and the other rebuilds and
reinstalls a wheel to move one file.

`cargo build` plus a copy is what the dev layout actually needs, and naming it
here means the command that keeps the two in sync is the same command every
time, on every platform, rather than a `cp` line remembered from a terminal
three days ago.

[D-107]: ../docs/architecture/s13-decision-register.md#d-107
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import sysconfig
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "bindings" / "python" / "Cargo.toml"
PACKAGE = REPO / "python" / "macrame"

# What cargo writes, and what Python will import it as. The two differ on every
# platform: cargo names a `cdylib` by the platform's library convention and
# CPython looks for the module name plus `EXT_SUFFIX`, and only the copy in
# between makes them the same file.
#
# `EXT_SUFFIX` is read rather than hardcoded because the wheel is abi3: it is
# `.pyd` on Windows and `.abi3.so` or `.cpython-313-x86_64-linux-gnu.so`
# elsewhere depending on the interpreter, and a plain `.so` is not always found.
CDYLIB = {
    "win32": "_macrame.dll",
    "darwin": "lib_macrame.dylib",
}.get(sys.platform, "lib_macrame.so")


def extension_suffix() -> str:
    """`.pyd` on Windows, and whatever this interpreter accepts elsewhere.

    Windows is special-cased rather than trusting `EXT_SUFFIX`, which reports
    `.cp313-win_amd64.pyd` there — a name a *specific* interpreter accepts,
    where the abi3 build is meant to be loadable by any 3.10+. `.pyd` is the
    generic form and is what every wheel this repository ships uses.
    """
    if sys.platform == "win32":
        return ".pyd"
    return sysconfig.get_config_var("EXT_SUFFIX") or ".so"


def declared_version() -> str:
    """The version in the binding's manifest — the one the wheel would carry."""
    text = MANIFEST.read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not match:
        raise SystemExit(f"no version in {MANIFEST}")
    return match.group(1)


def newest_source_mtime() -> tuple[float, Path]:
    """The most recently touched Rust source, and which one it was.

    Both crates: the binding is a thin layer over `macrame-db` and an edit to
    either changes what the extension does. `Cargo.toml` counts too — a version
    bump with no code change is exactly the case that went red twice.
    """
    newest = (0.0, REPO)
    for root in (REPO / "src", REPO / "bindings" / "python" / "src"):
        for path in root.rglob("*.rs"):
            stamp = path.stat().st_mtime
            if stamp > newest[0]:
                newest = (stamp, path)
    for manifest in (REPO / "Cargo.toml", MANIFEST):
        stamp = manifest.stat().st_mtime
        if stamp > newest[0]:
            newest = (stamp, manifest)
    return newest


def build() -> None:
    cmd = [
        "cargo",
        "build",
        "--release",
        "-p",
        "macrame-py",
        # Turned on here for the reason `pyproject.toml` gives for not making it
        # a default: `cargo test -p macrame-py` must build *without* it, or the
        # test binary links against Python symbols only the interpreter has.
        "--features",
        "extension-module",
    ]
    print("$ " + " ".join(cmd), flush=True)
    proc = subprocess.run(cmd, cwd=REPO)
    if proc.returncode != 0:
        raise SystemExit(f"cargo build failed (exit {proc.returncode})")


def install() -> Path:
    built = REPO / "target" / "release" / CDYLIB
    if not built.exists():
        raise SystemExit(
            f"cargo reported success and {built} does not exist. If this "
            f"platform names its cdylib something else, CDYLIB in this file is "
            f"what needs to know."
        )
    dest = PACKAGE / f"_macrame{extension_suffix()}"

    # `copy2` keeps the source's mtime, which would make a freshly installed
    # extension look exactly as old as the build directory it came from — and
    # the staleness check downstream reads mtimes. Copy the bytes, then stamp
    # it now: the question that check asks is "was this installed after the
    # sources changed", and the honest answer is the time of *this* copy.
    shutil.copyfile(built, dest)
    os.utime(dest, None)
    return dest


def verify(dest: Path) -> None:
    """Import the thing that was just installed, in a fresh interpreter.

    A subprocess because this one may already have imported `macrame`, and a
    module cannot be replaced under a running interpreter — an in-process check
    would report on the old extension while claiming to check the new one.
    """
    expected = declared_version()
    proc = subprocess.run(
        [sys.executable, "-c", "import macrame; print(macrame.__version__)"],
        cwd=REPO,
        capture_output=True,
        text=True,
        env={**os.environ, "PYTHONPATH": str(REPO / "python")},
    )
    if proc.returncode != 0:
        raise SystemExit(
            f"the extension was installed to {dest} and does not import:\n"
            f"{proc.stdout}{proc.stderr}"
        )
    got = proc.stdout.strip()
    if got != expected:
        raise SystemExit(
            f"installed {dest} reports {got!r}, and "
            f"bindings/python/Cargo.toml declares {expected!r}. The build did "
            f"not use the manifest this script read, which should not be "
            f"possible — check for a second checkout on PYTHONPATH."
        )
    print(f"ok: macrame {got} at {dest.relative_to(REPO)}")


def staleness() -> str | None:
    """Why the installed extension cannot be trusted, or `None` if it can.

    Shared with `tests_py/run_suite.py`, which refuses to run when this returns
    a reason. Kept here rather than there because the message has to name the
    command that fixes it, and that command is this file.
    """
    dest = PACKAGE / f"_macrame{extension_suffix()}"
    fix = "python scripts/build_python_ext.py"

    if not dest.exists():
        return (
            f"no built extension at {dest.relative_to(REPO)}: the suite would "
            f"import nothing, or something else's. Build it:\n    {fix}"
        )

    newest, source = newest_source_mtime()
    if dest.stat().st_mtime < newest:
        minutes = round((newest - dest.stat().st_mtime) / 60.0)
        age = "a minute" if minutes == 1 else f"{minutes} minutes"
        return (
            f"{dest.relative_to(REPO)} was built {age} before "
            f"{source.relative_to(REPO)} was last edited, so the suite would "
            f"measure code that is not in the tree. Rebuild it:\n    {fix}"
        )

    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check",
        action="store_true",
        help="do not build; report whether the installed extension is current",
    )
    args = parser.parse_args()

    if args.check:
        stale = staleness()
        if stale:
            print(stale, file=sys.stderr)
            return 1
        print("ok: the installed extension is current")
        return 0

    build()
    dest = install()
    verify(dest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
