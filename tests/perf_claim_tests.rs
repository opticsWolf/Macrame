//! Published performance claims, and what substantiates each one (W5, D-139).
//!
//! `doc_sync_tests` is explicit that it pins the **API surface** and nothing
//! else, and that shallowness is right for what it does. This is the other
//! kind of check, and the release that added it exists because nothing had it:
//! §9 published `47.7 ms` for a chunk commit while `connection.rs` published
//! `8.0 ms` for the same operation, 460 lines apart, for four releases. Nine
//! texts, two facts, and no structure connecting them.
//!
//! [`index_plan_tests`] is the template — a registry keyed by the thing being
//! claimed, where adding a claim without its justification is a red test. Same
//! inversion, applied to prose.
//!
//! # What this does not do
//!
//! **It does not assert that any number is correct.** D-055 rules out making
//! the benches CI gates (an absolute threshold asserted on a shared runner is
//! an assertion about the runner) and D-070 measured the ~29% session-to-session
//! spread that makes it a bad idea anyway. What it asserts is that every
//! published claim is traceable to a bench that exists and a decision that
//! ratified it, and that one fact is not published as two numbers.
//!
//! # The key, and why it has three parts rather than two
//!
//! The plan proposed keying on `(operation, metric)`. The dry-run W5.1 required
//! *before* this file existed found that both seed claims would have failed it,
//! for good reasons: `224 µs` and `983 µs` are both the single edge assertion's
//! median latency, measured by `write_path` on a warm handle and by
//! `overlap_guard` on a database rebuilt per iteration. `2.39 ms` and `9.06 ms`
//! are both the 90-row edge chunk, empty and seeded. In each case the fixture
//! *is* the difference, and the difference is the finding.
//!
//! So the key carries the fixture — which is D-088's own rule, that a
//! performance decision names its fixture, arriving where it was missing. A
//! gate that goes red in its first week against correct documentation is worse
//! than the drift it was built to catch; this project has that failure on
//! record twice (`ci.yml` on rustfmt, `doc_sync_tests` on the API check).
//!
//! # `text` is the quotation; `value` is the fact
//!
//! They are different fields on purpose. Four documents state one measurement
//! four ways — one gives `983 / 920 / 882 µs`, another says "sub-millisecond",
//! a third says "it does not move" — and all three are the same claim. `text`
//! pins each wording to the document it lives in. `value` is the canonical
//! statement of the fact, identical across a key, and [`one_operation_fixture_metric_key_carries_one_value`]
//! is what makes editing one document without the others a red test.
//!
//! # What gets an entry, and what does not (0.10.0, D-141)
//!
//! This registry covers **claims about the current cost** — the figures
//! `quickref`, §9 and the rustdoc publish as *what this operation costs*.
//!
//! It does **not** inventory every printed digit. `README`'s performance table
//! is a *per-release measurement series*: each column is a release-stamped
//! observation, and the older ones are history by construction. Registering
//! every cell would make each new column collide with the current-cost entry
//! for the same key on the first run, because two honest measurements of the
//! same thing never agree to three digits — which is the false-positive the
//! three-part key was chosen to avoid.
//!
//! So a series cell earns an entry in exactly one case: **it contradicts a
//! current-cost claim by more than measurement noise.** That is the finding the
//! registry exists to surface, and [`Status::Contested`] is how it is recorded.
//! 0.10.0 has one — the 90-row edge chunk, published at 2.39 ms in four places
//! and re-measured at 2.71 ms with a 1.1% spread and a normal control.

