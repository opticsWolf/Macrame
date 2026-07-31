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
