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
//! because at v12 no public API could produce or observe a second lineage.
//! Neither half is true any more, and they stopped being true one release
//! apart: `TraversalBuilder::on_branch` made the *observing* half public at
//! 0.14.4, and `Database::fork` made the *producing* half public at 0.14.7.
//!
//! The fixtures here still cut their second lineage by raw SQL, and that is now
//! a choice rather than a necessity. These cases pin the reader against
//! `branches` rows, including shapes `fork()` refuses to write — a fork point
//! chosen in the past, a chain assembled in one statement — so building them
//! through the writer would narrow what the reader is tested on to what the
//! writer currently emits. `tests/branch_lifecycle_tests.rs` is where `fork()`
//! is exercised as itself, and it asserts on the same reader.
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
//!
//! # The fork point, which 0.14.4 resolved without (0.14.6, D-223)
//!
//! Everything above is about *which lineage holds an edge*. It says nothing
//! about *when*, and 0.14.4 shipped a reader that never looked at
//! `branches.forked_at` — so a branch kept absorbing its parent's later writes.
//! `examples/branch_cutoff_probe.rs` §1 is that measurement, and §2 is why the
//! repair is a hybrid rather than a `WHERE` clause: the sync trigger's
//! `DO UPDATE` carries `recorded_at` forward, so once an ancestor churns an
//! edge the pre-fork version is not in `links_current` to be filtered.
//!
//! The section at the foot of this file is the matrix that separates the churn
//! kinds, because they fail *differently*. A branch that wrongly inherits a
//! post-fork **new edge** gains a node; one that wrongly inherits a post-fork
//! **reweight** gets a plausible number; one whose ancestor **retired** an edge
//! loses a subtree under the naive filter, silently.

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
/// A second post-fork instant, so an ancestor can churn one key twice.
const TS4: &str = "2026-04-01T00:00:00.000000Z";
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
        DbError::UnknownBranch(what) => {
            assert_eq!(what, "ghost", "the refusal must name it")
        }
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
        matches!(err, DbError::UnknownBranch(ref w) if w == "ghost"),
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
        matches!(err, DbError::UnknownBranch(ref w) if w == "ghost"),
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
/// and the monotonicity guard keeps two writes from sharing a stamp, and there
/// is no branch-scoped *write* at all. `fork()` at 0.14.7 does not change that
/// — it registers a lineage and writes no ledger row, so every assertion still
/// lands on the trunk and no two lineages can yet reach this key. It becomes
/// reachable the moment a write takes a branch, which is the branch-scoped view
/// — a bulk write that stamps one instant across a chunk is the obvious way in.
/// Recorded here as a fixture constraint the tests above already have to work
/// around, so that the v13 rung either widens it deliberately or declines to
/// and says why.
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
        matches!(err, DbError::UnknownBranch(ref w) if w == "ghost"),
        "{err:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// The fork point as a visibility cutoff (0.14.6, D-223)
// ───────────────────────────────────────────────────────────────────────────

/// The chain depths every case below is run at.
///
/// Not because the semantics change with depth — the cutoff a chain resolves
/// for the trunk is the *first* fork off it whether that is one hop away or a
/// hundred — but because the ancestry is a recursion that now carries a running
/// minimum, and [D-219] measured its cost as an addend on the query rather than
/// a factor on the hops. Running the same assertions at 1, 10 and 100 keeps
/// both claims under test with one fixture. Composition down the chain is a
/// separate question and has its own test: `a_grandchild_reads_its_parent_as_of
/// _its_own_fork`, which needs a write on an *intermediate* lineage and cannot
/// be reached by deepening a chain the trunk alone writes to.
///
/// [D-219]: ../docs/architecture/s13-decision-register.md
const DEPTHS: [usize; 3] = [1, 10, 100];

/// Fork instants, one microsecond apart and all after the trunk's own writes.
fn fork_instant(hop: usize) -> String {
    format!("2026-02-01T00:00:00.{hop:06}Z")
}

