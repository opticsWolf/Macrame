"""Shared fixtures.

**R15 applies here, and that is measured rather than inherited.** The suite runs
single-process — see the ``addopts`` note in ``pyproject.toml``, and
``.cargo/config.toml`` for the underlying measurement, the refuted churn
hypothesis, and the reporting hazard.

``probes/r15_concurrent_open.py`` reproduced the fault *through the binding* at
**2 in 12 runs** with 48 concurrent opens — the same rate as the Rust control
arm. The GIL does not protect us, and the reason is the point of P1: ``block_on``
releases it, so every thread is genuinely inside a concurrent open, which is what
the fault counts. Do not add ``pytest-xdist`` here.

**How a crash reports here is not how it reports on the Rust side.** ``cargo
test`` runs a process per binary, so a crash leaves a smaller pass count and no
failures — green, and wrong. pytest runs one process, and P6 measured what that
looks like: exit 3 and no summary line at all. The inverse is possible too and is
specific to this extension — ``Drop`` enters the tokio runtime, so a fault during
interpreter teardown lands *after* a green summary is printed.

Run ``python tests_py/run_suite.py`` rather than bare pytest wherever the answer
gates something: it keys on the summary, the counts and the exit code together,
and retries only the crash.
"""

from __future__ import annotations

import gc
from pathlib import Path

import pytest


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    """A fresh database path, in a directory of its own.

    Its own directory because the ledger derives two siblings by convention —
    ``foo_archive.db`` and ``foo_snapshots/`` — and tests that assert on the
    snapshot directory need it not to contain another test's anchors.
    """
    return tmp_path / "ledger.db"


@pytest.fixture(autouse=True)
def _collect_between_tests():
    """Force collection after each test so a leaked handle is *this* test's.

    The ResourceWarning for a handle that was never closed fires from the
    destructor, and CPython's collector is not prompt about cycles. Without
    this, a warning raised by a leak in one test can surface during another,
    which is the kind of cross-talk that gets a real warning dismissed as
    flaky.
    """
    yield
    gc.collect()
