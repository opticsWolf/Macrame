//! Executable form of the 1.0 on-disk compatibility contract (D-036).
//!
//! The contract bifurcates the schema: the ledger tables (`concepts`, `links`,
//! `transaction_log`) are frozen and may only be extended additively, while
//! derivative tables (`links_current`, and the per-model embedding tables when
//! they land) are explicitly disposable and rebuilt during migration.
//!
//! Both halves rest on mechanical premises that are easy to assume and were, in
//! fact, worth checking. These tests check them. A compatibility contract whose
//! feasibility has never been executed is a plan, not a contract.

#[path = "common/harness.rs"]
mod harness;

use std::collections::BTreeSet;

use harness::TestHarness;
use macrame::integrity::{audit_current, rebuild_current};
use macrame::schema::ddl;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const SENTINEL: &str = "9999-12-31T23:59:59.999999Z";

async fn seeded(harness: &TestHarness) -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    macrame::schema::run_migrations(&conn).await.unwrap();
    for id in ["c0", "c1"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    for (etype, vt, ra) in [
        ("A", SENTINEL, TS),
        ("B", "2026-03-01T00:00:00.000000Z", TS),
        (
            "B",
            "2026-04-01T00:00:00.000000Z",
            "2026-02-01T00:00:00.000000Z",
        ),
    ] {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES ('c0','c1',?1,?2,?3,1.0,'{}',?4)",
            libsql::params![etype, TS, vt, ra],
        )
        .await
        .unwrap();
    }
    (db, conn)
}

/// The structural shape of one table: column name, declared type, NOT NULL, and
/// primary-key position. `PRAGMA table_info` rather than the `sql` text from
/// `sqlite_master`, so reformatting the DDL is not a contract breach but
/// renaming a column is.
async fn shape(conn: &libsql::Connection, table: &str) -> Vec<(String, String, i64, i64)> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((
            r.get(1).unwrap(),
            r.get(2).unwrap(),
            r.get(3).unwrap(),
            r.get(5).unwrap(),
        ));
    }
    out
}

/// **The frozen core, pinned.**
///
/// Post-1.0 the ledger tables may gain columns and indexes and nothing else:
/// changing a primary key or dropping a column requires a major version and a
/// purpose-built ETL, because bitemporal data cannot be migrated by
/// `CREATE TABLE AS SELECT` — you would have to replay history and re-resolve
/// conflicting assertions.
///
/// This test is what makes that a contract rather than a paragraph. Pre-1.0 it
/// is a change-detector: if it fails, you changed the ledger shape, which is
/// still allowed and should be a deliberate edit to this list. **At 1.0 the
/// recorded shape freezes and a failure here becomes a release blocker.**
///
/// Additive changes are represented honestly: a new column appends an entry and
/// leaves every existing one untouched, which is exactly the diff the contract
/// permits.
#[tokio::test]
async fn the_ledger_tables_have_the_shape_the_contract_freezes() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;

    assert_eq!(
        shape(&conn, "links").await,
        vec![
            ("source_id".into(), "TEXT".into(), 1, 1),
            ("target_id".into(), "TEXT".into(), 1, 2),
            ("edge_type".into(), "TEXT".into(), 1, 3),
            ("valid_from".into(), "TEXT".into(), 1, 4),
            ("recorded_at".into(), "TEXT".into(), 1, 5),
            ("valid_to".into(), "TEXT".into(), 1, 0),
            ("weight".into(), "REAL".into(), 1, 0),
            ("properties".into(), "TEXT".into(), 1, 0),
        ],
        "links is a frozen ledger table (D-003: the 5-column bitemporal PK)"
    );

    // **This entry records a primary-key change, and that is the point of it
    // being here.** Through v7 `concepts` was `id TEXT PRIMARY KEY` — the fourth
    // field below was `1` on `id` and there was no `rowid_pk`. v8 made the rowid
    // an explicit column so `VACUUM` cannot renumber it out from under
    // `concepts_fts` (D-119), which costs `id` the primary key, because SQLite
    // permits one per table.
    //
    // That is exactly the diff this contract forbids **after 1.0**: not additive,
    // not migratable by `CREATE TABLE AS SELECT` in the general case. Pre-1.0 it
    // is a deliberate edit to this list, which is what it is. It is also the last
    // opportunity — D-036 names a primary-key change as requiring a major version
    // and a purpose-built ETL, so if this had waited for 0.9.0's archival to make
    // the hazard visible, the fix would have arrived after the door closed.
    assert_eq!(
        shape(&conn, "concepts").await,
        vec![
            ("rowid_pk".into(), "INTEGER".into(), 0, 1),
            ("id".into(), "TEXT".into(), 1, 0),
            ("title".into(), "TEXT".into(), 1, 0),
            ("content".into(), "TEXT".into(), 1, 0),
            ("embedding_model".into(), "TEXT".into(), 0, 0),
            ("valid_from".into(), "TEXT".into(), 1, 0),
            ("valid_to".into(), "TEXT".into(), 1, 0),
            ("recorded_at".into(), "TEXT".into(), 1, 0),
            ("retired".into(), "INTEGER".into(), 1, 0),
        ],
        "concepts is a frozen ledger table"
    );

    assert_eq!(
        shape(&conn, "transaction_log").await,
        vec![
            ("seq_id".into(), "INTEGER".into(), 0, 1),
            ("table_name".into(), "TEXT".into(), 1, 0),
            ("entity_id".into(), "TEXT".into(), 1, 0),
            ("operation".into(), "TEXT".into(), 1, 0),
            ("payload".into(), "TEXT".into(), 1, 0),
            ("recorded_at".into(), "TEXT".into(), 1, 0),
        ],
        "transaction_log is a frozen ledger table (D-002)"
    );
}

