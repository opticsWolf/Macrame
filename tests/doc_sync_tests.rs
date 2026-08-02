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
        (
            "raw",
            "#[doc(hidden)] — D-068/D-091: reachable, not advertised",
        ),
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

// ---------------------------------------------------------------------------
// A decision that names a release must not outlive it (0.8.0, A5, D-113)
// ---------------------------------------------------------------------------
//
// The failure this catches has already happened, twice, and is the reason the
// 0.8.0 plan exists. D-087 and D-089 both read "Scheduled for 0.7.0". 0.7.0
// shipped as the Python bindings release with neither of them in it, and
// **nothing anywhere went red** — a scheduled decision is prose, and prose is
// not executed. `doc_link_tests` was satisfied throughout, because it checks
// that anchors resolve and both entries' anchors resolved perfectly.
//
// So the register is read as data: an entry that names a version it is waiting
// for must either be waiting for one still in the future, or say plainly that
// it arrived.
//
// # Why the pattern is narrow, deliberately
//
// D-088 is the precedent, and `every_performance_decision_names_its_fixture`
// learned it the hard way: a tripwire over prose that fires on prose gets
// disabled, and a disabled tripwire is worse than none, because its name goes
// on suggesting coverage. Two narrowings follow.
//
// **A version must look like a version.** The phrase alone is not enough. This
// register says "deferred until something measures it" (D-047) and "deferred to
// the release that implements erasure" (D-084). Both are honest deferrals with
// no date to miss, and a check that fired on them would be instructing a
// maintainer to invent a release number in order to go green.
//
// **The delivery marker is a token that cannot arise by accident.** All-caps
// `DELIVERED`. Matching "delivered" case-insensitively would be satisfied by
// any entry that happens to discuss delivery, which is most of them — the check
// would then pass by coincidence rather than by intent, which is precisely the
// defect D-030 found in `audit_current()`, where a query that reduced to a
// constant zero certified every corruption as clean.
//
// # What this does not cover, said plainly so the gap is not read as coverage
//
// Only `s13-decision-register.md`. Scheduling language in a README row, a plan
// document or a source comment is not scanned. The register is where decisions
// are *authoritative*; widening the scan is a larger change than this tripwire,
// not a free one.

const REGISTER: &str = include_str!("../docs/architecture/s13-decision-register.md");

/// How an entry closes out a release it named. **The marker must name that
/// release**, and both halves of that are load-bearing.
///
/// *All caps*, so ordinary prose cannot produce it by accident.
///
/// *Naming the release* is what keeps the tripwire armed. A bare `DELIVERED`
/// anywhere in the entry would settle it forever -- including the **next**
/// release the entry goes on to name, which is the very failure being guarded.
/// Keyed to a version, closing out 0.7.0 says nothing about 0.8.0, and the
/// entry comes due again at the next boundary by itself.
///
/// Two closures, because there are two honest outcomes and only one is delivery:
///
/// * `DELIVERED in 0.8.0` -- it shipped in the release it named.
/// * `RESCHEDULED from 0.7.0` -- that release came and went without it, and the
///   entry says what happens now.
///
/// **The original sentence stays as written.** It was true when it was made,
/// and Doctrine III governs this register as much as it governs `links`: a
/// belief is superseded by a later one, never rewritten in place. Rewording
/// "Scheduled for 0.7.0" out of existence would make the test green by
/// destroying the evidence that it was ever wrong.
fn closure_markers(version: &str) -> [String; 2] {
    [
        format!("DELIVERED in {version}"),
        format!("RESCHEDULED from {version}"),
    ]
}

/// The phrases that make a sentence a *schedule* rather than a description.
const SCHEDULING: &[&str] = &[
    "scheduled for",
    "scheduled in",
    "deferred to",
    "deferred until",
    "revisit at",
    "revisited at",
    "revisit in",
];

/// A scheduling claim found in the register.
struct Claim {
    /// The decision that made it, e.g. `D-087`.
    entry: String,
    phrase: String,
    version: String,
    /// Byte offset of the phrase, used to locate the entry it sits in.
    at: usize,
}

