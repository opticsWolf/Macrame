"""P8: the stub describes the extension, and keeps describing it.

A ``.pyi`` is documentation that a type checker reads, and it has the failure
mode all documentation has — it is not executed, so nothing notices when it
stops being true. The specific way it goes wrong here is narrow and predictable:
**a method added in Rust and never stubbed.** It works perfectly at runtime, and
a checker reports `"Database" has no attribute "…"` at the one call site that
uses it.

So the stub is compared against the extension in **both** directions, class by
class and member by member. The same shape as ``test_errors.py``'s exhaustive
match and ``test_packaging.py``'s ``__all__`` comparison, and for the same
reason: the surface is wide, hand-maintained in two places, and the drift is
silent.

Exception *attributes* cannot be checked this way — the Rust mapping layer sets
them on the raised instance, so they exist on an error that was raised and on
nothing else. Those are compared against ``errors.rs`` itself instead, which is
where they are written.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

import pytest

import macrame
import macrame._macrame as ext

REPO = Path(__file__).resolve().parent.parent
STUB = REPO / "python" / "macrame" / "_macrame.pyi"

# Names the stub declares that have no runtime counterpart, deliberately.
# `BulkProgress` is a `TypedDict` describing the dict a `progress=` callback is
# handed (0.13.8) — a shape a checker enforces at the call site, with no class
# to export, which is the same reason `Timestamp` and `Embedding` are here.
STUB_ONLY = {"Timestamp", "Embedding", "Edge", "BulkProgress"}

# Inherited from `Exception`/`object`; every exception class has them and none
# of them is this project's to describe.
INHERITED = {"args", "add_note", "with_traceback"}

# Not inherited, but universal for the same practical purpose: `written` is set
# on every raised instance by `errors.rs`'s two construction sites rather than
# by any one arm (0.13.9, D-182), so it belongs to no class in particular. The
# stub declares it once on `MacrameError`; the per-class comparisons below would
# otherwise read that as a declaration `errors.rs` never makes, since the regex
# that reads the arms cannot see a setattr that is outside all of them.
# `test_every_raised_error_carries_written` in test_errors.py is what actually
# holds this one to the runtime.
UNIVERSAL = {"written"}

# What the per-class comparisons ignore: members no single class owns.
IGNORED = INHERITED | UNIVERSAL


@pytest.fixture(scope="module")
def stub() -> ast.Module:
    return ast.parse(STUB.read_text(encoding="utf-8"))


def _declared(body: list[ast.stmt]) -> set[str]:
    """Every name a stub body declares: defs, classes, and annotations."""
    out: set[str] = set()
    for node in body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            out.add(node.name)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            out.add(node.target.id)
        elif isinstance(node, ast.Assign):
            out.update(t.id for t in node.targets if isinstance(t, ast.Name))
    return out


def _classes(stub: ast.Module) -> dict[str, ast.ClassDef]:
    return {n.name: n for n in stub.body if isinstance(n, ast.ClassDef)}


# Dunders that change how an object is *used* rather than how it prints, so a
# checker needs them: `for n in graph`, `len(graph)`, `with db as …`. Everything
# pyo3 generates unasked — the six rich-comparison slots, `__int__` on the
# enums, `__new__` — is left out on purpose. Requiring those would make this test
# a transcript of pyo3's codegen, and it would go red on a pyo3 upgrade that
# changed nothing about this library's surface.
BEHAVIOURAL_DUNDERS = {"__len__", "__iter__", "__contains__", "__getitem__", "__enter__", "__exit__"}


def _runtime_members(cls: type) -> set[str]:
    """The public surface a caller can reach, plus the dunders that matter."""
    inherited: set[str] = set()
    for base in cls.__mro__[1:]:
        inherited |= set(vars(base))
    public = {k for k in dir(cls) if not k.startswith("_") and k not in inherited}
    return public | (BEHAVIOURAL_DUNDERS & set(vars(cls)))


def test_the_stub_names_every_public_name_the_extension_exports(stub):
    """Direction one: nothing in the extension is missing from the stub.

    This is the failure the file exists to catch. Adding a `#[pymethod]` in Rust
    and forgetting the stub costs nothing at runtime, so nothing reports it — the
    symptom is a type error in somebody else's editor, months later.
    """
    runtime = {n for n in dir(ext) if not n.startswith("_")} | {"__version__"}
    missing = runtime - _declared(stub.body)
    assert not missing, (
        f"the extension exports {sorted(missing)} and the stub does not declare "
        f"them. Add them to python/macrame/_macrame.pyi."
    )


def test_the_stub_invents_nothing(stub):
    """Direction two: a stub may not describe a surface that does not exist.

    Cheaper to break and easier to miss — a renamed method leaves the old name in
    the stub, so a checker approves a call that raises `AttributeError`.
    """
    extra = _declared(stub.body) - {n for n in dir(ext)} - STUB_ONLY
    assert not extra, (
        f"the stub declares {sorted(extra)}, which the extension does not export. "
        f"Either it was renamed in Rust, or the stub is describing a plan."
    )


def test_every_stubbed_class_matches_the_real_one_member_for_member(stub):
    """The wide surface, where drift actually happens.

    `Database` alone has 40 members across five phases' worth of Rust modules.
    Comparing the classes as wholes is the only check that scales with it.
    """
    problems: list[str] = []
    for name, node in _classes(stub).items():
        # A stub-only class describes a shape rather than an object — the
        # `BulkProgress` TypedDict is the dict a `progress=` callback receives,
        # and there is nothing at runtime to compare it against (0.13.8).
        if name in STUB_ONLY:
            continue
        cls = getattr(ext, name)
        # Exception attributes live on the instance, not the class — checked
        # against errors.rs by the next test instead.
        if issubclass(cls, Exception):
            continue
        declared = _declared(node.body) - IGNORED
        # `__init__` in a stub describes what `__new__` accepts at runtime.
        stubbed = {k for k in declared if not k.startswith("_") or k in BEHAVIOURAL_DUNDERS}
        real = _runtime_members(cls) - IGNORED
        for name_ in declared - stubbed - {"__init__"}:
            if not hasattr(cls, name_):
                problems.append(f"{name}: stub declares {name_}, which does not exist")
        if missing := real - stubbed:
            problems.append(f"{name}: not in the stub: {sorted(missing)}")
        if extra := stubbed - real:
            problems.append(f"{name}: in the stub only: {sorted(extra)}")
    assert not problems, "\n".join(problems)


def test_exception_attributes_match_the_mapping_layer(stub):
    """The one part of the stub the runtime cannot confirm.

    `errors.rs` sets these with `setattr` on the raised instance, so they exist
    on an error that was raised and nowhere else — `hasattr(SomeError, "reason")`
    is False, correctly. The source that writes them is therefore the only thing
    that can be compared against, and it is also the thing that changes when a
    `DbError` variant gains a field.
    """
    src = (REPO / "bindings" / "python" / "src" / "errors.rs").read_text(encoding="utf-8")

    # Each arm reads `raise::<SomeError, _>(py, m, |e| { e.setattr("a", …)… })`.
    # Splitting on the raise sites keeps each arm's setattrs with their class.
    arms = re.split(r"raise::<(\w+),", src)[1:]
    from_rust: dict[str, set[str]] = {}
    for cls, body in zip(arms[::2], arms[1::2]):
        from_rust[cls] = set(re.findall(r'setattr\("(\w+)"', body))

    assert from_rust, "no raise sites found — has errors.rs been restructured?"

    problems: list[str] = []
    for name, node in _classes(stub).items():
        if name in STUB_ONLY:
            continue
        cls = getattr(ext, name)
        if not issubclass(cls, Exception):
            continue
        stubbed = _declared(node.body) - IGNORED
        # `UNIVERSAL` comes off both sides. `written` is set outside every arm
        # (0.13.9, D-182), and the split above has no end delimiter -- whatever
        # follows the last `raise::<…>` in the file is folded into that arm's
        # body, so `closed_error`'s central default would otherwise be reported
        # as a field of whichever class happens to be matched last.
        actual = from_rust.get(name, set()) - UNIVERSAL
        if missing := actual - stubbed:
            problems.append(f"{name}: errors.rs sets {sorted(missing)}, stub does not declare it")
        if extra := stubbed - actual:
            problems.append(f"{name}: stub declares {sorted(extra)}, errors.rs never sets it")
    assert not problems, "\n".join(problems)


def test_a_raised_error_really_carries_what_the_stub_promises():
    """And the mapping layer is itself checked against a raised error.

    Without this the chain is stub → source → nothing: two documents agreeing
    with each other. `_raise_db_error` constructs a real `DbError` per variant
    and pushes it through the same mapping a failed write uses.
    """
    for variant in ext._db_error_variants():
        try:
            ext._raise_db_error(variant)
        except macrame.MacrameError as e:
            cls = type(e).__name__
            node = _classes(ast.parse(STUB.read_text(encoding="utf-8"))).get(cls)
            assert node is not None, f"{cls} is raised and not stubbed at all"
            for attr in _declared(node.body) - IGNORED:
                assert hasattr(e, attr), (
                    f"the stub says {cls}.{attr} exists; a raised {cls} has no "
                    f"such attribute"
                )
        else:
            pytest.fail(f"_raise_db_error({variant!r}) raised nothing")


def test_ci_type_checks_the_stub():
    """The half of stub correctness this file cannot reach.

    Every test above compares *names*. None of them can see a wrong **type** — a
    stub claiming ``-> int`` where the runtime answers a ``datetime`` passes all
    of them, and is exactly the error a stub exists to prevent. Only a type
    checker reads annotations, so one has to run somewhere, and a checker that is
    not in CI is a checker that ran once.
    """
    workflow = (REPO / ".github" / "workflows" / "python.yml").read_text(encoding="utf-8")
    assert re.search(r"mypy --strict python/macrame", workflow), (
        "no `mypy --strict python/macrame` step in python.yml — nothing checks "
        "that the stub's types are right, only that its names are"
    )


def test_the_stub_and_the_marker_ship_with_the_extension():
    """Both are inert files, and both are silently droppable.

    ``py.typed`` is PEP 561: without it a checker ignores the stub entirely, and
    nothing anywhere reports that — the types simply become ``Any`` and every
    call type-checks. The stub without the marker is a file nobody reads; the
    marker without the stub is a promise of types that do not exist.

    Checked against the *installed* package rather than the repository, because
    that is where being packaged is decided. ``maturin`` copies the whole
    ``python-source`` package directory, so this passes today — and it is a
    default, not a declaration, which is exactly the kind of thing a
    ``pyproject.toml`` edit turns off by accident.
    """
    installed = Path(macrame.__file__).resolve().parent
    assert (installed / "py.typed").is_file(), f"no py.typed in {installed}"
    assert (installed / "_macrame.pyi").is_file(), (
        f"no _macrame.pyi in {installed} — the stub is in the repository and not "
        f"in what gets installed, so no user of the wheel ever sees it"
    )