/// A published performance claim, and what substantiates it.
struct Claim {
    /// What is being measured. Part of the key — never the number.
    operation: &'static str,
    /// The fixture it was measured on. The part the plan's key was missing.
    fixture: &'static str,
    /// What is being reported about it.
    metric: &'static str,
    /// The fact, canonically. Identical across every entry sharing a key.
    value: &'static str,
    /// Verbatim fragment as it appears in the document.
    text: &'static str,
    /// Path of the document, for the failure message.
    doc_name: &'static str,
    /// The document itself, via `include_str!`.
    doc: &'static str,
    /// What measured it.
    ///
    /// Either a criterion group named in `benches/budgets.rs`, or — for a claim
    /// substantiated by a controlled comparison rather than by a benchmark —
    /// `"example:<file>/<experiment>"`. The second form was added in 0.11.0
    /// with [D-142](../docs/architecture/s13-decision-register.md#d-142),
    /// because attribution is not a benchmark: it is a set of arms that differ
    /// by one component, and criterion measures throughput on one arm at a
    /// time. Without the second form the registry could only cover figures
    /// produced by `benches/`, which is a minority of what this project
    /// publishes — [D-056](../docs/architecture/s13-decision-register.md#d-056)
    /// and [D-059](../docs/architecture/s13-decision-register.md#d-059) are
    /// both example-backed — and a registry that cannot express its evidence
    /// silently narrows to the claims that happen to fit it.
    bench_group: &'static str,
    /// The register entry that last ratified it, lower-case anchor form.
    decision: &'static str,
    /// Live, or preserved history.
    status: Status,
}

enum Status {
    /// A current claim. Subject to the one-value rule.
    Live,
    /// A figure this register deliberately keeps after retiring it.
    ///
    /// Exempt from the one-value rule and from nothing else. §9's D-127
    /// paragraph publishes `258 µs` for an operation whose live figure is
    /// `224 µs`, because this register keeps history verbatim rather than
    /// rewriting it. A registry that could not express that would have forced
    /// the practice to be deleted to make a test pass — so the category is
    /// part of the schema, not an exception to it.
    Superseded {
        /// The decision that retired it.
        by: &'static str,
    },
    /// A published figure that **disagrees with another live one under the same
    /// key**, knowingly, with the reconciliation owned somewhere.
    ///
    /// Added in 0.10.0 when the README's new per-release column measured the
    /// 90-row edge chunk at 2.71 ms against the 2.39 ms four documents publish
    /// as the current cost. Both are real, both are published, and the
    /// disagreement is not resolvable by editing: 2.39 is the figure
    /// `chunk_rows::EDGES` was *solved from* (D-058), and one afternoon on one
    /// machine with no mechanism is not grounds to re-derive a load-bearing
    /// constant.
    ///
    /// The alternative was to split `metric` so the two stopped sharing a key,
    /// which would have made the test pass by hiding the thing it detected.
    /// This keeps the key intact and makes the conflict a recorded fact with an
    /// owner — and [`every_contested_claim_names_who_reconciles_it`] is what
    /// stops "contested" from becoming a way to silence the gate.
    Contested {
        /// The other live value, as published.
        with: &'static str,
        /// Anchor of the entry or appendix that owns the reconciliation.
        owner: &'static str,
    },
}

use Status::{Contested, Live, Superseded};

const README: &str = include_str!("../README.md");
const QUICKREF: &str = include_str!("../docs/quickref.md");
const S5: &str = include_str!("../docs/architecture/s5-modules.md");
const S9: &str = include_str!("../docs/architecture/s6-s10-flows-to-dependencies.md");
const CONNECTION: &str = include_str!("../src/connection.rs");
const BUDGETS: &str = include_str!("../benches/budgets.rs");
const REGISTER: &str = include_str!("../docs/architecture/s13-decision-register.md");
const APPENDICES: &str = include_str!("../docs/architecture/appendices.md");
const CHUNK_DIAG: &str = include_str!("../examples/chunk_diag.rs");
const CHUNK_MATRIX: &str = include_str!("../examples/chunk_matrix.rs");

