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
//! Since 0.15.0 it also checks the README's dependency pin, which is the
//! same category reached one document further out: a mechanical claim, in a
//! file nobody rereads while writing code, that is wrong in a way no reader
//! of the code can see. It was found by a user rather than by this file.
//!
//! # The quickref, and the document that described it
//!
//! Since 0.19.4 it reaches `docs/quickref.md`, which drifted exactly as the
//! revision history had and for the same reason: no test read it. Its header
//! said v0.18.0 against a 0.19.4 crate, its footer dated itself to v0.12.0 and
//! 2026-08-08, its §9 table stopped at D-277 against a register holding D-296,
//! and §5's struct listing had been missing two shipped `ConceptUpsert` fields
//! since 0.14.8 and 0.18.0.
//!
//! **The finding that made this a gate rather than a sweep is one document
//! further out again.** The entry for the quickref in the top-level `README.md`
//! *described* the quickref's staleness in detail — and was itself the staler
//! of the two, advertising an unscheduled prose refresh for several releases
//! after that refresh had largely happened, while being wrong about the schema
//! rung and the newest decisions. A document whose job is to say another
//! document is out of date goes out of date the same way. Prose that admits a
//! gap is not a gate on it; `doc_sync_tests` and `perf_claim_tests` already
//! pinned parts of the quickref — the analytics types, the `Subgraph`
//! accessors, the performance numbers — and the drift happened in the space
//! between them.
//!
//! # What it deliberately does not check
//!
//! That the prose in a row is *true*. No test can. It checks that a row exists
//! for the release the crate is at, that the register range is the register's
//! range, and that the header names this version — the mechanical claims, which
//! are the ones that rot silently. A wrong row is at least a row someone wrote.
//!
//! The quickref assertions inherit that limit and add one of their own. A
//! bounded *lag* is not currency: [`MAX_DECISION_LAG`] says the document has
//! been touched since the register moved, not that what it says about the
//! decisions it cites is right. It is a staleness bound, and the thing it
//! actually buys is that a reader can tell how old the worst case is.

/// How far behind the register the quickref's newest citation may fall.
///
/// Not every decision belongs in an API quickref — a CI pin says nothing about
/// the surface — so demanding the register's highest anchor would force
/// unrelated entries into the document to keep a test quiet, which is how a
/// gate starts producing the drift it was meant to catch. The bound is set at
/// about one ordinary release instead. Some releases have produced more (0.5.4
/// recorded seventeen, 0.7.0 and 0.8.0 fifteen each), and after one of those
/// the gate fires at once: that is intended, since a release that large has
/// moved the surface the quickref describes, and the quickref is updated in it.
/// A lag inside the window is a small release not yet written up. The drift
/// this file was extended for was **nineteen**.
const MAX_DECISION_LAG: u32 = 10;

const MANIFEST: &str = include_str!("../Cargo.toml");
const ARCH_README: &str = include_str!("../docs/architecture/README.md");
const QUICKREF: &str = include_str!("../docs/quickref.md");
const MIGRATIONS: &str = include_str!("../src/schema/migrations.rs");
const REGISTER: &str = include_str!("../docs/architecture/s13-decision-register.md");
const README: &str = include_str!("../README.md");
const SITE: &str = include_str!("../site/index.html");

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

/// `SCHEMA_VERSION` as the migration runner declares it.
///
/// The authority on the ladder, which the quickref's §4.3 note says in so many
/// words: the table is a transcription, and this is the number.
fn schema_version() -> u32 {
    MIGRATIONS
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("pub const SCHEMA_VERSION: u32 = "))
        .and_then(|rest| rest.trim_end_matches(';').parse().ok())
        .expect("migrations.rs declares SCHEMA_VERSION")
}

/// The highest rung the quickref's §4.3 ladder transcribes.
///
/// The rows are `| vNN | …`; nothing else in the document opens a table cell
/// with a bare `v` and digits.
fn quickref_ladder_top() -> u32 {
    QUICKREF
        .lines()
        .filter_map(|l| l.trim().strip_prefix("| v"))
        .filter_map(|rest| {
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            n.parse::<u32>().ok()
        })
        .max()
        .expect("the quickref transcribes a schema ladder")
}

