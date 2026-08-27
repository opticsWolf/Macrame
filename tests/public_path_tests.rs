//! Every public item has exactly one canonical path (0.13.35, W11.2d, D-208).
//!
//! # The rule
//!
//! One **canonical** path per item, plus two convenience surfaces that carry
//! *names* and never *namespaces*: the crate root and [`macrame::prelude`]. So
//! `macrame::DbError`, `macrame::prelude::DbError` and `macrame::error::DbError`
//! are one item at one canonical path with two flat aliases — fine — while
//! `macrame::graph::AttributeMode` beside `macrame::graph::builder::AttributeMode`
//! is one item at two canonical paths, and both are supported for the life of
//! 1.x once 1.0 ships.
//!
//! The difference matters because a canonical path names a **module**, and a
//! public module freezes the file layout: `AttributeMode` could not move out of
//! `builder.rs` without a major version, for a path nobody chose to offer.
//!
//! # Why this is a test and not a review
//!
//! [D-205](../../docs/architecture/s13-decision-register.md) found 39 public
//! modules by generating the surface once. The modules were not added in one
//! sitting — they accreted, one `pub mod` at a time, each invisible in its own
//! diff. Reviewing the list once fixes the list once. Only an assertion stops it
//! re-accreting, which is why this outlives the release that needed it.
//!
//! # Why it reads the baseline rather than measuring
//!
//! The surface itself is nightly-only (`cargo-public-api` reads rustdoc's
//! unstable JSON), so measuring here would put a nightly toolchain in the path
//! of `cargo test`. `docs/architecture/public-api.txt` is checked in and kept
//! true by the CI job in
//! [D-205](../../docs/architecture/s13-decision-register.md); this reads that
//! file and checks its *shape*. The chain is: the nightly job keeps the file
//! honest, and these tests keep the file's structure honest, in the stable loop
//! where the person adding a `pub mod` is already working.

use std::collections::{BTreeMap, BTreeSet};

const BASELINE: &str = include_str!("../docs/architecture/public-api.txt");

/// Every public module, and adding one is a decision made here.
///
/// The crate root and ten top-level modules, plus the three inner modules that
/// are the canonical home of what they hold rather than a second path to it:
///
/// - `connection::chunk_rows` — `chunk_rows::EDGES` and `chunk_rows::CONCEPTS`
///   are only readable qualified. Flattened they are `connection::EDGES`, which
///   says nothing about what it counts.
/// - `schema::ddl` — twenty-two DDL constants whose module *is* the
///   qualification.
/// - `util::timestamp` — `timestamp::parse` and `timestamp::format` flatten to
///   `util::parse` and `util::format`, which are too generic to be anyone's API.
///
/// None of the three is re-exported by its parent, so each holds exactly one
/// path and none of them is the duplication D-205 found. `prelude` is here
/// because it is a module; the tests below pin that it carries no others, and
/// `macrame` is the crate root, which the listing reports as a module like any
/// other.
const PUBLIC_MODULES: &[&str] = &[
    "macrame",
    "macrame::connection",
    "macrame::connection::chunk_rows",
    "macrame::error",
    "macrame::graph",
    "macrame::integrity",
    "macrame::metrics",
    "macrame::prelude",
    "macrame::schema",
    "macrame::schema::ddl",
    "macrame::temporal",
    "macrame::util",
    "macrame::util::timestamp",
    "macrame::vector",
];

/// The `macrame::…` path a listing line is about.
fn subject(line: &str) -> Option<&str> {
    let start = line.find("macrame::")?;
    let rest = &line[start..];
    let end = rest
        .char_indices()
        .find(|(i, c)| {
            !(c.is_alphanumeric()
                || *c == '_'
                || (*c == ':' && rest[*i..].starts_with("::"))
                || (*c == ':' && rest[..*i].ends_with(':')))
        })
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches(':'))
}

/// The module chain of a path: the segments before the item.
///
/// `macrame::graph::builder::AttributeMode` → `["graph", "builder"]`.
/// `macrame::graph::astar` → `["graph"]`. `macrame::DbError` → `[]`, which is
/// the crate root and one of the two convenience surfaces.
///
/// An uppercase segment is where the item starts, because everything below it
/// is a variant, a field or an associated item rather than a module. Where
/// there is none — a free function or a constant — the last segment is the item.
fn module_chain(path: &str) -> Vec<&str> {
    let seg: Vec<&str> = path.split("::").skip(1).collect();
    match seg
        .iter()
        .position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))
    {
        Some(i) => seg[..i].to_vec(),
        None => seg[..seg.len().saturating_sub(1)].to_vec(),
    }
}