/// The fixture strings, named once so a typo cannot silently split a key.
mod fx {
    pub const WARM: &str = "warm handle, 2,000 concepts, no links (write_path/assert_edge)";
    pub const PER_ITER: &str =
        "database rebuilt per iteration, Shape::StarOfStars at 0 / 2,000 / 8,000 edges";
    pub const EMPTY: &str = "empty database (chunk_budget, concepts seeded, no links)";
    pub const SEEDED: &str = "8,000-edge table (chunk_budget's seeded arm, W4.13)";
    /// All four D-088 shapes at one population — the fixture *is* the plural.
    pub const MATRIX: &str =
        "all four D-088 shapes, each populated to 8,000 edges (chunk_matrix)";
}

const LATENCY: &str = "latency, median, reference hardware";

/// Not a latency. A *decomposition* of one, which is why it is its own metric:
/// `9.06 ms` and `89% of the growth` are two facts about the same operation and
/// the same fixture, and collapsing them onto one key would make the one-value
/// rule fire on two claims that do not disagree (D-139, D-142).
const ATTRIBUTION: &str = "share of growth 0 -> 8,000 edges, median of three sessions";

/// A *derivation*, not an observation: the largest chunk size meeting the
/// 3 ms bound. Its own metric for the same reason ATTRIBUTION is — `9.06 ms`,
/// `89% of the growth` and `20 rows` are three facts about one operation and
/// one fixture, and sharing a key would fire the one-value rule on claims that
/// do not disagree (D-139, D-143).
const DERIVED: &str = "largest chunk size within the 3 ms bound, populated database";

/// Named once: three documents state it three ways.
const DERIVED_EDGES: &str =
    "20 rows on all four D-088 shapes, against a constant of 90 that is      deliberately not changed";

/// The fact itself, named once: three documents state it three ways.
const ATTRIBUTED: &str =
    "89% of the growth is links_current secondary-index maintenance; the \r
     single-open guard contributes none";

/// The floor's own cost, measured against the loop rather than extrapolated
/// from a sweep — which is the correction D-146 is partly about.
const FLOOR_COST: &str = "3.11-3.43 ms at 35 rows, all four shapes, two sessions";

/// Two metrics on one operation, because the two numbers moved in different
/// directions and the register publishes both. Collapsing them would have let
/// the good one stand for the pair.
const WORST_STALL: &str = "longest hold, adaptive against fixed ceiling-sized chunks";
const WORST_STALL_VALUE: &str = "7.7-10.2 ms against 7.6-15.2 ms (0.67-1.01x)";
const TYPICAL_STALL: &str = "mean hold, adaptive against fixed ceiling-sized chunks";
const TYPICAL_STALL_VALUE: &str = "3.4-3.7 ms against 7.1-8.3 ms (2.1-2.4x shorter)";

