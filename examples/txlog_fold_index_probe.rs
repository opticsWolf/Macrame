//! W15.2 — the fold partitions on `(table_name, entity_id, branch_id)` and
//! nothing indexes that. What does it cost, and what would an index buy?
//!
//! Review C-4 asks for `idx_txlog_entity_lineage` and states the reason in one
//! line: the fold partitions on `(entity_id, branch_id)`, orders by `seq_id`,
//! and no index covers that. Both halves of that sentence are worth checking
//! before a rung is spent on it, because the crate has bought an index on a
//! believed benefit before ([D-089]) and has a standing rule about it.
//!
//! The partition is not the one the review names. `temporal::replay`'s four
//! folds partition on **`(table_name, entity_id, branch_id)`** — `table_name`
//! leads, deliberately, because a concept id and a link's synthetic id share a
//! namespace and a partition keyed on the id alone silently drops one of them.
//! So the index C-4 asks for by name cannot serve the window: its leading
//! column is not the partition's.
//!
//! # What it reports
//!
//! 1. **The fixture** — rows in `transaction_log`, distinct partitions, and
//!    revisions per partition, because a fold's cost is a function of all
//!    three and a fixture with one row per partition would make the sort free.
//! 2. **The plan today**, from `EXPLAIN QUERY PLAN` on the shipped fold text,
//!    with the temp B-tree the window function needs called out.
//! 3. **Five index shapes × the plan and the wall time** of
//!    `Database::reconstruct` — the public call the fold serves — best and mean
//!    over `--iterations` runs.
//! 4. **The write side.** `transaction_log` takes a row per concept and per
//!    edge write, so every index on it is paid on the hottest path the crate
//!    has.
//! 5. **The other two folds.** `replay.rs`'s four are not the only windows over
//!    this table. `graph::plan::links_at_tx_cte` partitions on
//!    `(entity_id, branch_id)` under `WHERE table_name = 'links'`, and
//!    `temporal::as_of::hydrate_at_time` on `entity_id` alone under
//!    `WHERE table_name = 'concepts'`. An index chosen for the first four is
//!    free to capture those two, and capturing them is not the same as helping
//!    them — so both are swept at four transaction-time bounds, because what
//!    the new index takes away is a *seek on `recorded_at`* and what it gives
//!    back is the order, and which of those is worth more is a function of how
//!    much of the log the bound admits.
//!
//! # The reproduced-query hazard
//!
//! `HOT_FOLD` and its three siblings are private `const`s, so the `EXPLAIN`
//! here runs a copy. A copy can outlive its original — the failure
//! `index_plan_tests` bounds with `include_str!` fragments, and the same bound
//! is applied to whatever this probe recommends. The wall-clock numbers do not
//! share the hazard: they go through `Database::reconstruct`, which runs the
//! real text.
//!
//! # Reading it after 0.15.12
//!
//! The rung shipped, so `ddl::CREATE_INDICES` now declares the winning shape
//! and every fixture built through `Database::open` starts with it. Both
//! sweeps therefore **drop it first** and re-create it by hand, and the row
//! labelled `no index (today)` means *today as of the question this probe was
//! asked*, which is v16. Without that drop the probe compares the shipped index
//! against itself and reports, correctly and uselessly, that nothing changes.
//!
//! Run it:
//!
//! ```text
//! cargo run --release --example txlog_fold_index_probe -- --entities 4000
//! cargo run --release --example txlog_fold_index_probe -- --arm other-folds
//! ```
//!
//! [D-089]: ../docs/architecture/s13-decision-register.md#d-089

use std::time::Instant;

use macrame::branch::BranchId;
use macrame::graph::{AttributeMode, EdgeAssertion, TraversalBuilder};
use macrame::temporal::{hydrate_attributes, AsOf};
use macrame::ConceptUpsert;
use macrame::Database;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const FUTURE: &str = "2027-01-01T00:00:00.000000Z";
/// When half the edges stop being believed — inside the fold's window, so
/// both rows of those partitions are candidates and the window has to choose.
const RETIRED_AT: &str = "2026-06-01T00:00:00.000000Z";

/// Single-edge assertions per write measurement. Single, not bulk: the index
/// is paid per row either way, and this is the path with the tightest budget.
const WRITES: usize = 400;

