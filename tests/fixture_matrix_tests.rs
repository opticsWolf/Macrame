//! The fixture matrix pins its own shapes (T4.1, D-088).
//!
//! A fixture is only useful if what it *is* is checked. The defect T4.1 exists
//! to prevent — D-070 — was not "the wrong fixture was chosen"; it was that
//! nobody had stated what the fixture was, so a conclusion that only held for
//! trees was recorded as general. A shape whose properties drift is the same
//! defect with extra steps: `clustered` quietly becoming a tree would make
//! every future measurement on it agree with `star_of_stars` for reasons no
//! results table would show.
//!
//! So each shape asserts the property it was chosen for, and — more
//! importantly — asserts that it **differs from the others** on that property.
//! Four fixtures that all behave alike are one fixture.

#[path = "common/fixtures.rs"]
mod fixtures;

use fixtures::{facts, node_id, Shape, ALL_SHAPES, CLUSTER_SIZE, DENSE_SMALL_CAP};

use std::collections::HashSet;

const NODES: usize = 600;
const DEPTH: usize = 3;

/// No shape may assert the same ordered pair twice.
///
/// `trg_links_single_open` rejects a second **open** interval for one
/// (source, target, edge_type), and every fixture edge is open. A duplicate
/// therefore does not produce a slightly-wrong fixture; it produces a
/// `SingleOpenViolation` at seed time, which is a confusing failure a long way
/// from its cause. Checked here so the failure names the shape.
#[test]
fn no_shape_emits_a_duplicate_ordered_pair() {
    for &shape in ALL_SHAPES {
        let edges = shape.edges(NODES);
        let mut seen = HashSet::new();
        for e in &edges {
            assert!(
                seen.insert((e.source.clone(), e.target.clone(), e.edge_type.clone())),
                "{}: emits {} -> {} twice, which the single-open trigger rejects",
                shape.name(),
                e.source,
                e.target
            );
        }
    }
}

/// No shape may assert a self-loop, and every endpoint must be a seeded concept.
///
/// A dangling edge is not a denser graph; it is an edge the traversal drops
/// when it joins to `concepts`, so the shape a measurement runs on would be
/// quietly smaller than the shape this file describes.
#[test]
fn every_edge_endpoint_is_a_seeded_concept() {
    for &shape in ALL_SHAPES {
        let ids: HashSet<String> = (0..NODES).map(node_id).collect();
        for e in shape.edges(NODES) {
            assert_ne!(e.source, e.target, "{}: self-loop", shape.name());
            assert!(
                ids.contains(&e.source),
                "{}: dangling {}",
                shape.name(),
                e.source
            );
            assert!(
                ids.contains(&e.target),
                "{}: dangling {}",
                shape.name(),
                e.target
            );
        }
    }
}

/// Every shape is a pure function of its node count.
///
/// The property that makes a figure re-runnable. Cheap to assert and the kind
/// of thing that is true until someone reaches for a timestamp or a hash of the
/// hostname to "vary" a fixture.
#[test]
fn every_shape_is_deterministic() {
    for &shape in ALL_SHAPES {
        let a = shape.edges(NODES);
        let b = shape.edges(NODES);
        assert_eq!(a.len(), b.len(), "{}: edge count varies", shape.name());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(
                (&x.source, &x.target),
                (&y.source, &y.target),
                "{}: edge set varies between calls",
                shape.name()
            );
        }
    }
}

/// `star_of_stars` is a tree, and that is exactly why it hid D-070.
///
/// The assertion worth having is not "it is a tree" on its own — it is that a
/// tree's simple-path count **equals** its reached-node count, so every cost
/// that grows with path count is invisible on it. That equality is the
/// mechanism of the defect, stated as a test.
#[test]
fn star_of_stars_is_a_tree_so_its_path_multiplier_is_one() {
    let f = facts(Shape::StarOfStars, NODES, DEPTH);
    // A tree on n reachable nodes has one path to each.
    assert_eq!(
        f.simple_paths, f.reached,
        "star_of_stars is no longer a tree: {f:?} — every pre-0.6.0 figure was \
         taken on the tree, so changing it orphans the record"
    );
    assert!(
        (f.path_multiplier() - 1.0).abs() < f64::EPSILON,
        "expected multiplier 1.0, got {}",
        f.path_multiplier()
    );
    // And it is the high-fan-out case: the root holds a third of all edges.
    assert!(
        f.max_out_degree > NODES / 4,
        "star_of_stars lost its hub: max out-degree {} over {} nodes",
        f.max_out_degree,
        NODES
    );
}

