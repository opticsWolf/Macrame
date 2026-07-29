//! Property tests for the Doctrine VI invariant.
//!
//! `links_current` is derivative: it must always equal the latest-belief
//! projection of `links`. `audit_current` is the check that says so, and
//! `rebuild_current` is the repair. Both were previously covered only by
//! seeded unit tests — a handful of rows and three hand-chosen corruptions.
//!
//! That is not enough, and the pre-0.5.4 audit is the proof. It parsed as
//! `A EXCEPT A`, a constant zero, and it passed every seeded test *because a
//! constant zero is the right answer on clean data*. A check whose failure mode
//! is "always says fine" cannot be validated by examples that are fine.
//!
//! So these tests do not seed. They generate arbitrary histories, compute the
//! projection **independently in Rust**, and require the SQL and the model to
//! agree on the exact drift count. A degenerate query cannot survive that,
//! because the model disagrees the moment anything is actually wrong.
//!
//! # On the case count (R15)
//!
//! 32, not proptest's default 256, and the number is a workaround rather than a
//! judgement about coverage. libSQL faults intermittently with
//! STATUS_ACCESS_VIOLATION when local databases are opened concurrently in one
//! process — reproducible with no Macrame types, no proptest, just
//! open/migrate/drop in a loop, which puts it below the Doctrine I line. It is
//! not a stale-dependency problem: moving 0.6.0 → 0.9.30 left the rate
//! unchanged.
//!
//! Most of the suite is fixed by `RUST_TEST_THREADS = "1"` (.cargo/config.toml,
//! 0/30 bad runs). This binary is the residue that serialising does not reach,
//! because a property case needs a database of its own, so the only lever left
//! here is how many cases run.
//!
//! This does not weaken what the suite proves. The generator domains are tiny
//! on purpose, so 32 cases still saturate the interesting shapes, and any
//! failure proptest has ever found is replayed from `.proptest-regressions`
//! before a single new case is generated — the archive defect these tests
//! caught stays pinned no matter what this number is. Raise it deliberately
//! (`PROPTEST_CASES=512`) when changing the audit or the archive predicates,
//! and expect the occasional crash to be the engine, not the change.
//!
//! For the same reason **this binary is gated behind the `property-tests`
//! feature** and does not run under a plain `cargo test`. The gate is not a
//! demotion — these are the tests that found D-035 — it is an admission that a
//! suite failing for reasons unrelated to the code under test teaches
//! developers to ignore red. Run them with
//! `cargo test --features property-tests`, as their own step, so a genuine
//! failure is still a genuine failure.

mod harness;

use std::collections::BTreeSet;
use std::future::Future;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::integrity::{audit_current, rebuild_current};
use macrame::schema::migrations;
use proptest::prelude::*;

const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

/// Canonical (D-029) instants, in chronological *and* lexicographic order.
const TS: [&str; 5] = [
    "2026-01-01T00:00:00.000000Z",
    "2026-02-01T00:00:00.000000Z",
    "2026-03-01T00:00:00.000000Z",
    "2026-04-01T00:00:00.000000Z",
    "2026-05-01T00:00:00.000000Z",
];

/// Deliberately tiny domains. Bugs in a set-difference live at collisions —
/// same key, different payload; same partition, different `recorded_at` — and a
/// generator with room to spread out never produces one.
const NODES: [&str; 2] = ["c0", "c1"];
const TYPES: [&str; 2] = ["A", "B"];
/// Exactly representable, so the Rust model and SQLite's `EXCEPT` cannot
/// disagree over float rounding rather than over drift.
const WEIGHTS: [f64; 3] = [0.5, 1.0, 2.0];

/// One runtime for the whole binary. Building one per case is the obvious
/// shape and is one of the two things that provoke R15 — see the header note
/// on case counts.
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
// The model
// ---------------------------------------------------------------------------

/// A row of `links` / `links_current`, comparable the way `EXCEPT` compares.
///
/// `weight` is carried as raw bits so the row can be `Ord` for a `BTreeSet`.
/// Both sides come from the same generated constants, so bit equality and
/// SQLite's numeric equality coincide.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Row {
    source_id: String,
    target_id: String,
    edge_type: String,
    valid_from: String,
    valid_to: String,
    weight_bits: u64,
    properties: String,
    recorded_at: String,
}