/// Fresh databases per write measurement; the reported figure is the best.
const WRITE_REPEATS: usize = 5;

/// The shipped fold, copied. See the module note on why a copy is acceptable
/// here and what bounds it.
const HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#;

/// The anchored form, which is what a database with snapshots actually runs.
///
/// Included because the two have different selectivity — `seq_id > ?2` cuts the
/// input to the delta — and an index that helps one may be irrelevant to the
/// other. Reporting only the unanchored plan would describe the cold-start read
/// and call it the read.
const ANCHORED_HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1 AND seq_id > ?2
    ) WHERE rn = 1
"#;

/// The fold that runs once a database has been archived, copied.
///
/// The `{cold}` projection is spelled out as `branch_id`, which is what
/// `ColdLineage` emits for a cold file carrying the column. The other arm is
/// the literal `'main'` and makes no difference to the access path.
const COLD_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM (
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id FROM main.transaction_log
            UNION ALL
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id FROM cold.transaction_log
        ) WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#;

/// The log's other readers, which an index added for the fold must not disturb.
///
/// This is the D-042 / D-059 / D-064 category the plan-pinning tests exist for:
/// a covering index captures a query because it *contains* the columns, not
/// because it discriminates. Two of these are the named justification of an
/// index already on this table, so if the new one takes them, that index has
/// lost its reader and the rung owes a decision about it.
const OTHER_READERS: &[(&str, &str)] = &[
    (
        "the archive's log predicate (idx_txlog_time, idx_txlog_entity)",
        "SELECT seq_id FROM transaction_log WHERE recorded_at < ?1 AND EXISTS ( \
           SELECT 1 FROM transaction_log newer \
           WHERE newer.entity_id = transaction_log.entity_id \
             AND newer.branch_id = transaction_log.branch_id \
             AND newer.seq_id    > transaction_log.seq_id)",
    ),
    (
        "the hot log's newest stamp (idx_txlog_time)",
        "SELECT MAX(recorded_at) FROM transaction_log",
    ),
    (
        "the reach guard's counts",
        "SELECT COUNT(*), MIN(seq_id), MAX(seq_id), MIN(recorded_at) FROM transaction_log",
    ),
    (
        "the archive's branch arm",
        "SELECT seq_id FROM transaction_log WHERE branch_id = ?1",
    ),
];

/// The winning shape, by name, for the cold arm below.
const WINNER: &str = "(table_name, entity_id, branch_id, seq_id DESC)";

/// How deep the traversal arm walks.
///
/// Four, because the fold is `MATERIALIZED` and is therefore built **once**
/// however deep the walk goes (0.15.2, D-244) — the depth decides how many rows
/// come back out of it, not how much the fold costs, and it is the fold that is
/// under test. A depth of one would still measure the same fold but would leave
/// the recursive step with nothing to do, which is the shape that hid D-244 for
/// eleven releases.
const WALK_DEPTH: usize = 4;

/// Where the transaction-time bound is placed, as a percentage of log rows
/// admitted.
///
/// The whole question for the other two folds is a trade between a seek and an
/// order, and its answer moves with this number: at 100% the seek on
/// `recorded_at` discriminates nothing and the partition index is free order,
/// while at 25% the seek is the cheaper half of the plan and giving it up to
/// avoid a sort may not pay. A single instant would report whichever side of
/// the crossing it happened to land on.
const BOUNDS: &[usize] = &[25, 50, 75, 100];

/// The shapes under test.
///
/// `(entity_id, branch_id)` is C-4's, by name, so the finding is measured as
/// written rather than as corrected. The three after it lead on the partition
/// the code actually uses; they differ in what they carry past the seek, which
/// is the whole question — a window function wants its input *ordered*, and an
/// index that supplies the order but not the columns still sends the planner
/// back to the table for every row.
const SHAPES: &[(&str, &str)] = &[
    ("C-4 as written", "(entity_id, branch_id)"),
    (
        "partition order",
        "(table_name, entity_id, branch_id, seq_id)",
    ),
    (
        "partition order + recorded_at",
        "(table_name, entity_id, branch_id, seq_id, recorded_at)",
    ),
    (
        "covering the fold",
        "(table_name, entity_id, branch_id, seq_id, recorded_at, operation, payload)",
    ),
    (
        "recorded_at first",
        "(recorded_at, table_name, entity_id, branch_id, seq_id)",
    ),
    // The window orders `seq_id DESC` within the partition. An ascending index
    // supplies the partition columns and then has to re-sort inside each one,
    // which is what `USE TEMP B-TREE FOR RIGHT PART OF ORDER BY` says. This is
    // the only shape that can satisfy the whole ordering.
    (
        "partition order, seq_id DESC",
        "(table_name, entity_id, branch_id, seq_id DESC)",
    ),
    (
        "covering, seq_id DESC",
        "(table_name, entity_id, branch_id, seq_id DESC, recorded_at, operation, payload)",
    ),
];