/// `clustered` enumerates paths, which is the cost the tree cannot show.
///
/// The bound is deliberately loose — the exact count is a function of
/// `CLUSTER_SIZE` and depth and would make this a change-detector — but the
/// *order* is the claim: at depth 3 inside a 12-clique there must be many more
/// distinct paths than reachable nodes.
#[test]
fn clustered_enumerates_far_more_paths_than_nodes() {
    let f = facts(Shape::Clustered, NODES, DEPTH);
    assert!(
        f.path_multiplier() > 10.0,
        "clustered stopped enumerating paths ({} paths for {} nodes, {:.1}x) — \
         this is the shape T0.1 and D-076 exist for",
        f.simple_paths,
        f.reached,
        f.path_multiplier()
    );
    // It must be genuinely clustered: a community is a clique of CLUSTER_SIZE,
    // so a member links to every other member.
    assert!(
        f.max_out_degree >= CLUSTER_SIZE - 1,
        "clustered communities are no longer dense: max out-degree {}",
        f.max_out_degree
    );
}

/// `chain` is deep and narrow: depth-bounded reach, near-unit branching.
#[test]
fn chain_is_deep_and_narrow() {
    let f = facts(Shape::Chain, NODES, DEPTH);
    // A depth-3 walk down a spine with spurs reaches a handful of nodes, not a
    // sizeable fraction of the graph. That is the property: cost here is fold
    // *length*, reached only by walking far, not by fanning out.
    assert!(
        f.reached < 20,
        "chain fans out: {} nodes reached in {DEPTH} hops",
        f.reached
    );
    assert!(
        f.max_out_degree <= 2,
        "chain is branching: max out-degree {}",
        f.max_out_degree
    );
    // The spine spans the whole node range, which is what makes it long.
    assert!(
        f.edges >= NODES - 1,
        "chain lost its spine: {} edges over {NODES} nodes",
        f.edges
    );
}

/// `dense_small` is quadratic in edges and capped in nodes.
///
/// The cap is a stated property, not an implementation detail: a caller asking
/// for 10,000 nodes gets 300, and a results table that reports "10,000 nodes"
/// for this shape is reporting the request rather than the fixture.
#[test]
fn dense_small_is_quadratic_and_capped() {
    let f = facts(Shape::DenseSmall, 10_000, 2);
    assert_eq!(
        f.edges,
        DENSE_SMALL_CAP * (DENSE_SMALL_CAP - 1),
        "dense_small is no longer near-complete at its cap"
    );
    // Capped: the request was 10,000 nodes and the fixture is 300.
    assert!(
        f.max_out_degree == DENSE_SMALL_CAP - 1,
        "dense_small node is not linked to every other: out-degree {}",
        f.max_out_degree
    );
    // Two hops from anywhere reaches everything, and the path count between any
    // pair is the whole point — this is the DISTINCT sort's worst case.
    assert_eq!(f.reached, DENSE_SMALL_CAP);
    assert!(f.path_multiplier() > 100.0, "{:?}", f);
}