/// **The additive escape hatch is real, and D-029 survives it.**
///
/// The contract permits `ALTER TABLE ADD COLUMN` post-1.0. For a temporal
/// column that is only acceptable if the canonical-form guarantee travels with
/// it — and SQLite has no "add table constraint", so the table-level
/// `canonical_ts_check!` that covers today's columns **cannot be extended**.
/// The whole additive story therefore depends on `ADD COLUMN` accepting a
/// *column-level* CHECK, which it does, and enforcing it, which it also does.
///
/// Verified rather than assumed, because if it were false the contract would
/// silently permit a post-1.0 temporal column with no canonical-form guard —
/// reintroducing the mixed-precision ordering defect D-029 exists to kill, on a
/// schema nobody is allowed to rebuild.
#[tokio::test]
async fn an_added_temporal_column_can_still_carry_its_canonical_check() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;

    conn.execute(
        &format!(
            "ALTER TABLE links ADD COLUMN asserted_at TEXT NOT NULL DEFAULT '{TS}' \
             CHECK (asserted_at GLOB {})",
            macrame::util::timestamp::CANONICAL_TS_GLOB
        ),
        (),
    )
    .await
    .expect("ADD COLUMN must accept a column-level CHECK");

    let triggers: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'",
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
    assert_eq!(
        triggers as usize,
        ddl::CREATE_TRIGGERS.len(),
        "ADD COLUMN must not disturb the guards"
    );

    let bad = conn
        .execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, \
             properties, recorded_at, asserted_at) \
             VALUES ('c0','c1','C',?1,?2,1.0,'{}',?1,'2026-01-01T00:00:00Z')",
            libsql::params![TS, SENTINEL],
        )
        .await;
    assert!(
        bad.is_err(),
        "the added column's CHECK must reject non-canonical timestamps"
    );
}

/// **The disposable periphery is genuinely disposable.**
///
/// The contract's other half says a future migration may simply `DROP TABLE
/// links_current` and let `rebuild_current()` recreate it. That is only true if
/// no information lives *solely* in the materialization — Doctrine VI's "there
/// is no third category," stated as an executable claim.
///
/// So: capture the table, destroy it entirely, recreate it from the DDL alone,
/// rebuild from `links`, and require the result to be byte-identical to what
/// was destroyed. If a future column on `links_current` is ever populated from
/// anything but `links`, this fails, and the contract has to change rather than
/// quietly becoming false.
#[tokio::test]
async fn dropping_and_rebuilding_the_materialization_loses_nothing() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;

    let row = "source_id||'|'||target_id||'|'||edge_type||'|'||valid_from||'|'||valid_to\
               ||'|'||weight||'|'||properties||'|'||recorded_at";
    let capture = |c: &libsql::Connection| {
        let c = c.clone();
        async move {
            let mut rows = c
                .query(&format!("SELECT {row} FROM links_current"), ())
                .await
                .unwrap();
            let mut out = BTreeSet::new();
            while let Some(r) = rows.next().await.unwrap() {
                out.insert(r.get::<String>(0).unwrap());
            }
            out
        }
    };

    let before = capture(&conn).await;
    assert!(
        !before.is_empty(),
        "the fixture must have something to lose"
    );
    assert_eq!(audit_current(&conn).await.unwrap(), 0);

    // A migration's-eye view: the derivative table ceases to exist.
    conn.execute("DROP TABLE links_current", ()).await.unwrap();
    conn.execute(ddl::CREATE_LINKS_CURRENT_TABLE, ())
        .await
        .unwrap();
    assert!(capture(&conn).await.is_empty());

    let report = rebuild_current(&conn).await.unwrap();

    assert_eq!(capture(&conn).await, before, "the rebuild must be lossless");
    assert_eq!(report.rows_rebuilt, before.len());
    assert_eq!(report.drift_after, 0);
    assert_eq!(audit_current(&conn).await.unwrap(), 0);
}

/// The sync trigger lives on `links`, not on `links_current`, so dropping the
/// derivative table takes no guard with it — which is what makes the
/// drop-and-rebuild migration strategy safe to run inside a step transaction.
///
/// Stated as a test because the opposite arrangement is the natural one to
/// reach for, and it would make the periphery quietly non-disposable: dropping
/// the table would silently disarm the mechanism that keeps it correct.
#[tokio::test]
async fn no_guard_is_attached_to_the_disposable_table() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'links_current'",
            (),
        )
        .await
        .unwrap();
    let mut attached = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        attached.push(r.get::<String>(0).unwrap());
    }
    assert!(
        attached.is_empty(),
        "triggers on the disposable table would not survive a migration DROP: {attached:?}"
    );
}