struct Args {
    entities: usize,
    revisions: usize,
    branches: usize,
    iterations: usize,
    /// Which sections to run: `all`, or one section's name.
    ///
    /// The write sweep alone is seven shapes x [`WRITE_REPEATS`] fresh
    /// databases, so a full run costs minutes. When one section is being
    /// re-measured — which is what happens every time a number in it is
    /// questioned — the rest is noise between the question and the answer.
    arm: String,
}

fn args() -> Args {
    let mut a = Args {
        entities: 1_000,
        revisions: 3,
        branches: 3,
        iterations: 9,
        arm: "all".to_string(),
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i + 1 < argv.len() {
        if argv[i] == "--arm" {
            a.arm = argv[i + 1].clone();
            i += 2;
            continue;
        }
        let v = argv[i + 1]
            .parse()
            .unwrap_or_else(|_| panic!("bad {}", argv[i]));
        match argv[i].as_str() {
            "--entities" => a.entities = v,
            "--revisions" => a.revisions = v,
            "--branches" => a.branches = v,
            "--iterations" => a.iterations = v,
            other => panic!("unknown flag {other}"),
        }
        i += 2;
    }
    a
}

// ───────────────────────────────────────────────────────────────────────────
// Fixture
// ───────────────────────────────────────────────────────────────────────────

/// A ledger with history: every concept is upserted `revisions` times and half
/// the edges are retired, so partitions have more than one row and
/// `ROW_NUMBER() = 1` has something to choose between.
///
/// **The revisions come from upserts and retirements, not from re-asserting an
/// edge.** A link's `entity_id` is the synthetic `source|target|type|valid_from`,
/// so two assertions with different `valid_from` are two *entities*, one row
/// each, and a fixture built that way would have a partition per row and no
/// sort to speak of. The two ways a link entity gains a second log row are a
/// retirement and a rebuild; the two ways a concept gains one are an upsert and
/// a retirement. Those are what a real ledger accumulates and they are what is
/// built here.
///
/// Spread across `branches` lineages, because `branch_id` is a partition column
/// and a single-lineage fixture would leave it constant — which is exactly the
/// case where an index on it looks free and is worth nothing.
async fn populate(db: &Database, a: &Args) {
    let mut lineages = vec![BranchId::main()];
    for i in 1..a.branches {
        let id = BranchId::new(format!("b{i}")).unwrap();
        db.fork(id.clone(), BranchId::main()).await.unwrap();
        lineages.push(id);
    }

    // Concepts first: a link's endpoints are foreign keys. They also put a
    // second `table_name` in the log, which is the discriminator the partition
    // leads on — a fixture of links alone would leave that column constant and
    // make the leading position look free.
    for rev in 0..a.revisions {
        let mut concepts = Vec::with_capacity(a.entities * 2);
        for e in 0..a.entities {
            concepts
                .push(ConceptUpsert::new(format!("n{e}"), format!("n rev {rev}")).valid_from(TS));
            concepts
                .push(ConceptUpsert::new(format!("m{e}"), format!("m rev {rev}")).valid_from(TS));
        }
        let written = db.write_concepts(concepts).await.unwrap();
        assert_eq!(written, a.entities * 2);
    }

    let mut edges = Vec::with_capacity(a.entities);
    for e in 0..a.entities {
        edges.push(
            EdgeAssertion::new(format!("n{e}"), format!("m{e}"), "LINKS")
                .valid_from(TS)
                .on_branch(lineages[e % lineages.len()].clone()),
        );
    }
    let written = db.bulk_import(edges).await.unwrap();
    assert_eq!(written, a.entities);

    // Half of them retired, which is the update that gives a *link* partition a
    // second row without giving it a second identity.
    for e in (0..a.entities).step_by(2) {
        let lineage = &lineages[e % lineages.len()];
        db.retire_edge_on(
            &format!("n{e}"),
            &format!("m{e}"),
            "LINKS",
            TS,
            RETIRED_AT,
            lineage.clone(),
        )
        .await
        .unwrap();
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Measurement
// ───────────────────────────────────────────────────────────────────────────

fn text(s: &str) -> libsql::Value {
    libsql::Value::Text(s.to_string())
}

/// Pages in use, which is `page_count` minus the freelist.
///
/// Not `page_count`: dropping an index returns its pages to the freelist and
/// leaves the file the size it grew to, so a sweep that creates and drops
/// several shapes reports the high-water mark for every one of them after the
/// first. This measurement said "+51.0%" for three different indexes before
/// the freelist was subtracted, which is what a high-water mark looks like.
async fn used_pages(conn: &libsql::Connection) -> i64 {
    scalar(conn, "PRAGMA page_count").await - scalar(conn, "PRAGMA freelist_count").await
}

async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// `EXPLAIN QUERY PLAN`, flattened to one line per step.
async fn plan(conn: &libsql::Connection, sql: &str, params: Vec<libsql::Value>) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(3).unwrap());
    }
    out
}