const REGISTRY: &[Claim] = &[
    // ---- single edge assertion, warm handle -------------------------------
    Claim {
        operation: "single edge assertion",
        fixture: fx::WARM,
        metric: LATENCY,
        value: "224 µs",
        text: "224 µs, and the caveat is **retired on measurement** (D-134)",
        doc_name: "README.md",
        doc: README,
        bench_group: "write_path",
        decision: "d-134",
        status: Live,
    },
    Claim {
        operation: "single edge assertion",
        fixture: fx::WARM,
        metric: LATENCY,
        value: "224 µs",
        text: "| Single assertion | ≤ 5 ms | 224 µs |",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "write_path",
        decision: "d-134",
        status: Live,
    },
    Claim {
        operation: "single edge assertion",
        fixture: fx::WARM,
        metric: LATENCY,
        value: "258 µs",
        text: "single assertion **258 µs** on this fixture",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "write_path",
        decision: "d-127",
        status: Superseded { by: "d-134" },
    },
    // ---- single edge assertion, the flatness measurement ------------------
    //
    // Four documents, one measurement, three wordings. This is the group the
    // `value` field exists for.
    Claim {
        operation: "single edge assertion",
        fixture: fx::PER_ITER,
        metric: LATENCY,
        value: "983 / 920 / 882 µs — sub-millisecond and flat in out-degree",
        text: "at 983 / 920 / 882 µs, median of three sessions against a 1.52 µs control",
        doc_name: "README.md",
        doc: README,
        bench_group: "overlap_guard",
        decision: "d-134",
        status: Live,
    },
    Claim {
        operation: "single edge assertion",
        fixture: fx::PER_ITER,
        metric: LATENCY,
        value: "983 / 920 / 882 µs — sub-millisecond and flat in out-degree",
        text: "measured into tables of 0 / 2,000 / 8,000 edges — hub out-degree \
               0 / 666 / 2,666 — with no rise",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "overlap_guard",
        decision: "d-134",
        status: Live,
    },
    Claim {
        operation: "single edge assertion",
        fixture: fx::PER_ITER,
        metric: LATENCY,
        value: "983 / 920 / 882 µs — sub-millisecond and flat in out-degree",
        text: "sub-millisecond into tables of 0 / 2,000 / 8,000 edges, at which \
               the probed hub carries out-degree 0 / 666 / 2,666",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "overlap_guard",
        decision: "d-134",
        status: Live,
    },
    Claim {
        operation: "single edge assertion",
        fixture: fx::PER_ITER,
        metric: LATENCY,
        value: "983 / 920 / 882 µs — sub-millisecond and flat in out-degree",
        text: "Measured into tables of 0 / 2,000 / 8,000 edges — hub out-degree \
               0 / 666 / 2,666 on this same fixture — it does not move.",
        doc_name: "docs/architecture/s5-modules.md",
        doc: S5,
        bench_group: "overlap_guard",
        decision: "d-134",
        status: Live,
    },
    // ---- chunk commit, edges, 90 rows, empty ------------------------------
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.39 ms",
        text: "| Chunk commit (edges, 90 rows) | ≤ 3 ms | 2.39 ms |",
        doc_name: "README.md",
        doc: README,
        bench_group: "chunk_budget",
        decision: "d-058",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.39 ms",
        text: "| Chunk commit, edges 90 rows | ≤ 3 ms | ~2.39 ms |",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "chunk_budget",
        decision: "d-058",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.39 ms",
        text: "| Edges (`bulk_import`) | 90 | ~2.39 ms |",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "chunk_budget",
        decision: "d-058",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.39 ms",
        text: "2.39 ms **on an empty database**",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "chunk_budget",
        decision: "d-058",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.39 ms",
        text: "each at its own size: edges **2.39 ms**",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "chunk_budget",
        decision: "d-058",
        status: Live,
    },
    // The 0.10.0 re-measurement of the same arm, in README's per-release table.
    //
    // `Contested` for two releases, and **closed in 0.12.0** (D-145): six
    // sessions showed the arm tracking `control/select_1` monotonically, and
    // conditioned on a normal control it reads 2.36 ms at a 0.4% spread. The
    // 2.71 figure is the elevated-control state. So this is history now, not a
    // conflict — `Superseded` rather than deleted, because README's column is a
    // per-release series and 2.71 is genuinely what was measured then.
    //
    // Worth keeping as an example of what the category is for: the entry that
    // opened the contest (D-141) was wrong about *why* the figures differed,
    // and the registry still did its job — it kept the disagreement addressable
    // for two releases instead of letting one number quietly overwrite another.
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::EMPTY,
        metric: LATENCY,
        value: "2.71 ms — the 0.10.0 column's figure, since explained as an elevated-control session",
        text: "**2.71 ms — see below**",
        doc_name: "README.md",
        doc: README,
        bench_group: "chunk_budget",
        decision: "d-145",
        status: Superseded { by: "d-145" },
    },
    // ---- chunk commit, edges, 90 rows, into a populated table -------------
    //
    // The three that replaced 47.7 ms. They are the reason this file exists:
    // the old figure and its post-index successor were published for the same
    // operation, in two documents, and nothing compared them.
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: LATENCY,
        value: "9.06 ms — the 3 ms bound missed by ~3×, residual attributed to the links_current sync trigger",
        text: "**9.06 ms into an 8,000-edge table**",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "chunk_budget",
        decision: "d-136",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: LATENCY,
        value: "9.06 ms — the 3 ms bound missed by ~3×, residual attributed to the links_current sync trigger",
        text: "90-edge chunk takes **9.06 ms** into an 8,000-edge table",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "chunk_budget",
        decision: "d-136",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: LATENCY,
        value: "9.06 ms — the 3 ms bound missed by ~3×, residual attributed to the links_current sync trigger",
        text: "edges into an 8,000-edge table take **9.06 ms**",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "chunk_budget",
        decision: "d-136",
        status: Live,
    },
    // ---- what that 9.06 ms is made of (0.11.0, D-142) ---------------------
    //
    // Registered for the same reason the three above are: one figure, restated
    // in three documents, and nothing structural connecting them. This one was
    // published in four places on the day it was measured, which is exactly the
    // condition that produced the 47.7 / 8.0 split this file exists for.
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: ATTRIBUTION,
        value: ATTRIBUTED,
        text: "89% of it secondary-index maintenance on `links_current`",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "example:chunk_diag/sync",
        decision: "d-142",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: ATTRIBUTION,
        value: ATTRIBUTED,
        text: "89% of the growth is maintenance of `idx_lc_traversal_cover`",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "example:chunk_diag/sync",
        decision: "d-142",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::SEEDED,
        metric: ATTRIBUTION,
        value: ATTRIBUTED,
        text: "89% of the growth from an empty table to an 8,000-edge one",
        doc_name: "docs/architecture/appendices.md",
        doc: APPENDICES,
        bench_group: "example:chunk_diag/sync",
        decision: "d-142",
        status: Live,
    },
    // ---- the re-derivation, and the constant it did not change (D-143) ----
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::MATRIX,
        metric: DERIVED,
        value: DERIVED_EDGES,
        text: "**the largest size within the bound is 20**, not 90",
        doc_name: "docs/architecture/s6-s10-flows-to-dependencies.md",
        doc: S9,
        bench_group: "example:chunk_matrix/edges",
        decision: "d-143",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::MATRIX,
        metric: DERIVED,
        value: DERIVED_EDGES,
        text: "the largest size meeting the bound is **20**",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "example:chunk_matrix/edges",
        decision: "d-143",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 90 rows",
        fixture: fx::MATRIX,
        metric: DERIVED,
        value: DERIVED_EDGES,
        text: "the largest size within the bound is **20**, against a constant of 90",
        doc_name: "docs/architecture/appendices.md",
        doc: APPENDICES,
        bench_group: "example:chunk_matrix/edges",
        decision: "d-143",
        status: Live,
    },
    // ---- what the adaptive loop settled at, and what it bought (D-146) ----
    //
    // Three keys, one operation. The floor's cost, the worst stall and the
    // typical stall are three facts about one import on one fixture, and the
    // second and third disagree in *direction* — the worst stall barely moved
    // while the typical one halved. Sharing a metric would fire the one-value
    // rule on claims that do not disagree; separating them is what makes the
    // uncomfortable one publishable beside the good one (D-139).
    Claim {
        operation: "chunk commit, edges, 35 rows (the floor)",
        fixture: fx::MATRIX,
        metric: LATENCY,
        value: FLOOR_COST,
        text: "A 35-row chunk costs **3.11–3.43 ms**",
        doc_name: "src/connection.rs",
        doc: CONNECTION,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 35 rows (the floor)",
        fixture: fx::MATRIX,
        metric: LATENCY,
        value: FLOOR_COST,
        text: "a 35-row chunk costs **3.11–3.43 ms** across the four shapes over two sessions",
        doc_name: "docs/architecture/s5-modules.md",
        doc: S5,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 35 rows (the floor)",
        fixture: fx::MATRIX,
        metric: LATENCY,
        value: FLOOR_COST,
        text: "measured at **3.11–3.43 ms** on the four D-088 shapes",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "chunk commit, edges, 35 rows (the floor)",
        fixture: fx::MATRIX,
        metric: LATENCY,
        value: FLOOR_COST,
        text: "**3.11–3.43 ms** across four shapes and two sessions",
        doc_name: "docs/architecture/s13-decision-register.md",
        doc: REGISTER,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: WORST_STALL,
        value: WORST_STALL_VALUE,
        text: "**7.7–10.2 ms** against **7.6–15.2 ms** for fixed ceiling-sized chunks",
        doc_name: "docs/architecture/s5-modules.md",
        doc: S5,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: WORST_STALL,
        value: WORST_STALL_VALUE,
        text: "(7.7–10.2 vs 7.6–15.2 ms)",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: WORST_STALL,
        value: WORST_STALL_VALUE,
        text: "**7.7–10.2 ms against 7.6–15.2 ms**",
        doc_name: "docs/architecture/s13-decision-register.md",
        doc: REGISTER,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: TYPICAL_STALL,
        value: TYPICAL_STALL_VALUE,
        text: "mean hold **3.4–3.7 ms against 7.1–8.3 ms**, a 2.1–2.4× shorter wait",
        doc_name: "docs/architecture/s5-modules.md",
        doc: S5,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: TYPICAL_STALL,
        value: TYPICAL_STALL_VALUE,
        text: "mean hold **3.4–3.7 ms against 7.1–8.3 ms** fixed, a 2.1–2.4× shorter typical stall",
        doc_name: "docs/quickref.md",
        doc: QUICKREF,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
    Claim {
        operation: "bulk_import of 900 edges",
        fixture: fx::MATRIX,
        metric: TYPICAL_STALL,
        value: TYPICAL_STALL_VALUE,
        text: "mean hold **3.4–3.7 ms against 7.1–8.3 ms**, 2.1–2.4× shorter",
        doc_name: "docs/architecture/s13-decision-register.md",
        doc: REGISTER,
        bench_group: "example:chunk_matrix/converge",
        decision: "d-146",
        status: Live,
    },
];

