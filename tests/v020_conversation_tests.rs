//! Macrame 0.20.0 — the conversation-branching release (M1–M5, M7;
//! D-291…D-295).
//!
//! One test module per spec item. Every lineage here is cut through the public
//! API, and every scenario is the shape a chat harness produces: immutable
//! turns, a head pointer per branch, forks at past turns, and a search scoped
//! to the branch being read.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::{AttributeMode, EdgeAssertion, TraversalBuilder};
use macrame::{BranchId, ConceptUpsert, Database, DbError, PointerSet, ReadPlan, WriteBatch};
use std::time::{Duration, SystemTime};

const VF: &str = "2026-01-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";

fn harness() -> TestHarness {
    // 2026-01-01: a plausible clock, so recorded stamps read like dates.
    TestHarness::starting_at(SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600))
}

fn b(name: &str) -> BranchId {
    BranchId::new(name).unwrap()
}

/// Ids a lineage reaches from `start`, through every edge type.
async fn reach(db: &Database, start: &str, branch: Option<&str>) -> Vec<String> {
    let mut t = TraversalBuilder::new(start).max_depth(64);
    if let Some(b) = branch {
        t = t.on_branch(b);
    }
    t.execute_ids(db.read_conn(), NOW).await.unwrap()
}

async fn title_on(db: &Database, id: &str, branch: Option<&str>) -> Option<String> {
    let mut t = TraversalBuilder::new(id)
        .max_depth(0)
        .attribute_mode(AttributeMode::Current);
    if let Some(b) = branch {
        t = t.on_branch(b);
    }
    t.execute(db.read_conn(), NOW)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.id == id)
        .map(|n| n.title)
}

/// A session with turns `t0..t{n-1}` on the trunk, each written as one
/// `write_batch` — the turn, its NEXT edge from the previous turn, and the
/// session's HEAD pointer — as a harness would.
async fn trunk_turns(db: &Database, n: usize) {
    db.register_edge_kind("HEAD", true).await.unwrap();
    db.upsert_concept(ConceptUpsert::new("session", "s").valid_from(VF))
        .await
        .unwrap();
    for i in 0..n {
        append(db, None, &format!("t{i}"), &format!("turn {i}"), i).await;
    }
}

/// One turn on `branch`: concept, NEXT edge from the current head, HEAD moved.
async fn append(db: &Database, branch: Option<&str>, id: &str, title: &str, k: usize) {
    let plan = match branch {
        Some(name) => ReadPlan::new().on(BranchId::new(name).unwrap()),
        None => ReadPlan::new(),
    };
    let head = db
        .pointer("session", "HEAD", plan)
        .await
        .unwrap()
        .unwrap_or_else(|| "session".to_string());
    // Valid time is the harness's own clock (Doctrine II): one minute a turn.
    let vf = format!("2026-01-01T00:{:02}:00.000000Z", k % 60);
    let mut batch = WriteBatch::new()
        .concept(ConceptUpsert::new(id, title).valid_from(VF))
        .edge(EdgeAssertion::new(&head, id, "NEXT").valid_from(VF))
        .pointer(PointerSet::new("session", "HEAD", id).valid_from(vf));
    if let Some(b) = branch {
        batch = batch.on_branch(BranchId::new(b).unwrap());
    }
    db.write_batch(batch).await.unwrap();
}

// ── M1 · fork at a past instant ─────────────────────────────────────────────

mod m1_fork_at {
    use super::*;