/// The highest `D-NNN` the quickref cites anywhere.
///
/// Prose citations, table cells and link targets all count: the question is
/// whether anyone has written about the register's recent entries here, not
/// where in the document they did it.
fn quickref_highest_decision() -> u32 {
    QUICKREF
        .match_indices("D-")
        .filter_map(|(i, _)| {
            let rest = &QUICKREF[i + 2..];
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (n.len() == 3).then(|| n.parse::<u32>().ok())?
        })
        .max()
        .expect("the quickref cites decisions")
}

/// The version the README's Rust quick start tells a new user to depend on.
///
/// The pin is a two-component `major.minor`, which pre-1.0 is the whole of the
/// compatibility claim: `"0.15"` resolves any `0.15.z` and refuses `0.16.0`.
fn readme_dep_pin() -> Option<String> {
    README
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("macrame-db = \""))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
}

/// Every `X.Y.Z` the landing page names, parsed.
fn site_versions() -> Vec<(u32, u32, u32)> {
    versions_in(SITE)
}

/// Every `X.Y.Z` in `text`. Dots around a token are punctuation, so `0.18.2.`
/// is read as `0.18.2` rather than dropped (D-299).
fn versions_in(text: &str) -> Vec<(u32, u32, u32)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|tok| {
            let parts: Vec<u32> = tok
                .trim_matches('.')
                .split('.')
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            match parts.as_slice() {
                [a, b, c] => Some((*a, *b, *c)),
                _ => None,
            }
        })
        .collect()
}

fn parse_version(v: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = v.split('.').map(|p| p.parse().expect("numeric")).collect();
    (parts[0], parts[1], parts[2])
}

/// The series the crate is at, as the README would have to spell it.
fn crate_series() -> String {
    let v = crate_version();
    let mut parts = v.split('.');
    let major = parts.next().expect("a version has a major");
    let minor = parts.next().expect("a version has a minor");
    format!("{major}.{minor}")
}