/// Whitespace-normalised, for the reason `index_plan_tests` documents: the
/// sources are CRLF and prose wraps, so a byte-exact `contains` fails for
/// reasons that have nothing to do with the claim — and a nuisance test is a
/// deleted test.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every registered claim still appears in the document it was quoted from.
///
/// `every_reproduced_query_still_exists_in_its_source` applied to prose. A
/// claim edited or deleted without updating the registry goes red — which is
/// the direction that matters, because the registry is the only thing that
/// knows the other five copies exist.
#[test]
fn every_claim_still_appears_in_its_document() {
    for c in REGISTRY {
        assert!(
            flat(c.doc).contains(&flat(c.text)),
            "{} / {} — {}\n  no longer contains: {:?}\n  \
             The claim was edited or moved without updating the registry. If \
             the number changed, every other entry under this key changed with \
             it (see D-139).",
            c.doc_name,
            c.operation,
            c.fixture,
            c.text
        );
    }
}

/// Every claim names a criterion group — or an example experiment — that exists.
///
/// Matched as `controlled_group(c, "name")` rather than as a bare substring:
/// the group names appear in prose in that file too, and a check that a string
/// occurs *somewhere* in a 1,600-line bench file is not a check. The example
/// form is held to the same standard — it must match the `match` arm that
/// selects the experiment, not the name anywhere in the file, which in
/// `chunk_diag.rs` also appears in several comments.
///
/// An unrecognised example file is a **panic, not a pass**. That is the whole
/// risk of widening this field: a check that silently accepts anything it does
/// not recognise is worse than no check, because it reads as one.
#[test]
fn every_claim_names_a_bench_group_that_exists() {
    for c in REGISTRY {
        if let Some(rest) = c.bench_group.strip_prefix("example:") {
            let (file, experiment) = rest.split_once('/').unwrap_or_else(|| {
                panic!(
                    "{}: example evidence must be \"example:<file>/<experiment>\", got {:?}",
                    c.operation, c.bench_group
                )
            });
            let source = match file {
                "chunk_diag" => CHUNK_DIAG,
                "chunk_matrix" => CHUNK_MATRIX,
                other => panic!(
                    "{}: no example source is registered under {other:?}. Add it \
                     beside CHUNK_DIAG — an unknown name must not pass silently.",
                    c.operation
                ),
            };
            let arm = format!("\"{experiment}\" =>");
            assert!(
                source.contains(&arm),
                "{}: claims are substantiated by examples/{file}.rs experiment \
                 {experiment:?}, which that file does not select. A renamed or \
                 deleted experiment leaves the claim standing on nothing.",
                c.operation
            );
            continue;
        }
        let decl = format!("controlled_group(c, \"{}\")", c.bench_group);
        assert!(
            BUDGETS.contains(&decl),
            "{}: claims are substantiated by criterion group {:?}, which \
             benches/budgets.rs does not declare. A renamed or deleted bench \
             leaves the claim standing on nothing.",
            c.operation,
            c.bench_group
        );
    }
}