/// Best and mean milliseconds to run the copied fold and drain every row.
///
/// The half of `reconstruct` that an index could possibly change. Everything
/// `reconstruct` does beyond this — decoding a JSON payload per row, building
/// the `HashMap` and the edge list — is Rust and pays the same regardless of
/// how the rows were found. Reporting only the end-to-end number would let a
/// 4% move in a 17 ms call be read as a 4% move in the query, and the two are
/// not the same claim.
async fn time_sql(conn: &libsql::Connection, sql: &str, n: usize) -> (f64, f64) {
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..n {
        let t = Instant::now();
        let mut rows = conn.query(sql, vec![text(FUTURE)]).await.unwrap();
        let mut seen = 0usize;
        while let Some(row) = rows.next().await.unwrap() {
            // Read one column rather than none: libsql decodes lazily, and a
            // loop that touches nothing measures the cursor rather than the
            // query.
            let _: String = row.get(2).unwrap();
            seen += 1;
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(seen > 0);
        best = best.min(ms);
        total += ms;
    }
    (best, total / n as f64)
}

/// Best and mean milliseconds over `n` reconstructions.
///
/// Through the public call rather than the copied text: the copy is for the
/// plan, and a timing taken on a copy would be measuring this file.
async fn time_reconstruct(db: &Database, n: usize) -> (f64, f64) {
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..n {
        let t = Instant::now();
        let state = db.reconstruct(FUTURE).await.unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        // Unwrapped and read: a discarded result in a best-of loop measures
        // whichever path failed fastest.
        assert!(!state.edges.is_empty());
        best = best.min(ms);
        total += ms;
    }
    (best, total / n as f64)
}

/// Milliseconds for `n` single-edge assertions, on a database of its own.
///
/// **A fresh database per shape, not one database measured repeatedly.** The
/// first arrangement of this measurement reused the fixture, and reported that
/// adding an index made writes *16% faster* — the runs were not comparable,
/// because each one left its rows behind and the next started warmer and
/// larger. A per-shape file costs a few seconds and makes the comparison one.
async fn time_writes(shape: Option<&str>, n: usize) -> f64 {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_cadence(dir.path().join("w.db"), None)
        .await
        .unwrap();
    if let Some(columns) = shape {
        let conn = db.raw().connect().unwrap();
        conn.execute(
            &format!("CREATE INDEX idx_probe ON transaction_log {columns}"),
            (),
        )
        .await
        .unwrap();
    }

    // The endpoints first, and outside the timed region: `assert_edge` refuses
    // an edge whose concepts do not exist, and creating them inside the loop
    // would be timing three writes and calling it one.
    let mut concepts = Vec::with_capacity(n * 2);
    for i in 0..n {
        concepts.push(ConceptUpsert::new(format!("w{i}"), "w").valid_from(TS));
        concepts.push(ConceptUpsert::new(format!("x{i}"), "x").valid_from(TS));
    }
    db.write_concepts(concepts).await.unwrap();

    let t = Instant::now();
    for i in 0..n {
        db.assert_edge(
            EdgeAssertion::new(format!("w{i}"), format!("x{i}"), "LINKS").valid_from(TS),
        )
        .await
        .unwrap();
    }
    let ms = t.elapsed().as_secs_f64() * 1e3;
    db.close().await.unwrap();
    ms
}

/// The best of [`WRITE_REPEATS`] such runs.
///
/// Best rather than mean, as everywhere else in this repository's probes: the
/// distribution has a floor and a long tail of scheduler and flush noise, and
/// the first arrangement of this measurement showed a **10% spread between two
/// runs of the identical baseline** — enough to swallow the whole effect being
/// looked for.
async fn best_writes(shape: Option<&str>, n: usize) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..WRITE_REPEATS {
        best = best.min(time_writes(shape, n).await);
    }
    best
}

