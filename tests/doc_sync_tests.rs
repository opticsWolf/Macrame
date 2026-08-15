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
            let id: String = text[i + m.len()..]
                .chars()
                .take_while(|c| *c != '"')
                .collect();
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
    assert!(
        settled(missed, "0.1.0"),
        "a missed release is closed out too"
    );

    // The property that keeps the tripwire armed: closing 0.1.0 must say
    // nothing about 0.2.0. Without it one marker would silence an entry
    // forever, and the next missed release would pass exactly as 0.7.0 did.
    assert!(
        !settled(missed, "0.2.0"),
        "a marker naming one release must not settle a claim on another"
    );
}

// ---------------------------------------------------------------------------
// The `Subgraph` surface is documented where it is quoted (0.8.0, B1, D-114)
// ---------------------------------------------------------------------------
//
// `docs/quickref.md` reproduces the three analytics types **verbatim**, fields
// and all, and that block was correct until B1 made every one of those fields
// private. A copied declaration is the same failure mode §7 and Appendix A had
// (this file's opening comment): it is not executed, and nothing notices when
// it stops being true.
//
// Two directions, and the second is the one that actually rots:
//
// * no document may still advertise a **field** that is now private — the
//   specific untruth B1 introduces;
// * every public **method** on `Subgraph` must appear in the quoted block —
//   the drift that accumulates afterwards, one accessor at a time.
//
// Deliberately shallow, for the reason the rest of this file is: mentioned by
// name, not matched on signature. A stricter check fails on reformatting and
// gets relaxed the first time it cries wolf.

const QUICKREF: &str = include_str!("../docs/quickref.md");
const SUBGRAPH_RS: &str = include_str!("../src/graph/subgraph.rs");

/// The three types whose fields B1 made private.
///
/// **Scoped to these declarations, and the first version was not.** It searched
/// the whole document for `pub title:`, `pub content:`, `pub weight:` and so on,
/// and went red on `ConceptUpsert` and `EdgeAssertion` — different types, whose
/// fields are genuinely public and share five of the names. That is the D-088
/// failure exactly: a check wide enough to fire on correct documentation is one
/// that gets deleted rather than fixed. It survived about a minute.
const PRIVATE_FIELD_TYPES: &[&str] = &["Subgraph", "NodeData", "EdgeRef"];

