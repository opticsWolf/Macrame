//! The read path resolves lineage (§15.3, W12.4, D-220).
//!
//! v12 gave `links_current` a lineage-widened primary key, so a branch that
//! corrects or retires an edge it inherited writes its **own** row beside the
//! ancestor's instead of over it — the only form of either that Doctrine III
//! permits across lineages, because closing the ancestor's row is the parent
//! corruption branching exists to prevent. Storing two rows is half of it. This
//! file is the other half: which of them a given lineage *sees*.
//!
//! # Why these are not in `branch_storage_tests`
//!
//! That file says of itself that nothing in it goes through the public API,
//! because at v12 no public API could produce or observe a second lineage. The
//! first half of that is still true — `fork()` is 0.14.5 and every fixture here
//! still reaches the second lineage by raw SQL — and the second half stopped
//! being true at this release. `TraversalBuilder::on_branch` is a public read
//! that names a lineage, so its behaviour belongs with the reads.
//!
//! # The shape under test, and the one it must not become
//!
//! `branch_id IN (ancestry)` is not a resolution. It admits *every* row on the
//! path, so a branch that corrects an edge gets its own row and its ancestor's
//! both, and a branch that retires one gets the retirement plus the live
//! ancestor row that the retirement was supposed to shadow —
//! `examples/branch_traversal_probe.rs` §4b measures that as the whole subtree
//! coming back. `a_branch_that_retires_an_inherited_edge_loses_what_it_reached`
//! is the test that separates the two forms, and it is the reason this file
//! exists rather than a smaller one about `on_branch` returning rows.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::TraversalBuilder;
use macrame::{Database, DbError};

/// When every fixture edge starts.
const TS: &str = "2026-01-01T00:00:00.000000Z";
/// When the branches fork and write.
const TS2: &str = "2026-02-01T00:00:00.000000Z";
/// The instant every traversal below reads at — after every write, so nothing
/// here turns on valid time except where a fixture closes an interval on purpose.
const TS3: &str = "2026-03-01T00:00:00.000000Z";
const NOW: &str = "2026-06-01T00:00:00.000000Z";
const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

async fn connect(harness: &TestHarness) -> libsql::Connection {
    libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

/// A chain `a → b → c → d` on the trunk, and two lineages hanging off it.
///
/// `b1` forks from `main` and `b2` from `b1`, so the fixture can tell a
/// resolution apart from a filter: an edge `b2` has never touched must still be
/// reached through two hops of ancestry, and one that `b1` has touched must
/// reach `b2` in `b1`'s version rather than the trunk's.
async fn seed(conn: &libsql::Connection) {
    macrame::schema::run_migrations(conn).await.unwrap();
    for id in ["a", "b", "c", "d"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    for (parent, child) in [("main", "b1"), ("b1", "b2")] {
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES (?1, ?2, ?3, ?3)",
            libsql::params![child, parent, TS2],
        )
        .await
        .unwrap();
    }
    for (source, target) in [("a", "b"), ("b", "c"), ("c", "d")] {
        edge(conn, source, target, "main", SENTINEL, 1.0, TS).await;
    }
}

/// One edge assertion, on a named lineage.
///
/// Written to `links` rather than `links_current` so the sync trigger and the
/// log trigger both fire — the transaction-time tests below read what the
/// second of those wrote.
async fn edge(
    conn: &libsql::Connection,
    source: &str,
    target: &str,
    branch: &str,
    valid_to: &str,
    weight: f64,
    recorded_at: &str,
) {
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
         weight, properties, recorded_at, branch_id) \
         VALUES (?1, ?2, 'LINKS', ?3, ?4, ?5, '{}', ?6, ?7)",
        libsql::params![source, target, TS, valid_to, weight, recorded_at, branch],
    )
    .await
    .unwrap();
}

/// Node ids a traversal from `a` reaches, sorted, on the named lineage.
async fn reached(conn: &libsql::Connection, branch: Option<&str>) -> Vec<String> {
    let mut walk = TraversalBuilder::new("a").max_depth(5);
    if let Some(b) = branch {
        walk = walk.on_branch(b);
    }
    walk.execute_ids(conn, NOW).await.unwrap()
}