/// The traversal fold's fixture: one chain, written interleaved with the
/// concept revisions.
///
/// [`populate`] writes every concept revision and *then* every edge, which is
/// the right shape for the four folds in `replay.rs` — they read the whole log
/// unconditionally — but it puts all the links rows in the last third of it. A
/// transaction-time bound below that third would admit no edge at all, and the
/// sweep would be timing an empty walk and reporting it as a cheap one. Here
/// the two kinds are interleaved, so a bound anywhere in the log admits a
/// proportional slice of both.
///
/// The walk's own first `WALK_DEPTH` edges are written in the first pass and
/// are never retired, so **every bound returns the same answer**. That is
/// deliberate: a sweep whose arms return different numbers of rows is measuring
/// the answer's size as much as the plan.
async fn traversal_fixture(db: &Database, a: &Args) {
    let chunk = (a.entities / a.revisions.max(1)).max(1);
    for rev in 0..a.revisions {
        let concepts: Vec<ConceptUpsert> = (0..a.entities)
            .map(|e| ConceptUpsert::new(format!("n{e}"), format!("n rev {rev}")).valid_from(TS))
            .collect();
        db.write_concepts(concepts).await.unwrap();

        let lo = rev * chunk;
        let hi = ((rev + 1) * chunk).min(a.entities.saturating_sub(1));
        let edges: Vec<EdgeAssertion> = (lo..hi)
            .map(|e| {
                EdgeAssertion::new(format!("n{e}"), format!("n{}", e + 1), "LINKS").valid_from(TS)
            })
            .collect();
        if !edges.is_empty() {
            db.bulk_import(edges).await.unwrap();
        }

        // Retirements interleaved too, and only past the walk's own path: a
        // link partition gains its second row from a retirement, and a fixture
        // with one row per partition has no window worth ordering.
        for e in lo..hi {
            if e % 2 == 1 && e > WALK_DEPTH {
                db.retire_edge_on(
                    &format!("n{e}"),
                    &format!("n{}", e + 1),
                    "LINKS",
                    TS,
                    RETIRED_AT,
                    BranchId::main(),
                )
                .await
                .unwrap();
            }
        }
    }
}

/// The instant that admits `pct` percent of the log, taken from the log itself.
///
/// By row rank rather than by clock arithmetic: the stamps are whatever the
/// actor wrote, they are not evenly spaced, and "the bound admits a quarter of
/// the rows" is the property the sweep is varying.
async fn bound_at(conn: &libsql::Connection, pct: usize) -> String {
    let rows = scalar(conn, "SELECT COUNT(*) FROM transaction_log").await;
    let offset = ((rows as usize * pct / 100).max(1) - 1).min(rows as usize - 1);
    let mut r = conn
        .query(
            &format!(
                "SELECT recorded_at FROM transaction_log ORDER BY seq_id LIMIT 1 OFFSET {offset}"
            ),
            (),
        )
        .await
        .unwrap();
    r.next().await.unwrap().unwrap().get(0).unwrap()
}

/// Best and mean milliseconds for a transaction-time traversal.
async fn time_walk(conn: &libsql::Connection, at: &str, n: usize) -> (f64, f64) {
    let walk = TraversalBuilder::new("n0")
        .max_depth(WALK_DEPTH)
        .as_of_recorded(at);
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..n {
        let t = Instant::now();
        let ids = walk.execute_ids(conn, FUTURE).await.unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(!ids.is_empty(), "the walk returned nothing at {at}");
        best = best.min(ms);
        total += ms;
    }
    (best, total / n as f64)
}

