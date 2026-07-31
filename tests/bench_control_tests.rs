//! The bench control row is enforced, not remembered (T4.3, D-090).
//!
//! D-070 measured ~29% session-to-session noise in this project's absolute
//! timings and recorded the consequence as a caveat: *"the absolute figures in
//! this cycle's measurements carry that much noise; the ratios, taken within a
//! single run, do not."* A caveat is only as good as the next person's memory of
//! it, and the failure it guards against is silent — a 20% regression and a 20%
//! quiet machine print identically.
//!
//! T4.3's fix is a fixed trivial operation in every bench group, so every figure
//! can be read as a ratio to something whose true cost did not change.
//! `benches/budgets.rs` implements it in the group *constructor*, so obtaining a
//! group without a control requires going around the constructor. This is the
//! test that notices.
//!
//! It reads the bench source with `include_str!` because benches are a separate
//! compilation target: a test cannot call into `budgets.rs`, and the property
//! being asserted is about the source anyway ("no group is opened this way"),
//! not about a value any run produces.

/// The bench file, at compile time.
const BUDGETS: &str = include_str!("../benches/budgets.rs");

/// The only `c.benchmark_group(` in the file is the one inside the constructor.
///
/// A textual assertion, which is worth being explicit about: it does not prove
/// every group has a control, it proves there is exactly one way to make one and
/// that way adds it. That is the enforceable half. The other half — that the
/// constructor still adds the row — is [`the_constructor_still_adds_the_row`].
#[test]
fn no_group_is_opened_without_its_control() {
    let sites: Vec<(usize, &str)> = BUDGETS
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("benchmark_group("))
        .map(|(i, l)| (i + 1, l.trim()))
        .collect();

    assert_eq!(
        sites.len(),
        1,
        "benches/budgets.rs opens a criterion group somewhere other than \
         `controlled_group`, so that group has no control row and its figures \
         cannot be normalised against session noise (T4.3, D-090). Sites:\n{}",
        sites
            .iter()
            .map(|(n, l)| format!("  budgets.rs:{n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // And that one site is inside the constructor.
    let (line, _) = sites[0];
    let ctor = BUDGETS
        .lines()
        .position(|l| l.starts_with("fn controlled_group"))
        .expect("controlled_group must exist")
        + 1;
    assert!(
        line > ctor && line < ctor + 12,
        "the single benchmark_group call is at line {line}, not inside \
         controlled_group at line {ctor}"
    );
}

/// The constructor still adds a control row before returning the group.
///
/// Guards the case `no_group_is_opened_without_its_control` cannot see: every
/// group could go through `controlled_group` while `controlled_group` itself no
/// longer benched anything, and the suite would stay green with the control
/// silently gone from every group in the file.
#[test]
fn the_constructor_still_adds_the_row() {
    let body: String = BUDGETS
        .lines()
        .skip_while(|l| !l.starts_with("fn controlled_group"))
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        body.contains("bench_function(\"control/select_1\""),
        "controlled_group no longer benches a control operation, so every group \
         in the file lost its T4.3 row at once:\n{body}"
    );
}

/// Every group's control has the same id, so runs are comparable.
///
/// The point of the control is cross-group and cross-run comparison. Two
/// spellings of the name would produce two criterion series, and a baseline
/// comparison would then silently compare a group against a control it did not
/// have last time.
#[test]
fn the_control_has_exactly_one_name() {
    let names: Vec<&str> = BUDGETS
        .match_indices("\"control/")
        .map(|(i, _)| {
            let rest = &BUDGETS[i + 1..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect();
    assert!(!names.is_empty(), "no control row found at all");
    assert!(
        names.iter().all(|n| *n == names[0]),
        "the control row is spelled more than one way: {names:?}"
    );
}
