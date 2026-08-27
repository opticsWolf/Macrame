//! The architecture set's cross-references resolve, and its nav is one path.
//!
//! [`docs/architecture/README.md`] ends with a promise: *"Cross-references are
//! links. Every `§5.5`, `D-029`, `R15`, `Doctrine VI` and `Appendix A` in the
//! prose resolves to where it is defined, across files as well as within them."*
//! Nothing checked it, and by 0.7.0 **six links did not resolve** — all of them
//! anchors left behind when sections were renamed during the 0.5.x restoration,
//! including a `[§4.4](s4-schema.md#44-indices)` naming a section that does not
//! exist and never did under that title.
//!
//! That is the failure mode this file exists for, and it is characteristic: a
//! rename is a local edit whose cost is paid somewhere else, silently, by a
//! reader who follows a link and lands at the top of the wrong file. Markdown
//! does not error on a dead anchor; it scrolls nowhere.
//!
//! # What is checked, and what is not
//!
//! Links **between files** in `docs/architecture`, and the anchor each one
//! names. Not checked: links out to the repository (`../../src/...`), which
//! resolve against a checkout rather than against this set, and external URLs.
//!
//! The anchor rule is GitHub's: lowercase, drop everything that is not a word
//! character, whitespace or hyphen, then replace **each** whitespace character
//! with a hyphen. The last part is the one that looks wrong and is not —
//! `## 5.1 connection.rs — the handle` loses the `.` and the em dash and is left
//! with two adjacent spaces, so its anchor carries two adjacent hyphens.
//! Collapsing runs of whitespace produces a slug GitHub never generates, and a
//! checker that did would report every such heading as broken. It cost one
//! false run of 187 "failures" to notice.

use std::collections::{BTreeMap, BTreeSet};

/// Every file in the set, with its text.
///
/// Listed rather than discovered so that adding a document is a decision made
/// here, in the file that explains what membership costs.
/// [`every_document_in_the_directory_is_checked`] fails if the list falls behind
/// the directory.
const DOCS: &[(&str, &str)] = &[
    ("README.md", include_str!("../docs/architecture/README.md")),
    ("REJOIN.md", include_str!("../docs/architecture/REJOIN.md")),
    // Generated (D-212). It carries no navigation footer and is not part of the
    // §0-§14 chain, but it is linked from the README and from the register, so
    // it has to be here or those links point at a file this gate has never seen.
    (
        "api-review-0.14.0.md",
        include_str!("../docs/architecture/api-review-0.14.0.md"),
    ),
    (
        "appendices.md",
        include_str!("../docs/architecture/appendices.md"),
    ),
    (
        "s0-s3-foundations.md",
        include_str!("../docs/architecture/s0-s3-foundations.md"),
    ),
    (
        "s11-s12-milestones-and-risks.md",
        include_str!("../docs/architecture/s11-s12-milestones-and-risks.md"),
    ),
    (
        "s13-decision-register.md",
        include_str!("../docs/architecture/s13-decision-register.md"),
    ),
    (
        "s14-python-bindings.md",
        include_str!("../docs/architecture/s14-python-bindings.md"),
    ),
    (
        "s4-schema.md",
        include_str!("../docs/architecture/s4-schema.md"),
    ),
    (
        "s5-modules.md",
        include_str!("../docs/architecture/s5-modules.md"),
    ),
    (
        "s6-s10-flows-to-dependencies.md",
        include_str!("../docs/architecture/s6-s10-flows-to-dependencies.md"),
    ),
];

/// GitHub's heading slug. See the module docs for the whitespace rule.
fn slug(heading: &str) -> String {
    let cleaned: String = heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| *c != '`' && *c != '*')
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || c.is_whitespace())
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    cleaned.trim_matches('-').to_string()
}

/// Anchors a document defines: explicit `<a id="…">` plus every heading's slug.
fn anchors(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = body;
    while let Some(i) = rest.find("<a id=\"") {
        rest = &rest[i + 7..];
        if let Some(j) = rest.find('"') {
            out.insert(rest[..j].to_string());
        }
    }
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let text = trimmed.trim_start_matches('#');
            if text.starts_with(' ') {
                out.insert(slug(text));
            }
        }
    }
    out
}

/// Fenced code removed, so a `.md` path inside an example is not a link.
fn without_code_fences(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut inside = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `(target file, optional anchor)` for every `](file.md#frag)` in `body`.
fn links(body: &str) -> Vec<(String, Option<String>)> {
    let text = without_code_fences(body);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end..];
        // Only intra-set links: no scheme, no parent directory.
        if target.contains("://") || target.starts_with('.') || target.starts_with('/') {
            continue;
        }
        let (file, frag) = match target.split_once('#') {
            Some((f, a)) => (f, Some(a.to_string())),
            None => (target, None),
        };
        if file.ends_with(".md") {
            out.push((file.to_string(), frag));
        }
    }
    out
}