/// The same read under a weight floor, which is how the fixture below tells
/// two lineages' versions of one edge apart.
async fn reached_above(conn: &libsql::Connection, branch: &str, floor: f64) -> Vec<String> {
    TraversalBuilder::new("a")
        .max_depth(5)
        .min_weight(floor)
        .on_branch(branch)
        .execute_ids(conn, NOW)
        .await
        .unwrap()
}

/// The same read under a transaction-time instant, which folds the log instead
/// of reading `links_current`.
async fn reached_at_tx(conn: &libsql::Connection, branch: &str) -> Vec<String> {
    TraversalBuilder::new("a")
        .max_depth(5)
        .as_of_recorded(NOW)
        .attribute_mode(macrame::graph::AttributeMode::Omit)
        .on_branch(branch)
        .execute_ids(conn, NOW)
        .await
        .unwrap()
}

// ───────────────────────────────────────────────────────────────────────────
// What a lineage sees
// ───────────────────────────────────────────────────────────────────────────

/// A branch inherits its ancestors' edges, across more than one hop.
#[tokio::test]
async fn a_branch_reads_the_edges_its_ancestors_asserted() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    for branch in ["main", "b1", "b2"] {
        assert_eq!(
            reached(&conn, Some(branch)).await,
            ["a", "b", "c", "d"],
            "{branch} did not inherit the trunk's chain"
        );
    }
}

/// **The test the whole shape is for.**
///
/// `b1` retires `b → c` by shadowing: its own row at the ancestor's key with a
/// closed valid interval. The trunk's row is untouched, which is the point —
/// closing it would be the parent corruption Doctrine III forbids.
///
/// Under a nearest-ancestor resolution `b1` sees its own closed row, the edge
/// is not live at `NOW`, and `c` and `d` are both gone with it. Under the
/// `branch_id IN (ancestry)` form the plan proposed, `b1` sees *both* rows, the
/// trunk's is still open, and the retirement has no effect at all — measured at
/// 1,111 nodes against 1,000 in probe §4b. That is not a stale weight, it is a
/// retirement that silently did not happen, so this assertion is what separates
/// a resolution from a union and it is why it is written as a reachability
/// claim rather than a row count.
#[tokio::test]
async fn a_branch_that_retires_an_inherited_edge_loses_what_it_reached() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    edge(&conn, "b", "c", "b1", TS2, 1.0, TS2).await;

    assert_eq!(
        reached(&conn, Some("b1")).await,
        ["a", "b"],
        "the shadow row did not close the inherited edge for its own lineage"
    );
    assert_eq!(
        reached(&conn, Some("b2")).await,
        ["a", "b"],
        "a descendant must inherit the retirement, not the row it shadowed"
    );
    assert_eq!(
        reached(&conn, Some("main")).await,
        ["a", "b", "c", "d"],
        "a branch retiring an edge changed what the trunk believes"
    );

    // Both rows are still stored. The retirement is a fact about what `b1`
    // sees, not a deletion, and a later `fork()` from `main` must still find
    // the trunk's edge intact.
    let stored: i64 = conn
        .query(
            "SELECT COUNT(*) FROM links_current \
             WHERE source_id = 'b' AND target_id = 'c'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(stored, 2, "shadowing must add a row, never overwrite one");
}

