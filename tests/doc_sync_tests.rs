//! The normative documents match the code they describe.
//!
//! Two of the architecture set's documents are *copies* of the source rather
//! than descriptions of it: [§7](../docs/architecture/s6-s10-flows-to-dependencies.md)
//! reproduces `DbError`, and [Appendix A](../docs/architecture/appendices.md) —
//! marked **normative** — enumerates the public API. Both were hand-maintained,
//! and by 0.7.0 both had rotted:
//!
//! - §7 was **eleven variants** behind, and named `SingleOpenViolation`'s fields
//!   `source` / `target`, which the code cannot use because `thiserror` reserves
//!   `source`. So it did not merely omit; it described a type that could not
//!   compile.
//! - Appendix A cited nothing past D-075 and was missing the entire 0.6.0
//!   surface — `diagnostic_conn`, `verify_snapshot_chain`,
//!   `rebuild_current_chunked`, `shadow_step`, `archive_windowed`,
//!   `estimated_bulk_hold`, `metrics`, `path`.
//!
//! A normative document that does not describe the surface is worse than none,
//! because it is cited. These tests make the drift a build failure.
//!
//! # Deliberately shallow
//!
//! §7 is checked on the **set of variant names**, not on field types or message
//! strings; Appendix A on whether each public `Database` method is *mentioned*,
//! not on whether its signature is right. A stricter check would fail on
//! reformatting and get relaxed the first time it cried wolf. What is being
//! guarded is the failure that actually happened — something was added to the
//! code and nobody added it here — and a name is enough to catch that.

use std::collections::BTreeSet;

const ERROR_RS: &str = include_str!("../src/error.rs");
const CONNECTION_RS: &str = include_str!("../src/connection.rs");
const SECTION_7: &str = include_str!("../docs/architecture/s6-s10-flows-to-dependencies.md");
const APPENDIX_A: &str = include_str!("../docs/architecture/appendices.md");

/// The body of `braced` block that follows `header`, brace-matched.
fn block_after<'a>(text: &'a str, header: &str) -> &'a str {
    let start = text
        .find(header)
        .unwrap_or_else(|| panic!("{header:?} not found"))
        + header.len();
    let rest = &text[start..];
    let mut depth = 1usize;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after {header:?}");
}

/// Variant names declared at one level of indentation inside an enum body.
fn variant_names(body: &str) -> BTreeSet<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            // Skip doc comments, comments and attributes; a variant starts with
            // an uppercase letter.
            if trimmed.starts_with("//") || trimmed.starts_with('#') {
                return None;
            }
            // Only at the enum's own indentation — nested struct fields are
            // lowercase anyway, but the depth check keeps this honest.
            let indent = line.len() - trimmed.len();
            if indent != 4 {
                return None;
            }
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            match name.chars().next() {
                Some(c) if c.is_ascii_uppercase() => Some(name),
                _ => None,
            }
        })
        .collect()
}

/// **§7 reproduces every `DbError` variant, and only those.**
#[test]
fn the_documented_error_enum_matches_the_code() {
    let in_code = variant_names(block_after(ERROR_RS, "pub enum DbError {"));
    let in_docs = variant_names(block_after(SECTION_7, "pub enum DbError {"));

    assert!(
        in_code.len() >= 27,
        "only {} variants parsed out of src/error.rs — the parser has broken, \
         not the docs",
        in_code.len()
    );
    assert_eq!(
        in_code,
        in_docs,
        "§7's copy of DbError has drifted.\n  missing from the docs: {:?}\n  \
         in the docs but not the code: {:?}\n\
         §7 is a reproduction of src/error.rs; regenerate it rather than \
         patching one variant.",
        in_code.difference(&in_docs).collect::<Vec<_>>(),
        in_docs.difference(&in_code).collect::<Vec<_>>(),
    );
}

/// Public `async` methods on `Database`, by name.
fn public_database_methods() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in CONNECTION_RS.lines() {
        let t = line.trim_start();
        for prefix in ["pub async fn ", "pub fn "] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.insert(name);
                }
            }
        }
    }
    out
}

/// **Appendix A mentions every public method on the handle.**
///
/// The exemptions are named individually rather than pattern-matched, because
/// "this does not belong in the normative surface" is a judgement that should
/// have to be written down.
#[test]
fn every_public_database_method_appears_in_appendix_a() {
    // Deliberately undocumented, each for a stated reason.
    const EXEMPT: &[(&str, &str)] = &[
        ("raw", "#[doc(hidden)] — D-068/D-091: reachable, not advertised"),
        (
            "new",
            "constructors of other types in the same file, not handle methods",
        ),
        (
            "start",
            "`Turn::start`, an internal actor helper in the same file",
        ),
        ("elapsed", "`HoldTimer::elapsed`, internal"),
        ("epoch", "`Turn::epoch`, internal"),
        (
            "estimated_bulk_hold",
            "free function, documented in A.1 under its own name",
        ),
        ("content", "`ConceptUpsert` builder setter"),
        ("embedding_model", "`ConceptUpsert` builder setter"),
        ("valid_from", "builder setter"),
        ("valid_to", "builder setter"),
        ("retired", "`ConceptUpsert` builder setter"),
        ("normalized", "builder finaliser, described in prose"),
        ("chunk_rows", "module, not a method"),
    ];
    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(n, _)| *n).collect();

    let missing: Vec<String> = public_database_methods()
        .into_iter()
        .filter(|m| !exempt.contains(m.as_str()))
        .filter(|m| !APPENDIX_A.contains(m.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "Appendix A is normative and does not mention: {missing:?}\n\
         Add them to A.1, or add an entry to EXEMPT saying why they do not \
         belong in the public surface."
    );
}