/// **The promise the README makes.**
#[test]
fn every_cross_reference_resolves() {
    let by_name: BTreeMap<&str, &str> = DOCS.iter().copied().collect();
    let anchor_sets: BTreeMap<&str, BTreeSet<String>> =
        DOCS.iter().map(|(n, b)| (*n, anchors(b))).collect();

    let mut broken = Vec::new();
    for (name, body) in DOCS {
        for (target, frag) in links(body) {
            let Some(_) = by_name.get(target.as_str()) else {
                broken.push(format!("{name}: no such file -> {target}"));
                continue;
            };
            if let Some(anchor) = frag {
                if !anchor_sets[target.as_str()].contains(&anchor) {
                    broken.push(format!("{name}: dead anchor -> {target}#{anchor}"));
                }
            }
        }
    }

    assert!(
        broken.is_empty(),
        "{} cross-reference(s) do not resolve. A renamed heading is a local \
         edit whose cost lands on a reader following a link:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
}

/// The list above has not fallen behind the directory.
///
/// Without this, a document added to `docs/architecture` is simply not checked,
/// and the suite stays green while the promise stops holding for the newest and
/// least-reviewed file in the set.
#[test]
fn every_document_in_the_directory_is_checked() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/architecture");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("docs/architecture is missing")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    let listed: BTreeSet<String> = DOCS.iter().map(|(n, _)| n.to_string()).collect();

    assert_eq!(
        on_disk,
        listed,
        "DOCS is out of step with docs/architecture. Unchecked on disk: {:?}; \
         listed but absent: {:?}",
        on_disk.difference(&listed).collect::<Vec<_>>(),
        listed.difference(&on_disk).collect::<Vec<_>>()
    );
}

/// The `<!--nav-->` footers form one path through the set.
///
/// Every document carries a hand-written previous/next pair, so inserting one —
/// §14 went in between §13 and the appendices — means editing three files. Miss
/// the third and the chain still *looks* right from either end while skipping
/// the new document entirely.
///
/// Two documents are deliberately outside the chain, and the list is here
/// rather than inline so that adding to it is a decision someone has to write
/// down. `REJOIN.md` links to the index and nothing links to it.
/// `api-review-0.14.0.md` is **generated** (D-212): it is evidence a reader is
/// sent to from the register, not a section of the architecture document, and
/// giving it a `previous`/`next` would put a generated file in the middle of a
/// narrative that has to be read in order.
/// The documents that are in `DOCS` but not in the narrative chain.
const OUTSIDE_THE_CHAIN: &[&str] = &["REJOIN.md", "api-review-0.14.0.md"];

#[test]
fn the_navigation_footers_form_a_single_chain() {
    fn next_of(body: &str) -> Option<String> {
        let i = body.find("[next](")? + 7;
        let rest = &body[i..];
        Some(rest[..rest.find(')')?].to_string())
    }
    fn prev_of(body: &str) -> Option<String> {
        let i = body.find("[previous](")? + 11;
        let rest = &body[i..];
        Some(rest[..rest.find(')')?].to_string())
    }
    let by_name: BTreeMap<&str, &str> = DOCS.iter().copied().collect();

    let mut walked = vec!["README.md".to_string()];
    let mut here = "README.md".to_string();
    while let Some(next) = next_of(by_name[here.as_str()]) {
        assert!(
            by_name.contains_key(next.as_str()),
            "{here} points next at {next}, which is not in the set"
        );
        // The first hop is asymmetric by convention, not by oversight: the
        // document after the index links back with `[index](README.md)` rather
        // than `[previous]`, because README *is* the index. Every later hop
        // carries a real `previous`, and that is what is checked.
        if here != "README.md" {
            assert_eq!(
                prev_of(by_name[next.as_str()]).as_deref(),
                Some(here.as_str()),
                "{next}'s `previous` does not point back at {here}"
            );
        } else {
            assert!(
                by_name[next.as_str()].contains("[index](README.md)"),
                "{next} is first in the chain but does not link back to the index"
            );
        }
        assert!(
            !walked.contains(&next),
            "the nav chain loops: reached {next} twice"
        );
        walked.push(next.clone());
        here = next;
    }

    let expected: BTreeSet<String> = DOCS
        .iter()
        .map(|(n, _)| n.to_string())
        .filter(|n| !OUTSIDE_THE_CHAIN.contains(&n.as_str()))
        .collect();
    let reached: BTreeSet<String> = walked.into_iter().collect();
    assert_eq!(
        reached,
        expected,
        "the nav chain does not visit every document. Unreachable: {:?}",
        expected.difference(&reached).collect::<Vec<_>>()
    );
}