/// Every claim names a register entry that exists.
///
/// Not in the plan's three. The registry's value is the *link*, and a claim
/// citing a D-number nobody wrote is the same defect one level up — a
/// reference that looks like substantiation and is not.
#[test]
fn every_claim_names_a_decision_that_exists() {
    for c in REGISTRY {
        let anchor = format!("<a id=\"{}\"></a>", c.decision);
        assert!(
            REGISTER.contains(&anchor),
            "{}: cites {} , which has no anchor in the decision register",
            c.operation,
            c.decision
        );
        if let Superseded { by } = c.status {
            let anchor = format!("<a id=\"{by}\"></a>");
            assert!(
                REGISTER.contains(&anchor),
                "{}: recorded as superseded by {by}, which has no anchor in the \
                 decision register",
                c.operation
            );
        }
    }
}

/// One `(operation, fixture, metric)` carries one value.
///
/// **The check this release exists to have had.** 47.7 ms and 8.0 ms were
/// published for the same operation, in two documents, 460 lines apart, and
/// nothing noticed for four releases.
///
/// Named for the key rather than for the number, so its soundness is readable
/// from its name: keying on the value would make `2.39 ms` appearing
/// legitimately for two different operations a red test, and a gate that cries
/// wolf is a gate that gets ignored.
///
/// `Superseded` entries are excluded, which is not a loophole — they are the
/// figures this register deliberately keeps after retiring them, and each one
/// must still name the decision that did the retiring (see
/// [`every_claim_names_a_decision_that_exists`]).
#[test]
fn one_operation_fixture_metric_key_carries_one_value() {
    let exempt = |s: &Status| matches!(s, Superseded { .. } | Contested { .. });
    for c in REGISTRY {
        if exempt(&c.status) {
            continue;
        }
        for other in REGISTRY {
            if exempt(&other.status) {
                continue;
            }
            let same_key = c.operation == other.operation
                && c.fixture == other.fixture
                && c.metric == other.metric;
            assert!(
                !same_key || c.value == other.value,
                "one fact, two values.\n  operation: {}\n  fixture:   {}\n  \
                 metric:    {}\n  {} says {:?}\n  {} says {:?}\n\
                 Either these are the same measurement and one document is \
                 stale, or they are different measurements and the fixtures \
                 must say so (D-139).",
                c.operation,
                c.fixture,
                c.metric,
                c.doc_name,
                c.value,
                other.doc_name,
                other.value
            );
        }
    }
}