/// The body of `pub struct <name> {` in `doc`, or `None` if it is not declared.
fn documented_struct_body<'a>(doc: &'a str, name: &str) -> Option<&'a str> {
    let header = format!("pub struct {name} ");
    let at = doc
        .find(&header)
        .or_else(|| doc.find(&format!("pub struct {name} {{")))?;
    let open = at + doc[at..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in doc[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&doc[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Public method names declared in a `impl <name> {` block of the source.
fn public_methods_of(source: &str, header: &str) -> BTreeSet<String> {
    block_after(source, header)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .map(|l| l.trim_start_matches("async ").trim_start_matches("fn "))
        .filter_map(|l| l.split(['(', '<', ' ']).next())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn no_document_still_advertises_a_field_b1_made_private() {
    let leaked: Vec<String> = PRIVATE_FIELD_TYPES
        .iter()
        .filter_map(|name| {
            let body = documented_struct_body(QUICKREF, name)?;
            body.contains("pub ")
                .then(|| format!("{name} {{{}}}", body.trim()))
        })
        .collect();

    assert!(
        leaked.is_empty(),
        "docs/quickref.md still declares public fields on types B1 made \
         private:\n  {}\n\
         The accessors are the surface now. A document that reproduces a \
         declaration has the same failure mode as a copied error enum: nobody \
         executes it.",
        leaked.join("\n  ")
    );

    // The check is only worth anything if it found the declarations at all.
    for name in PRIVATE_FIELD_TYPES {
        assert!(
            documented_struct_body(QUICKREF, name).is_some(),
            "docs/quickref.md no longer declares `{name}` — either it was \
             dropped from the document, or this test is looking for the wrong \
             thing and is now passing vacuously"
        );
    }
}

#[test]
fn the_quoted_subgraph_surface_lists_every_public_method() {
    let declared = public_methods_of(SUBGRAPH_RS, "impl Subgraph {");
    assert!(
        declared.len() > 10,
        "parsed only {} methods off `impl Subgraph`, which means this test is \
         reading the wrong thing rather than that the type shrank: {declared:?}",
        declared.len()
    );

    // The loaders are `Database` methods documented elsewhere in the same file,
    // and `write_back_annotations` is listed under its own heading.
    let missing: Vec<&String> = declared
        .iter()
        .filter(|m| !QUICKREF.contains(m.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "`impl Subgraph` has public methods that docs/quickref.md does not \
         mention: {missing:?}\n\
         quickref reproduces this surface verbatim, so an accessor added \
         without it is a document that describes a smaller type than the crate \
         has."
    );
}

// ---------------------------------------------------------------------------
// A document that says where a constant lives must be right about it
//
// `docs/quickref.md` credited `util/limits.rs` with "chunking and operational
// constants" and listed five: `CHUNK_BUDGET`, `HYDRATE_CHUNK`,
// `BULK_ATOMIC_WARN_HOLD`, `SAMPLE_LIMIT`, `MAX_ARCHIVE_SESSIONS`. That module
// holds the second one and has never held any of the other four — they are in
// `connection.rs` and `temporal/replay.rs`, and the module's own doc comment
// draws exactly the line the quickref erased: what is here is a ceiling SQLite
// imposes, what is elsewhere is a choice measurement can move.
//
// Nothing caught it. It was found by a reader passing through on unrelated
// work, which is the D-133 / D-140 / D-144 shape for the fourth time: a claim
// about layout that no test can see, so it drifts silently and is corrected by
// accident. This is the executable form.
//
// # The rule, and why it is this rule
//
// Not "every constant must be documented" — the quickref is a selection, and a
// gate that demanded completeness would be answered by deleting the section.
// The rule is narrower and matches the failure: **a passage that places a
// constant in a file must place it in the right file.** A passage that names no
// file makes no location claim and is not checked; a passage that names a file
// the constant is not in fails.
//
// Table rows are their own passage. A markdown table is one block of text, so
// treating it as a paragraph would let a constant in one row be excused by a
// file named three rows away — which is precisely the row that was also wrong
// here (`util` credited with `CHUNK_BUDGET`).
//
// # It runs on the inventories and not on the prose, and §5.1 is why
//
// The first version ran on §5.1 too and failed on a passage that is correct:
// *"`HYDRATE_CHUNK` … is not a latency budget — these are reads, and
// `CHUNK_BUDGET` bounds what the writer holds"*. That names `util/limits.rs`
// and mentions a constant declared elsewhere, and the mention is a **contrast**
// — the sentence exists to say the two are different. No rule reading names and
// proximity can tell that apart from a misplacement, and a rule that guesses
// would be relaxed the first time it cried wolf, which is this file's own
// standing warning.
//
// So the gate runs where a passage naming a file *is* an inventory: the
// quickref's module table and constants section, README's summaries, and
// Appendix A. §5.1 and the rest of the architecture set argue rather than
// enumerate, and the register keeps history verbatim — an entry describing
// where something lived in 0.6.0 is doing its job. Narrower than "every
// document", and it covers every document where the failure could occur
// unnoticed.
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

const README_MD: &str = include_str!("../README.md");

/// Every `const` declared under `src/`, by name, with the files declaring it.
///
/// Read from disk rather than `include_str!`: the point is to cover the whole
/// tree, and a fixed list of includes is the same hand-maintained inventory
/// this test exists to stop trusting.
fn constants_by_file() -> BTreeMap<String, BTreeSet<String>> {
    fn walk(
        dir: &std::path::Path,
        root: &std::path::Path,
        out: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .expect("under the manifest dir")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).expect("valid utf-8");
                for line in text.lines() {
                    if let Some(name) = declared_const(line) {
                        out.entry(name).or_default().insert(rel.clone());
                    }
                }
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = BTreeMap::new();
    walk(&root.join("src"), root, &mut out);
    out
}

/// `NAME` from a line declaring `const NAME:`, screaming-case only.
///
/// Deliberately not a parser. What is wanted is the set of names a document
/// could plausibly be talking about, and a `const` line that this misses is a
/// constant the quickref is then free to misplace — which is the pre-existing
/// state, not a regression this introduces.
fn declared_const(line: &str) -> Option<String> {
    let rest = line
        .trim_start()
        .strip_prefix("pub ")
        .unwrap_or(line.trim_start());
    let rest = rest.strip_prefix("const ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let screaming = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && name.chars().any(|c| c.is_ascii_uppercase());
    screaming.then_some(name)
}

/// The passages of a markdown document, for the purpose above: a table row is
/// one, and everything else is separated by blank lines.
fn passages(doc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in doc.lines() {
        let t = line.trim();
        if t.starts_with('|') {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.push(line.to_string());
        } else if t.is_empty() {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push_str(line);
            current.push(' ');
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Every `something.rs` path named in a passage.
fn files_named(passage: &str) -> BTreeSet<String> {
    let bytes: Vec<char> = passage.chars().collect();
    let mut out = BTreeSet::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_alphanumeric() || bytes[i] == '_' || bytes[i] == '/' || bytes[i] == '.' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_alphanumeric()
                    || bytes[i] == '_'
                    || bytes[i] == '/'
                    || bytes[i] == '.')
            {
                i += 1;
            }
            let token: String = bytes[start..i].iter().collect();
            if token.ends_with(".rs") {
                out.insert(token);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Backticked screaming-case identifiers in a passage.
///
/// Backticks required: prose sometimes shouts, and `WAL` or `NORMAL` in running
/// text is not a claim about a constant.
fn constants_named(passage: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, chunk) in passage.split('`').enumerate() {
        if i % 2 == 1 && declared_const(&format!("const {chunk}:")).is_some() {
            out.insert(chunk.to_string());
        }
    }
    out
}

#[test]
fn a_passage_that_places_a_constant_names_the_file_it_is_in() {
    let defined = constants_by_file();
    let mut wrong = Vec::new();

    const DOCS: [(&str, &str); 3] = [
        ("docs/quickref.md", QUICKREF),
        ("README.md", README_MD),
        ("docs/architecture/appendices.md", APPENDIX_A),
    ];

    for (doc_name, doc) in DOCS {
        for passage in passages(doc) {
            let files = files_named(&passage);
            if files.is_empty() {
                continue; // no location claim to be wrong about
            }
            for name in constants_named(&passage) {
                let Some(homes) = defined.get(&name) else {
                    continue; // not a constant of this crate
                };
                let credited = homes
                    .iter()
                    .any(|home| files.iter().any(|named| home.ends_with(named.as_str())));
                if !credited {
                    wrong.push(format!(
                        "  {doc_name}: `{name}` is declared in {} — the passage names only {}\n    ...{}",
                        homes.iter().cloned().collect::<Vec<_>>().join(", "),
                        files.iter().cloned().collect::<Vec<_>>().join(", "),
                        passage.chars().take(160).collect::<String>().trim_end()
                    ));
                }
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "{} constant(s) are put in a file they are not in:\n{}\n\n\
         A passage naming a source file is making a claim about where something \
         lives, and this is the claim `util/limits.rs` carried wrongly for five \
         releases. Correct the passage, or stop naming a file in it.",
        wrong.len(),
        wrong.join("\n")
    );
}
