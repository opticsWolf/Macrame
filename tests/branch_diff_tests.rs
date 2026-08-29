//! What one lineage believes that another does not (§15.4, W12.11, D-228).
//!
//! The payload of the motivating use case: *what did this exploration conclude
//! that the trunk does not know*. Four releases built the parts — a lineage in
//! every read (0.14.4), bounded by the fork point (0.14.6), a lifecycle
//! (0.14.7), writable (0.14.8) — and this is the question they were for.
//!
//! # The claim these tests were written against, and why it is false
//!
//! §15.4 says the answer "is exactly the set of rows carrying the branch's own
//! id", and calls that what makes the query cheap. It is a special case: true
//! when the other lineage is the branch's parent **and has not churned since
//! the fork**, which is the state a fresh fork is in and nothing stays in.
//!
//! Two ordinary shapes break it, and both have a test here:
//!
//! * `the_divergence_can_be_a_row_the_other_lineage_wrote` — the trunk
//!   reweights an inherited edge after the fork. The branch still sees the
//!   pre-fork version, which is the whole of D-223, so it believes something
//!   the trunk does not — and the row carries **`main`'s** id. The branch has
//!   written nothing at all.
//! * `siblings_diverge_through_their_common_ancestor` — one sibling
//!   shadow-retires an edge both inherited. The other still believes it, and
//!   that row belongs to the ancestor.
//!
//! And it errs the other way too: `re_asserting_what_was_already_believed_is
//! _not_a_divergence` writes a row on the branch and changes no belief, so a
//! row-provenance answer would report a conclusion nobody reached.
//!
//! # Why there is no instant parameter
//!
//! `a_shadow_retirement_is_a_divergence_no_instant_filtered_read_can_show` is
//! the reason. A retirement is precisely a divergence about an instant having
//! passed, so any `valid_from <= ts < valid_to` filter drops it from the
//! branch's side and the reader shows an absence instead. The diff compares the
//! whole of both views.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::{BranchId, ConceptUpsert, Database, DbError, Divergence};
use std::sync::Arc;
use std::time::Duration;

/// Valid from long before anything reads, so no assertion here turns on valid
/// time except the two that close an interval on purpose.
const VF: &str = "2020-01-01T00:00:00.000000Z";
/// What a retirement closes an interval at.
const VT: &str = "2021-01-01T00:00:00.000000Z";
/// After every fixture write, for the readers that take an instant.
const NOW: &str = "2030-01-01T00:00:00.000000Z";
const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

/// A step of the injected clock, so a fork point falls strictly between two
/// trunk writes. Transaction time is the axis every fixture here moves on.
const STEP: Duration = Duration::from_secs(86_400);

/// The trunk chain `a → b → c → d`, all recorded before anything forks.
async fn seed(h: &TestHarness) -> Database {
    let db = h.db_with_fake_clock().await;
    for id in ["a", "b", "c", "d"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(VF))
            .await
            .unwrap();
    }
    for (source, target) in [("a", "b"), ("b", "c"), ("c", "d")] {
        db.assert_edge(EdgeAssertion::new(source, target, "LEADSTO").valid_from(VF))
            .await
            .unwrap();
    }
    h.advance(STEP);
    db
}