/// A flat alias rather than a canonical path: the crate root, or the prelude.
fn is_convenience(path: &str) -> bool {
    let chain = module_chain(path);
    chain.is_empty() || chain == ["prelude"]
}

/// One line, with its own path replaced by the item it names.
///
/// Two lines that reduce to the same string are the same item seen at two
/// paths. The rest of the line — the kind, the signature, the field type — comes
/// along, so two unrelated functions that happen to share a short name are not
/// collapsed into one.
fn identity(line: &str, path: &str) -> String {
    let seg: Vec<&str> = path.split("::").skip(1).collect();
    let tail = match seg
        .iter()
        .position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))
    {
        Some(i) => seg[i..].join("::"),
        None => seg.last().copied().unwrap_or_default().to_string(),
    };
    line.replacen(path, &tail, 1)
}

fn items() -> Vec<(String, String)> {
    BASELINE
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && (!l.starts_with('#') || l.starts_with("#[")))
        .filter(|l| !l.starts_with("pub mod "))
        .filter_map(|l| subject(l).map(|p| (identity(l, p), p.to_string())))
        .collect()
}

fn declared_modules() -> BTreeSet<String> {
    BASELINE
        .lines()
        .filter_map(|l| l.strip_prefix("pub mod "))
        .map(|p| p.trim().to_string())
        .collect()
}

#[test]
fn every_public_item_has_exactly_one_canonical_path() {
    let mut by_item: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (ident, path) in items() {
        if !is_convenience(&path) {
            by_item.entry(ident).or_default().insert(path);
        }
    }

    let dupes: Vec<_> = by_item.iter().filter(|(_, v)| v.len() > 1).collect();
    let surplus: usize = dupes.iter().map(|(_, v)| v.len() - 1).sum();

    let mut report = String::new();
    for (ident, paths) in dupes.iter().take(40) {
        report.push_str(&format!("\n  {ident}\n      {:?}", paths));
    }

    assert!(
        dupes.is_empty(),
        "{} items are reachable at more than one canonical path ({surplus} \
         surplus paths). Each surplus path is a module this crate would have to \
         keep, and a file it could then never move, for the life of 1.x. The \
         fix is to make the inner module `pub(crate)` and let the parent's \
         `pub use` be the one path (D-208). The first 40:{report}",
        dupes.len()
    );
}

#[test]
fn the_public_module_list_is_the_one_recorded_here() {
    let declared = declared_modules();
    let expected: BTreeSet<String> = PUBLIC_MODULES.iter().map(|s| s.to_string()).collect();

    let added: Vec<_> = declared.difference(&expected).collect();
    assert!(
        added.is_empty(),
        "public modules that this file does not account for: {added:?}. A `pub \
         mod` is a path the crate supports for the life of 1.x and a file it \
         cannot then move. Adding one is fine; adding one silently is what \
         produced thirty-nine of them (D-205). Say why here."
    );

    let gone: Vec<_> = expected.difference(&declared).collect();
    assert!(
        gone.is_empty(),
        "this file expects public modules the surface does not have: {gone:?}"
    );
}

#[test]
fn the_convenience_surfaces_carry_names_and_not_namespaces() {
    let nested: Vec<_> = declared_modules()
        .into_iter()
        .filter(|m| m.starts_with("macrame::prelude::"))
        .collect();

    assert!(
        nested.is_empty(),
        "the prelude re-exports whole modules: {nested:?}. A prelude exists so \
         that one `use` brings in the names a caller needs; re-exporting a \
         module brings in a second *namespace*, which is a second canonical \
         path to everything inside it and a second name for the module itself. \
         `pub use crate::temporal::{{archive, …}}` is how this happens by \
         accident: `archive` is both a module and a function there, an explicit \
         import binds the name in both namespaces, and re-exporting the \
         `pub(crate)` module through a `pub use` is neither an error nor a \
         warning — it silently republishes it here (D-208)."
    );
}

#[test]
fn the_baseline_is_shaped_the_way_these_tests_assume() {
    // A parser that silently matches nothing passes every test above. This is
    // the floor that says it did not.
    let all = items();
    assert!(
        all.len() > 1_000,
        "only {} item lines parsed out of the baseline; the format changed and \
         these tests are measuring nothing",
        all.len()
    );
    assert!(
        declared_modules().len() >= 10,
        "only {} modules parsed; see above",
        declared_modules().len()
    );
    assert_eq!(
        module_chain("macrame::graph::builder::AttributeMode"),
        ["graph", "builder"]
    );
    assert_eq!(module_chain("macrame::graph::astar"), ["graph"]);
    assert!(module_chain("macrame::DbError").is_empty());
    assert!(is_convenience("macrame::prelude::DbError"));
    assert!(!is_convenience("macrame::error::DbError"));
}