/// `a → b → c → d` on the trunk, and a chain of `depth` lineages under it.
///
/// Returns the deepest branch's name, which is the reader in every case below.
/// The trunk's edges are recorded at `TS`, before every fork instant, so they
/// are inherited; anything the trunk writes at `TS3` or later is post-fork for
/// the whole chain.
async fn chain(conn: &libsql::Connection, depth: usize) -> String {
    macrame::schema::run_migrations(conn).await.unwrap();
    for id in ["a", "b", "c", "d"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    let mut parent = "main".to_string();
    for hop in 1..=depth {
        let child = format!("L{hop}");
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES (?1, ?2, ?3, ?3)",
            libsql::params![child.as_str(), parent.as_str(), fork_instant(hop)],
        )
        .await
        .unwrap();
        parent = child;
    }
    for (source, target) in [("a", "b"), ("b", "c"), ("c", "d")] {
        edge(conn, source, target, "main", SENTINEL, 1.0, TS).await;
    }
    parent
}

/// A concept minted on the trunk after every fork on the chain.
async fn late_concept(conn: &libsql::Connection, id: &str) {
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
        libsql::params![id, TS3],
    )
    .await
    .unwrap();
}

/// **The finding, as an assertion.** A post-fork trunk write is not inherited.
///
/// This is the case `examples/branch_cutoff_probe.rs` §1 probed and found
/// wrong: the branch forked in February, the trunk recorded `d → e` in March,
/// and the branch saw it. §15.3 says a branch reads its ancestors *before the
/// fork point on the path down from A*, and nothing computed that point.
///
/// It is the *cheapest* of the kinds to get right — the new edge's
/// `links_current` row is post-cutoff, so the projection arm simply declines it
/// — and it motivates none of the machinery. The three below are why the repair
/// is a hybrid rather than a predicate.
#[tokio::test]
async fn a_branch_does_not_inherit_what_its_parent_recorded_after_the_fork() {
    for depth in DEPTHS {
        let harness = TestHarness::new();
        let conn = connect(&harness).await;
        let reader = chain(&conn, depth).await;

        late_concept(&conn, "e").await;
        edge(&conn, "d", "e", "main", SENTINEL, 1.0, TS3).await;

        assert_eq!(
            reached(&conn, None).await,
            ["a", "b", "c", "d", "e"],
            "depth {depth}: the trunk lost its own write"
        );
        assert_eq!(
            reached(&conn, Some(&reader)).await,
            ["a", "b", "c", "d"],
            "depth {depth}: {reader} absorbed a trunk write made after it forked"
        );
    }
}

/// A reweight after the fork leaves the branch on the weight it inherited.
///
/// The first kind the naive filter cannot serve. `trg_links_current_sync` is
/// `ON CONFLICT … DO UPDATE … recorded_at = excluded.recorded_at`, so once the
/// trunk corrects `b → c` the projection holds **only** the March row: the
/// weight the branch inherited is in `transaction_log` and nowhere else. A
/// `recorded_at <= cutoff` predicate over `links_current` would drop the edge
/// rather than restore its weight.
///
/// Asserted through a weight floor rather than by reading the number back,
/// because the floor is what a caller's query does with it — and because a
/// wrong weight that still clears the floor is a difference no reachability
/// assertion could see.
#[tokio::test]
async fn a_branch_keeps_the_weight_its_parent_had_at_the_fork() {
    for depth in DEPTHS {
        let harness = TestHarness::new();
        let conn = connect(&harness).await;
        let reader = chain(&conn, depth).await;

        // The trunk drops `b → c` to a tenth, after every fork on the chain.
        edge(&conn, "b", "c", "main", SENTINEL, 0.1, TS3).await;

        assert_eq!(
            reached_above(&conn, "main", 0.5).await,
            ["a", "b"],
            "depth {depth}: the trunk's own correction did not take"
        );
        assert_eq!(
            reached_above(&conn, &reader, 0.5).await,
            ["a", "b", "c", "d"],
            "depth {depth}: {reader} was handed a weight recorded after it forked"
        );
    }
}