    #[tokio::test]
    async fn fork_after_a_turn_inherits_up_to_it_and_nothing_later() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 10).await;

        let alt = db
            .fork_after(b("alt"), BranchId::main(), "t7")
            .await
            .unwrap();
        assert!(
            alt.forked_at.as_deref().unwrap() < alt.created_at.as_str(),
            "a historical fork has its cutoff behind its creation"
        );

        // The trunk keeps writing after the fork.
        db.upsert_concept(ConceptUpsert::new("t10", "late").valid_from(VF))
            .await
            .unwrap();
        db.assert_edge(EdgeAssertion::new("t9", "t10", "NEXT").valid_from(VF))
            .await
            .unwrap();

        let on_alt = reach(&db, "session", Some("alt")).await;
        let want: Vec<String> = (0..=7).map(|i| format!("t{i}")).collect();
        assert_eq!(
            on_alt
                .iter()
                .filter(|i| i.starts_with('t'))
                .cloned()
                .collect::<Vec<_>>(),
            want,
            "alt sees t0..t7 and none of t8, t9, t10"
        );
        let on_main = reach(&db, "session", None).await;
        assert_eq!(on_main.len(), 12, "{on_main:?}");
    }

    #[tokio::test]
    async fn fork_at_names_an_instant_and_refuses_the_future() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 3).await;
        let mid = db
            .concepts_by_id(&["t1".to_string()], ReadPlan::new())
            .await
            .unwrap()
            .remove(0)
            .recorded_at;

        let past = db.fork_at(b("past"), BranchId::main(), &mid).await.unwrap();
        assert_eq!(past.forked_at.as_deref(), Some(mid.as_str()));
        let seen = reach(&db, "session", Some("past")).await;
        assert!(seen.contains(&"t0".to_string()) && !seen.contains(&"t2".to_string()));

        let err = db
            .fork_at(b("later"), BranchId::main(), "2099-01-01T00:00:00.000000Z")
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ForkInFuture { .. }), "{err:?}");

        let err = db
            .fork_at(b("bad"), BranchId::main(), "yesterday")
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidTimestamp { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_child_cannot_be_cut_before_its_parent() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 4).await;
        db.fork_after(b("b1"), BranchId::main(), "t2")
            .await
            .unwrap();
        // t0 was minted before b1's own fork point.
        let err = db.fork_after(b("c1"), b("b1"), "t0").await;
        // t0 is visible on b1, but cutting c1 at t0's mint precedes b1's cut.
        assert!(
            matches!(err, Err(DbError::ForkPrecedesParent { .. })),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn fork_after_refuses_a_concept_the_parent_cannot_see() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(
            ConceptUpsert::new("only-alt", "x")
                .valid_from(VF)
                .on_branch(b("alt")),
        )
        .await
        .unwrap();
        // minted on a sibling of main's other children, invisible from main
        let err = db
            .fork_after(b("x"), BranchId::main(), "only-alt")
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ConceptNotVisible { .. }), "{err:?}");
        // and an id that does not exist at all is the same answer
        let err = db
            .fork_after(b("y"), BranchId::main(), "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ConceptNotVisible { .. }), "{err:?}");
        // a trunk concept minted after alt's fork is invisible from alt
        db.upsert_concept(ConceptUpsert::new("late", "x").valid_from(VF))
            .await
            .unwrap();
        let err = db.fork_after(b("z"), b("alt"), "late").await.unwrap_err();
        assert!(matches!(err, DbError::ConceptNotVisible { .. }), "{err:?}");
    }
}

// ── M4 · lineage-correct concept reads ──────────────────────────────────────

mod m4_concepts {
    use super::*;