/// `(major, minor, patch)` for ordering. `0.8` reads as `0.8.0`.
fn semver(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// A version-looking token at the start of `rest`, tolerating the leading
/// backtick and `v` the register actually uses.
///
/// At least `major.minor` is required, so "deferred to 2 releases later" and a
/// stray full stop are not versions.
fn version_at(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let token = token.trim_end_matches('.').to_string();
    if token.contains('.') && semver(&token).is_some() {
        Some(token)
    } else {
        None
    }
}

/// The id of the decision entry whose anchor most recently precedes `offset`.
fn entry_containing(text: &str, offset: usize) -> String {
    match text[..offset].rmatch_indices("<a id=\"d-").next() {
        Some((i, m)) => {
            let id: String = text[i + m.len()..].chars().take_while(|c| *c != '"').collect();
            format!("D-{id}")
        }
        None => "(no enclosing decision entry)".to_string(),
    }
}

/// The whole entry containing `offset`: its anchor through to the next one.
fn entry_body(text: &str, offset: usize) -> &str {
    let start = text[..offset]
        .rmatch_indices("<a id=\"d-")
        .next()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[start + 1..]
        .find("<a id=\"d-")
        .map(|i| start + 1 + i)
        .unwrap_or(text.len());
    &text[start..end]
}

/// Is the phrase at `at` being **quoted** rather than asserted?
///
/// Found by the test firing on itself. [D-113](../docs/architecture/s13-decision-register.md)
/// explains the failure by quoting it — *"Scheduled for 0.7.0"* — and was
/// reported as a decision overdue for a release it has no stake in. That is a
/// class, not an instance: a register that records its own history will go on
/// quoting the schedules it is recording, and every such quotation would be a
/// false positive.
///
/// The rule is narrow on purpose. A quotation mark **immediately** before the
/// phrase, and nothing cleverer: an entry that quotes a schedule is citing
/// somebody else's, while an entry that makes one writes it as prose. Both real
/// claims in this register — D-087 and D-089 — are preceded by a space, and
/// neither is exempted.
///
/// The gap this leaves, said rather than hidden: a genuine schedule written as
/// the first words inside quotation marks is missed. No entry has ever been
/// written that way, and the alternative — deciding *which* quotations are
/// citations — is the kind of cleverness [D-088](../docs/architecture/s13-decision-register.md)
/// warns produces a test nobody trusts.
fn is_quoted(text: &str, at: usize) -> bool {
    matches!(
        text[..at].chars().last(),
        Some('"') | Some('\'') | Some('\u{201c}') | Some('\u{2018}')
    )
}

/// Every scheduling claim that names an actual version.
fn scheduling_claims(text: &str) -> Vec<Claim> {
    let lower = text.to_lowercase();
    let mut found: Vec<Claim> = SCHEDULING
        .iter()
        .flat_map(|phrase| {
            lower.match_indices(phrase).filter_map(move |(at, _)| {
                if is_quoted(text, at) {
                    return None;
                }
                version_at(&text[at + phrase.len()..]).map(|version| Claim {
                    entry: entry_containing(text, at),
                    phrase: (*phrase).to_string(),
                    version,
                    at,
                })
            })
        })
        .collect();
    found.sort_by_key(|c| c.at);
    found
}

/// A decision that named a release at or below this one must say it arrived.
///
/// **The tripwire the 0.8.0 plan was written around.** Not a style check: the
/// two entries it was built for had been wrong for a whole release while the
/// suite stayed green.
#[test]
fn no_decision_still_awaits_a_release_that_has_shipped() {
    let current =
        semver(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is not a semver triple");

    let overdue: Vec<String> = scheduling_claims(REGISTER)
        .into_iter()
        .filter(|c| semver(&c.version).is_some_and(|v| v <= current))
        // Searched across the whole entry rather than beside the phrase, so one
        // marker settles an entry naming the same release twice -- but keyed to
        // the version, so it settles that release and no other.
        .filter(|c| {
            let body = entry_body(REGISTER, c.at);
            !closure_markers(&c.version).iter().any(|m| body.contains(m))
        })
        .map(|c| format!("{}: \"{} {}\"", c.entry, c.phrase, c.version))
        .collect();

    assert!(
        overdue.is_empty(),
        "current version is {}, and these decisions are still waiting for a \
         release that has already shipped:\n  {}\n\n\
         This is exactly how D-087 and D-089 sat wrong through the whole of \
         0.7.0 with a green suite. Do one of two things, and not a third:\n\
         \x20 - it shipped:     write `DELIVERED in <that version>` into the entry;\n\
         \x20 - it did not ship: write `RESCHEDULED from <that version>`, and say \
         which release it waits for now.\n\
         Leave the original sentence exactly as it stands. It was true when it was \
         written, and a deferral that leaves no trace is the failure this test \
         exists to catch.",
        env!("CARGO_PKG_VERSION"),
        overdue.join("\n  "),
    );
}

/// The pattern reads versions and not prose, in both directions.
///
/// Pinned because both halves are how this class of test dies: too wide and it
/// gets disabled (D-088), too narrow and it is decoration.
#[test]
fn the_schedule_pattern_reads_versions_and_not_prose() {
    for text in [
        "Scheduled for 0.7.0 alongside the other schema work.",
        "scheduled for `0.9.0` with the API break named.",
        "Deferred to 1.0 because the surface freezes there.",
        "revisit at v0.8.0 once the measurement exists.",
    ] {
        assert_eq!(
            scheduling_claims(text).len(),
            1,
            "should have found a scheduling claim in: {text}"
        );
    }

    // Real sentences from this register, and two near misses. Not firing on
    // these is what keeps the test alive to be useful.
    for text in [
        "the integer-index rewrite of `Subgraph` is deferred until something measures it",
        "`rowid_pk` on `concepts` is deferred to the release that implements erasure",
        "deferred to 2 releases later",
        "Scheduled for the next cycle.",
        // A quoted schedule is a citation, not a commitment. D-113 quotes the
        // very sentence it exists to catch, and the first run of this test
        // reported D-113 as overdue for a release it has no stake in.
        "D-087 and D-089 both read *\"Scheduled for 0.7.0\"*, and nothing went red.",
    ] {
        assert!(
            scheduling_claims(text).is_empty(),
            "should not have fired on prose: {text}"
        );
    }
}

/// The delivery marker is what distinguishes shipped from overdue, so both
/// arms are asserted rather than only the one that currently holds.
#[test]
fn the_delivery_marker_is_what_settles_an_overdue_entry() {
    let settled = |text: &str, version: &str| {
        let claim = &scheduling_claims(text)[0];
        let body = entry_body(text, claim.at);
        closure_markers(version).iter().any(|m| body.contains(m))
    };

    let bare = "<a id=\"d-999\"></a>D-999 - a thing. Scheduled for 0.1.0.\n";
    assert_eq!(scheduling_claims(bare)[0].entry, "D-999");
    assert!(!settled(bare, "0.1.0"), "an unmarked entry is overdue");

    let shipped = "<a id=\"d-999\"></a>D-999 - a thing. Scheduled for 0.1.0. \
                   Prose in between. **DELIVERED in 0.1.0.**\n";
    assert!(
        settled(shipped, "0.1.0"),
        "the marker must be found anywhere in the entry, not only beside the phrase"
    );

    let missed = "<a id=\"d-999\"></a>D-999 - a thing. Scheduled for 0.1.0. \
                  **RESCHEDULED from 0.1.0** to 0.2.0.\n";
    assert!(settled(missed, "0.1.0"), "a missed release is closed out too");

    // The property that keeps the tripwire armed: closing 0.1.0 must say
    // nothing about 0.2.0. Without it one marker would silence an entry
    // forever, and the next missed release would pass exactly as 0.7.0 did.
    assert!(
        !settled(missed, "0.2.0"),
        "a marker naming one release must not settle a claim on another"
    );
}
