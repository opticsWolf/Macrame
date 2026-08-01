"""P0 acceptance: the wheel builds, imports, and has the engine inside it.

The Rust side of this is ``tests/packaging_tests.rs``, which pins the workspace
shape. It deliberately stops short of the cross-manifest agreements, because
neither ``pyproject.toml`` nor ``bindings/python/Cargo.toml`` is in the crate
tarball and an ``include_str!`` of either would compile locally and then fail
during ``cargo publish``'s verify build. Those checks live here, where they can
be made against the artifact rather than against text.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import pytest

import macrame

REPO = Path(__file__).resolve().parent.parent


def _read(path: Path) -> str:
    # UTF-8 explicitly: these manifests carry non-ASCII (the description has a
    # `·`), and Windows' default encoding is not UTF-8.
    return path.read_text(encoding="utf-8")


def test_the_extension_imports_from_the_package():
    """``import macrame`` resolves, which means module-name matched [lib] name.

    The failure this catches is the one nothing catches at build time: maturin
    writes the compiled object to wherever ``module-name`` says, and if that
    disagrees with ``[lib] name`` in the binding manifest the wheel builds
    perfectly and raises ImportError on first use.
    """
    assert macrame._macrame is not None
    assert macrame.__version__


def test_the_libsql_engine_is_linked_into_the_extension():
    """The actual question P0 exists to answer.

    An extension module exporting only constants would import cleanly while
    telling us nothing about whether a cdylib in this workspace can bind the
    statically-linked libsql-ffi amalgamation. ``engine_linked()`` takes the
    address of ``Database::open``, which forces the engine into the link — so
    this passing means the rest of the plan is buildable, and it failing at P0
    would mean the plan needs rewriting before any of P1 is worth starting.
    """
    assert macrame.engine_linked() is True


def test_the_chunk_budget_crosses_intact():
    """The crate's one cross-cutting number, unchanged through the boundary."""
    assert macrame.chunk_budget_ms() == 3


def test_the_wheel_version_matches_the_binding_crate():
    """``__version__`` comes from the binding manifest, so this pins the wiring.

    maturin takes the wheel version from ``bindings/python/Cargo.toml``. If it
    ever silently fell back to something else, a released wheel would report a
    version no manifest in the repository claims.
    """
    manifest = _read(REPO / "bindings" / "python" / "Cargo.toml")
    declared = re.search(r'^version\s*=\s*"([^"]+)"', manifest, re.M)
    assert declared, "no version in bindings/python/Cargo.toml"
    assert macrame.__version__ == declared.group(1)


def test_the_binding_tracks_the_ledger_version():
    """The wheel and the crate underneath it report the same version.

    Not cosmetic. A wheel at 0.7.1 wrapping a ledger at 0.6.0 makes every bug
    report cite a version whose behaviour is not the behaviour observed, and
    nothing else in either build would notice — the path dependency does not
    check versions.
    """
    binding = _read(REPO / "bindings" / "python" / "Cargo.toml")
    root = _read(REPO / "Cargo.toml")
    b = re.search(r'^version\s*=\s*"([^"]+)"', binding, re.M)
    r = re.search(r'^version\s*=\s*"([^"]+)"', root, re.M)
    assert b and r
    assert b.group(1) == r.group(1), (
        f"macrame-py is {b.group(1)} and macrame-db is {r.group(1)}. "
        "Bump them together, or the wheel reports a version the ledger "
        "underneath it does not have."
    )


def test_the_binding_crate_is_not_published_to_crates_io():
    """`publish = false` keeps `cargo publish` a one-package operation.

    A source crate for the bindings would be a second way to build the same
    thing, free to drift out of step with pyproject.toml.
    """
    manifest = _read(REPO / "bindings" / "python" / "Cargo.toml")
    assert re.search(r"^publish\s*=\s*false", manifest, re.M)


def test_extension_module_is_not_a_default_feature():
    """It must be off for `cargo test -p macrame-py` and on only via maturin.

    With ``pyo3/extension-module`` enabled, the object links against Python
    symbols that only the interpreter loading it supplies, so a plain
    ``cargo build`` of the binding fails at link time on Linux and macOS. The
    feature is therefore declared but never defaulted, and pyproject.toml turns
    it on.
    """
    manifest = _read(REPO / "bindings" / "python" / "Cargo.toml")
    assert 'extension-module = ["pyo3/extension-module"]' in manifest
    assert not re.search(r"^default\s*=.*extension-module", manifest, re.M)

    pyproject = _read(REPO / "pyproject.toml")
    assert 'features = ["extension-module"]' in pyproject


def test_the_package_ships_its_typing_marker():
    """PEP 561: without ``py.typed`` the stubs added in P8 are never consulted."""
    assert (Path(macrame.__file__).parent / "py.typed").is_file()


