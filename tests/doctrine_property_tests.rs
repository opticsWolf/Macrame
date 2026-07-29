//! Property tests for the doctrine (§0), driven through the **public API**.
//!
//! The invariants in §0 are load-bearing claims about what a caller can and
//! cannot make this crate do. A unit test proves a claim about the state it
//! constructs; only a generated history proves a claim about the surface.
//!
//! Each property below names the doctrine it pins. One is absent on purpose:
//!
//! * **Doctrine I** (never fork the engine) is a claim about the dependency
//!   graph, not about runtime state. Its test is a build-level one — no
//!   `[patch]`, no vendored C — and belongs in CI, not here.
//!
//! **Doctrine VII** was the other absence until Phase 5. It could not be pinned
//! here until the `embeddings_*` tables existed (Phase 3) and a caller could
//! reach one through the handle (D-048); both now hold, so the generated half
//! lives at the foot of this file. The half that needs no database — that no
//! trigger's `json_object(…)` mentions a vector — stays in
//! `doctrine_static_tests.rs`, outside this file's feature gate, because a
//! doctrine check should not become conditional merely because a sibling had to.
//!
//! Doctrine VI is pinned separately, in `integrity_property_tests.rs`.
//!
//! **This binary is gated behind the `property-tests` feature** and does not run
//! under a plain `cargo test`. See the `RT` comment below and R15: libSQL 0.6
//! segfaults intermittently under the churn of local databases that generated
//! histories require, so leaving these in the default target made an otherwise
//! deterministic suite flaky. Run them with
//! `cargo test --features property-tests`, which CI does as its own step.

mod harness;

use std::collections::BTreeSet;
use std::future::Future;

use harness::TestHarness;
use macrame::prelude::*;
use macrame::temporal::snapshot::save_snapshot;
use proptest::prelude::*;

const NODES: [&str; 3] = ["c0", "c1", "c2"];
const TYPES: [&str; 2] = ["A", "B"];
const TS: [&str; 5] = [
    "2026-01-01T00:00:00.000000Z",
    "2026-02-01T00:00:00.000000Z",
    "2026-03-01T00:00:00.000000Z",
    "2026-04-01T00:00:00.000000Z",
    "2026-05-01T00:00:00.000000Z",
];
const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

/// One runtime for the whole binary, and a low case count — both are R15
/// mitigations, not preferences.
///
/// libSQL faults intermittently with STATUS_ACCESS_VIOLATION when local
/// databases are opened concurrently in one process. It reproduces with no
/// Macrame type involved, so it is below the Doctrine I line, and it survived
/// the move from 0.6.0 to 0.9.30 unchanged. A stress run also showed 200
/// databases each in its own short-lived Tokio runtime crashing 2/6 against
/// 0/6 for the same 200 inside one runtime — hence the shared runtime here.
///
/// Serialising libtest (`RUST_TEST_THREADS = "1"`, .cargo/config.toml) takes
/// the rest of the suite to 0/30. It does not save this binary, which still
/// faults ~3/25, because every generated case needs a database of its own —
/// which is why the whole binary sits behind the `property-tests` feature.
///
/// An application holds one runtime and one database for its lifetime and is
/// not exposed. A property-test harness is the only thing that churns either,
/// so the harness is what accommodates it.
static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn block_on<F: Future>(f: F) -> F::Output {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    })
    .block_on(f)
}

// ---------------------------------------------------------------------------
// Generated public-API histories
// ---------------------------------------------------------------------------