    #[tokio::test]
    async fn a_branch_keeps_the_version_it_inherited() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.upsert_concept(ConceptUpsert::new("run", "before fork").valid_from(VF))
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("run", "after fork").valid_from(VF))
            .await
            .unwrap();

        assert_eq!(
            title_on(&db, "run", Some("alt")).await.as_deref(),
            Some("before fork")
        );
        assert_eq!(
            title_on(&db, "run", None).await.as_deref(),
            Some("after fork")
        );

        let v = db
            .concepts_by_id(&["run".to_string()], ReadPlan::new().on(b("alt")))
            .await
            .unwrap();
        assert_eq!(v[0].title, "before fork");
        assert_eq!(v[0].branch_id, "main");
    }

    #[tokio::test]
    async fn a_sibling_concept_is_invisible_from_the_trunk() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.upsert_concept(ConceptUpsert::new("root", "r").valid_from(VF))
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(
            ConceptUpsert::new("mine", "m")
                .valid_from(VF)
                .on_branch(b("alt")),
        )
        .await
        .unwrap();

        assert!(reach(&db, "mine", None).await.is_empty());
        assert_eq!(
            reach(&db, "mine", Some("alt")).await,
            vec!["mine".to_string()]
        );
        assert!(db
            .concepts_by_id(&["mine".to_string()], ReadPlan::new())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn retirement_after_the_fork_does_not_reach_the_branch() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.upsert_concept(ConceptUpsert::new("x", "x").valid_from(VF))
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("x", "x").valid_from(VF).retired(true))
            .await
            .unwrap();
        assert!(reach(&db, "x", None).await.is_empty(), "retired on main");
        assert_eq!(reach(&db, "x", Some("alt")).await, vec!["x".to_string()]);
    }

    #[tokio::test]
    async fn a_recorded_instant_composes_with_the_cutoff() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.upsert_concept(ConceptUpsert::new("c", "v1").valid_from(VF))
            .await
            .unwrap();
        let v1_at = db
            .concepts_by_id(&["c".to_string()], ReadPlan::new())
            .await
            .unwrap()
            .remove(0)
            .recorded_at;
        db.upsert_concept(ConceptUpsert::new("c", "v2").valid_from(VF))
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("c", "v3").valid_from(VF))
            .await
            .unwrap();

        let at = |plan: ReadPlan| {
            let db = &db;
            async move {
                db.concepts_by_id(&["c".to_string()], plan)
                    .await
                    .unwrap()
                    .remove(0)
                    .title
            }
        };
        assert_eq!(at(ReadPlan::new().on(b("alt"))).await, "v2");
        assert_eq!(at(ReadPlan::new()).await, "v3");
        assert_eq!(
            at(ReadPlan::new().on(b("alt")).recorded_at(&v1_at)).await,
            "v1"
        );
        assert_eq!(at(ReadPlan::new().recorded_at(&v1_at)).await, "v1");
    }

    #[tokio::test]
    async fn the_subgraph_loader_hydrates_the_lineage_version() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("t1", "edited on main").valid_from(VF))
            .await
            .unwrap();
        let g = db
            .load_subgraph_with(
                &TraversalBuilder::new("session")
                    .max_depth(4)
                    .on_branch("alt"),
                NOW,
                1 << 20,
            )
            .await
            .unwrap();
        assert_eq!(g.node("t1").unwrap().title(), "turn 1");
        let g = db
            .load_subgraph_with(&TraversalBuilder::new("session").max_depth(4), NOW, 1 << 20)
            .await
            .unwrap();
        assert_eq!(g.node("t1").unwrap().title(), "edited on main");
    }

    #[tokio::test]
    async fn a_single_lineage_database_reads_exactly_as_before() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 3).await;
        db.upsert_concept(ConceptUpsert::new("t1", "edited").valid_from(VF))
            .await
            .unwrap();
        assert_eq!(title_on(&db, "t1", None).await.as_deref(), Some("edited"));
        assert_eq!(reach(&db, "session", None).await.len(), 4);
    }
}

// ── M5 · lineage-scoped search ──────────────────────────────────────────────

mod m5_search {
    use super::*;
    use macrame::vector::{
        keyword_search, keyword_search_on, search_vector_on, HybridSearch, ModelName,
    };