@pytest.mark.skipif(sys.version_info < (3, 10), reason="requires-python is >=3.10")
def test_abi3_floor_matches_requires_python():
    """The abi3 feature and ``requires-python`` must name the same floor.

    They are set in different files and neither validates the other. A wheel
    built ``abi3-py310`` but tagged ``>=3.9`` installs on 3.9 and crashes on
    import; tagged ``>=3.11`` it simply refuses installs that would have worked.
    """
    binding = _read(REPO / "bindings" / "python" / "Cargo.toml")
    pyproject = _read(REPO / "pyproject.toml")

    abi3 = re.search(r"abi3-py3(\d+)", binding)
    assert abi3, "no abi3 feature on the pyo3 dependency"

    floor = re.search(r'requires-python\s*=\s*">=3\.(\d+)"', pyproject)
    assert floor, "no requires-python in pyproject.toml"

    assert abi3.group(1) == floor.group(1), (
        f"pyo3 is abi3-py3{abi3.group(1)} but requires-python is "
        f">=3.{floor.group(1)}. The wheel's real floor is the abi3 one."
    )


# ---------------------------------------------------------------------------
# P5: the public surface, and the matrix that ships it
# ---------------------------------------------------------------------------


def test_every_name_the_extension_exports_is_re_exported():
    """``__all__`` is hand-written, and the extension's exports are not.

    P4 added twelve classes and four constants across five Rust modules, each
    registered in `lib.rs` and each needing a second, manual entry in
    ``python/macrame/__init__.py``. Forget one and it is invisible: importable
    only as ``macrame._macrame.Thing``, absent from ``dir(macrame)``, absent
    from ``from macrame import *``, and absent from the stubs P8 generates from
    this list. Nothing fails — the wheel builds and every test passes — which is
    exactly the kind of gap that survives a release.
    """
    import macrame._macrame as ext

    exported = {n for n in dir(ext) if not n.startswith("_")}
    re_exported = set(macrame.__all__)

    missing = sorted(exported - re_exported)
    assert not missing, (
        f"{len(missing)} name(s) registered in the extension but not re-exported "
        f"from `macrame`: {missing}. Add them to the import block and to "
        f"`__all__` in python/macrame/__init__.py."
    )


def test_everything_in_all_actually_exists():
    """The other direction, which breaks louder but is worth pinning here too.

    A name in ``__all__`` that was never imported makes ``from macrame import *``
    raise ``AttributeError`` — and only that form, so an ordinary
    ``import macrame`` in every test would not notice.
    """
    for name in macrame.__all__:
        assert hasattr(macrame, name), f"__all__ names {name!r}, which does not exist"


def test_public_classes_claim_the_package_rather_than_the_extension():
    """``module = "macrame"`` on every ``#[pyclass]``.

    pyo3 defaults a class's ``__module__`` to the extension module, so a
    forgotten attribute gives ``macrame._macrame.Subgraph`` in every repr and
    traceback — naming a private module the caller is told not to import — and
    sends ``pickle`` looking there too.
    """
    wrong = {
        name: obj.__module__
        for name in macrame.__all__
        if isinstance(obj := getattr(macrame, name), type)
        and obj.__module__ != "macrame"
    }
    assert not wrong, f"classes not claiming the `macrame` module: {wrong}"


def test_the_wheel_workflow_covers_the_platforms_the_plan_names():
    """The matrix is a claim about what a user can `pip install`.

    Dropping a target is a one-line edit with no local symptom: the wheels build,
    CI is green, and the platform that lost its wheel finds out at install time
    by falling back to a source build that needs a Rust toolchain it may not
    have. `musllinux` is deliberately absent — out of scope for 0.7.0 until
    someone asks, since it doubles the Linux matrix for a `libsql-ffi` build
    nobody here has checked against musl.
    """
    workflow = _read(REPO / ".github" / "workflows" / "wheels.yml")
    for target in (
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "universal2-apple-darwin",
        "x86_64-pc-windows-msvc",
    ):
        assert target in workflow, f"{target} is not in the wheel matrix"
    assert "command: sdist" in workflow, "no sdist job — the no-wheel fallback is unbuilt"
    assert "--no-binary :all:" in workflow, (
        "the sdist job does not force a source install, so it proves only that a "
        "tarball was produced, not that it builds"
    )


def test_the_wheel_workflow_holds_no_api_token():
    """Trusted Publishing, so there is no long-lived secret in this repository.

    Asserted rather than assumed because the easy fix for a failing upload is to
    paste a token into the workflow, and that change looks small in review.
    """
    workflow = _read(REPO / ".github" / "workflows" / "wheels.yml")
    assert "secrets." not in workflow, (
        "wheels.yml references a secret. PyPI uploads here use Trusted "
        "Publishing (id-token: write); a token in this file is a long-lived "
        "credential that does not need to exist."
    )
    assert "id-token: write" in workflow
