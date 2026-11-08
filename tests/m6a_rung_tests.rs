//! M6-a's rung tests (0.22.0, [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.1, revision 3).
//!
//! What the integration suite as a whole cannot answer about the *rung*:
//! that the two overlay tables exist at the version the engine stamps, that
//! their columns are §5.1's (no `kind`, the collision proof revision 3
//! recorded), that a cut and a version genuinely can share one
//! `(id, branch_id, valid_from)` start, and that the three triggers each
//! table carries — the log mirror (D-056's rule, one payload for both
//! writers), the single-open audit on versions, and the provenance guards —
//! fire at the database level, where the write actor is not present to
//! refuse first.
//!
//! The overlay's *routing* has its own file (`m6a_routing_tests.rs`); these
//! tests deliberately go through `raw` SQL so a future actor regression
//! cannot mask a schema break behind the actor's own refusals.
//!
//! [D-056]: ../docs/architecture/s13-decision-register.md#d-056

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::{BranchId, ConceptUpsert, Database};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-01-04T00:00:00.000000Z";
const T2: &str = "2026-01-09T00:00:00.000000Z";
const USAGE: &str = "the_history";

/// The trunk holds `{USAGE}` open from `T0`; `b0` forks after the mint, so
/// the id is minted on an ancestor and visible at the fork cutoff (§5.2's
/// W2-shape fixture), and every schema test below runs against a store that
/// already has something in it.
async fn minted_store(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new(USAGE, "the trunk's belief")
            .valid_from(T0)
            .valid_to("9999-12-31T23:59:59.999999Z"),
    )
    .await
    .unwrap();
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();
    db
}

/// The one-shot write handle for the schema tests, as `harness.rs`'s marks
/// fixture spells it: opened for the statements of one test, dropped before
/// any read runs.
async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

/// A column list, in declared order, from one `PRAGMA table_info`.
async fn columns(conn: &libsql::Connection, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info('{table}')"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(1).unwrap();
        out.push(name);
    }
    out
}

/// A scalar read against the store's write handle.
async fn scalar(conn: &libsql::Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    v
}

// ---------------------------------------------------------------------------
// The rung
// ---------------------------------------------------------------------------

/// §5.1's two tables exist at the version this build stamps, with the exact
/// column lists the sketch spells: `kind` is gone, and a cut and a version of
/// one id on one lineage are different rows in different tables — the
/// revision 3 finding, settled by the schema rather than widened into the
/// key.
#[tokio::test]
async fn v26_carries_the_overlay_tables_without_the_kind_column() {
    let harness = TestHarness::new();
    let db = minted_store(&harness).await;

    let conn = raw_conn(db.path()).await;
    let versions = columns(&conn, "concept_versions").await;
    let cuts = columns(&conn, "concept_cuts").await;
    drop(conn);

    assert_eq!(
        versions,
        vec![
            "id",
            "branch_id",
            "valid_from",
            "valid_to",
            "title",
            "content",
            "extra",
            "embedding_model",
            "recorded_at",
        ],
        "concept_versions' columns, in declared order"
    );
    assert_eq!(
        cuts,
        vec!["id", "branch_id", "cut_at", "recorded_at"],
        "concept_cuts' columns, in declared order"
    );

    let count = {
        let conn = raw_conn(db.path()).await;
        scalar(
            &conn,
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'table' AND name IN ('concept_versions', 'concept_cuts')",
        )
        .await
    };
    assert_eq!(count, 2, "both overlay tables exist");
}