/// The four shapes actually differ on the property they were chosen for.
///
/// This is the test that makes the matrix a matrix. If a future edit made all
/// four behave alike, every individual assertion above could still pass while
/// the set as a whole had stopped discriminating anything — which is the
/// single-fixture problem restored under four names.
#[test]
fn the_matrix_discriminates() {
    let all: Vec<_> = ALL_SHAPES
        .iter()
        .map(|&s| (s.name(), facts(s, NODES, DEPTH)))
        .collect();

    let star = &all[0].1;
    let clustered = &all[1].1;
    let chain = &all[2].1;
    let dense = &all[3].1;

    // Path enumeration: flat on the tree, enormous elsewhere. This ordering is
    // the finding D-076 recorded, promoted to an assertion.
    assert!(star.path_multiplier() < clustered.path_multiplier());
    assert!(clustered.path_multiplier() < dense.path_multiplier());

    // Fan-out: the star is a hub, the chain is not, by two orders.
    assert!(star.max_out_degree > 10 * chain.max_out_degree);

    // Reach at fixed depth: the chain is the narrowest of the four.
    for (name, f) in &all[..] {
        if *name != "chain" {
            assert!(
                f.reached > chain.reached,
                "{name} reaches {} nodes, no more than chain's {}",
                f.reached,
                chain.reached
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The rule the matrix exists to enforce (T4.1, D-088)
// ---------------------------------------------------------------------------

/// The decision register, at compile time.
const REGISTER: &str = include_str!("../docs/architecture/s13-decision-register.md");

/// Decision entries whose conclusion rests on a measurement over a graph.
///
/// A hand-maintained list, and the limitation is worth stating rather than
/// hiding: nothing here can tell that a *newly added* entry is a performance
/// one, so adding a decision without adding it to this list is not caught. What
/// is caught is the failure that actually happened — an annotation that rots,
/// is deleted in an edit, or names a fixture that no longer exists. That is the
/// same partial enforcement `no_group_is_opened_without_its_control` provides
/// for T4.3, and it is stated there for the same reason.
const PERFORMANCE_DECISIONS: &[&str] = &[
    "d-047", // byte-budget accounting and the loader's growth-rate test
    "d-055", // the §9 budget table
    "d-059", // the single-open probe's index
    "d-063", // the Subgraph rewrite, retired on CPU
    "d-064", // the overlap guard's plan-shape regression
    "d-070", // load_subgraph's superlinearity
    "d-076", // the correction that produced the rule
    "d-086", // the bulk-path pipelining sweep
    "d-087", // Subgraph interning, on bytes rather than milliseconds
    "d-088", // the matrix itself
];

/// The body of one register entry: from its anchor to the next entry's.
fn entry(id: &str) -> &'static str {
    let anchor = format!("<a id=\"{id}\"></a>");
    let start = REGISTER
        .find(&anchor)
        .unwrap_or_else(|| panic!("{id} is not in the decision register"));
    let rest = &REGISTER[start + anchor.len()..];
    match rest.find("\n<a id=\"d-") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

/// **Every performance decision names the fixture it was measured on.**
///
/// T4.1's stated rule, and the reason the matrix is worth its maintenance:
/// D-070's entry *"would then have read 'inherent on `star_of_stars`'"*, and the
/// gap D-076 later found would have been a qualifier rather than a wave.
///
/// The rule was applied prospectively when the matrix landed and the six older
/// performance entries were annotated afterwards. This is what keeps that from
/// happening again — and, more usefully, what makes the annotations break if a
/// shape is ever renamed, since the accepted names come from `ALL_SHAPES`
/// rather than from a second list.
#[test]
fn every_performance_decision_names_its_fixture() {
    for id in PERFORMANCE_DECISIONS {
        let body = entry(id);
        // Backticked only. Matching the bare word would accept "chain" from
        // "snapshot chain" and "the chain of stars", which it did on the first
        // run — a tripwire that fires on prose is one that gets disabled.
        // `star_of_stars` is hyphenated in entries that predate the matrix.
        let named = ALL_SHAPES.iter().map(|s| s.name()).any(|n| {
            body.contains(&format!("`{n}`")) || body.contains(&format!("`{}`", n.replace('_', "-")))
        });
        // Not every performance conclusion rests on a *graph*: D-087 was
        // measured on `Subgraph`'s own byte accounting with no database at all.
        // The opt-out is explicit and still requires a sentence, because "this
        // was not measured on a fixture" is itself worth stating.
        let opted_out = body.contains("**Fixture: none");
        assert!(
            named || opted_out,
            "{id} draws a conclusion from a measurement and does not name the \
             fixture it was taken on (T4.1, D-088). Add a `**Fixture: …**` line \
             naming one of {:?} in backticks, or `**Fixture: none — …**` if it \
             was not measured over a graph, or drop it from \
             PERFORMANCE_DECISIONS if it is no longer a performance entry.",
            ALL_SHAPES.iter().map(|s| s.name()).collect::<Vec<_>>()
        );
    }
}

/// No entry names a fixture the matrix does not have.
///
/// The other direction, and the one that catches a rename. `Shape::name` is the
/// single source of these strings, so renaming `dense_small` turns every
/// register reference to it into a dangling name — which this reports, rather
/// than leaving the document quietly describing a fixture that is gone.
#[test]
fn the_register_names_no_fixture_the_matrix_lacks() {
    let known: Vec<String> = ALL_SHAPES
        .iter()
        .flat_map(|s| [s.name().to_string(), s.name().replace('_', "-")])
        .collect();

    // Every backticked token in the register that looks like a fixture name:
    // lowercase words joined by underscores, at least two of them. Deliberately
    // narrow — this is a tripwire, not a parser.
    for token in REGISTER.split('`').skip(1).step_by(2) {
        let looks_like_a_fixture = token.contains('_')
            && token.len() > 4
            && token.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            && token.starts_with(|c: char| c.is_ascii_lowercase());
        if !looks_like_a_fixture {
            continue;
        }
        // Only tokens that are *claimed* as fixtures, i.e. appear next to the
        // word. Without this the test would flag every snake_case identifier in
        // the document, which is most of them.
        if !known.iter().any(|k| k == token) && REGISTER.contains(&format!("fixture `{token}`")) {
            panic!(
                "the register calls `{token}` a fixture and the matrix has no such \
                 shape (T4.1, D-088). Known: {known:?}"
            );
        }
    }
}
