//! The Python binding covers every variant the ledger can produce (0.13.34,
//! W11.2c, D-207).
//!
//! # What this replaces
//!
//! Until 0.13.33 `bindings/python/src/errors.rs` held a `match` over `DbError`
//! with no wildcard arm, so a new variant upstream failed to *compile* the
//! binding at the line that needed a decision. That was the stronger mechanism.
//! It was traded, on purpose, for `#[non_exhaustive]` on `DbError` — a crate
//! that will certainly add error variants after 1.0 cannot have each addition
//! be a major version, and `#[non_exhaustive]` is what buys that. Its price is
//! that the wildcard arm becomes mandatory, and a wildcard arm is precisely
//! what makes "a new variant quietly falls through to the base class" invisible.
//!
//! So this file exists to make it visible again, and it is honest about being
//! the weaker instrument: it runs rather than compiles, and it reads text, so
//! an arm that exists but is unreachable would satisfy it. What it buys back
//! is placement — it lives in the Rust suite, in the crate that *defines*
//! `DbError`, so it fails for the person who adds the variant, in the command
//! they were already running. The equivalent Python assertions need a built
//! wheel, and the whole risk of a wildcard arm is a variant added by someone
//! who never builds the binding.
//!
//! In one respect it is wider than the compiler was. The compiler checked that
//! every variant had an *arm*. This also checks that every variant is
//! **sampled** for the mapping tests and that no two variants share an
//! exception class — both previously pinned only from Python.
//!
//! # Why it reads from disk
//!
//! `include_str!("../bindings/…")` would be a compile error on the published
//! crate: `bindings/` is deliberately excluded from the `.crate` tarball
//! ([`packaging_tests`]), so the file is simply not there. Reading from disk
//! lets the same test source do the right thing in both places — and when the
//! tree is absent, the property is *vacuous* rather than unverified, because a
//! binding that was not shipped cannot disagree with anything. That is a
//! genuinely different case from D-147's "could not be measured", and it is
//! reported rather than merely returned.
//!
//! [`packaging_tests`]: ../packaging_tests/index.html

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Every `DbError` variant is declared here, and adding one is a ten-step act.
///
/// A count rather than a floor, because every reason this number changes is a
/// reason to look at this file.
///
/// **It said *four* until 0.14.23, and four was wrong** — the list had been
/// read off one variant's commit, so it recorded what that commit happened to
/// touch. Adding `SnapshotWriteFailed` found the rest by failing: `§7` of the
/// architecture set *reproduces* this enum (`doc_sync_tests`), and
/// `python/macrame/__init__.py` re-exports the exception class by name. The
/// measured list is `src/error.rs`, `DbError::kind` beneath it (the compiler
/// enforces that one), `bindings/python/src/errors.rs` — three edits there:
/// the class, the arm, the registration — `bindings/python/src/testing.rs`,
/// this constant, `tests_py/test_errors.py`, `python/macrame/_macrame.pyi`,
/// `python/macrame/__init__.py`, `docs/architecture/s6-s10-flows-to-dependencies.md`,
/// and the blessed `docs/architecture/public-api.txt`.
const DB_ERROR_VARIANTS: usize = 42;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The binding source, or `None` when the tree is absent (a `.crate` tarball).
fn binding(file: &str) -> Option<String> {
    let path = repo()
        .join("bindings")
        .join("python")
        .join("src")
        .join(file);
    if !path.exists() {
        println!(
            "bindings/python/src/{file} is absent, so there is no binding to \
             disagree with the ledger. This is the published-tarball case, not \
             a skipped check."
        );
        return None;
    }
    Some(std::fs::read_to_string(&path).expect("the binding source is valid utf-8"))
}