/// Best and mean milliseconds for a transaction-time attribute hydrate.
async fn time_hydrate(conn: &libsql::Connection, ids: &[String], at: &str, n: usize) -> (f64, f64) {
    let as_of = AsOf::recorded_at(at);
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..n {
        let t = Instant::now();
        let got = hydrate_attributes(conn, ids, &as_of, AttributeMode::AtTime)
            .await
            .unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(!got.is_empty());
        best = best.min(ms);
        total += ms;
    }
    (best, total / n as f64)
}

/// The two folds that are not in `replay.rs`, swept across the bound.
///
/// A fixture of its own, for the reason `traversal_fixture` gives, and a
/// database of its own so the sweep above is not measured against a file this
/// one has grown.
async fn other_folds(a: &Args) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_cadence(dir.path().join("walk.db"), None)
        .await
        .unwrap();
    traversal_fixture(&db, a).await;
    db.analyze().await.unwrap();
    let conn = db.raw().connect().unwrap();

    let rows = scalar(&conn, "SELECT COUNT(*) FROM transaction_log").await;
    let links = scalar(
        &conn,
        "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'",
    )
    .await;
    println!(
        "\n== the other two folds: a chain of {} on one lineage ==",
        a.entities
    );
    println!("  transaction_log: {rows} rows, {links} of them links\n");

    let hydrated: Vec<String> = (0..=WALK_DEPTH).map(|e| format!("n{e}")).collect();

    // **The baseline is the index dropped, not the index never created.**
    // `ddl::CREATE_INDICES` ships `idx_txlog_fold_partition` as of 0.15.12, so
    // a fixture built through `Database::open` already has it — the first run
    // of this arm compared the index against itself and reported that it
    // changed nothing, which is exactly what that comparison would say.
    conn.execute("DROP INDEX idx_txlog_fold_partition", ())
        .await
        .unwrap();

    for (state, index) in [
        ("without it (the v16 plan)", false),
        ("with it (v17)", true),
    ] {
        if index {
            conn.execute(
                &format!("CREATE INDEX idx_txlog_fold_partition ON transaction_log {WINNER}"),
                (),
            )
            .await
            .unwrap();
        }
        conn.execute("ANALYZE", ()).await.unwrap();
        println!("  {state}");
        println!(
            "    {:<10} {:>9} {:>9}   {:>9} {:>9}",
            "bound", "walk best", "mean", "hydr best", "mean"
        );
        for pct in BOUNDS {
            let at = bound_at(&conn, *pct).await;
            let (wb, wm) = time_walk(&conn, &at, a.iterations).await;
            let (hb, hm) = time_hydrate(&conn, &hydrated, &at, a.iterations).await;
            println!(
                "    {:<10} {wb:9.3} {wm:9.3}   {hb:9.3} {hm:9.3}",
                format!("{pct}%")
            );
        }

        // One plan each, at the widest bound — the plan does not vary across
        // the sweep, and printing four copies of it would bury the numbers.
        let at = bound_at(&conn, 100).await;
        let walk_sql = TraversalBuilder::new("n0")
            .max_depth(WALK_DEPTH)
            .as_of_recorded(&at)
            .build_sql();
        for step in plan(&conn, &walk_sql, Vec::new()).await {
            // `B-TREE` as well as the table: whether the walk's window sorts
            // is the whole question, and that step names neither.
            if step.contains("transaction_log")
                || step.contains("links_at_tx")
                || step.contains("B-TREE")
            {
                println!("      walk     {step}");
            }
        }
        // The hydrate fold, copied — `hydrate_at_time` builds its SQL from a
        // chunk length and the text is not reachable from here. Bounded the
        // same way the others are: `index_plan_tests` holds the fragment.
        let hydrate_sql = "SELECT entity_id, seq_id, payload FROM ( \
             SELECT entity_id, seq_id, payload, \
                    ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) as rn \
             FROM transaction_log \
             WHERE table_name = 'concepts' AND recorded_at <= ?1 \
               AND entity_id IN ('n0','n1','n2','n3','n4')) WHERE rn = 1";
        for step in plan(&conn, hydrate_sql, vec![text(&at)]).await {
            println!("      hydrate  {step}");
        }
        if index {
            conn.execute("DROP INDEX idx_txlog_fold_partition", ())
                .await
                .unwrap();
        }
    }

    db.close().await.unwrap();
}