/// The revision 3 point: the cut and the version of one supersede are **two
/// rows in two tables**, not a `kind` widening of one key. Both start at the
/// supersede instant; the version later retires (its window ends) without
/// the cut moving.
#[tokio::test]
async fn a_cut_and_a_version_share_a_start_in_two_tables() {
    let harness = TestHarness::new();
    let db = minted_store(&harness).await;
    let conn = raw_conn(db.path()).await;

    conn.execute(
        "INSERT INTO concept_versions \
         (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
         VALUES (?1, 'b0', ?2, ?3, 'v', '', '{}', ?4)",
        libsql::params![
            USAGE,
            T1,
            "9999-12-31T23:59:59.999999Z",
            "2026-01-10T00:00:00.000000Z"
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO concept_cuts \
         (id, branch_id, cut_at, recorded_at) \
         VALUES (?1, 'b0', ?2, ?3)",
        libsql::params![USAGE, T1, "2026-01-10T00:00:00.000000Z"],
    )
    .await
    .unwrap();
    drop(conn);

    let check = {
        let conn = raw_conn(db.path()).await;
        (
            scalar(&conn, "SELECT COUNT(*) FROM concept_versions").await,
            scalar(&conn, "SELECT COUNT(*) FROM concept_cuts").await,
        )
    };
    assert_eq!(
        check,
        (1, 1),
        "cut [T1, open) and version [T1, …) both stored"
    );
}

/// A second version row whose window overlaps the existing one is refused
/// at the database level — one belief per instant per `(id, branch_id)` is
/// D-291's invariant spelled by a trigger, exactly as `links` has carried
/// one since v18. From v29 the trigger checks the interval, not an
/// open-ended window: a bounded row written under an open one is the same
/// overlap the write actor's guard refuses (0.22.0's post-review fix, the
/// confirmed finding — the sentence "a named end is not an open interval",
/// which had stood here, was the hole's own docstring). What stays admitted
/// is the window that does not share an instant with what stands: a row
/// that ends where an open row begins is adjacent, and the same-start
/// rewrite is the D-291 heal.
#[tokio::test]
async fn the_versions_single_open_trigger_refuses() {
    let harness = TestHarness::new();
    let db = minted_store(&harness).await;
    let conn = raw_conn(db.path()).await;

    conn.execute(
        "INSERT INTO concept_versions \
         (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
         VALUES (?1, 'b0', ?2, '9999-12-31T23:59:59.999999Z', '', '', '{}', ?3)",
        libsql::params![USAGE, T1, "2026-01-10T00:00:00.001000Z"],
    )
    .await
    .unwrap();

    let refused = conn
        .execute(
            "INSERT INTO concept_versions \
             (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
             VALUES (?1, 'b0', ?2, '9999-12-31T23:59:59.999999Z', '', '', '{}', ?3)",
            libsql::params![USAGE, T2, "2026-01-11T00:00:00.000000Z"],
        )
        .await;
    match refused {
        Ok(_) => panic!("an open interval under an open one was stored"),
        Err(e) => {
            let m = e.to_string();
            assert!(
                m.contains("already has an open interval"),
                "the refusal is the single-open sentence, not engine text: {m}"
            );
        }
    }

    // The review's confirmed shape, now at the database level too: a
    // bounded row overlapping the open one is the same overlap.
    let bounded_refused = conn
        .execute(
            "INSERT INTO concept_versions \
             (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
             VALUES (?1, 'b0', ?2, ?3, '', '', '{}', ?4)",
            libsql::params![
                USAGE,
                T2,
                "2026-01-14T00:00:00.000000Z",
                "2026-01-11T00:00:00.000000Z"
            ],
        )
        .await;
    match bounded_refused {
        Ok(_) => panic!("a bounded row under an open one was stored"),
        Err(e) => {
            let m = e.to_string();
            assert!(
                m.contains("already has an open interval"),
                "the widened trigger speaks the same sentence: {m}"
            );
        }
    }

    // And the window that shares no instant with what stands is admitted:
    // the pre-open-window row ends exactly where the open row begins.
    let ok = conn
        .execute(
            "INSERT INTO concept_versions \
             (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
             VALUES (?1, 'b0', ?2, ?3, '', '', '{}', ?4)",
            libsql::params![
                USAGE,
                "2026-01-02T00:00:00.000000Z",
                "2026-01-04T00:00:00.000000Z",
                "2026-01-12T00:00:00.000000Z"
            ],
        )
        .await;
    assert!(ok.is_ok(), "an adjacent closed row is not an overlap");

    drop(conn);
}

/// `branch_id` is provenance on the new tables too, and rows are not
/// physically removable outside an archive session. Both mirrors of the
/// guards `concepts` has carried since v9/v12 fire here.
#[tokio::test]
async fn the_provenance_guards_cover_both_overlay_tables() {
    let harness = TestHarness::new();
    let db = minted_store(&harness).await;

    for table in ["concept_versions", "concept_cuts"] {
        let conn = raw_conn(db.path()).await;
        match table {
            "concept_versions" => {
                conn.execute(
                    "INSERT INTO concept_versions \
                     (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
                     VALUES (?1, 'b0', ?2, '9999-12-31T23:59:59.999999Z', '', '', '{}', ?3)",
                    libsql::params![USAGE, T1, "2026-01-10T00:00:00.000000Z"],
                )
                .await
                .unwrap();
            }
            _ => {
                conn.execute(
                    "INSERT INTO concept_cuts (id, branch_id, cut_at, recorded_at) \
                     VALUES (?1, 'b0', ?2, ?3)",
                    libsql::params![USAGE, T1, "2026-01-10T00:00:00.000000Z"],
                )
                .await
                .unwrap();
            }
        }

        let moved = conn
            .execute(
                &format!("UPDATE {table} SET branch_id = 'main' WHERE branch_id = 'b0'"),
                (),
            )
            .await;
        assert!(
            moved.is_err(),
            "a branch_id move on {table} was not refused"
        );

        let deleted = conn
            .execute(&format!("DELETE FROM {table} WHERE branch_id = 'b0'"), ())
            .await;
        assert!(
            deleted.is_err(),
            "a physical delete on {table} was not refused"
        );
        drop(conn);
    }
}

/// The log mirrors: a row in either overlay table keeps its ledger row —
/// the same `AFTER INSERT` shape `concepts` carries, one payload per table
/// (D-056: the fold and the trigger cannot drift). The archive-session
/// gate is part of the same rule; these are the ungated writes' mirror.
#[tokio::test]
async fn the_log_mirror_covers_both_overlay_tables() {
    let harness = TestHarness::new();
    let db = minted_store(&harness).await;
    let conn = raw_conn(db.path()).await;

    conn.execute(
        "INSERT INTO concept_versions \
         (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
         VALUES (?1, 'b0', ?2, '9999-12-31T23:59:59.999999Z', '', '', '{}', ?3)",
        libsql::params![USAGE, T1, "2026-01-10T00:00:00.000000Z"],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO concept_cuts (id, branch_id, cut_at, recorded_at) \
         VALUES (?1, 'b0', ?2, ?3)",
        libsql::params![USAGE, T1, "2026-01-10T00:00:00.000000Z"],
    )
    .await
    .unwrap();
    drop(conn);

    let check: Option<(i64,)> = {
        let conn = raw_conn(db.path()).await;
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM transaction_log \
                 WHERE table_name IN ('concept_versions', 'concept_cuts') AND entity_id = ?1",
                libsql::params![USAGE],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap();
        row.map(|r| (r.get(0).unwrap(),))
    };
    let Some((n,)) = check else {
        panic!("the log read answered nothing");
    };
    assert_eq!(n, 2, "each overlay row logged once, one per table");
}