/// The body of `pub enum NAME {` in a file under `src/`, braces matched.
fn enum_body(rel: &str, name: &str) -> String {
    let src = std::fs::read_to_string(repo().join(rel)).expect("valid utf-8");
    let decl = format!("pub enum {name} {{");
    let after = src
        .split_once(&decl)
        .unwrap_or_else(|| panic!("`{decl}` not found in {rel}"))
        .1;
    let mut depth = 1usize;
    for (i, ch) in after.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return after[..i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in `{name}`");
}

/// Variant names from an enum body.
///
/// Not a parser, and it does not need to be — but it does need to survive
/// attributes, because a wrapped `#[error("…")]` message is the thing that
/// broke the Python version of this in 0.13.5 (W7.4). Skipping only the line
/// that *starts* with `#[` left the continuations in, so
/// `FutureStampPolicy::Allow` — sitting in the message that tells a caller how
/// to open a refused database — was read as a `DbError` variant. Track the
/// attribute's parentheses instead.
fn variant_names(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut attr_depth = 0i32;
    for line in body.lines() {
        let stripped = line.trim();
        if attr_depth == 0 && stripped.starts_with("#[") {
            attr_depth = 1;
        }
        if attr_depth > 0 {
            attr_depth += stripped.matches('(').count() as i32;
            attr_depth -= stripped.matches(')').count() as i32;
            if stripped.ends_with(']') && attr_depth <= 1 {
                attr_depth = 0;
            }
            continue;
        }
        if stripped.is_empty() || stripped.starts_with("//") {
            continue;
        }
        let name: String = stripped
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.starts_with(|c: char| c.is_ascii_uppercase()) {
            names.push(name);
        }
    }
    names
}

/// Source with `//` comment lines removed, so a variant named in prose is not
/// mistaken for a match arm.
fn without_comments(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Variant name -> the exception class its arm raises, from `fn build`.
///
/// Arms are ordered and each holds exactly one `raise::<Class, _>`, so the text
/// between one `DbError::` and the next is that variant's arm. Extracted at
/// 0.14.25 so the C-3 test below reads the same mapping rather than spelling a
/// second one — which is the defect that test exists to prevent, and it would
/// be a poor gate that reproduced it.
fn class_of_variant(src: &str) -> BTreeMap<String, String> {
    let build = src
        .split_once("fn build(")
        .expect("`fn build` is the mapping")
        .1;
    let mut class_of = BTreeMap::new();
    let starts: Vec<usize> = build.match_indices("DbError::").map(|(i, _)| i).collect();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(build.len());
        let arm = &build[start..end];
        let variant: String = arm["DbError::".len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let class = arm
            .split_once("raise::<")
            .map(|(_, rest)| {
                rest.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .unwrap_or_else(|| panic!("`DbError::{variant}`'s arm raises nothing"));
        class_of.insert(variant, class);
    }
    class_of
}

/// How many times each `Enum::Variant` is written in `src`, for one enum.
///
/// A count and not a set, because a set answers the wrong question. `types.rs`
/// converts `AttributeMode` in *both* directions, so deleting one arm leaves
/// the variant's name in the file and a set-difference sees nothing missing —
/// which is what the first version of this file did, and what the mutation run
/// caught. Counts make the omission visible without needing to know where the
/// conversion sites are or how many there will be next time.
fn mentions(src: &str, enum_name: &str) -> BTreeMap<String, usize> {
    let needle = format!("{enum_name}::");
    let mut out = BTreeMap::new();
    for (i, _) in src.match_indices(&needle) {
        // `AttributeMode::` is a suffix of `PyAttributeMode::`, and counting
        // the binding's own mirror enum alongside the ledger's would double
        // every count in step -- invisible, because the rule below compares
        // counts to each other.
        let preceded = src[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if preceded {
            continue;
        }
        let rest = &src[i + needle.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.starts_with(|c: char| c.is_ascii_uppercase()) {
            *out.entry(name).or_insert(0) += 1;
        }
    }
    out
}

fn declared_db_error_variants() -> Vec<String> {
    variant_names(&enum_body("src/error.rs", "DbError"))
}

// ---------------------------------------------------------------------------
// The count
// ---------------------------------------------------------------------------

#[test]
fn the_db_error_variant_count_is_the_one_recorded_here() {
    let declared = declared_db_error_variants();
    assert_eq!(
        declared.len(),
        DB_ERROR_VARIANTS,
        "`DbError` has {} variants and this file says {DB_ERROR_VARIANTS}. If a \
         variant was added, it needs an arm in bindings/python/src/errors.rs, an \
         entry in that crate's `DB_ERROR_VARIANTS`, a row in \
         tests_py/test_errors.py's `EXPECTED`, and this number updated. \
         Declared: {declared:?}",
        declared.len()
    );
}

// ---------------------------------------------------------------------------
// The mapping
// ---------------------------------------------------------------------------

#[test]
fn every_db_error_variant_has_an_arm_in_the_binding() {
    let Some(src) = binding("errors.rs") else {
        return;
    };
    let declared: BTreeSet<String> = declared_db_error_variants().into_iter().collect();
    let matched: BTreeSet<String> = mentions(&without_comments(&src), "DbError")
        .into_keys()
        .collect();

    let missing: Vec<_> = declared.difference(&matched).collect();
    assert!(
        missing.is_empty(),
        "these `DbError` variants have no arm in bindings/python/src/errors.rs, \
         so they reach Python as the bare `MacrameError` the wildcard arm \
         raises: {missing:?}. Until 0.13.33 this was a compile error; \
         `#[non_exhaustive]` traded that away and this test is the replacement \
         (D-207)."
    );

    let stale: Vec<_> = matched.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "bindings/python/src/errors.rs names `DbError` variants that no longer \
         exist in src/error.rs: {stale:?}"
    );
}

#[test]
fn every_db_error_variant_reaches_a_class_of_its_own() {
    let Some(src) = binding("errors.rs") else {
        return;
    };
    let class_of = class_of_variant(&without_comments(&src));

    let mut by_class: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (variant, class) in &class_of {
        by_class.entry(class).or_default().push(variant);
    }
    let shared: Vec<_> = by_class.iter().filter(|(_, v)| v.len() > 1).collect();
    assert!(
        shared.is_empty(),
        "two or more variants share an exception class: {shared:?}. Sharing is \
         the quiet form of flattening — a caller who catches the class is back \
         to reading the message to find out what happened."
    );
}

#[test]
fn every_db_error_variant_is_sampled_for_the_mapping_tests() {
    let Some(src) = binding("testing.rs") else {
        return;
    };
    let declared: BTreeSet<String> = declared_db_error_variants().into_iter().collect();

    let list = src
        .split_once("DB_ERROR_VARIANTS: &[&str] = &[")
        .expect("the sample table")
        .1
        .split_once("];")
        .expect("the sample table ends")
        .0;
    let sampled: BTreeSet<String> = list
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();

    assert_eq!(
        declared, sampled,
        "the binding's `DB_ERROR_VARIANTS` and `src/error.rs` disagree. A \
         variant that is mapped but never sampled has a plausible-looking arm \
         nothing has ever executed."
    );
}

// ---------------------------------------------------------------------------
// The domain enums that cross the boundary
// ---------------------------------------------------------------------------

/// The other enums `#[non_exhaustive]` reached, and the files that convert them.
///
/// Only two of the nine are converted in the binding; the rest never cross. The
/// conversions are infallible `From` impls, so their wildcard arms panic rather
/// than return a wrong hydration mode or a wrong strategy name — which makes
/// this test the thing standing between a new variant and a panic in a wheel.
const CONVERTED: &[(&str, &str, &str)] = &[
    ("src/graph/builder.rs", "AttributeMode", "types.rs"),
    (
        "src/graph/vector_filter.rs",
        "VectorFilterStrategy",
        "vector.rs",
    ),
];

#[test]
fn every_variant_of_a_converted_domain_enum_is_converted_everywhere_its_peers_are() {
    for &(rel, name, file) in CONVERTED {
        let Some(src) = binding(file) else {
            return;
        };
        let declared = variant_names(&enum_body(rel, name));
        let counted = mentions(&without_comments(&src), name);

        let stale: Vec<_> = counted.keys().filter(|k| !declared.contains(k)).collect();
        assert!(
            stale.is_empty(),
            "bindings/python/src/{file} names `{name}` variants that no longer \
             exist in {rel}: {stale:?}"
        );

        // Every variant, the same number of times. One occurrence per
        // conversion site, so an equal count means every site handles every
        // variant — without this test needing to know how many sites there are
        // or where they live. Unequal is the interesting answer either way: a
        // variant handled once where its peers are handled twice is a missing
        // arm, and a variant handled twice where its peers are handled once is
        // a site somebody special-cased.
        let per: BTreeSet<usize> = declared
            .iter()
            .map(|v| counted.get(v).copied().unwrap_or(0))
            .collect();
        assert!(
            per.len() == 1 && !per.contains(&0),
            "`{name}` is converted unevenly in bindings/python/src/{file}: \
             {counted:?}. Every variant should appear once per conversion site. \
             The wildcard arms there are `unreachable!`, so a variant that is \
             short an arm is a panic in a released wheel — this test is where \
             that is supposed to be caught."
        );
    }
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

#[test]
fn the_variant_parser_survives_a_wrapped_attribute() {
    // The 0.13.5 defect, as a fixture: a `#[error("…")]` whose message wraps
    // onto a line beginning with a capitalised path.
    let body = r#"
    /// Doc.
    #[error(
        "refused; open with FutureStampPolicy::Allow to inspect it, then \
         Tolerance(d) to carry on"
    )]
    RealVariant { field: String },

    #[error("plain")]
    Another,
"#;
    assert_eq!(variant_names(body), vec!["RealVariant", "Another"]);
}

/// `DbError::kind` classifies each variant the way the Python hierarchy does
/// (0.14.25, C-3, [D-242]).
///
/// [`ErrorKind`] is deliberately **not** a new taxonomy: it is the one the
/// bindings have published since they existed — seven groups a caller can catch
/// as a set, and five failures that belong to no group. Two spellings of one
/// taxonomy is [D-227]'s finding waiting to happen, so this test is the thing
/// that makes them one: for every variant, the group `kind()` assigns and the
/// base class `errors.rs` derives from must agree.
///
/// It reads text on both sides and inherits this file's stated weakness. What
/// it cannot drift past is the direction that matters — the compiler already
/// refuses a variant with no arm in `kind()`, so the only way to get here with
/// a mismatch is to classify it twice, differently.
///
/// [D-227]: ../docs/architecture/s13-decision-register.md#d-227
/// [D-242]: ../docs/architecture/s13-decision-register.md#d-242
#[test]
fn the_kind_of_a_variant_and_the_base_of_its_exception_agree() {
    let Some(errors_rs) = binding("errors.rs") else {
        return;
    };

    // ErrorKind -> the Python base class that kind means. Written out rather
    // than derived, because this table IS the claim under test.
    let base_of_kind: BTreeMap<&str, &str> = [
        ("Integrity", "IntegrityError"),
        ("Validation", "ValidationError"),
        ("Vector", "VectorError"),
        ("Temporal", "TemporalError"),
        ("Writer", "WriterError"),
        ("Budget", "BudgetError"),
        ("Branch", "BranchError"),
        ("Cancelled", "MacrameError"),
        ("Diagnostic", "MacrameError"),
        ("Engine", "MacrameError"),
        ("Migration", "MacrameError"),
        ("NotFound", "MacrameError"),
    ]
    .into_iter()
    .collect();

    let kind_of = kind_arms();
    let variants = variant_names(&enum_body("src/error.rs", "DbError"));
    assert_eq!(
        kind_of.len(),
        variants.len(),
        "`DbError::kind` classifies {} variants and the enum has {}. The \
         compiler enforces the arms, so this is the parser losing one -- fix \
         `kind_arms`, not `kind()`.",
        kind_of.len(),
        variants.len()
    );

    // class -> base, from `create_exception!(macrame, Class, Base, "...")`.
    let mut base_of_class: BTreeMap<String, String> = BTreeMap::new();
    for (i, _) in errors_rs.match_indices("create_exception!(") {
        let rest = &errors_rs[i + "create_exception!(".len()..];
        let head: Vec<String> = rest
            .split(',')
            .take(3)
            .map(|f| f.trim().to_string())
            .collect();
        if head.len() == 3 && head[0] == "macrame" {
            base_of_class.insert(head[1].clone(), head[2].clone());
        }
    }

    let class_of = class_of_variant(&without_comments(&errors_rs));
    let mut disagreements = Vec::new();
    for variant in &variants {
        let (Some(kind), Some(class)) = (kind_of.get(variant), class_of.get(variant)) else {
            continue; // covered, and named, by the tests above
        };
        let expected = base_of_kind
            .get(kind.as_str())
            .unwrap_or_else(|| panic!("`ErrorKind::{kind}` has no row in this test's table"));
        let actual = base_of_class
            .get(class)
            .unwrap_or_else(|| panic!("`{class}` is raised but never declared"));
        if actual != expected {
            disagreements.push(format!(
                "{variant}: kind() says {kind} (-> {expected}), {class} derives from {actual}"
            ));
        }
    }

    assert!(
        disagreements.is_empty(),
        "one taxonomy, two answers -- the Rust classification and the Python \
         hierarchy disagree about {} variant(s):\n  {}",
        disagreements.len(),
        disagreements.join("\n  ")
    );

    // A kind nothing produces is a name with nothing behind it.
    let used: BTreeSet<&String> = kind_of.values().collect();
    let unused: Vec<&&str> = base_of_kind
        .keys()
        .filter(|k| !used.contains(&k.to_string()))
        .collect();
    assert!(
        unused.is_empty(),
        "`ErrorKind` has {} variant(s) no `DbError` produces: {unused:?}",
        unused.len()
    );
}

/// Variant name -> `ErrorKind` name, read out of `DbError::kind`'s match.
///
/// The arms are `Self::A { .. } | Self::B => ErrorKind::K,` across as many
/// lines as rustfmt wants, so names accumulate until an arrow names the kind.
fn kind_arms() -> BTreeMap<String, String> {
    let src = std::fs::read_to_string(repo().join("src/error.rs")).expect("valid utf-8");
    let body = src
        .split_once("pub fn kind(&self) -> ErrorKind {")
        .expect("`DbError::kind` is where C-3 put it")
        .1;

    let mut out = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    for line in without_comments(body).lines() {
        let line = line.trim();
        if line == "}" && pending.is_empty() && !out.is_empty() {
            break;
        }
        for (i, _) in line.match_indices("Self::") {
            let name: String = line[i + "Self::".len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                pending.push(name);
            }
        }
        if let Some((_, tail)) = line.split_once("=> ErrorKind::") {
            let kind: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            for name in pending.drain(..) {
                out.insert(name, kind.clone());
            }
        }
    }
    out
}