impl Row {
    /// The projection's partition key (§5.8): one row of current belief per
    /// interval, *not* per edge.
    fn key(&self) -> (&str, &str, &str, &str) {
        (
            &self.source_id,
            &self.target_id,
            &self.edge_type,
            &self.valid_from,
        )
    }
}

/// The latest-belief projection of `links`, computed in Rust.
///
/// This is the oracle. It is deliberately written as a fold rather than as
/// anything resembling the SQL, so that a mistake in the window function has
/// nowhere to hide: the two implementations share no code and no reasoning.
///
/// Well-defined without a tie-break because `links` has
/// `PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)` —
/// two rows in one partition cannot share a `recorded_at`.
fn projection(links: &[Row]) -> BTreeSet<Row> {
    let mut best: Vec<Row> = Vec::new();
    for row in links {
        match best.iter_mut().find(|b| b.key() == row.key()) {
            Some(b) if row.recorded_at > b.recorded_at => *b = row.clone(),
            Some(_) => {}
            None => best.push(row.clone()),
        }
    }
    best.into_iter().collect()
}

/// Symmetric difference cardinality — what `audit_current` claims to return.
fn expected_drift(links: &[Row], current: &[Row]) -> usize {
    let projected = projection(links);
    let materialized: BTreeSet<Row> = current.iter().cloned().collect();
    materialized.difference(&projected).count() + projected.difference(&materialized).count()
}

async fn read_rows(conn: &libsql::Connection, table: &str) -> Vec<Row> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT source_id, target_id, edge_type, valid_from, valid_to, \
                 weight, properties, recorded_at FROM {table}"
            ),
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        let weight: f64 = r.get(5).unwrap();
        out.push(Row {
            source_id: r.get(0).unwrap(),
            target_id: r.get(1).unwrap(),
            edge_type: r.get(2).unwrap(),
            valid_from: r.get(3).unwrap(),
            valid_to: r.get(4).unwrap(),
            weight_bits: weight.to_bits(),
            properties: r.get(6).unwrap(),
            recorded_at: r.get(7).unwrap(),
        });
    }
    out
}

/// `audit_current`'s answer as a plain number, so a property can compare it to
/// the model without caring which arm of the `Result` carried it.
async fn audit_count(conn: &libsql::Connection) -> usize {
    match audit_current(conn).await {
        Ok(n) => n,
        Err(DbError::CurrentDrift { n }) => n,
        Err(e) => panic!("audit failed for a reason other than drift: {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// Generated operations
// ---------------------------------------------------------------------------

/// An assertion into `links`. Indices, not strings: the shrinker can then walk
/// a failing case down to the smallest indices rather than to arbitrary text.
#[derive(Debug, Clone, Copy)]
struct Assert {
    src: usize,
    tgt: usize,
    etype: usize,
    valid_from: usize,
    /// `None` is the open sentinel.
    valid_to: Option<usize>,
    weight: usize,
    recorded_at: usize,
}

fn assert_strategy() -> impl Strategy<Value = Assert> {
    (
        0..NODES.len(),
        0..NODES.len(),
        0..TYPES.len(),
        0..3usize,             // valid_from  ∈ TS[0..3]
        prop::option::of(2..5usize), // valid_to ∈ TS[2..5] or open
        0..WEIGHTS.len(),
        0..TS.len(),
    )
        .prop_map(
            |(src, tgt, etype, valid_from, valid_to, weight, recorded_at)| Assert {
                src,
                tgt,
                etype,
                valid_from,
                valid_to,
                weight,
                recorded_at,
            },
        )
}

/// Deliberate damage to the derivative table. `audit_current` exists to notice
/// these; the property is that it notices *exactly* them.
#[derive(Debug, Clone, Copy)]
enum Corruption {
    /// Drop the nth row — a missed materialisation.
    Drop(usize),
    /// Rewrite the nth row's weight — right key, wrong payload, which is drift
    /// in *both* directions at once.
    Stale(usize),
    /// Insert a row the projection does not have — spurious materialisation.
    Ghost(usize),
}

fn corruption_strategy() -> impl Strategy<Value = Corruption> {
    prop_oneof![
        (0..8usize).prop_map(Corruption::Drop),
        (0..8usize).prop_map(Corruption::Stale),
        (0..3usize).prop_map(Corruption::Ghost),
    ]
}

/// Open a migrated database with both concepts present (`links` has a FK into
/// `concepts` and `PRAGMA foreign_keys` is on).
///
/// Returns the `libsql::Database` alongside the `Connection` because it **must**
/// outlive it. Returning the connection alone drops the Database at the end of
/// this function and leaves the caller holding a connection into freed state:
/// an intermittent `STATUS_ACCESS_VIOLATION` whose rate scales with how many
/// allocations follow, which is why it showed up under proptest and never in a
/// unit test. Rust cannot catch it — the relationship is inside the FFI, not in
/// the borrow checker — so the shape of the helper is the only guard.
async fn fresh(harness: &TestHarness) -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();
    for id in NODES {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS[0]],
        )
        .await
        .unwrap();
    }
    (db, conn)
}

