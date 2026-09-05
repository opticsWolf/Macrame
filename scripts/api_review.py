"""Diff two checked-in public-API baselines *by identity* rather than by line.

    python scripts/api_review.py v0.15.0                 # against a git tag
    python scripts/api_review.py v0.15.0 --out docs/architecture/api-review-0.16.0.md

`cargo-public-api` reports one line per **path**, so an item reachable at three
paths is three lines and a module demotion reads as a mass removal. The raw
line diff across a release cycle is uninformative; D-212 turned it into a
review with three collapses, and this is that method made executable::

    1. `pub mod` lines are namespace, not item (D-208), and are counted apart.
    2. Paths collapse to identities: `macrame::a::b::Name` and
       `macrame::prelude::Name` are one item. The item's own path keeps
       everything from its first type-like segment — `Tuning::cadence`, not
       `cadence`, because two types may both have a `new` — and crate paths
       *inside* a signature reduce to their last segment. Non-crate paths
       (`core::time::Duration`) are left whole, because a change in one of
       those is a change.
    3. `#[non_exhaustive]` is stripped into a flag, since it renders inline on
       the item's own line and is a decision worth reporting on its own.

What survives all three is a genuine addition or a genuine removal.

Why this script exists at all
-----------------------------
`api-review-0.14.0.md` says *"Regenerate with the script recorded in
[D-212]"* — and the script was never checked in, in that entry or anywhere
else. Its own header points at `scripts/../`, a path that was never filled in.
So the file that D-205's rule was quoted over — *a review nobody can re-run is
a review nobody can check* — was itself unre-runnable for eighteen releases.
This closes that (0.15.13, W15.3, D-255).

Why it reads baselines instead of running the tool
--------------------------------------------------
`docs/architecture/public-api.txt` is checked in and blessed in the same commit
as the change it describes (D-205), so the surface at any past release is
`git show <tag>:docs/architecture/public-api.txt`. The 0.14.0 review needed a
worktree and a nightly toolchain to see 0.13.0 because the baseline did not
exist yet. It does now, which makes the review a text operation on two files
and reproducible on any machine.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = "docs/architecture/public-api.txt"

# `macrame::a::b::Name` -> `Name`. Anchored on the crate name so that
# `core::option::Option` and `alloc::string::String` are left alone: a move
# inside *this* crate is a path change, and a change in one of those is not.
CRATE_PATH = re.compile(r"\bmacrame(?:::[A-Za-z0-9_]+)+")
LEADING = re.compile(r"^macrame(?:::[A-Za-z0-9_]+)*")

# The declaration keywords `cargo-public-api` puts before an item's path. Order
# matters: the longest match wins, or `pub fn` is read as a bare `pub `.
PREFIXES = (
    "pub async unsafe fn ",
    "pub unsafe fn ",
    "pub async fn ",
    "pub const fn ",
    "pub struct ",
    "pub enum ",
    "pub trait ",
    "pub union ",
    "pub const ",
    "pub type ",
    "pub macro ",
    "pub fn ",
    "pub ",
)


def strip_modules(path: str) -> str:
    """`macrame::connection::Tuning::cadence` -> `Tuning::cadence`.

    Leading lowercase segments are modules and are dropped; everything from the
    first segment that is not one is kept, because that is where the item's own
    name begins. A free function or a const has no such segment, so the last
    one is kept alone -- which is what a re-exported name collapses to anyway.
    """
    segs = path.split("::")[1:]
    if not segs:
        return "macrame"
    i = 0
    while i < len(segs) - 1 and segs[i][:1].islower():
        i += 1
    return "::".join(segs[i:])


def collapse_inner(text: str) -> str:
    """Crate paths inside a signature, reduced to the name they end with."""
    return CRATE_PATH.sub(lambda m: m.group(0).rsplit("::", 1)[-1], text)


def identity(line: str) -> str:
    """One item line, reduced to the identity it names."""
    for prefix in PREFIXES:
        if line.startswith(prefix):
            rest = line[len(prefix) :]
            break
    else:
        return collapse_inner(line)
    m = LEADING.match(rest)
    if not m:
        return prefix + collapse_inner(rest)
    return prefix + strip_modules(m.group(0)) + collapse_inner(rest[m.end() :])


class Surface:
    """A baseline, read the three ways the review needs it."""

    def __init__(self, text: str, label: str) -> None:
        self.label = label
        self.lines = [l for l in text.splitlines() if l.strip()]
        self.modules: set[str] = set()
        self.items: set[str] = set()
        self.non_exhaustive: set[str] = set()
        self.paths = 0

        for line in self.lines:
            if line.startswith("pub mod "):
                # A module's identity *is* its path, so this one is not
                # collapsed: D-208 demoted twenty-five of them, and the point
                # of counting is to be able to see which.
                self.modules.add(
                    line[len("pub mod macrame") :].lstrip(":") or "(root)"
                )
                continue
            # `impl` lines are the grouping the tool prints, not items.
            if line.startswith("impl ") or line.startswith("pub impl "):
                continue
            flagged = line.startswith("#[non_exhaustive] ")
            if flagged:
                line = line[len("#[non_exhaustive] ") :]
            ident = identity(line)
            self.paths += 1
            self.items.add(ident)
            if flagged:
                self.non_exhaustive.add(ident)

    @property
    def surplus(self) -> int:
        """Paths beyond one per item — what D-208 spent a release reducing."""
        return self.paths - len(self.items)


def read_tag(tag: str) -> str:
    out = subprocess.run(
        ["git", "show", f"{tag}:{BASELINE}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if out.returncode != 0:
        sys.exit(
            f"cannot read {BASELINE} at {tag}: {out.stderr.strip()}\n"
            "The baseline has been checked in since 0.13.32; before that this "
            "script cannot help and the 0.14.0 review's worktree method is the "
            "only one."
        )
    return out.stdout


def report(old: Surface, new: Surface) -> str:
    out: list[str] = []
    w = out.append

    w(f"{old.label:<8}: {len(old.lines)} lines, {len(old.modules)} modules, "
      f"{len(old.items)} distinct items")
    w(f"{new.label:<8}: {len(new.lines)} lines, {len(new.modules)} modules, "
      f"{len(new.items)} distinct items")
    w(f"net lines: {len(new.lines) - len(old.lines):+d}   "
      f"net items: {len(new.items) - len(old.items):+d}")
    w("")

    gone_mods = sorted(old.modules - new.modules)
    new_mods = sorted(new.modules - old.modules)
    w(f"--- modules ({len(old.modules)} -> {len(new.modules)}) ---")
    w(f"demoted: {len(gone_mods)}   new: {len(new_mods)}")
    for m in gone_mods:
        w(f"  - {m}")
    for m in new_mods:
        w(f"  + {m}")
    w("")

    ne_added = sorted(new.non_exhaustive - old.non_exhaustive)
    ne_gone = sorted(old.non_exhaustive - new.non_exhaustive)
    w(f"--- non_exhaustive ({len(old.non_exhaustive)} -> "
      f"{len(new.non_exhaustive)}) ---")
    for i in ne_gone:
        w(f"  - {i}")
    for i in ne_added:
        w(f"  + {i}")
    w("")

    w(f"--- surplus paths on items present in both: {old.surplus} -> "
      f"{new.surplus} ---")
    w("")

    removed = sorted(old.items - new.items)
    added = sorted(new.items - old.items)
    w(f"=== REMOVED FROM THE SURFACE: {len(removed)} ===")
    for i in removed:
        w(f"  {i}")
    w("")
    w(f"=== ADDED TO THE SURFACE: {len(added)} ===")
    for i in added:
        w(f"  {i}")
    return "\n".join(out)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("old", help="git tag or revision to compare against")
    ap.add_argument(
        "--new",
        default=None,
        help="git revision for the new side; default is the working tree",
    )
    ap.add_argument("--out", default=None, help="write the report to this file")
    args = ap.parse_args()

    old = Surface(read_tag(args.old), args.old)
    if args.new:
        new = Surface(read_tag(args.new), args.new)
    else:
        new = Surface((ROOT / BASELINE).read_text(encoding="utf-8"), "working")

    text = report(old, new)
    if args.out:
        Path(args.out).write_text(text + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {args.out}")
    else:
        print(text)


if __name__ == "__main__":
    main()