fn id(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// A divergence as one readable line: what, whose, and whether it is still open.
fn shown(rows: &[Divergence]) -> Vec<String> {
    rows.iter()
        .map(|d| {
            format!(
                "{}→{} on {} w{} {}",
                d.source_id,
                d.target_id,
                d.branch_id,
                d.weight,
                if d.valid_to == SENTINEL {
                    "open"
                } else {
                    "closed"
                }
            )
        })
        .collect()
}

/// How many rows in `links` carry this lineage's id — the quantity §15.4
/// predicted the diff would be equal to.
async fn rows_on(db: &Database, branch: &str) -> i64 {
    db.read_conn()
        .query(
            "SELECT COUNT(*) FROM links WHERE branch_id = ?1",
            libsql::params![branch],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

// ───────────────────────────────────────────────────────────────────────────
// The cases the plan predicted
// ───────────────────────────────────────────────────────────────────────────

/// A fork that has concluded nothing diverges in nothing, in either direction.
///
/// The baseline the O(1) fork buys: inheriting a ledger by resolution rather
/// than by copying means a fresh branch is *identical* to its parent, and the
/// diff is where that stops being an argument about the schema and becomes an
/// observation.
#[tokio::test]
async fn a_fork_that_wrote_nothing_diverges_in_nothing() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();

    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        [] as [String; 0]
    );
    assert_eq!(
        shown(&db.diff(&BranchId::main(), &alt.id).await.unwrap()),
        [] as [String; 0]
    );

    db.close().await.unwrap();
}

/// An assertion the branch made is what the branch concluded.
///
/// The case the plan had in mind, and the one where row provenance and belief
/// difference agree. It is here as the floor rather than as the definition.
#[tokio::test]
async fn an_assertion_on_the_branch_is_what_the_branch_concluded() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(VF)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();

    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        ["a→c on alt w1 open"]
    );
    assert_eq!(
        shown(&db.diff(&BranchId::main(), &alt.id).await.unwrap()),
        [] as [String; 0],
        "the trunk believes nothing the branch does not: the branch only added"
    );

    db.close().await.unwrap();
}

/// A reweight is a divergence on both sides, and the two sides disagree.
///
/// One edge key, two beliefs about its weight, and `diff` is not symmetric:
/// each direction reports the *asking* lineage's value. That is what makes the
/// pair usable — a caller carrying `alt`'s conclusion back to the trunk needs
/// to know what it would be superseding, and `diff(main, alt)` is that.
#[tokio::test]
async fn a_reweight_is_a_divergence_on_both_sides_and_they_disagree() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(VF)
            .weight(0.25)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();

    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        ["a→b on alt w0.25 open"]
    );
    assert_eq!(
        shown(&db.diff(&BranchId::main(), &alt.id).await.unwrap()),
        ["a→b on main w1 open"],
        "the same key from the other side, carrying the trunk's belief"
    );

    db.close().await.unwrap();
}

/// **Why there is no instant parameter.**
///
/// `alt` retires `b → c` by shadowing: its own row at the trunk's key with a
/// closed interval, the trunk's row untouched. That is a conclusion — *this
/// relationship ended* — and it is the one a caller most wants carried back.
///
/// Any read filtered to an instant shows it as an **absence**: the closed row
/// is not live at `NOW`, so `alt`'s edge list is simply shorter, and a diff
/// built on such a read would report nothing from `alt`'s side at all. The
/// second half of this test is that absence, asserted, so the reason the API
/// has no `ts` is pinned rather than explained.
#[tokio::test]
async fn a_shadow_retirement_is_a_divergence_no_instant_filtered_read_can_show() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.retire_edge_on("b", "c", "LEADSTO", VF, VT, alt.id.clone())
        .await
        .unwrap();

    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        ["b→c on alt w1 closed"]
    );
    assert_eq!(
        shown(&db.diff(&BranchId::main(), &alt.id).await.unwrap()),
        ["b→c on main w1 open"],
        "and the trunk still believes what the branch retired"
    );

    // The half that makes the absence of a `ts` parameter a decision.
    let live = macrame::temporal::query_as_of_edges_on(db.read_conn(), NOW, Some(alt.id.as_str()))
        .await
        .unwrap();
    let mut pairs: Vec<_> = live.iter().map(|e| (e.0.as_str(), e.1.as_str())).collect();
    pairs.sort_unstable();
    assert_eq!(
        pairs,
        [("a", "b"), ("c", "d")],
        "an instant-filtered read shows the retirement as a shorter list: \
         `b → c` is simply not in it, and this reader is not anchored at a \
         start node, so `c → d` stays. A diff built on it would report \
         nothing at all from the branch's side"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// The cases the plan's cheap characterisation gets wrong
// ───────────────────────────────────────────────────────────────────────────

/// **The headline.** A divergence can be a row the *other* lineage wrote.
///
/// `alt` forks and writes nothing whatever. The trunk then reweights an edge
/// `alt` inherited. `alt` still sees the pre-fork version — the fork point is a
/// visibility cutoff and the pre-fork weight comes back out of the log
/// ([D-223]) — so `alt` believes something the trunk does not, and the row
/// carrying that belief is **`main`'s**.
///
/// `rows_on(alt) == 0` is asserted beside it, because that is the number §15.4
/// predicted the answer's size would be.
///
/// [D-223]: ../docs/architecture/s13-decision-register.md#d-223
#[tokio::test]
async fn the_divergence_can_be_a_row_the_other_lineage_wrote() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    // The trunk moves on, and `alt` does not.
    db.assert_edge(
        EdgeAssertion::new("b", "c", "LEADSTO")
            .valid_from(VF)
            .weight(0.25),
    )
    .await
    .unwrap();

    assert_eq!(rows_on(&db, "alt").await, 0, "the branch wrote nothing");
    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        ["b→c on main w1 open"],
        "a lineage that has written nothing still believes something the trunk \
         does not, and the row carries the trunk's own id"
    );
    assert_eq!(
        shown(&db.diff(&BranchId::main(), &alt.id).await.unwrap()),
        ["b→c on main w0.25 open"],
        "the same key from the trunk's side, at the weight it corrected to"
    );

    db.close().await.unwrap();
}