/// Apply a generated history. Insert failures are *expected and skipped*: the
/// generator is free to propose a second open interval (blocked by
/// `trg_links_single_open`) or a duplicate primary key. Those are the schema
/// working, not the property failing — what matters is the state that results.
async fn apply(conn: &libsql::Connection, history: &[Assert]) {
    for a in history {
        let _ = conn
            .execute(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
                 weight, properties, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', ?7)",
                libsql::params![
                    NODES[a.src],
                    NODES[a.tgt],
                    TYPES[a.etype],
                    TS[a.valid_from],
                    a.valid_to.map_or(SENTINEL, |i| TS[i]),
                    WEIGHTS[a.weight],
                    TS[a.recorded_at],
                ],
            )
            .await;
    }
}

async fn corrupt(conn: &libsql::Connection, damage: &[Corruption]) {
    for c in damage {
        match *c {
            Corruption::Drop(n) | Corruption::Stale(n) => {
                let current = read_rows(conn, "links_current").await;
                if current.is_empty() {
                    continue;
                }
                let victim = &current[n % current.len()];
                let sql = match c {
                    Corruption::Drop(_) => {
                        "DELETE FROM links_current WHERE source_id = ?1 AND target_id = ?2 \
                         AND edge_type = ?3 AND valid_from = ?4"
                    }
                    _ => {
                        "UPDATE links_current SET weight = 99.0 WHERE source_id = ?1 \
                         AND target_id = ?2 AND edge_type = ?3 AND valid_from = ?4"
                    }
                };
                conn.execute(
                    sql,
                    libsql::params![
                        victim.source_id.clone(),
                        victim.target_id.clone(),
                        victim.edge_type.clone(),
                        victim.valid_from.clone()
                    ],
                )
                .await
                .unwrap();
            }
            Corruption::Ghost(n) => {
                // May collide with a real row's primary key; that is a no-op,
                // not damage, and the model sees the same thing either way.
                let _ = conn
                    .execute(
                        "INSERT OR IGNORE INTO links_current (source_id, target_id, edge_type, \
                         valid_from, valid_to, weight, properties, recorded_at) \
                         VALUES ('c0', 'c1', 'GHOST', ?1, ?2, 7.0, '{}', ?2)",
                        libsql::params![TS[n % 3], TS[0]],
                    )
                    .await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

// A fresh SQLite file per case, so the case count is a runtime budget rather
// than a coverage target. 96 cases over domains this small saturates the
// interesting shapes; the shrinker does the rest.
proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// **The trigger maintains the invariant.** For any history the schema
    /// accepts, `links_current` is already the projection — no rebuild needed.
    ///
    /// This is the property the old seeded tests thought they were checking.
    /// It is also the one a constant-zero audit passes trivially, which is why
    /// it is worthless on its own and why the next property exists.
    #[test]
    fn the_sync_trigger_keeps_current_equal_to_the_projection(
        history in prop::collection::vec(assert_strategy(), 0..12)
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let (_db, conn) = fresh(&harness).await;
            apply(&conn, &history).await;

            let links = read_rows(&conn, "links").await;
            let current = read_rows(&conn, "links_current").await;
            prop_assert_eq!(
                expected_drift(&links, &current), 0,
                "the AFTER INSERT trigger did not maintain Doctrine VI"
            );
            prop_assert_eq!(audit_count(&conn).await, 0);
            Ok(())
        })?;
    }

    /// **The audit is exact.** Not "detects corruption" — *equals the model*.
    ///
    /// An audit that over-reports is as broken as one that under-reports: it
    /// makes `rebuild_current` fail its own post-check and turns a healthy
    /// database into an unopenable one. Comparing counts, not just
    /// `is_err()`, is what makes this a check on the query rather than on the
    /// existence of a query.
    #[test]
    fn the_audit_count_equals_the_model(
        history in prop::collection::vec(assert_strategy(), 0..12),
        damage in prop::collection::vec(corruption_strategy(), 0..5),
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let (_db, conn) = fresh(&harness).await;
            apply(&conn, &history).await;
            corrupt(&conn, &damage).await;

            let links = read_rows(&conn, "links").await;
            let current = read_rows(&conn, "links_current").await;
            prop_assert_eq!(
                audit_count(&conn).await,
                expected_drift(&links, &current),
                "audit disagreed with the model\nlinks: {:#?}\ncurrent: {:#?}",
                links, current
            );
            Ok(())
        })?;
    }

    /// **Rebuild is a fixpoint, and reaches it from anywhere.**
    ///
    /// Three claims in one: rebuilding produces exactly the projection, the
    /// resulting drift is zero, and rebuilding again changes nothing. The third
    /// is what catches a repair that is only correct on the state it was
    /// written against.
    #[test]
    fn rebuild_restores_the_projection_from_any_damage(
        history in prop::collection::vec(assert_strategy(), 0..12),
        damage in prop::collection::vec(corruption_strategy(), 0..5),
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let (_db, conn) = fresh(&harness).await;
            apply(&conn, &history).await;
            corrupt(&conn, &damage).await;

            let links = read_rows(&conn, "links").await;
            let report = rebuild_current(&conn).await.unwrap();

            let rebuilt = read_rows(&conn, "links_current").await;
            let expected = projection(&links);
            prop_assert_eq!(rebuilt.iter().cloned().collect::<BTreeSet<_>>(), expected.clone());
            prop_assert_eq!(report.rows_rebuilt, expected.len());
            prop_assert_eq!(report.drift_after, 0);
            prop_assert_eq!(audit_count(&conn).await, 0);

            // Idempotence: the second pass must be a no-op, not a re-derivation
            // that happens to land somewhere else.
            let second = rebuild_current(&conn).await.unwrap();
            prop_assert_eq!(second, report);
            prop_assert_eq!(read_rows(&conn, "links_current").await, rebuilt);
            Ok(())
        })?;
    }

    /// **Archiving preserves the invariant.**
    ///
    /// `archive()` deletes from `links` under `LINKS_ARCHIVABLE` and separately
    /// deletes from `links_current` to compensate. Those are two different
    /// predicates over two different clocks, and nothing else in the suite
    /// exercises them jointly. Doctrine VI does not pause for an archive: if
    /// the compensation is not the exact image of the deletion, the ledger is
    /// left permanently unauditable.
    #[test]
    fn archiving_leaves_the_ledger_auditable(
        history in prop::collection::vec(assert_strategy(), 0..12),
        cutoff in 0..TS.len(),
    ) {
        block_on(async {
            let harness = TestHarness::new();
            let (_db, conn) = fresh(&harness).await;
            apply(&conn, &history).await;
            prop_assume!(audit_count(&conn).await == 0);

            let cold = harness.temp_dir.path().join("cold.db");
            macrame::temporal::archive(&conn, TS[cutoff], &cold).await.unwrap();

            let links = read_rows(&conn, "links").await;
            let current = read_rows(&conn, "links_current").await;
            prop_assert_eq!(
                audit_count(&conn).await, 0,
                "archive left drift behind\ncutoff: {}\nlinks: {:#?}\ncurrent: {:#?}",
                TS[cutoff], links, current
            );
            Ok(())
        })?;
    }
}