/// Operations a caller can actually reach. Deliberately *only* public methods:
/// a doctrine that holds solely when nobody uses the API is not a doctrine.
#[derive(Debug, Clone, Copy)]
enum Op {
    Assert {
        src: usize,
        tgt: usize,
        etype: usize,
        valid_from: usize,
        /// `None` is the open sentinel.
        valid_to: Option<usize>,
    },
    Retire {
        src: usize,
        tgt: usize,
        etype: usize,
        valid_from: usize,
        valid_to: usize,
    },
    Upsert {
        id: usize,
        title: bool,
    },
    /// Soft-delete, and the only op that produces a *tombstone* in a fold: the
    /// winning log row for the concept says `retired = 1`, so a reconstruction
    /// must show it gone. Added with D-049 — before snapshot composition,
    /// "gone" and "never there" were the same outcome and nothing distinguished
    /// them, so the generator had no reason to reach the case.
    RetireConcept {
        id: usize,
    },
    Bulk {
        src: usize,
        tgt: usize,
        etype: usize,
        valid_from: usize,
    },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..NODES.len(), 0..NODES.len(), 0..TYPES.len(), 0..3usize, prop::option::of(3..5usize))
            .prop_map(|(src, tgt, etype, valid_from, valid_to)| Op::Assert {
                src, tgt, etype, valid_from, valid_to
            }),
        2 => (0..NODES.len(), 0..NODES.len(), 0..TYPES.len(), 0..3usize, 3..5usize)
            .prop_map(|(src, tgt, etype, valid_from, valid_to)| Op::Retire {
                src, tgt, etype, valid_from, valid_to
            }),
        1 => (0..NODES.len(), any::<bool>()).prop_map(|(id, title)| Op::Upsert { id, title }),
        1 => (0..NODES.len()).prop_map(|id| Op::RetireConcept { id }),
        1 => (0..NODES.len(), 0..NODES.len(), 0..TYPES.len(), 0..3usize)
            .prop_map(|(src, tgt, etype, valid_from)| Op::Bulk { src, tgt, etype, valid_from }),
    ]
}

/// Open a `Database` (write actor and all) with the node set present.
async fn open_db(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    for id in NODES {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS[0]))
            .await
            .unwrap();
    }
    db
}

/// Run one generated op. Rejections are expected and ignored: the generator is
/// free to propose a second open interval, or to retire an edge that was never
/// asserted. Those are the schema and the API refusing, which is the system
/// working — the properties are about the state that results either way.
async fn step(db: &Database, op: Op) {
    match op {
        Op::Assert { src, tgt, etype, valid_from, valid_to } => {
            let mut e = EdgeAssertion::new(NODES[src], NODES[tgt], TYPES[etype])
                .valid_from(TS[valid_from]);
            e = e.valid_to(valid_to.map_or(SENTINEL, |i| TS[i]));
            let _ = db.assert_edge(e).await;
        }
        Op::Retire { src, tgt, etype, valid_from, valid_to } => {
            let _ = db
                .retire_edge(
                    NODES[src],
                    NODES[tgt],
                    TYPES[etype],
                    TS[valid_from],
                    TS[valid_to],
                )
                .await;
        }
        Op::Upsert { id, title } => {
            let t = if title { "Renamed" } else { "N" };
            let _ = db
                .upsert_concept(ConceptUpsert::new(NODES[id], t).valid_from(TS[0]))
                .await;
        }
        Op::RetireConcept { id } => {
            let _ = db
                .upsert_concept(
                    ConceptUpsert::new(NODES[id], "N").valid_from(TS[0]).retired(true),
                )
                .await;
        }
        Op::Bulk { src, tgt, etype, valid_from } => {
            let _ = db
                .write_bulk_atomic(vec![EdgeAssertion::new(
                    NODES[src],
                    NODES[tgt],
                    TYPES[etype],
                )
                .valid_from(TS[valid_from])
                .valid_to(SENTINEL)])
                .await;
        }
    }
}

/// Every row of a table as one delimited string, as a set. Sufficient for
/// "these rows did not change" and immune to column-order drift in a way that
/// comparing `SELECT *` positionally is not.
async fn snapshot(conn: &libsql::Connection, expr: &str, table: &str) -> BTreeSet<String> {
    let mut rows = conn
        .query(&format!("SELECT {expr} FROM {table}"), ())
        .await
        .unwrap();
    let mut out = BTreeSet::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.insert(r.get::<String>(0).unwrap());
    }
    out
}