/// Two siblings diverge through a row neither of them wrote.
///
/// `b1` and `b2` both fork from `main`. `b2` shadow-retires `c → d`. `b1` still
/// believes it, and the belief it holds is the trunk's row — so the divergence
/// between two branches is attributed to their common ancestor, which is
/// correct and is not expressible as "rows carrying `b1`'s id" under any
/// reading.
#[tokio::test]
async fn siblings_diverge_through_their_common_ancestor() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let b1 = db.fork(id("b1"), BranchId::main()).await.unwrap();
    let b2 = db.fork(id("b2"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.retire_edge_on("c", "d", "LEADSTO", VF, VT, b2.id.clone())
        .await
        .unwrap();

    assert_eq!(rows_on(&db, "b1").await, 0);
    assert_eq!(
        shown(&db.diff(&b1.id, &b2.id).await.unwrap()),
        ["c→d on main w1 open"]
    );
    assert_eq!(
        shown(&db.diff(&b2.id, &b1.id).await.unwrap()),
        ["c→d on b2 w1 closed"]
    );

    db.close().await.unwrap();
}

/// A row on the branch that changes no belief is not a divergence.
///
/// The error in the other direction. `alt` re-asserts an edge it inherited at
/// exactly the value it already had: `links` gains a row carrying `alt`'s id,
/// `links_current` gains one too — the v12 key is lineage-widened — and
/// nothing whatever has been concluded. A row-provenance answer would report
/// it as one.
#[tokio::test]
async fn re_asserting_what_was_already_believed_is_not_a_divergence() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    db.assert_edge(
        EdgeAssertion::new("a", "b", "LEADSTO")
            .valid_from(VF)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();

    assert_eq!(rows_on(&db, "alt").await, 1, "the write did land");
    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        [] as [String; 0],
        "a row that restates what was already believed is not a conclusion"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Shape, order, and the refusals
// ───────────────────────────────────────────────────────────────────────────

/// Several divergences at once, in edge-key order, from both directions.
///
/// The ordering is the query's rather than the engine's, so two diffs of the
/// same pair are directly comparable — which matters because `Divergence`
/// carries an `f64` and cannot derive `Ord` to sort itself.
#[tokio::test]
async fn divergences_come_back_in_edge_key_order() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);

    for (source, target) in [("c", "a"), ("a", "d"), ("b", "d")] {
        db.assert_edge(
            EdgeAssertion::new(source, target, "LEADSTO")
                .valid_from(VF)
                .on_branch(alt.id.clone()),
        )
        .await
        .unwrap();
    }

    assert_eq!(
        shown(&db.diff(&alt.id, &BranchId::main()).await.unwrap()),
        [
            "a→d on alt w1 open",
            "b→d on alt w1 open",
            "c→a on alt w1 open"
        ]
    );

    db.close().await.unwrap();
}