/// The nearest holder wins, and *nearest* is what makes it a resolution.
///
/// `b1` and `b2` both correct the trunk's `a → b`, to weights the filter can
/// tell apart. `b2` must read its own; `b1` must read its own; `main` must read
/// the trunk's. A union over the ancestry passes the first of those three by
/// accident — every row it needs is in the set — and fails the moment the
/// weight filter has to pick one.
#[tokio::test]
async fn the_nearest_lineage_holding_an_edge_is_the_one_that_is_read() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    // Distinct `recorded_at` because `links` is keyed
    // `(source, target, type, valid_from, recorded_at)` and **not** by lineage —
    // see the note at the foot of this file.
    edge(&conn, "a", "b", "b1", SENTINEL, 0.5, TS2).await;
    edge(&conn, "a", "b", "b2", SENTINEL, 0.1, TS3).await;

    // A floor of 0.3 keeps the trunk's 1.0 and `b1`'s 0.5, and drops `b2`'s 0.1.
    assert_eq!(
        reached_above(&conn, "main", 0.3).await,
        ["a", "b", "c", "d"]
    );
    assert_eq!(reached_above(&conn, "b1", 0.3).await, ["a", "b", "c", "d"]);
    assert_eq!(
        reached_above(&conn, "b2", 0.3).await,
        ["a"],
        "b2's own correction must win over both of its ancestors' rows"
    );
}

/// An unbranched read on a forked ledger is the trunk's belief, not the union.
///
/// This is the case a shape-picking read path can get wrong in the direction
/// nobody notices. `b1` asserts an edge the trunk never had; a traversal that
/// names no branch must not see it, because "no branch" has always meant the
/// trunk and a caller written before `fork()` existed cannot have meant
/// anything else. The extra node would arrive looking entirely ordinary.
#[tokio::test]
async fn an_unbranched_read_on_a_forked_ledger_stays_on_the_trunk() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id) \
         VALUES ('e', 'minted on b1', ?1, ?1, 'b1')",
        libsql::params![TS2],
    )
    .await
    .unwrap();
    edge(&conn, "a", "e", "b1", SENTINEL, 1.0, TS2).await;

    assert_eq!(
        reached(&conn, None).await,
        ["a", "b", "c", "d"],
        "an unbranched traversal read another lineage's edge"
    );
    assert_eq!(
        reached(&conn, Some("main")).await,
        ["a", "b", "c", "d"],
        "naming the trunk explicitly must mean the same thing as not naming it"
    );
    assert!(reached(&conn, Some("b1")).await.contains(&"e".to_string()));
}

/// A lineage that was never registered is refused, not answered for the trunk.
///
/// The D-069 shape: a right-looking answer to a question that was not asked,
/// and the one a caller is least able to detect — on a database that has never
/// forked, the trunk's view is exactly what they expected to see.
#[tokio::test]
async fn a_traversal_naming_an_unregistered_lineage_is_refused() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    let err = TraversalBuilder::new("a")
        .on_branch("ghost")
        .execute_ids(&conn, NOW)
        .await
        .expect_err("a traversal named a lineage that does not exist");
    match err {
        DbError::NotFound(what) => assert_eq!(what, "ghost", "the refusal must name it"),
        other => panic!("wrong error for an unregistered lineage: {other:?}"),
    }

    // And on a database that never forked, where the trunk shape is emitted and
    // there is no ancestry query to fall out of.
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    let err = TraversalBuilder::new("a")
        .on_branch("ghost")
        .execute_ids(&conn, NOW)
        .await
        .expect_err("the fast shape skipped the check as well as the resolution");
    assert!(
        matches!(err, DbError::NotFound(ref w) if w == "ghost"),
        "{err:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// The transaction-time fold, which is where this was already wrong
// ───────────────────────────────────────────────────────────────────────────

/// The fold under `as_of_recorded` resolves lineage too (the D-216 miss).
///
/// `TraversalBuilder::links_at_tx_cte` partitioned on `entity_id` alone, and
/// `entity_id` for a link is `source|target|type|valid_from` — the edge key,
/// shared across lineages by design. So an ancestor's assertion and a
/// descendant's correction of it landed in one partition and the fold kept
/// whichever had the higher `seq_id`: one of the two beliefs was gone *before*
/// any resolution ran, and which one survived was decided by write order.
///
/// D-216 fixed exactly this shape in `temporal::replay` one release earlier and
/// this fold was not in that sweep, because its own rustdoc argued the
/// partition was sound — correctly, about the concept/link collision, and about
/// nothing else.
#[tokio::test]
async fn a_transaction_time_traversal_resolves_lineage_in_the_fold() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    // `b1` retires the inherited edge, and does it *later* in log order — which
    // is what made the collapsed partition look right on this fixture and wrong
    // on the mirror of it below.
    edge(&conn, "b", "c", "b1", TS2, 1.0, TS2).await;

    assert_eq!(
        reached_at_tx(&conn, "b1").await,
        ["a", "b"],
        "the retirement is in the log and the fold must carry it to b1"
    );
    assert_eq!(
        reached_at_tx(&conn, "main").await,
        ["a", "b", "c", "d"],
        "the fold handed the trunk a branch's belief: the partition lost a row"
    );
}

