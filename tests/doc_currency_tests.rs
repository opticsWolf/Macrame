//! The architecture set says which release it describes, and it is this one
//! (0.13.36, W11.3, D-209).
//!
//! # The finding this closes
//!
//! §6.1 of [Road to 1.0](../../docs/Macrame%20Road%20to%201.0.md): the revision
//! history in `docs/architecture/README.md` stopped at **0.9.0** while the crate
//! was at 0.13.35 — twenty-six releases, four of them minor, absent from the
//! table that exists to record them. The file table beside it still offered
//! `D-001…D-133` against a register holding two hundred and eight.
//!
//! # Why a test and not a sweep
//!
//! [D-144](../../docs/architecture/s13-decision-register.md#d-144) named
//! documentation drift as a *category* rather than an incident, and a sweep
//! clears the incident. Nothing in a release makes the revision history
//! conspicuous: the table is at the top of a file nobody opens while writing
//! code, and a row missing from it looks exactly like a row that was never due.
//! Three of these four assertions would have failed at 0.10.0, which is the
//! release the drift started in.
//!
//! # What it deliberately does not check
//!
//! That the prose in a row is *true*. No test can. It checks that a row exists
//! for the release the crate is at, that the register range is the register's
//! range, and that the header names this version — the mechanical claims, which
//! are the ones that rot silently. A wrong row is at least a row someone wrote.

const MANIFEST: &str = include_str!("../Cargo.toml");
const ARCH_README: &str = include_str!("../docs/architecture/README.md");
const REGISTER: &str = include_str!("../docs/architecture/s13-decision-register.md");

/// The crate's version, from the `[package]` table's first `version =`.
fn crate_version() -> String {
    MANIFEST
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("Cargo.toml has a package version")
        .to_string()
}

/// The cells of the last row of the revision-history table.
///
/// The table is the run of `|`-leading lines after the `## Revision history`
/// heading; the run ends at the first line that is not one, which is the blank
/// line before the contents paragraph.
fn revision_rows() -> Vec<Vec<String>> {
    let after = ARCH_README
        .split_once("## Revision history")
        .expect("the architecture README has a revision history")
        .1;
    after
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .filter(|l| !l.starts_with("|---"))
        .map(|l| {
            l.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect::<Vec<String>>()
        })
        // The `| Version | Cycle | Substance |` header is a row to markdown and
        // not to this test.
        .filter(|r| r.first().map(String::as_str) != Some("Version"))
        .collect()
}

/// The highest `D-NNN` the register defines an anchor for.
fn highest_decision() -> u32 {
    REGISTER
        .match_indices("<a id=\"d-")
        .filter_map(|(i, _)| {
            let rest = &REGISTER[i + 9..];
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            n.parse::<u32>().ok()
        })
        .max()
        .expect("the register defines decisions")
}

#[test]
fn the_revision_history_reaches_the_release_the_crate_is_at() {
    let version = crate_version();
    let rows = revision_rows();
    let last = rows.last().expect("the table has rows");

    assert!(
        last[0].contains(&version),
        "the last revision-history row covers {:?}, and the crate is at {version}. \
         Every release is a row or an extended range in \
         `docs/architecture/README.md`; a release that does not appear there is \
         one nobody can find the substance of later. Extend the last row's \
         version span, or add a row (D-209, closing §6.1).",
        last[0]
    );
}

#[test]
fn the_header_names_the_version_the_document_describes() {
    let version = crate_version();
    let row = ARCH_README
        .lines()
        .find(|l| l.starts_with("| Document version |"))
        .expect("the architecture README declares its version");

    assert!(
        row.contains(&version),
        "the architecture README calls itself {row:?} while the crate is at \
         {version}. The header is the first thing a reader checks against the \
         code in front of them, and it said 0.9.0 for twenty-six releases \
         (D-209)."
    );
}

#[test]
fn the_file_table_offers_the_decisions_the_register_actually_holds() {
    let highest = highest_decision();
    let want = format!("D-001…D-{highest:03}");

    assert!(
        ARCH_README.contains(&want),
        "the file table in the architecture README does not say {want:?}, and \
         the register's highest entry is D-{highest:03}. That cell is the \
         register's advertised range; a reader who trusts it stops looking at \
         the number it names (D-209)."
    );
}

#[test]
fn the_documents_are_shaped_the_way_these_tests_assume() {
    // A parser that matches nothing passes the three tests above.
    let rows = revision_rows();
    assert!(
        rows.len() > 20,
        "only {} revision-history rows parsed; the table's format changed and \
         these tests are measuring nothing",
        rows.len()
    );
    assert!(
        rows.iter().all(|r| r.len() >= 3),
        "a revision-history row has fewer than three cells; the columns changed"
    );
    assert_eq!(rows[0][0], "0.1.0", "the table no longer starts at 0.1.0");
    assert!(highest_decision() >= 208);
    assert!(crate_version().starts_with("0."));
}