    /// main: "zebra trunk" (minted before the fork) and "zebra late" (after);
    /// alt: "zebra alt"; beta: "zebra beta". Each concept has a 4-d vector.
    async fn seeded(db: &Database) -> ModelName {
        let m = ModelName::new("m4").unwrap();
        db.register_model(&m, 4).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("trunk", "zebra trunk").valid_from(VF))
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.fork(b("beta"), BranchId::main()).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("late", "zebra late").valid_from(VF))
            .await
            .unwrap();
        db.upsert_concept(
            ConceptUpsert::new("alt1", "zebra alt")
                .valid_from(VF)
                .on_branch(b("alt")),
        )
        .await
        .unwrap();
        db.upsert_concept(
            ConceptUpsert::new("beta1", "zebra beta")
                .valid_from(VF)
                .on_branch(b("beta")),
        )
        .await
        .unwrap();
        db.upsert_embeddings(
            &m,
            vec![
                ("trunk".into(), vec![1.0, 0.0, 0.0, 0.0]),
                ("late".into(), vec![0.9, 0.1, 0.0, 0.0]),
                ("alt1".into(), vec![0.8, 0.2, 0.0, 0.0]),
                ("beta1".into(), vec![0.7, 0.3, 0.0, 0.0]),
            ],
        )
        .await
        .unwrap();
        m
    }

    fn ids(v: Vec<(String, f64)>) -> Vec<String> {
        let mut out: Vec<String> = v.into_iter().map(|(i, _)| i).collect();
        out.sort();
        out
    }

    #[tokio::test]
    async fn keyword_search_on_a_branch_finds_only_what_it_holds() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        seeded(&db).await;
        let c = db.read_conn();
        assert_eq!(
            ids(keyword_search_on(c, "zebra", 10, None, None, Some("alt"))
                .await
                .unwrap()),
            ["alt1", "trunk"],
            "alt holds its own concept and the pre-fork trunk concept"
        );
        assert_eq!(
            ids(keyword_search_on(c, "zebra", 10, None, None, Some("main"))
                .await
                .unwrap()),
            ["late", "trunk"],
        );
        assert_eq!(
            ids(keyword_search(c, "zebra", 10, None, None).await.unwrap()),
            ["alt1", "beta1", "late", "trunk"],
            "unscoped search is still the whole file"
        );
        let err = keyword_search_on(c, "zebra", 10, None, None, Some("nope"))
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::UnknownBranch(_)), "{err:?}");
    }

    #[tokio::test]
    async fn the_limit_applies_after_the_lineage_filter() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        seeded(&db).await;
        let hits = keyword_search_on(db.read_conn(), "zebra", 1, None, None, Some("beta"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(["beta1", "trunk"].contains(&hits[0].0.as_str()));
    }

    #[tokio::test]
    async fn vector_and_hybrid_search_are_scoped_too() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        let m = seeded(&db).await;
        let c = db.read_conn();
        let q = vec![1.0, 0.0, 0.0, 0.0];
        let mut v: Vec<String> = search_vector_on(c, &q, &m, 10, None, None, Some("beta"))
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.concept_id)
            .collect();
        v.sort();
        assert_eq!(v, ["beta1", "trunk"]);

        let mut hy: Vec<String> = HybridSearch::new(m.clone(), "zebra", q.clone())
            .on_branch("alt")
            .execute(c)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.concept_id)
            .collect();
        hy.sort();
        assert_eq!(hy, ["alt1", "trunk"]);
    }

    #[tokio::test]
    async fn a_trunk_retirement_after_the_fork_still_shows_on_the_branch() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        seeded(&db).await;
        db.upsert_concept(
            ConceptUpsert::new("trunk", "zebra trunk")
                .valid_from(VF)
                .retired(true),
        )
        .await
        .unwrap();
        let c = db.read_conn();
        assert_eq!(
            ids(keyword_search_on(c, "zebra", 10, None, None, Some("alt"))
                .await
                .unwrap()),
            ["alt1", "trunk"]
        );
        assert_eq!(
            ids(keyword_search_on(c, "zebra", 10, None, None, Some("main"))
                .await
                .unwrap()),
            ["late"]
        );
    }
}

// ── M2 · atomic mixed writes ────────────────────────────────────────────────

mod m2_write_batch {
    use super::*;
    use macrame::Retirement;

