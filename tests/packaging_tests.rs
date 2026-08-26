//! The Python bindings must not cost the Rust crate anything (P0, D-098).
//!
//! Adding `bindings/python` as a workspace member is safe only because of a
//! specific Cargo behaviour: when a workspace root is *itself* a package,
//! unspecified `default-members` is that package alone. Verified with
//! `cargo metadata` — `workspace_default_members` reports `macrame-db` and
//! nothing else — which is what keeps `cargo test`, `cargo clippy
//! --all-targets`, `cargo check --all-features` and `cargo publish` scoped to
//! this crate, with pyo3 compiled only when something names it.
//!
//! That property is one line away from being lost, in two different ways, and
//! **neither failure is loud**. Adding `default-members` makes every `cargo
//! test` build pyo3 and the libSQL amalgamation a second time; turning the root
//! into a virtual manifest changes what bare cargo commands mean everywhere.
//! Both would be noticed as "the build got slow" or "publish broke", weeks
//! later. These are the tripwires.
//!
//! # Why this file only reads the root manifest
//!
//! It would be natural to also assert that `pyproject.toml`'s `module-name`
//! agrees with `[lib] name` in `bindings/python/Cargo.toml`, since a mismatch
//! there is an `ImportError` at first import and nothing catches it at build
//! time. **That check cannot live here**, and the reason is the property this
//! file exists to protect: Cargo excludes a subdirectory carrying its own
//! `Cargo.toml`, and the `exclude` list drops `pyproject.toml`, so neither file
//! is in the `.crate` tarball. An `include_str!` of either compiles here and is
//! broken for everyone who gets the crate from crates.io.
//!
//! **And `cargo publish` would not catch it**, which makes the trap worse
//! rather than better: the verify step *builds* the unpacked tarball, it does
//! not test it, so a missing file referenced only from `tests/` sails through a
//! clean `--dry-run` and surfaces as a compile error in somebody else's
//! `cargo test`. Same hazard as the one that kept the crate at the repo root —
//! `tests/` reaches `docs/` and `src/` exactly this way, and those are in the
//! tarball only because the package root is the repo root.
//!
//! So that agreement is asserted where it can be — `tests_py/test_packaging.py`
//! proves it empirically by importing the built wheel, which is a better test
//! of it anyway.

const MANIFEST: &str = include_str!("../Cargo.toml");

/// Lines with comments and indentation stripped, so a `#`-commented example of
/// a key cannot satisfy or trip an assertion about the key itself.
fn directives() -> Vec<&'static str> {
    MANIFEST
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The root is still a package, not a virtual manifest.
///
/// The single change that would undo everything else here. A virtual root has
/// no default package, so `default-members` falls back to *all* members and
/// every bare cargo command picks up the binding; `cargo publish` at the root
/// stops having a package to publish at all.
#[test]
fn the_workspace_root_is_still_a_package() {
    let d = directives();
    assert!(
        d.contains(&"[package]"),
        "the root manifest no longer declares [package]. A virtual workspace \
         root has no default package, so bare `cargo test` would build every \
         member — including the pyo3 binding — and `cargo publish` here would \
         have nothing to publish."
    );
    assert!(
        d.iter().any(|l| l.starts_with("name = \"macrame-db\"")),
        "the root package is no longer macrame-db"
    );
}

/// `default-members` stays absent, which is what scopes bare cargo commands.
///
/// Spelling it out — even as `default-members = ["."]`, which looks harmless
/// and equivalent — is worth failing on: the moment the key exists, the next
/// person to add a member has an obvious-looking place to add it, and the
/// separation is gone with no other symptom than a slower build.
#[test]
fn the_workspace_declares_no_default_members() {
    let offender = directives()
        .into_iter()
        .find(|l| l.starts_with("default-members"));
    assert!(
        offender.is_none(),
        "Cargo.toml declares `{}`. Leave it absent: for a workspace root that \
         is itself a package, Cargo already defaults to that package alone, and \
         an explicit list is where `bindings/python` gets added by accident — \
         after which every `cargo test` rebuilds pyo3 and the libSQL \
         amalgamation, silently.",
        offender.unwrap()
    );
}

/// The binding is a member, and it is the only one.
///
/// Pinned as a set rather than as a substring so that adding a second member
/// is a decision someone makes here, in the file that explains what membership
/// costs, rather than a line that slides in.
#[test]
fn the_only_workspace_member_is_the_python_binding() {
    let d = directives();
    let members = d
        .iter()
        .find(|l| l.starts_with("members"))
        .expect("no `members` key under [workspace]");
    assert_eq!(
        members.replace(' ', ""),
        "members=[\"bindings/python\"]",
        "workspace membership changed. Every member is built by any command \
         that is not scoped to a package, so this list is a build-cost decision."
    );
}

/// The Python tree stays out of the `.crate` tarball.
///
/// The package root is the repo root — deliberately, since `tests/` reaches
/// `docs/` and `src/` through `include_str!` — so *everything* at the root is
/// packaged unless excluded. Shipping `python/` and `pyproject.toml` inside a
/// crates.io tarball is not an error and produces no warning; it just quietly
/// puts a second, unbuildable copy of the wheel's packaging in front of anyone
/// reading the crate.
///
/// `bindings/` is deliberately not in the list and does not need to be: Cargo
/// skips a subdirectory carrying its own `Cargo.toml`. Confirmed with
/// `cargo package --list`, which reports the same top-level entries as
/// before the workspace existed — 105 files then, 154 at 0.13.32, and none of
/// them under `bindings/`. The count is prose and not asserted: it moves
/// whenever a doc or a script is added, and a number that changes for reasons
/// unrelated to the claim is not worth gating on.
#[test]
fn the_python_tree_is_excluded_from_the_crate_tarball() {
    let text = MANIFEST.replace(' ', "");
    for needle in ["\"pyproject.toml\"", "\"python/\"", "\"tests_py/\""] {
        assert!(
            text.contains(needle),
            "`exclude` no longer names {needle}. The package root is the repo \
             root, so anything unlisted ships to crates.io — silently, since \
             an over-broad tarball is not an error."
        );
    }
}