#[tokio::main]
async fn main() {
    let a = args();
    if a.arm == "other-folds" {
        other_folds(&a).await;
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // No cadence: a background snapshot would change which fold runs partway
    // through the measurement, and the anchored form is measured deliberately
    // below rather than by accident.
    let db = Database::open_with_cadence(dir.path().join("probe.db"), None)
        .await
        .unwrap();

    println!(
        "fixture: {} entities x {} revisions on {} lineages",
        a.entities, a.revisions, a.branches
    );
    populate(&db, &a).await;
    db.analyze().await.unwrap();

    let conn = db.raw().connect().unwrap();

    // **The rung this probe recommended has shipped, so the fixture arrives
    // with the answer already in it.** `ddl::CREATE_INDICES` declares
    // `idx_txlog_fold_partition` as of 0.15.12, and a sweep run against a
    // database that has it reports every shape as making no difference —
    // because the winning shape is already there and the planner keeps it. The
    // run that first showed this printed
    // `SCAN transaction_log USING INDEX idx_txlog_fold_partition` under the
    // heading "no index (today)", which is as clear a statement of the mistake
    // as the tool can make.
    //
    // Dropped rather than the probe being retired: the numbers below are what
    // the register cites, and a measurement nobody can re-run is a number
    // nobody can question.
    conn.execute("DROP INDEX IF EXISTS idx_txlog_fold_partition", ())
        .await
        .unwrap();

    let rows = scalar(&conn, "SELECT COUNT(*) FROM transaction_log").await;
    let parts = scalar(
        &conn,
        "SELECT COUNT(*) FROM (SELECT 1 FROM transaction_log \
         GROUP BY table_name, entity_id, branch_id)",
    )
    .await;
    println!(
        "  transaction_log: {rows} rows, {parts} partitions, {:.1} rows per partition\n",
        rows as f64 / parts as f64
    );

    println!("== the plan today ==");
    for (label, sql, params) in [
        ("hot fold", HOT_FOLD, vec![text(FUTURE)]),
        (
            "anchored hot fold",
            ANCHORED_HOT_FOLD,
            vec![text(FUTURE), libsql::Value::Integer(0)],
        ),
    ] {
        println!("  {label}");
        for step in plan(&conn, sql, params).await {
            println!("    {step}");
        }
    }

    println!(
        "\n== ms over {} runs: the fold's SQL alone, then reconstruct() end to end ==",
        a.iterations
    );
    println!(
        "  {:<32} {:>8} {:>8}   {:>8} {:>8}   {:>8}",
        "", "sql best", "mean", "e2e best", "mean", "size"
    );
    let (sb, sm) = time_sql(&conn, HOT_FOLD, a.iterations).await;
    let (rb, rm) = time_reconstruct(&db, a.iterations).await;
    let base_pages = used_pages(&conn).await;
    println!(
        "  {:<32} {sb:8.3} {sm:8.3}   {rb:8.3} {rm:8.3}   {base_pages:5} pages",
        "no index (today)"
    );

    for (label, columns) in SHAPES {
        conn.execute(
            &format!("CREATE INDEX idx_probe ON transaction_log {columns}"),
            (),
        )
        .await
        .unwrap();
        conn.execute("ANALYZE", ()).await.unwrap();
        let (sb, sm) = time_sql(&conn, HOT_FOLD, a.iterations).await;
        let (rb, rm) = time_reconstruct(&db, a.iterations).await;
        // Pages, because the covering shapes duplicate `payload` — the widest
        // column in the log — and a read that is 1.7x faster for a database
        // that is 2x larger is a trade rather than a win.
        let pages = used_pages(&conn).await;
        println!(
            "  {label:<32} {sb:8.3} {sm:8.3}   {rb:8.3} {rm:8.3}   {:+5.1}%",
            (pages - base_pages) as f64 / base_pages as f64 * 100.0
        );
        for step in plan(&conn, HOT_FOLD, vec![text(FUTURE)]).await {
            println!("      {step}");
        }
        conn.execute("DROP INDEX idx_probe", ()).await.unwrap();
        conn.execute("ANALYZE", ()).await.unwrap();
    }

    // **The baseline is re-measured immediately before every shape.** Measured
    // once at the top and reused, this comparison reported +7% for a second run
    // of the identical baseline — the machine drifts over the minutes the sweep
    // takes, and a single baseline attributes that drift to whichever index
    // happened to be measured last. Paired runs cost twice as long and compare
    // two numbers taken seconds apart.
    // ── Collateral ────────────────────────────────────────────────────────
    println!("\n== what else reads the log, before and after ==");
    for (label, sql) in OTHER_READERS {
        println!("  {label}");
        for step in plan(&conn, sql, vec![text(FUTURE)]).await {
            println!("    before  {step}");
        }
        conn.execute(
            &format!("CREATE INDEX idx_probe ON transaction_log {WINNER}"),
            (),
        )
        .await
        .unwrap();
        conn.execute("ANALYZE", ()).await.unwrap();
        for step in plan(&conn, sql, vec![text(FUTURE)]).await {
            println!("    after   {step}");
        }
        conn.execute("DROP INDEX idx_probe", ()).await.unwrap();
        conn.execute("ANALYZE", ()).await.unwrap();
    }

    // ── The cold arm ──────────────────────────────────────────────────────
    //
    // Once a database has been archived the fold reads a UNION ALL of two
    // files, and a union is a co-routine: it has no index of its own and the
    // window has to sort what comes out of it. Whether an index on either side
    // can reach through that is a schema question — the cold file mirrors the
    // hot indexes today, and adding one there without a reader is exactly the
    // failure D-089 named.
    println!("\n== the cold fold, once the ledger has been archived ==");
    let cold_path = dir.path().join("cold.db");
    conn.execute(
        &format!("ATTACH DATABASE '{}' AS cold", cold_path.display()),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cold.transaction_log (             seq_id INTEGER PRIMARY KEY, table_name TEXT NOT NULL,             entity_id TEXT NOT NULL, operation TEXT NOT NULL,             payload TEXT NOT NULL, recorded_at TEXT NOT NULL,             branch_id TEXT NOT NULL DEFAULT 'main')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO cold.transaction_log          SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id          FROM main.transaction_log",
        (),
    )
    .await
    .unwrap();

    for (label, hot, cold) in [
        ("neither side indexed", false, false),
        ("hot only (what the rung would ship)", true, false),
        ("both sides indexed", true, true),
    ] {
        if hot {
            conn.execute(
                &format!("CREATE INDEX IF NOT EXISTS main.idx_probe ON transaction_log {WINNER}"),
                (),
            )
            .await
            .unwrap();
        }
        if cold {
            conn.execute(
                &format!(
                    "CREATE INDEX IF NOT EXISTS cold.idx_probe_cold ON transaction_log {WINNER}"
                ),
                (),
            )
            .await
            .unwrap();
        }
        conn.execute("ANALYZE", ()).await.unwrap();
        println!("  {label}");
        for step in plan(&conn, COLD_FOLD, vec![text(FUTURE)]).await {
            println!("    {step}");
        }
        let (sb, sm) = time_sql(&conn, COLD_FOLD, a.iterations).await;
        println!("    {sb:8.3} best  {sm:8.3} mean");
        conn.execute("DROP INDEX IF EXISTS idx_probe", ())
            .await
            .unwrap();
        conn.execute("DROP INDEX IF EXISTS cold.idx_probe_cold", ())
            .await
            .unwrap();
    }
    conn.execute("DETACH DATABASE cold", ()).await.unwrap();

    println!("\n== the write side: {WRITES} assertions on a fresh database, ms ==");
    println!("  {:<32} {:>8} {:>8}", "", "no index", "with");
    for (label, columns) in SHAPES {
        let base = best_writes(None, WRITES).await;
        let with_index = best_writes(Some(columns), WRITES).await;
        println!(
            "  {label:<32} {base:8.1} {with_index:8.1}   {:+.1}%",
            (with_index - base) / base * 100.0
        );
    }

    other_folds(&a).await;

    db.close().await.unwrap();
}