/// Every contested claim names the value it contests and who reconciles it.
///
/// `Contested` exempts an entry from the one-value rule, so without this it is
/// simply a way to make a red test green. The bar is deliberately awkward: the
/// other value must be reproduced verbatim, and the owner must resolve to a
/// real heading or register anchor, so recording a conflict costs more than
/// fixing one.
#[test]
fn every_contested_claim_names_who_reconciles_it() {
    for c in REGISTRY {
        if let Err(why) = contested_is_well_formed(c, REGISTRY) {
            panic!("{}: {why}", c.doc_name);
        }
    }

    // **The registry currently holds no `Contested` entry** — D-145 closed the
    // only one — so the loop above passes over an empty set and proves nothing.
    // A gate that cannot fail is the thing this file exists to object to, so
    // the detector is exercised against fixtures here, the same way
    // `doc_sync_tests`'s schedule-pattern test keeps its parser alive.
    let peer = || Claim {
        operation: "synthetic",
        fixture: "synthetic",
        metric: "synthetic",
        value: "1.00 ms",
        text: "",
        doc_name: "none",
        doc: "",
        bench_group: "chunk_budget",
        decision: "d-145",
        status: Live,
    };
    let contested = |with, owner| Claim {
        status: Contested { with, owner },
        value: "2.00 ms",
        ..peer()
    };

    let well_formed = [contested("1.00 ms", "d-145"), peer()];
    assert!(
        contested_is_well_formed(&well_formed[0], &well_formed).is_ok(),
        "a contest naming a value its peer publishes, and a real anchor, must pass"
    );

    let no_peer = [contested("9.99 ms", "d-145"), peer()];
    assert!(
        contested_is_well_formed(&no_peer[0], &no_peer).is_err(),
        "a contest against a value nobody publishes must fail"
    );

    let no_owner = [contested("1.00 ms", "d-nonexistent"), peer()];
    assert!(
        contested_is_well_formed(&no_owner[0], &no_owner).is_err(),
        "a contest whose owner anchor resolves to nothing must fail"
    );
}