#[test]
fn the_readme_pins_the_series_the_crate_is_at() {
    let series = crate_series();
    let pin = readme_dep_pin().expect("the README's quick start pins a version");

    assert_eq!(
        pin,
        series,
        "the README's quick start says `macrame-db = \"{pin}\"` while the crate \
         is at {}. That line is not internal documentation: `README.md` is this \
         package's front page on crates.io and PyPI, so it is the first \
         instruction a new user follows, and a stale pin sends them to a series \
         this repository has stopped changing. It read \"0.13\" when 0.15.0 was \
         tagged and was caught by a reader rather than by a gate — the version \
         sweep that release ran searched for `0.13.0` and `0.14.0`, and a \
         two-component pin matches neither.",
        crate_version()
    );
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
fn the_quickref_header_and_footer_name_the_release_they_describe() {
    let version = crate_version();

    assert!(
        QUICKREF.contains(&format!("**v{version} ·")),
        "docs/quickref.md's header does not say v{version}, which is what the \
         crate is at. The header is the first thing a reader checks, and it \
         said v0.18.0 into 0.19.4."
    );
    assert!(
        QUICKREF.contains(&format!("· v{version} ·")),
        "docs/quickref.md's footer does not say v{version}. The footer claims \
         the document is *synced against* the architecture set as of a \
         version; it said v0.12.0 and 2026-08-08 for seven minor releases, \
         which is a sync claim for a document that had moved on without it."
    );
}

#[test]
fn the_quickref_ladder_reaches_the_schema_the_code_is_at() {
    let code = schema_version();
    let doc = quickref_ladder_top();

    assert_eq!(
        doc, code,
        "docs/quickref.md's §4.3 ladder stops at v{doc} and the migration \
         runner declares SCHEMA_VERSION = {code}. This table stopped at v10 \
         for seven rungs and fourteen releases before anyone noticed, and a \
         missing rung looks exactly like a rung that was never added."
    );
}

#[test]
fn the_quickref_cites_the_registers_recent_decisions() {
    let highest = highest_decision();
    let cited = quickref_highest_decision();
    let lag = highest.saturating_sub(cited);

    assert!(
        lag <= MAX_DECISION_LAG,
        "docs/quickref.md's newest citation is D-{cited:03} and the register \
         holds D-{highest:03} — a lag of {lag}, over the {MAX_DECISION_LAG} \
         this file allows. Either the document has not been written up for \
         the releases in between, or it has and the citation was left out. \
         The drift that added this assertion was a lag of nineteen, sitting \
         under a §9 table whose last row read D-277."
    );
}

#[test]
fn the_readme_does_not_advertise_a_quickref_version_it_is_not_at() {
    let version = crate_version();
    let entry = README
        .lines()
        .find(|l| l.contains("[Architecture Quick Reference](docs/quickref.md)"))
        .expect("the README points at the quickref");

    // The entry is allowed to say nothing about the version. What it is not
    // allowed to do is name a *different* one, which is the failure mode that
    // put this test here: the paragraph described the quickref's state in
    // detail and was wrong about it, for several releases, in a file that is
    // the first thing a new reader opens.
    for token in entry.split_whitespace() {
        let Some(named) = token
            .trim_start_matches(['(', '*', '_', '`'])
            .strip_prefix("v0.")
        else {
            continue;
        };
        let named = named.trim_end_matches([')', ',', '.', '*', '_', '`', ';', ':']);
        assert_eq!(
            format!("0.{named}"),
            version,
            "the README's quickref entry says v0.{named} while the crate is \
             at {version}. An entry that names a version is making a claim \
             about another document's currency; this one said the quickref's \
             prose dated to v0.12.0 for several releases after it no longer \
             did."
        );
    }
}

/// The landing page names one release, of the series the crate is at (0.19.5,
/// D-298).
///
/// It advertises what is **published**, which this test cannot see: the site
/// deploys from main, and on a development branch the crate is ahead of the
/// last release. So it bounds rather than equates. The page names one version
/// throughout, no later than the crate's, and in the crate's series, so a new
/// series cannot ship under a page that still advertises the last one. And its
/// docs link points at the architecture set, not at a plan a later plan
/// superseded.
#[test]
fn the_site_advertises_one_release_of_the_series_the_crate_is_at() {
    let named = site_versions();
    let first = *named.first().expect("site/index.html names a version");
    assert!(
        named.iter().all(|v| *v == first),
        "site/index.html names more than one version: {named:?}"
    );
    let crate_v = parse_version(&crate_version());
    assert!(
        first <= crate_v && (first.0, first.1) == (crate_v.0, crate_v.1),
        "site/index.html advertises {}.{}.{}, but the crate is at {} — the page \
         must name a release of the series {} and no later than the crate",
        first.0,
        first.1,
        first.2,
        crate_version(),
        crate_series()
    );
    assert!(
        !SITE.contains("Update%20Plan") && !SITE.contains("Update Plan"),
        "site/index.html links a plan document; plans are superseded, so the \
         docs link belongs on the architecture set or the README"
    );
}

/// A version that ends a sentence is still read (0.19.6, D-299): split on
/// everything but digits and dots, it arrives with the full stop attached.
#[test]
fn a_version_at_the_end_of_a_sentence_is_read() {
    assert_eq!(
        versions_in("Badge 0.19.1, and in prose: now at 0.18.4."),
        vec![(0, 19, 1), (0, 18, 4)]
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
    assert!(
        readme_dep_pin().is_some(),
        "the README no longer contains a `macrame-db = \"x.y\"` line; the \
         quickstart moved and the pin test is measuring nothing"
    );

    // The quickref parsers, same hazard: each returns a max over a filter, so
    // a format change makes them return something small rather than fail.
    assert!(
        schema_version() >= 22,
        "SCHEMA_VERSION parsed as {} — the declaration moved and the ladder \
         test is comparing against nothing",
        schema_version()
    );
    assert_eq!(
        quickref_ladder_top(),
        schema_version(),
        "the quickref ladder parser and SCHEMA_VERSION disagree here, which \
         the ladder test should have caught first; if that test passed and \
         this one failed, the parser is matching rows it should not"
    );
    assert!(
        quickref_highest_decision() >= 277,
        "the quickref's decision parser found D-{:03} at most. The §9 table \
         held D-277 before this file was extended, so anything below that is \
         a parser that stopped matching citations",
        quickref_highest_decision()
    );
    assert!(
        QUICKREF.len() > 40_000,
        "docs/quickref.md is {} bytes; it was ~60k when these assertions were \
         written, and a document that shrank by a third is not the document \
         they describe",
        QUICKREF.len()
    );
}