/// The same defect from the other side: the *older* branch row must survive.
///
/// Above, the branch wrote last. Here the branch's row is written first and the
/// trunk corrects afterwards, so a partition that keeps the highest `seq_id`
/// per edge key discards the branch's row instead of the trunk's. One fixture
/// can only catch one of those two directions, and a fold that is wrong catches
/// neither reliably — which is how it survived a release.
#[tokio::test]
async fn the_fold_keeps_the_older_lineages_row_as_well() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();
    for id in ["a", "b"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES ('b1', 'main', ?1, ?1)",
        libsql::params![TS],
    )
    .await
    .unwrap();

    edge(&conn, "a", "b", "b1", SENTINEL, 1.0, TS).await;
    edge(&conn, "a", "b", "main", TS2, 1.0, TS2).await;

    assert_eq!(
        reached_at_tx(&conn, "b1").await,
        ["a", "b"],
        "the later trunk row displaced the branch's own belief in the fold"
    );
    assert_eq!(
        reached_at_tx(&conn, "main").await,
        ["a"],
        "the trunk closed its own edge"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// The other execution path
// ───────────────────────────────────────────────────────────────────────────

/// `load_subgraph_with` resolves the same lineage the traversal does.
///
/// It carries its own copy of the projection and asks the shape itself, so it
/// is a second place the resolution can be missing. D-073 found this loader
/// taking neither `edge_types` nor `min_weight` while the builder took both,
/// and F-35 found it binding `now_ts` where the builder bound the traversal's
/// instant — twice now, a filter has reached the walk and not this. Both halves
/// are asserted, because filtering only the walk hands a caller a graph reached
/// on their lineage and populated from every other one.
#[tokio::test]
async fn the_subgraph_loader_reads_the_same_lineage() {
    let harness = TestHarness::new();
    {
        let conn = connect(&harness).await;
        seed(&conn).await;
        edge(&conn, "b", "c", "b1", TS2, 1.0, TS2).await;
    }

    let db = Database::open(&harness.db_path).await.unwrap();

    let trunk = db
        .load_subgraph_with(&TraversalBuilder::new("a").max_depth(5), NOW, 1 << 20)
        .await
        .unwrap();
    assert_eq!(trunk.node_count(), 4);
    assert_eq!(trunk.edge_count(), 3);

    let branched = db
        .load_subgraph_with(
            &TraversalBuilder::new("a").max_depth(5).on_branch("b1"),
            NOW,
            1 << 20,
        )
        .await
        .unwrap();
    assert_eq!(
        branched.node_count(),
        2,
        "the walk did not resolve: {:?}",
        branched.node_ids().collect::<Vec<_>>()
    );
    assert_eq!(
        branched.edge_count(),
        1,
        "the projection returned an edge the walk had already excluded"
    );

    let err = db
        .load_subgraph_with(&TraversalBuilder::new("a").on_branch("ghost"), NOW, 1 << 20)
        .await
        .expect_err("the loader answered for a lineage that does not exist");
    assert!(
        matches!(err, DbError::NotFound(ref w) if w == "ghost"),
        "{err:?}"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// A gap found while writing the fixtures above, pinned rather than remembered
// ───────────────────────────────────────────────────────────────────────────

/// `links` is keyed by `recorded_at`; `links_current` is keyed by lineage.
///
/// v12 widened the *materialization*'s primary key to
/// `(source, target, type, valid_from, branch_id)` so two lineages' beliefs
/// about one edge could coexist. The append-only table it is materialized from
/// kept `(source, target, type, valid_from, recorded_at)` — no lineage — so two
/// lineages asserting one edge key at the **same instant** collide, and collide
/// as a bare `UNIQUE constraint failed` rather than as a named refusal.
///
/// It is not reachable through the crate today: `recorded_at` is crate-stamped
/// and the monotonicity guard keeps two writes from sharing a stamp, and until
/// `fork()` lands at 0.14.5 there is no branch-scoped write at all. It becomes
/// reachable the moment there is — a bulk write that stamps one instant across
/// a chunk is the obvious way in. Recorded here as a fixture constraint the
/// tests above already have to work around, so that the v13 rung either widens
/// it deliberately or declines to and says why.
#[tokio::test]
async fn the_append_only_table_is_not_keyed_by_lineage() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    let err = conn
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at, branch_id) \
             VALUES ('a', 'b', 'LINKS', ?1, ?2, 1.0, '{}', ?1, 'b1')",
            libsql::params![TS, SENTINEL],
        )
        .await
        .expect_err("two lineages asserted one edge at one instant");
    assert!(
        err.to_string().contains("UNIQUE constraint failed: links."),
        "if this is red the key was widened — update the tests above, which \
         space their `recorded_at` apart only to avoid it: {err}"
    );

    // One instant apart is enough, which is what makes this a latent gap rather
    // than a live defect.
    edge(&conn, "a", "b", "b1", SENTINEL, 0.5, TS2).await;
}