/// **The kind that fails silently.** A post-fork retirement must not reach the
/// branch, and the naive filter deletes the edge instead of preserving it.
///
/// The trunk closes `b → c` in March by writing its own row at the same key
/// with a closed interval. `links_current` now holds one row for that key on
/// `main`: closed, recorded post-cutoff. Three behaviours are possible and only
/// one is right:
///
/// | read | `c`, `d` |
/// |---|---|
/// | 0.14.4, no cutoff | reachable, **wrongly** — the branch inherits a retirement asserted after it forked |
/// | `recorded_at <= cutoff` over `links_current` alone | unreachable, **wrongly** — the row is filtered and nothing replaces it |
/// | the hybrid | reachable, from the log entry the trunk wrote before the fork |
///
/// The middle row is why this is a separate test from the reweight above. Both
/// wrong answers are *plausible* — a subtree that quietly stops being reachable
/// looks like a branch that never had it — and they are wrong in opposite
/// directions, so a fixture that only checked "the branch differs from the
/// trunk" would pass on either.
#[tokio::test]
async fn a_branch_still_reaches_what_its_parent_retired_after_the_fork() {
    for depth in DEPTHS {
        let harness = TestHarness::new();
        let conn = connect(&harness).await;
        let reader = chain(&conn, depth).await;

        // Closed in March, which is after every fork instant on the chain.
        edge(&conn, "b", "c", "main", TS3, 1.0, TS3).await;

        assert_eq!(
            reached(&conn, None).await,
            ["a", "b"],
            "depth {depth}: the trunk's own retirement did not take"
        );
        assert_eq!(
            reached(&conn, Some(&reader)).await,
            ["a", "b", "c", "d"],
            "depth {depth}: {reader} lost a subtree to a retirement asserted \
             after it forked — the projection row was filtered and the log \
             entry that should have replaced it did not arrive"
        );
    }
}

/// A key an ancestor first asserted *after* the cutoff contributes nothing.
///
/// The fold arm's own bound, from the side where returning something is the
/// failure. The trunk asserts `a → e` in March and corrects it in April: both
/// entries are post-cutoff, the projection row is post-cutoff, so the key
/// reaches the fold arm — and the fold must come back **empty** rather than
/// hand the branch the older of two beliefs it was never entitled to.
///
/// This is also what pins the bound *inside* the window rather than after it.
/// `ROW_NUMBER()` picks the last entry per partition, so a `recorded_at <=
/// cutoff` applied to the fold's output instead of its input would discard the
/// winner and return nothing here — right answer, wrong reason — while doing
/// the same thing to the retirement case above, where nothing is exactly what
/// must not be returned. One placement is correct for both.
#[tokio::test]
async fn an_ancestor_that_first_asserted_after_the_fork_contributes_nothing() {
    for depth in DEPTHS {
        let harness = TestHarness::new();
        let conn = connect(&harness).await;
        let reader = chain(&conn, depth).await;

        late_concept(&conn, "e").await;
        edge(&conn, "a", "e", "main", SENTINEL, 1.0, TS3).await;
        edge(&conn, "a", "e", "main", SENTINEL, 0.2, TS4).await;

        assert_eq!(
            reached(&conn, None).await,
            ["a", "b", "c", "d", "e"],
            "depth {depth}: the trunk lost the key it asserted twice"
        );
        assert_eq!(
            reached(&conn, Some(&reader)).await,
            ["a", "b", "c", "d"],
            "depth {depth}: the fold resurrected a belief with no pre-fork \
             version — {reader} was handed the earlier of two post-cutoff rows"
        );
    }
}

/// The transaction-time path applies the same cutoffs, in its own way.
///
/// `links_at_tx` is a second reader and a second chance to be wrong: it folds
/// `transaction_log` rather than `links_current`, so it needs no hybrid — but
/// it does need the cutoff *inside* its window, for the reason the case above
/// states. The retirement fixture is reused deliberately, because it is the one
/// where a misplaced bound returns an empty partition that looks like an honest
/// absence.
#[tokio::test]
async fn a_transaction_time_read_honours_the_fork_point_too() {
    for depth in DEPTHS {
        let harness = TestHarness::new();
        let conn = connect(&harness).await;
        let reader = chain(&conn, depth).await;

        edge(&conn, "b", "c", "main", TS3, 1.0, TS3).await;

        assert_eq!(
            reached_at_tx(&conn, "main").await,
            ["a", "b"],
            "depth {depth}: the trunk's fold lost its own retirement"
        );
        assert_eq!(
            reached_at_tx(&conn, &reader).await,
            ["a", "b", "c", "d"],
            "depth {depth}: the fold applied the trunk's post-fork retirement \
             to {reader}"
        );
    }
}