/// The two conditions a `Contested` entry has to meet.
///
/// Extracted from the test so it can be run against fixtures as well as against
/// the registry. It returns rather than asserts for the same reason: a check
/// that can only panic cannot be shown to fire.
fn contested_is_well_formed(c: &Claim, registry: &[Claim]) -> Result<(), String> {
    let Contested { with, owner } = c.status else {
        return Ok(());
    };

    // The contested value must actually be published under this key, or the
    // entry is describing a disagreement that does not exist.
    let peer = registry.iter().any(|o| {
        o.operation == c.operation
            && o.fixture == c.fixture
            && o.metric == c.metric
            && o.value.contains(with)
    });
    if !peer {
        return Err(format!(
            "contests {with:?}, and no other entry under this key publishes it.              Either the conflict is stale or the peer entry was deleted."
        ));
    }

    let heading = format!("<a id=\"{owner}\"></a>");
    let slug = format!("#{owner}");
    if !(REGISTER.contains(&heading) || APPENDICES.contains(&slug) || REGISTER.contains(&slug)) {
        return Err(format!(
            "contested, and its reconciliation owner {owner:?} resolves to              nothing. A recorded conflict with no owner is an excuse."
        ));
    }
    Ok(())
}

/// The registry still covers the two claims that actually drifted.
///
/// Without this, every assertion above holds vacuously on an empty registry,
/// and the cheapest way to make a red test green is to delete the entry. The
/// seed is not arbitrary: these are the two facts W3 and W4.13 found spread
/// across nine documents in three inconsistent states.
#[test]
fn the_registry_covers_the_claims_that_drifted() {
    for (operation, least) in [("single edge assertion", 5), ("chunk commit, edges, 90 rows", 8)] {
        let n = REGISTRY.iter().filter(|c| c.operation == operation).count();
        assert!(
            n >= least,
            "{operation} has {n} registry entries and had at least {least}. \
             Entries are deleted when a claim is retired from a document — \
             check that the text really went, rather than that this test was \
             in the way."
        );
    }

    let superseded = REGISTRY
        .iter()
        .filter(|c| matches!(c.status, Superseded { .. }))
        .count();
    assert!(
        superseded >= 1,
        "no Superseded entry remains. §9's D-127 paragraph publishes 258 µs \
         for an operation whose live figure is 224 µs, deliberately — if that \
         history was deleted, the register's own practice changed and D-139 \
         needs revisiting."
    );
}