// ───────────────────────────────────────────────────────────────────────────
// The other public reader
// ───────────────────────────────────────────────────────────────────────────

/// `query_as_of_edges` reads the trunk, and `_on` reads a lineage (D-220).
///
/// It is the most-called reader in the crate and it read `links_current`
/// unfiltered, so on a forked ledger it returned every lineage's rows at once.
/// Fixed additively rather than by widening its signature: the default it had
/// always meant is the trunk, and that is now what it returns rather than what
/// it happened to return on a database with one lineage in it.
#[tokio::test]
async fn the_edge_query_reads_one_lineage_at_a_time() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    seed(&conn).await;

    // A retirement `b1` alone believes, and an assertion `b1` alone makes.
    edge(&conn, "b", "c", "b1", TS2, 1.0, TS2).await;
    edge(&conn, "a", "c", "b1", SENTINEL, 1.0, TS3).await;

    let trunk = macrame::temporal::query_as_of_edges(&conn, NOW)
        .await
        .unwrap();
    let mut trunk: Vec<_> = trunk.iter().map(|e| (e.0.as_str(), e.1.as_str())).collect();
    trunk.sort_unstable();
    assert_eq!(
        trunk,
        [("a", "b"), ("b", "c"), ("c", "d")],
        "the unbranched reader returned another lineage's rows"
    );

    let branched = macrame::temporal::query_as_of_edges_on(&conn, NOW, Some("b1"))
        .await
        .unwrap();
    let mut branched: Vec<_> = branched
        .iter()
        .map(|e| (e.0.as_str(), e.1.as_str()))
        .collect();
    branched.sort_unstable();
    assert_eq!(
        branched,
        [("a", "b"), ("a", "c"), ("c", "d")],
        "b1 keeps its own assertion and loses the edge it shadowed"
    );

    // Naming the trunk explicitly means the same thing as not naming it, on a
    // ledger where the two take different shapes to answer.
    assert_eq!(
        macrame::temporal::query_as_of_edges_on(&conn, NOW, Some("main"))
            .await
            .unwrap(),
        macrame::temporal::query_as_of_edges(&conn, NOW)
            .await
            .unwrap()
    );

    let err = macrame::temporal::query_as_of_edges_on(&conn, NOW, Some("ghost"))
        .await
        .expect_err("the reader answered for a lineage that does not exist");
    assert!(
        matches!(err, DbError::NotFound(ref w) if w == "ghost"),
        "{err:?}"
    );
}