/// **Inheritance composes**, and an empty fold falls through to the next
/// ancestor rather than to nothing.
///
/// `L2` forks from `L1` in April and `L1` from `main` in February, so `L2` sees
/// `L1` as of April and `main` as of February — each step can only narrow the
/// window, which is why `ancestry_cte` carries a running minimum rather than an
/// assignment. The chain fixture above cannot show this: the trunk is the only
/// writer there, so every ancestor's cutoff resolves to the same first fork.
/// This needs a write on the *intermediate* lineage, and its own instants.
///
/// The third assertion is the one worth having. `L1` retires the trunk's
/// `b → c` in May, after `L2` forked, so for `L2` that key is churned on `L1` —
/// and `L1` wrote nothing about it before April, so the fold arm returns
/// **empty** for that `(key, lineage)` pair. `L2` must then fall through to
/// `main`'s row, which is pre-February and stands. An implementation reading an
/// empty fold as "the nearest holder says this does not exist" would drop the
/// edge instead, which is the retirement failure one lineage further down.
#[tokio::test]
async fn a_grandchild_reads_its_parent_as_of_its_own_fork() {
    /// After `L2` forks, so `L1`'s writes here are its own alone.
    const T5: &str = "2026-05-01T00:00:00.000000Z";

    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    for id in ["a", "b", "c", "d"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    // February and April, far enough apart that `L1` can write between them.
    for (child, parent, forked) in [("L1", "main", TS2), ("L2", "L1", TS4)] {
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at)              VALUES (?1, ?2, ?3, ?3)",
            libsql::params![child, parent, forked],
        )
        .await
        .unwrap();
    }
    for (source, target) in [("a", "b"), ("b", "c"), ("c", "d")] {
        edge(&conn, source, target, "main", SENTINEL, 1.0, TS).await;
    }
    // `e` in March — before `L2` forked. `f` in May — after.
    for (id, at) in [("e", TS3), ("f", T5)] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at, branch_id)              VALUES (?1, 'N', ?2, ?2, 'L1')",
            libsql::params![id, at],
        )
        .await
        .unwrap();
    }
    edge(&conn, "b", "e", "L1", SENTINEL, 1.0, TS3).await;
    edge(&conn, "b", "f", "L1", SENTINEL, 1.0, T5).await;
    edge(&conn, "b", "c", "L1", T5, 1.0, T5).await;

    assert_eq!(
        reached(&conn, Some("L1")).await,
        ["a", "b", "e", "f"],
        "L1 must see all of its own writes, its retirement of b → c included"
    );
    assert_eq!(
        reached(&conn, Some("L2")).await,
        ["a", "b", "c", "d", "e"],
        "L2 must read L1 as of its own fork: the edge L1 asserted before it,          neither the one after nor the retirement after — and `c` is reached          through main because L1's fold came back empty rather than negative"
    );
}

/// Reading the trunk of a forked ledger is unchanged by any of this.
///
/// `main` is the root: no parent, no `forked_at`, and therefore no cutoff. The
/// ancestry it resolves is itself with a `NULL` cutoff, `churned` is empty by
/// its own `cutoff IS NOT NULL` clause, and the hybrid reduces to the read
/// 0.14.4 shipped. Asserted rather than assumed because it is the case every
/// existing caller is in the moment anybody calls `fork()`, and a cutoff that
/// leaked onto the trunk would make the trunk stop seeing its own writes.
#[tokio::test]
async fn the_trunk_of_a_forked_ledger_has_no_cutoff() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    chain(&conn, 3).await;

    late_concept(&conn, "e").await;
    edge(&conn, "d", "e", "main", SENTINEL, 1.0, TS3).await;
    edge(&conn, "b", "c", "main", SENTINEL, 0.1, TS4).await;

    assert_eq!(
        reached(&conn, None).await,
        ["a", "b", "c", "d", "e"],
        "the trunk stopped seeing writes it made itself"
    );
    assert_eq!(
        reached(&conn, Some("main")).await,
        ["a", "b", "c", "d", "e"],
        "naming the trunk must mean what not naming it means"
    );
    assert_eq!(
        reached_above(&conn, "main", 0.5).await,
        ["a", "b"],
        "the trunk's own correction stopped taking"
    );
}