/// A lineage does not diverge from itself, forked ledger or not.
///
/// The unforked case reaches the `Trunk` arm, where the answer is empty because
/// `branches` holding one row means both names resolve to it — exact rather
/// than a shortcut, for the same reason `lineage_shape` is exact.
#[tokio::test]
async fn a_lineage_does_not_diverge_from_itself() {
    let h = TestHarness::new();
    let db = seed(&h).await;

    assert_eq!(
        db.diff(&BranchId::main(), &BranchId::main())
            .await
            .unwrap()
            .len(),
        0,
        "a ledger that never forked"
    );

    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(VF)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();

    for name in [BranchId::main(), alt.id.clone()] {
        assert_eq!(
            db.diff(&name, &name).await.unwrap().len(),
            0,
            "{name} diverged from itself"
        );
    }

    db.close().await.unwrap();
}

/// An unregistered lineage is refused by name, on either side of the pair.
///
/// [D-069]'s rule: answering for the trunk would be a right-looking answer to a
/// question that was not asked, and on a diff it would be worse than usual —
/// `diff(ghost, main)` would come back empty, which reads exactly like *these
/// two agree*.
///
/// [D-069]: ../docs/architecture/s13-decision-register.md#d-069
#[tokio::test]
async fn an_unregistered_lineage_is_refused_by_name() {
    let h = TestHarness::new();
    let db = seed(&h).await;
    db.fork(id("alt"), BranchId::main()).await.unwrap();

    for (a, b, named) in [
        ("ghost", "main", "ghost"),
        ("main", "ghost", "ghost"),
        ("alt", "phantom", "phantom"),
        // Both unknown: the first is named, because it is the one the caller
        // asked about and a pair of names in one message helps nobody.
        ("ghost", "phantom", "ghost"),
    ] {
        let err = db
            .diff(&id(a), &id(b))
            .await
            .expect_err("the diff answered for a lineage that does not exist");
        assert!(
            matches!(err, DbError::UnknownBranch(ref w) if w == named),
            "diff({a}, {b}) named the wrong lineage: {err:?}"
        );
    }

    db.close().await.unwrap();
}

/// The view's diff is the handle's diff with the view's lineage in front.
///
/// One delegating call, like every other method on the view, and the assertion
/// is that the direction is the one the view's holder is in a position to ask:
/// *what did I conclude that they do not know*.
#[tokio::test]
async fn the_view_diffs_from_its_own_lineage() {
    let h = TestHarness::new();
    let db = Arc::new(seed(&h).await);
    let alt = db.fork(id("alt"), BranchId::main()).await.unwrap();
    h.advance(STEP);
    db.assert_edge(
        EdgeAssertion::new("a", "c", "LEADSTO")
            .valid_from(VF)
            .on_branch(alt.id.clone()),
    )
    .await
    .unwrap();

    let view = db.view(alt.id.clone());
    assert_eq!(
        view.diff(&BranchId::main()).await.unwrap(),
        db.diff(&alt.id, &BranchId::main()).await.unwrap()
    );
    assert_eq!(
        shown(&view.diff(&BranchId::main()).await.unwrap()),
        ["a→c on alt w1 open"]
    );

    // The view holds a clone of the `Arc` and the owner cannot close the
    // handle while it is alive (D-226); it is the caller's to drop.
    drop(view);
    Arc::try_unwrap(db)
        .unwrap_or_else(|_| panic!("the view outlived the assertion"))
        .close()
        .await
        .unwrap();
}