    #[tokio::test]
    async fn every_row_of_a_batch_shares_one_stamp() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 1).await;
        let report = db
            .write_batch(
                WriteBatch::new()
                    .concept(ConceptUpsert::new("a", "A").valid_from(VF))
                    .concept(ConceptUpsert::new("b", "B").valid_from(VF))
                    .edge(EdgeAssertion::new("a", "b", "REL").valid_from(VF)),
            )
            .await
            .unwrap();
        assert_eq!((report.concepts, report.edges), (2, 1));
        let v = db
            .concepts_by_id(&["a".into(), "b".into()], ReadPlan::new())
            .await
            .unwrap();
        assert!(v.iter().all(|c| c.recorded_at == report.recorded_at));
        let feed = db.changes(0, None, &["links"], 1000).await.unwrap();
        let rel = feed
            .iter()
            .find(|c| c.entity_id.starts_with("a|b|REL"))
            .unwrap();
        assert_eq!(rel.recorded_at, report.recorded_at);
    }

    #[tokio::test]
    async fn a_bad_row_rolls_back_the_whole_batch() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        // The edge names a concept that does not exist anywhere.
        let err = db
            .write_batch(
                WriteBatch::new()
                    .concept(ConceptUpsert::new("ok", "fine").valid_from(VF))
                    .edge(EdgeAssertion::new("ok", "missing", "REL").valid_from(VF)),
            )
            .await
            .unwrap_err();
        assert!(!matches!(err, DbError::BatchTooLarge { .. }), "{err:?}");
        assert!(
            db.concepts_by_id(&["ok".into()], ReadPlan::new())
                .await
                .unwrap()
                .is_empty(),
            "the concept rolled back with the edge"
        );
    }

    #[tokio::test]
    async fn rows_claim_the_batch_lineage_and_refuse_another() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 1).await;
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.fork(b("other"), BranchId::main()).await.unwrap();
        db.write_batch(
            WriteBatch::new()
                .on_branch(b("alt"))
                .concept(ConceptUpsert::new("x", "on alt").valid_from(VF)),
        )
        .await
        .unwrap();
        let v = db
            .concepts_by_id(&["x".into()], ReadPlan::new().on(b("alt")))
            .await
            .unwrap();
        assert_eq!(v[0].branch_id, "alt");

        let err = db
            .write_batch(
                WriteBatch::new().on_branch(b("alt")).concept(
                    ConceptUpsert::new("y", "y")
                        .valid_from(VF)
                        .on_branch(b("other")),
                ),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::BranchMismatch { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn the_cap_and_duplicate_ids_are_refused_before_the_lock() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        let mut big = WriteBatch::new();
        for i in 0..=macrame::MAX_BATCH_ROWS {
            big = big.concept(ConceptUpsert::new(format!("c{i}"), "c").valid_from(VF));
        }
        assert!(matches!(
            db.write_batch(big).await.unwrap_err(),
            DbError::BatchTooLarge { .. }
        ));
        let err = db
            .write_batch(
                WriteBatch::new()
                    .concept(ConceptUpsert::new("d", "1").valid_from(VF))
                    .concept(ConceptUpsert::new("d", "2").valid_from(VF)),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidId { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn retirements_ride_the_batch() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.write_batch(
            WriteBatch::new()
                .concept(ConceptUpsert::new("a", "A").valid_from(VF))
                .concept(ConceptUpsert::new("b", "B").valid_from(VF))
                .edge(EdgeAssertion::new("a", "b", "REL").valid_from(VF)),
        )
        .await
        .unwrap();
        db.write_batch(WriteBatch::new().retire(Retirement::new(
            "a",
            "b",
            "REL",
            VF,
            "2026-02-01T00:00:00.000000Z",
        )))
        .await
        .unwrap();
        assert_eq!(reach(&db, "a", None).await, vec!["a".to_string()]);
    }

    /// Why M2 is in the same release as M1: written as two calls, a turn's
    /// concept and its edge carry two stamps, and a fork after the concept
    /// does not inherit the edge.
    #[tokio::test]
    async fn separate_calls_split_a_turn_across_two_stamps() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.upsert_concept(ConceptUpsert::new("s", "s").valid_from(VF))
            .await
            .unwrap();
        db.upsert_concept(ConceptUpsert::new("t", "t").valid_from(VF))
            .await
            .unwrap();
        db.assert_edge(EdgeAssertion::new("s", "t", "NEXT").valid_from(VF))
            .await
            .unwrap();
        db.fork_after(b("alt"), BranchId::main(), "t")
            .await
            .unwrap();
        assert_eq!(reach(&db, "s", Some("alt")).await, vec!["s".to_string()]);
    }
}

// ── M3 · pointer edges ──────────────────────────────────────────────────────

mod m3_pointers {
    use super::*;

    #[tokio::test]
    async fn a_fork_inherits_the_head_it_was_cut_at() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 10).await;
        assert_eq!(
            db.pointer("session", "HEAD", ReadPlan::new())
                .await
                .unwrap()
                .as_deref(),
            Some("t9")
        );

        db.fork_after(b("alt"), BranchId::main(), "t7")
            .await
            .unwrap();
        let on_alt = ReadPlan::new().on(b("alt"));
        assert_eq!(
            db.pointer("session", "HEAD", on_alt.clone())
                .await
                .unwrap()
                .as_deref(),
            Some("t7")
        );

        // Continue the branch: the head moves on alt and main is untouched.
        append(&db, Some("alt"), "a8", "alt 8", 20).await;
        append(&db, Some("alt"), "a9", "alt 9", 21).await;
        assert_eq!(
            db.pointer("session", "HEAD", on_alt.clone())
                .await
                .unwrap()
                .as_deref(),
            Some("a9")
        );
        assert_eq!(
            db.pointer("session", "HEAD", ReadPlan::new())
                .await
                .unwrap()
                .as_deref(),
            Some("t9")
        );

        // Switching back to main and continuing it is an ordinary append.
        append(&db, None, "t10", "turn 10", 22).await;
        assert_eq!(
            db.pointer("session", "HEAD", ReadPlan::new())
                .await
                .unwrap()
                .as_deref(),
            Some("t10")
        );
        assert_eq!(
            db.pointer("session", "HEAD", on_alt)
                .await
                .unwrap()
                .as_deref(),
            Some("a9")
        );

        let path_alt = reach(&db, "session", Some("alt")).await;
        assert!(path_alt.contains(&"a9".to_string()) && !path_alt.contains(&"t8".to_string()));
    }

    /// The append primitive: the link is made from the head as it is inside
    /// the transaction, so a caller never reads a head that has moved.
    #[tokio::test]
    async fn link_previous_appends_from_the_head_inside_the_transaction() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.register_edge_kind("HEAD", true).await.unwrap();
        db.upsert_concept(ConceptUpsert::new("session", "s").valid_from(VF))
            .await
            .unwrap();
        for (k, id) in ["t0", "t1", "t2"].iter().enumerate() {
            db.write_batch(
                WriteBatch::new()
                    .concept(ConceptUpsert::new(*id, *id).valid_from(VF))
                    .pointer(
                        PointerSet::new("session", "HEAD", *id)
                            .valid_from(format!("2026-01-01T00:0{k}:00.000000Z"))
                            .link_previous("NEXT"),
                    ),
            )
            .await
            .unwrap();
        }
        let next = TraversalBuilder::new("session")
            .max_depth(8)
            .edge_types(vec!["NEXT".into()])
            .execute_ids(db.read_conn(), NOW)
            .await
            .unwrap();
        assert_eq!(
            next,
            ["session", "t0", "t1", "t2"],
            "session -> t0 -> t1 -> t2"
        );
        assert_eq!(
            db.pointer("session", "HEAD", ReadPlan::new())
                .await
                .unwrap()
                .as_deref(),
            Some("t2")
        );
    }

    #[tokio::test]
    async fn a_second_open_target_for_a_functional_kind_is_refused() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        let err = db
            .assert_edge(
                EdgeAssertion::new("session", "t0", "HEAD")
                    .valid_from("2026-03-01T00:00:00.000000Z"),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DbError::FunctionalEdgeConflict { .. }),
            "{err:?}"
        );
        // The same through a bulk batch, against an inherited head on a fork.
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        let err = db
            .write_bulk_atomic(vec![EdgeAssertion::new("session", "t0", "HEAD")
                .valid_from("2026-03-01T00:00:00.000000Z")
                .on_branch(b("alt"))])
            .await
            .unwrap_err();
        assert!(
            matches!(err, DbError::FunctionalEdgeConflict { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn moving_to_the_current_target_writes_nothing() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        let before = db.head_seq().await.unwrap();
        let change = db
            .set_pointer(
                PointerSet::new("session", "HEAD", "t1").valid_from("2026-04-01T00:00:00.000000Z"),
            )
            .await
            .unwrap();
        assert!(!change.changed);
        assert_eq!(db.head_seq().await.unwrap(), before);
    }

    #[tokio::test]
    async fn a_move_must_start_after_the_target_it_closes() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        let err = db
            .set_pointer(
                PointerSet::new("session", "HEAD", "t0").valid_from("2025-01-01T00:00:00.000000Z"),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidTimestamp { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn pointers_lists_the_workspace_and_answers_at_an_instant() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        db.register_edge_kind("CURRENT", true).await.unwrap();
        let mut w = WriteBatch::new();
        for f in ["f:a", "f:b", "v:a1", "v:b1", "v:a2"] {
            w = w.concept(ConceptUpsert::new(f, f).valid_from(VF));
        }
        let r1 = db
            .write_batch(
                w.pointer(PointerSet::new("f:a", "CURRENT", "v:a1").valid_from(VF))
                    .pointer(PointerSet::new("f:b", "CURRENT", "v:b1").valid_from(VF)),
            )
            .await
            .unwrap();
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        db.set_pointer(
            PointerSet::new("f:a", "CURRENT", "v:a2")
                .valid_from("2026-02-01T00:00:00.000000Z")
                .on_branch(b("alt")),
        )
        .await
        .unwrap();
        assert_eq!(
            db.pointers("CURRENT", ReadPlan::new().on(b("alt")))
                .await
                .unwrap(),
            vec![
                ("f:a".to_string(), "v:a2".to_string()),
                ("f:b".into(), "v:b1".into())
            ]
        );
        assert_eq!(
            db.pointers("CURRENT", ReadPlan::new()).await.unwrap(),
            vec![
                ("f:a".to_string(), "v:a1".to_string()),
                ("f:b".into(), "v:b1".into())
            ]
        );
        // `diff` already compares the per-lineage pointer rows.
        let d = db.diff(&b("alt"), &BranchId::main()).await.unwrap();
        assert!(!d.is_empty());
        // And at the first batch's stamp, alt's view is the trunk's.
        assert_eq!(
            db.pointer(
                "f:a",
                "CURRENT",
                ReadPlan::new().on(b("alt")).recorded_at(&r1.recorded_at)
            )
            .await
            .unwrap()
            .as_deref(),
            Some("v:a1")
        );
        assert_eq!(
            db.edge_kinds().await.unwrap(),
            vec![("CURRENT".to_string(), true)]
        );
    }
}

// ── M7 · change feed ────────────────────────────────────────────────────────

mod m7_changes {
    use super::*;

    #[tokio::test]
    async fn the_feed_returns_exactly_what_was_written_after_the_cursor() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 2).await;
        let cursor = db.head_seq().await.unwrap();
        let r = db
            .write_batch(
                WriteBatch::new()
                    .concept(ConceptUpsert::new("n", "new").valid_from(VF))
                    .edge(EdgeAssertion::new("t1", "n", "REL").valid_from(VF)),
            )
            .await
            .unwrap();
        let feed = db.changes(cursor, None, &[], 100).await.unwrap();
        assert_eq!(feed.len(), 2);
        assert!(feed
            .iter()
            .all(|c| c.recorded_at == r.recorded_at && c.seq_id > cursor));
        assert_eq!(
            db.changes(cursor, None, &["concepts"], 100)
                .await
                .unwrap()
                .len(),
            1
        );
        // A limit continues from the last seq_id.
        let first = db.changes(cursor, None, &[], 1).await.unwrap();
        let rest = db.changes(first[0].seq_id, None, &[], 100).await.unwrap();
        assert_eq!(first.len() + rest.len(), 2);
    }

    /// Archived rows inside the range are a refusal, not a short answer: a
    /// consumer told only about the survivors would silently miss the rest.
    #[tokio::test]
    async fn an_archived_range_is_refused_as_a_gap() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        let cursor = db.head_seq().await.unwrap();
        // Two versions of one concept: the first log row is superseded, so
        // the archive may take it.
        db.upsert_concept(ConceptUpsert::new("x", "v1").valid_from(VF))
            .await
            .unwrap();
        h.clock.advance(Duration::from_secs(60));
        db.upsert_concept(ConceptUpsert::new("x", "v2").valid_from(VF))
            .await
            .unwrap();
        let head = db.head_seq().await.unwrap();
        assert_eq!(db.changes(cursor, None, &[], 100).await.unwrap().len(), 2);

        h.clock.advance(Duration::from_secs(60));
        db.archive("2999-01-01T00:00:00.000000Z").await.unwrap();

        match db.changes(cursor, None, &[], 100).await.unwrap_err() {
            DbError::ChangeFeedGap { after_seq, removed } => {
                assert_eq!((after_seq, removed), (cursor, 1));
            }
            other => panic!("expected ChangeFeedGap, got {other:?}"),
        }
        // head_seq is not lowered by the archive, and resuming from it works.
        assert_eq!(db.head_seq().await.unwrap(), head);
        assert!(db.changes(head, None, &[], 100).await.unwrap().is_empty());
        // A cursor past the hole is served normally.
        assert_eq!(db.changes(head - 1, None, &[], 100).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_feed_filters_by_lineage() {
        let h = harness();
        let db = h.db_with_fake_clock().await;
        trunk_turns(&db, 1).await;
        db.fork(b("alt"), BranchId::main()).await.unwrap();
        let cursor = db.head_seq().await.unwrap();
        append(&db, Some("alt"), "a1", "alt", 5).await;
        append(&db, None, "t1", "main", 6).await;
        let alt = db.changes(cursor, Some("alt"), &[], 100).await.unwrap();
        assert!(!alt.is_empty() && alt.iter().all(|c| c.branch_id == "alt"));
        assert!(matches!(
            db.changes(cursor, Some("nope"), &[], 100)
                .await
                .unwrap_err(),
            DbError::UnknownBranch(_)
        ));
    }
}