const LINK_ROW: &str = "source_id||'|'||target_id||'|'||edge_type||'|'||valid_from||'|'||\
                        valid_to||'|'||weight||'|'||properties||'|'||recorded_at";
const LOG_ROW: &str = "seq_id||'|'||table_name||'|'||entity_id||'|'||operation||'|'||\
                       payload||'|'||recorded_at";

/// Shut the write actor down deliberately at the end of a case.
///
/// `Database` has no `Drop` impl, so dropping one detaches the actor's
/// `JoinHandle` while it still owns the sole write connection, and the runtime
/// then tears that task down underneath the FFI. Whether or not that is the
/// cause of the intermittent `STATUS_ACCESS_VIOLATION` seen while these tests
/// were being written (unreproduced since, and unattributed — see the note in
/// the plan), a test that opens thousands of databases should close them.
async fn shut_down(db: Database) {
    let _ = db.close().await;
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// **Doctrine III — assertions are immutable.**
    ///
    /// After every operation the public API offers, the set of `links` rows
    /// observed before that operation must still be present, byte for byte.
    /// Growth is the only legal change.
    ///
    /// This is the crate's central claim, and it is the one most easily lost by
    /// a well-meaning optimisation: `retire_edge` is a hair's breadth from
    /// being written as an `UPDATE … SET valid_to = ?`, which would pass every
    /// functional test in the suite while silently destroying the transaction-
    /// time axis. Checking the *row set* rather than the row count is what
    /// separates "nothing was deleted" from "nothing was rewritten".
    #[test]
    fn links_are_append_only_through_the_public_api(
        history in prop::collection::vec(op_strategy(), 1..14)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;
            let mut before = snapshot(db.read_conn(), LINK_ROW, "links").await;

            for op in &history {
                step(&db, *op).await;
                let after = snapshot(db.read_conn(), LINK_ROW, "links").await;
                let lost: Vec<_> = before.difference(&after).cloned().collect();
                prop_assert!(
                    lost.is_empty(),
                    "{:?} rewrote or removed an existing assertion: {:#?}",
                    op, lost
                );
                before = after;
            }
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **Doctrine IV — the ledger is a table, and it is append-only.**
    ///
    /// Two claims. First, every `links` assertion is captured in
    /// `transaction_log` exactly once — not "at least once", which a duplicated
    /// trigger would satisfy, and not "eventually", which nothing enforces.
    /// Second, log rows already written are never touched.
    ///
    /// The count equality is what notices a write path that reaches `links`
    /// without going through the trigger. Nothing else in the suite would.
    #[test]
    fn every_assertion_is_logged_exactly_once_and_the_log_never_changes(
        history in prop::collection::vec(op_strategy(), 1..14)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;
            let mut before = snapshot(db.read_conn(), LOG_ROW, "transaction_log").await;

            for op in &history {
                step(&db, *op).await;
                let after = snapshot(db.read_conn(), LOG_ROW, "transaction_log").await;
                let lost: Vec<_> = before.difference(&after).cloned().collect();
                prop_assert!(lost.is_empty(), "{:?} mutated the ledger: {:#?}", op, lost);
                before = after;
            }

            let links = scalar(db.read_conn(), "SELECT COUNT(*) FROM links").await;
            let logged = scalar(
                db.read_conn(),
                "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'",
            ).await;
            prop_assert_eq!(links, logged, "one assertion, one ledger entry");
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **Doctrine V — no physical deletion in hot tables.**
    ///
    /// "An ad-hoc DELETE issued from any other client aborts at the trigger
    /// layer." So the test issues one from another client: a second connection
    /// opened on the same file, outside the crate's write actor entirely.
    ///
    /// Both halves are asserted. That the delete *errors* is the guard firing;
    /// that the row set is *unchanged* is what the doctrine actually promises,
    /// and the two are not the same statement — a partial delete that then
    /// aborts would satisfy the first and violate the second.
    #[test]
    fn no_outside_client_can_delete_from_a_hot_table(
        history in prop::collection::vec(op_strategy(), 1..10)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;
            for op in &history {
                step(&db, *op).await;
            }

            // The `libsql::Database` must outlive the `Connection` taken from
            // it. Chaining `.build().await.unwrap().connect().unwrap()` drops
            // the Database at the end of the statement and leaves the
            // Connection dangling — which libSQL does not detect and Rust
            // cannot, because the borrow does not cross the FFI boundary. It
            // segfaults, and only on the path that uses the connection.
            // The `libsql::Database` must outlive the `Connection` taken from
            // it. Chaining `.build().await.unwrap().connect().unwrap()` drops
            // the Database at the end of the statement and leaves the
            // Connection pointing at freed state — which libSQL does not
            // detect and Rust cannot, because the relationship does not cross
            // the FFI boundary as a borrow.
            let outside = libsql::Builder::new_local(&harness.db_path)
                .build().await.unwrap();
            let intruder = outside.connect().unwrap();

            for (table, row_expr) in [
                ("links", LINK_ROW),
                ("transaction_log", LOG_ROW),
                ("concepts", "id||'|'||title||'|'||recorded_at"),
            ] {
                let before = snapshot(&intruder, row_expr, table).await;
                // A BEFORE DELETE trigger is a *row* trigger: it cannot fire on
                // an empty table, so an empty one proves nothing either way.
                if before.is_empty() {
                    continue;
                }
                let res = intruder.execute(&format!("DELETE FROM {table}"), ()).await;
                prop_assert!(res.is_err(), "an outside DELETE from {} succeeded", table);
                prop_assert_eq!(
                    snapshot(&intruder, row_expr, table).await, before,
                    "{} lost rows to a delete that reported failure", table
                );
            }
            drop(intruder);
            drop(outside);
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **Doctrine VIII — the two time axes answer the same question the same way.**
    ///
    /// `query_as_of_edges(t)` reads the trigger-maintained materialization.
    /// `reconstruct(ts)` folds JSON payloads out of the append-only log. They
    /// share no code, no query, and no storage — and when nothing has been
    /// recorded after `ts`, current belief *is* belief as of `ts`, so filtering
    /// the fold to valid time `t` must reproduce the materialized read exactly.
    ///
    /// This is the only check in the suite that the log is a faithful mirror of
    /// the tables. If a trigger's `json_object(…)` and its table drift apart —
    /// a renamed column, a payload field quietly dropped — every functional
    /// test still passes, `audit_current` still reports zero, and the crate
    /// silently loses the ability to reconstruct its own history. That failure
    /// has no other detector.
    #[test]
    fn the_log_fold_and_the_materialization_agree(
        history in prop::collection::vec(op_strategy(), 1..14)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;
            for op in &history {
                step(&db, *op).await;
            }

            // Any instant at or after the newest stamp: nothing was recorded
            // after it, so the fold and current belief must coincide.
            let now: String = db.read_conn()
                .query("SELECT MAX(recorded_at) FROM transaction_log", ())
                .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();

            let state = reconstruct(db.read_conn(), &now, None, None).await.unwrap();

            for t in TS {
                let materialized: BTreeSet<_> = query_as_of_edges(db.read_conn(), t)
                    .await.unwrap().into_iter().collect();
                let folded: BTreeSet<_> = state.edges.iter()
                    .filter(|(_, _, _, vf, vt)| vf.as_str() <= t && t < vt.as_str())
                    .cloned()
                    .collect();
                prop_assert_eq!(
                    &materialized, &folded,
                    "materialization and log fold disagree at valid time {}", t
                );
            }
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **Doctrine VIII — belief only ever accumulates.**
    ///
    /// Advancing the transaction-time cursor can supersede what is believed
    /// about an interval, but it can never make an interval stop being known
    /// about. The key set of `reconstruct(ts)` is therefore monotone in `ts`.
    /// A fold that dropped superseded entries instead of ranking them would
    /// violate this while still returning plausible-looking edges.
    #[test]
    fn belief_is_monotone_in_transaction_time(
        history in prop::collection::vec(op_strategy(), 1..14)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;

            let mut stamps = Vec::new();
            for op in &history {
                step(&db, *op).await;
                let ts: Option<String> = db.read_conn()
                    .query("SELECT MAX(recorded_at) FROM transaction_log", ())
                    .await.unwrap().next().await.unwrap().and_then(|r| r.get(0).ok());
                if let Some(ts) = ts {
                    stamps.push(ts);
                }
            }

            let mut previous: BTreeSet<(String, String, String, String)> = BTreeSet::new();
            for ts in &stamps {
                let state = reconstruct(db.read_conn(), ts, None, None).await.unwrap();
                let keys: BTreeSet<_> = state.edges.iter()
                    .map(|(s, t, e, vf, _)| (s.clone(), t.clone(), e.clone(), vf.clone()))
                    .collect();
                let forgotten: Vec<_> = previous.difference(&keys).cloned().collect();
                prop_assert!(
                    forgotten.is_empty(),
                    "reconstruct({}) forgot intervals it knew about earlier: {:#?}",
                    ts, forgotten
                );
                previous = keys;
            }
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **The acceptance gate for snapshot composition (§5.5, D-049).**
    ///
    /// `reconstruct` may answer from a full fold or by composing a snapshot
    /// with the delta above its anchor. Those are two mechanisms for one
    /// question, and the only honest way to hold them together is to require
    /// that they agree on every generated history at every instant — the
    /// "three paths, one rule, verified by the property suite" claim §5.5 had
    /// carried since 0.4.5 with nothing behind it.
    ///
    /// The snapshot is taken mid-history, so the delta is non-empty and spans
    /// asserts, retirements and re-assertions. That is what makes the
    /// difference between *absence* and *deletion* bite: onto an empty base a
    /// tombstone is a no-op, onto a snapshot it has to remove a row the base
    /// carries. A merge that dropped tombstones — as the fold did before D-049,
    /// where `op == "D"` was a bare `continue` — passes every full-fold test in
    /// this suite and fails here.
    #[test]
    fn composing_from_a_snapshot_equals_folding_from_genesis(
        history in prop::collection::vec(op_strategy(), 2..14)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db(&harness).await;
            let snaps = harness.temp_dir.path().join("compose_snaps");

            let split = history.len() / 2;
            for op in &history[..split] {
                step(&db, *op).await;
            }

            let mid: String = db.read_conn()
                .query("SELECT MAX(recorded_at) FROM transaction_log", ())
                .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
            let base = reconstruct(db.read_conn(), &mid, None, None).await.unwrap();
            save_snapshot(&snaps, &base).unwrap();

            // Instants *inside* the delta, so the anchored fold is exercised
            // with a partial tail and not only with the whole of it. These are
            // transaction-time stamps, not the valid-time constants in `TS`:
            // `reconstruct` asks "what was believed at", and belief is stamped
            // by the clock, not chosen by the caller.
            let mut instants = vec![mid.clone()];
            for op in &history[split..] {
                step(&db, *op).await;
                let at: String = db.read_conn()
                    .query("SELECT MAX(recorded_at) FROM transaction_log", ())
                    .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
                instants.push(at);
            }

            for ts in &instants {
                let ts = ts.as_str();
                let composed = reconstruct(db.read_conn(), ts, None, Some(&snaps))
                    .await.unwrap();
                let folded = reconstruct(db.read_conn(), ts, None, None)
                    .await.unwrap();

                prop_assert_eq!(
                    composed.edges.iter().collect::<BTreeSet<_>>(),
                    folded.edges.iter().collect::<BTreeSet<_>>(),
                    "edges disagree at {}", ts
                );
                prop_assert_eq!(
                    composed.concepts.keys().collect::<BTreeSet<_>>(),
                    folded.concepts.keys().collect::<BTreeSet<_>>(),
                    "concepts disagree at {}", ts
                );
            }
            shut_down(db).await;
            Ok(())
        })?;
    }
}

// ---------------------------------------------------------------------------
// Doctrine VII — a vector is a derived artifact (Phase 5)
// ---------------------------------------------------------------------------
//
// "Embeddings are immutable per version and excluded from the ledger… it never
// appears in transaction_log payloads; it lives in per-model tables so that a
// model migration can never produce a row whose dimension violates its type."
//
// Three claims, and the static scan in `doctrine_static_tests.rs` reaches only
// the first, and only as far as the DDL text: no trigger *shipped in the crate*
// names a vector. That says nothing about the tables `register_model` creates at
// runtime, which no static scan can see, and nothing at all about what an
// interleaved history of graph writes and embedding writes does.
//
// So these run one generated history through the handle alone (D-048), mixing
// ledger operations with embedding writes for two models of different widths.
// Two models is the point of the third claim: a vector that is the wrong width
// for the model it is handed to is exactly right for the *other* one, which is
// what a half-finished model migration produces.

/// Two registered models, deliberately of different dimensions.
const MODELS: [(&str, usize); 2] = [("alpha_v1", 4), ("beta_v1", 2)];

/// A history that interleaves the ledger with the derivative.
#[derive(Debug, Clone, Copy)]
enum Mixed {
    Ledger(Op),
    /// Embed `NODES[node]` for `MODELS[model]`, using the width `MODELS[width]`
    /// declares. `width != model` is the migration mistake, and it is generated
    /// rather than written out because which of the two it is matters: a vector
    /// too short and one too long fail at different places in the engine.
    Embed {
        node: usize,
        model: usize,
        width: usize,
        seed: u8,
    },
}

fn mixed_strategy() -> impl Strategy<Value = Mixed> {
    prop_oneof![
        3 => op_strategy().prop_map(Mixed::Ledger),
        2 => (0..NODES.len(), 0..MODELS.len(), 0..MODELS.len(), any::<u8>())
            .prop_map(|(node, model, width, seed)| Mixed::Embed { node, model, width, seed }),
    ]
}

fn model_name(i: usize) -> ModelName {
    ModelName::new(MODELS[i].0).unwrap()
}

/// A distinct, non-degenerate vector of `dim` components.
///
/// Never all-zero: cosine distance over a zero vector has no defined answer, and
/// a generator that produced one would be testing the engine's handling of an
/// undefined case rather than the doctrine.
fn vector(dim: usize, seed: u8) -> Vec<f32> {
    (0..dim).map(|i| 1.0 + seed as f32 + i as f32).collect()
}

/// `open_db`, plus both models registered through the handle.
async fn open_db_with_models(harness: &TestHarness) -> Database {
    let db = open_db(harness).await;
    for i in 0..MODELS.len() {
        db.register_model(&model_name(i), MODELS[i].1).await.unwrap();
    }
    db
}

/// The newest transaction-time stamp in a database, or `None` if nothing was
/// ever recorded — a generated history can consist entirely of rejected
/// operations, and an empty ledger is a legitimate outcome rather than a failure.
async fn newest_stamp(db: &Database) -> Option<String> {
    db.read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .and_then(|r| r.get(0).ok())
}

/// The ledger with its stamps removed, in `seq_id` order.
///
/// Stamps are what two runs of one history cannot share — the clock advances
/// with wall time — and `payload` carries none of them, so everything the log
/// *records* survives this projection and only the recording's own timing does
/// not.
async fn stampless_log(conn: &libsql::Connection) -> Vec<String> {
    let mut rows = conn
        .query(
            "SELECT table_name||'|'||entity_id||'|'||operation||'|'||payload \
             FROM transaction_log ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

proptest! {
    // A third of the cases of the suite above, and the number is measured
    // rather than chosen. R15 makes *database churn* the scarce resource, and
    // these two properties are the most expensive in the crate per case: the
    // first opens a database and builds two DiskANN indexes, the second opens
    // two databases and builds four, because "the ledger does not depend on the
    // vectors" is not a claim one database can be asked.
    //
    // Measured, and the measurement does not fully exonerate the reduction.
    // At 12 cases: the six older properties alone faulted 2/12 runs of this
    // binary, all eight faulted 4/12. At 8 cases, over 20 runs each: 3/20 and
    // 5/20. So the gap narrowed but did not close, and at n = 20 a two-run
    // difference is not a result — what the numbers establish is that the
    // *baseline* is 15% and R15 is the cause of both arms, not these two
    // properties. Anyone tempted to raise the count should re-measure rather
    // than assume the headroom is there.
    #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

    /// **Doctrine VII, exclusion.** An embedding write moves nothing in the
    /// ledger, and a mis-dimensioned one stores nothing anywhere.
    ///
    /// The check is made after *every* embedding write rather than once at the
    /// end, so the operation that reached the log is named. Both the log and
    /// `concepts` are held: routing a vector into `concepts.embedding_model`
    /// would leave `transaction_log` alone for exactly one write and then log the
    /// concept update — which is the D-041 defect's shape, where a derived value
    /// was written into a ledger table and versioned every rerun.
    ///
    /// The refusal half is the third claim of the doctrine. `expect_err` is not
    /// enough: an engine that accepted the blob and truncated it would fail here
    /// only because of the Rust-side width check, so the stored blob's *length*
    /// is asserted too, against the dimension the schema declares.
    #[test]
    fn an_embedding_write_never_reaches_the_ledger(
        history in prop::collection::vec(mixed_strategy(), 1..12)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let db = open_db_with_models(&harness).await;
            const CONCEPT_ROW: &str = "id||'|'||title||'|'||content||'|'||retired||'|'||\
                                       COALESCE(embedding_model,'-')||'|'||recorded_at";

            let mut log = snapshot(db.read_conn(), LOG_ROW, "transaction_log").await;
            let mut concepts = snapshot(db.read_conn(), CONCEPT_ROW, "concepts").await;

            for op in &history {
                match *op {
                    Mixed::Ledger(op) => {
                        step(&db, op).await;
                        log = snapshot(db.read_conn(), LOG_ROW, "transaction_log").await;
                        concepts = snapshot(db.read_conn(), CONCEPT_ROW, "concepts").await;
                    }
                    Mixed::Embed { node, model, width, seed } => {
                        let m = model_name(model);
                        let dim = MODELS[width].1;
                        let res = db
                            .upsert_embeddings(&m, vec![(NODES[node].to_string(), vector(dim, seed))])
                            .await;

                        if width == model {
                            prop_assert!(
                                res.is_ok(),
                                "a correctly dimensioned embedding was refused: {:?}", res.err()
                            );
                        } else {
                            prop_assert!(
                                matches!(res, Err(DbError::DimMismatch { .. })),
                                "a {}-wide vector was accepted for {} (declared {}): {:?}",
                                dim, MODELS[model].0, MODELS[model].1, res
                            );
                        }

                        prop_assert_eq!(
                            snapshot(db.read_conn(), LOG_ROW, "transaction_log").await, log.clone(),
                            "embedding {} for {} reached transaction_log", NODES[node], MODELS[model].0
                        );
                        prop_assert_eq!(
                            snapshot(db.read_conn(), CONCEPT_ROW, "concepts").await, concepts.clone(),
                            "embedding {} for {} rewrote a concept row", NODES[node], MODELS[model].0
                        );
                    }
                }
            }

            // Whatever did land is the declared width, and nothing watches the
            // per-model tables: a trigger on one is the only way a vector could
            // still make it into the ledger without any of the above noticing.
            for (i, (name, dim)) in MODELS.iter().enumerate() {
                let m = model_name(i);
                let wrong = scalar(
                    db.read_conn(),
                    &format!(
                        "SELECT COUNT(*) FROM {} WHERE LENGTH(embedding) <> {}",
                        m.table(), dim * 4
                    ),
                ).await;
                prop_assert_eq!(wrong, 0, "{} stored a vector of the wrong width", name);

                let triggers = scalar(
                    db.read_conn(),
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='{}'",
                        m.table()
                    ),
                ).await;
                prop_assert_eq!(triggers, 0, "{} has a trigger; it could carry a vector out", name);
            }
            shut_down(db).await;
            Ok(())
        })?;
    }

    /// **Doctrine VII, derivation.** Deleting the embeddings from a history
    /// leaves the ledger it wrote unchanged, and the state it reconstructs
    /// identical.
    ///
    /// This is the doctrine's real content, and it is a claim no single-database
    /// test can make: "excluded from the ledger" means the derivative is not an
    /// input to it, so the same graph operations must produce the same ledger
    /// whether or not vectors were ever computed. Two databases are run — one
    /// with the whole generated history, one with the embedding steps struck out
    /// — and the ledgers compared row for row in `seq_id` order, minus the
    /// stamps that no two runs can share.
    ///
    /// An implementation that logged an embedding fails on the row count. One
    /// that touched `concepts.embedding_model` fails on the payload. One that let
    /// a stored vector change *any* answer `reconstruct` gives fails on the
    /// state. The first is also caught by the property above; the last two are
    /// not caught anywhere else, because a single run has nothing to be compared
    /// against.
    #[test]
    fn striking_the_embeddings_out_of_a_history_leaves_the_ledger_identical(
        history in prop::collection::vec(mixed_strategy(), 1..12)
    ) {
        block_on(async {
            // With the embeddings.
            let mixed_harness = TestHarness::new();
            let mixed_db = open_db_with_models(&mixed_harness).await;
            for op in &history {
                match *op {
                    Mixed::Ledger(op) => step(&mixed_db, op).await,
                    Mixed::Embed { node, model, width, seed } => {
                        let dim = MODELS[width].1;
                        let _ = mixed_db
                            .upsert_embeddings(
                                &model_name(model),
                                vec![(NODES[node].to_string(), vector(dim, seed))],
                            )
                            .await;
                    }
                }
            }

            // Without them. The models are still registered, so the difference
            // between the two runs is the vectors themselves and not the schema.
            let bare_harness = TestHarness::new();
            let bare_db = open_db_with_models(&bare_harness).await;
            for op in &history {
                if let Mixed::Ledger(op) = *op {
                    step(&bare_db, op).await;
                }
            }

            prop_assert_eq!(
                stampless_log(mixed_db.read_conn()).await,
                stampless_log(bare_db.read_conn()).await,
                "the ledger depends on whether embeddings were written"
            );

            if let (Some(a), Some(b)) = (newest_stamp(&mixed_db).await, newest_stamp(&bare_db).await) {
                let with = reconstruct(mixed_db.read_conn(), &a, None, None).await.unwrap();
                let without = reconstruct(bare_db.read_conn(), &b, None, None).await.unwrap();

                prop_assert_eq!(
                    with.edges.iter().collect::<BTreeSet<_>>(),
                    without.edges.iter().collect::<BTreeSet<_>>(),
                    "reconstruct disagrees on edges once vectors exist"
                );
                // The whole attribute payload, not the key set: `NodeAttributes`
                // carries `embedding_model`, so a fold that learned to read the
                // per-model tables would diverge here and nowhere else.
                let attrs = |s: &MaterializedState| {
                    s.concepts.iter().map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<std::collections::BTreeMap<_, _>>()
                };
                prop_assert_eq!(
                    attrs(&with), attrs(&without),
                    "reconstruct disagrees on concept attributes once vectors exist"
                );
            }

            shut_down(mixed_db).await;
            shut_down(bare_db).await;
            Ok(())
        })?;
    }
}
