mod harness;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::integrity::{audit_current, rebuild_current};
use macrame::schema::migrations;

#[tokio::test]
async fn test_schema_initialization_and_version() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();

    migrations::run(&conn).await.unwrap();

    let version: u32 = conn
        .query("PRAGMA user_version", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();

    assert_eq!(version, macrame::schema::SCHEMA_VERSION);
}

#[tokio::test]
async fn test_trg_links_current_sync() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // Insert concepts
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Node 1', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'Node 2', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    // Insert link assertion
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    // Verify automatic sync into links_current
    let mut rows = conn
        .query(
            "SELECT source_id, target_id, edge_type FROM links_current WHERE source_id = 'c1'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let src: String = row.get(0).unwrap();
    let tgt: String = row.get(1).unwrap();
    let edge_type: String = row.get(2).unwrap();

    assert_eq!(src, "c1");
    assert_eq!(tgt, "c2");
    assert_eq!(edge_type, "KNOWS");

    // Verify transaction_log entry
    let log_count: i64 = conn
        .query("SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(log_count, 1);
}

#[tokio::test]
async fn test_trg_links_single_open_violation() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Node 1', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'Node 2', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // First open interval
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Second open interval with distinct valid_from must be rejected by trg_links_single_open
    let res = conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-02-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-02-01T00:00:00.000000Z')",
        (),
    ).await;

    assert!(res.is_err());
    let err_str = res.err().unwrap().to_string();
    assert!(err_str.contains("open interval"), "Expected single open interval error, got: {err_str}");
}

#[tokio::test]
async fn test_trg_concepts_monotonic_ra_violation() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Original Title', '2026-01-01T00:00:00.000000Z', '2026-01-01T12:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Attempt non-advancing recorded_at update
    let res = conn.execute(
        "UPDATE concepts SET title = 'Updated Title', recorded_at = '2026-01-01T10:00:00.000000Z' WHERE id = 'c1'",
        (),
    ).await;

    assert!(res.is_err());
    let err_str = res.err().unwrap().to_string();
    assert!(err_str.contains("strictly increasing"), "Expected monotonic recorded_at error, got: {err_str}");
}

#[tokio::test]
async fn test_delete_guard_triggers() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Title', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Concepts are never physically archived (D-022): the guard is unconditional.
    let res = conn.execute("DELETE FROM concepts WHERE id = 'c1'", ()).await;
    assert!(res.is_err());
    let err_str = res.err().unwrap().to_string();
    assert!(
        err_str.contains("never physically archived"),
        "Expected unconditional concepts delete guard, got: {err_str}"
    );

    // BEFORE DELETE is a *row* trigger: it cannot fire on an empty table, so
    // both guarded tables need at least one row for this to prove anything.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'Title', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // links and transaction_log are guarded by the archive-session marker.
    for table in ["links", "transaction_log"] {
        let res = conn.execute(&format!("DELETE FROM {table}"), ()).await;
        assert!(res.is_err(), "{table} delete should be blocked");
        let err_str = res.err().unwrap().to_string();
        assert!(
            err_str.contains("outside archive session"),
            "Expected archive-session guard on {table}, got: {err_str}"
        );
    }
}

/// D-008 (revised): the marker created inside the archive transaction unlocks
/// the guards, is invisible to other connections, and cannot outlive the
/// transaction.
#[tokio::test]
async fn test_archive_session_marker_lifecycle() {
    use libsql::TransactionBehavior;

    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '2026-02-01T00:00:00.000000Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Blocked before the session opens.
    assert!(conn.execute("DELETE FROM links", ()).await.is_err());

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    tx.execute("CREATE TABLE macrame_archive_session (x)", ()).await.unwrap();

    // Permitted inside the session.
    tx.execute("DELETE FROM links", ()).await.unwrap();

    // Invisible to a second connection while uncommitted.
    let db2 = libsql::Builder::new_local(&harness.db_path).build().await.unwrap();
    let conn2 = db2.connect().unwrap();
    let seen: i64 = conn2
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
            (),
        )
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(seen, 0, "uncommitted marker must not be visible to other connections");

    tx.execute("DROP TABLE macrame_archive_session", ()).await.unwrap();
    tx.commit().await.unwrap();

    // Re-armed after commit, and the marker is not committed state.
    let after: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
            (),
        )
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(after, 0);

    // Re-populate: BEFORE DELETE cannot fire on an empty table.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-03-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-03-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    assert!(conn.execute("DELETE FROM links", ()).await.is_err());
}

/// A rolled-back archive transaction must leave the guards armed.
#[tokio::test]
async fn test_archive_session_marker_rollback_rearms_guard() {
    use libsql::TransactionBehavior;

    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // Give transaction_log a row, so the BEFORE DELETE row trigger can fire.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    tx.execute("CREATE TABLE macrame_archive_session (x)", ()).await.unwrap();
    tx.rollback().await.unwrap();

    let after: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
            (),
        )
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(after, 0, "rollback must discard the marker");
    assert!(conn.execute("DELETE FROM transaction_log", ()).await.is_err());
}

#[tokio::test]
async fn test_audit_and_rebuild_current() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Node 1', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'Node 2', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Verify zero drift initially
    let initial_drift = audit_current(&conn).await.unwrap();
    assert_eq!(initial_drift, 0);

    // Inject artificial corruption into links_current
    conn.execute(
        "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'CORRUPT', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // Audit must detect drift
    let audit_res = audit_current(&conn).await;
    assert!(audit_res.is_err());
    if let Err(DbError::CurrentDrift { n }) = audit_res {
        assert_eq!(n, 1);
    } else {
        panic!("Expected CurrentDrift error");
    }

    // Rebuild links_current
    let report = rebuild_current(&conn).await.unwrap();
    assert_eq!(report.drift_after, 0);

    // Audit must return 0 drift after rebuild
    let post_drift = audit_current(&conn).await.unwrap();
    assert_eq!(post_drift, 0);
}

/// Drift is a *symmetric* difference. The pre-0.5.4 audit chained
/// `EXCEPT`/`UNION` flatly, which SQLite parses left-associatively into
/// `A EXCEPT A` — a constant zero. It therefore certified any corruption as
/// clean. This exercises both directions independently and together, so a
/// regression to a one-sided or degenerate query cannot pass.
#[tokio::test]
async fn test_audit_detects_drift_in_both_directions() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    for id in ["c1", "c2"] {
        conn.execute(
            &format!("INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('{id}', 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')"),
            (),
        ).await.unwrap();
    }
    for (etype, ra) in [("A", "2026-01-01T00:00:00.000000Z"), ("B", "2026-01-02T00:00:00.000000Z")] {
        conn.execute(
            &format!("INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
                      VALUES ('c1', 'c2', '{etype}', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{{}}', '{ra}')"),
            (),
        ).await.unwrap();
    }
    assert_eq!(audit_current(&conn).await.unwrap(), 0);

    let drift_count = |res: macrame::Result<usize>| match res {
        Err(DbError::CurrentDrift { n }) => n,
        other => panic!("expected CurrentDrift, got {other:?}"),
    };

    // Direction 1: links_current is MISSING a row the projection has.
    // A one-sided `materialized EXCEPT projection` audit reports 0 here.
    conn.execute("DELETE FROM links_current WHERE edge_type = 'A'", ())
        .await
        .unwrap();
    assert_eq!(drift_count(audit_current(&conn).await), 1, "missed materialisation must be drift");

    // Direction 2: links_current ALSO has a row the projection does not.
    // Both halves are now non-empty, so the count must be their sum -- catching
    // a formulation that returns only one side.
    conn.execute(
        "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'GHOST', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    assert_eq!(drift_count(audit_current(&conn).await), 2, "both directions must be summed");

    // A stale row -- right key, wrong payload -- counts once in each direction,
    // because it is simultaneously a row the projection lacks and one it wants.
    conn.execute("DELETE FROM links_current WHERE edge_type = 'GHOST'", ()).await.unwrap();
    conn.execute("UPDATE links_current SET weight = 99.0 WHERE edge_type = 'B'", ()).await.unwrap();
    assert_eq!(drift_count(audit_current(&conn).await), 3);

    rebuild_current(&conn).await.unwrap();
    assert_eq!(audit_current(&conn).await.unwrap(), 0, "rebuild must clear both directions");
}

/// §4.1: every temporal column is exactly `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// Enforced by CHECK rather than by convention, because the failure mode of a
/// mixed-precision column is silence: `'...T00:00:00Z' <= '...T00:00:00.000000Z'`
/// is FALSE, so a traversal predicated on it returns an empty set with no error.
#[tokio::test]
async fn test_non_canonical_timestamps_are_rejected_at_write_time() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // The exact literal that used to be written freely, and that silently
    // mis-ordered against every microsecond-precision stamp it was compared to.
    for bad in [
        "2026-01-01T00:00:00Z",            // second precision
        "2026-01-01T00:00:00.000Z",        // milliseconds
        "2026-01-01T00:00:00.000000",      // no zone
        "2026-01-01T00:00:00.000000+01:00", // offset
        "2026-01-01 00:00:00.000000Z",     // space separator
        "not-a-timestamp",
    ] {
        let res = conn
            .execute(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('x', 'N', ?1, '2026-01-01T00:00:00.000000Z')",
                libsql::params![bad],
            )
            .await;
        assert!(res.is_err(), "CHECK must reject valid_from = {bad:?}");
    }

    // The canonical form is accepted, and so is the canonical open sentinel.
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, valid_to, recorded_at) \
         VALUES ('ok', 'N', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .expect("canonical timestamps must be accepted");

    // links and transaction_log carry the same constraint.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('ok', 'ok', 'SELF', '2026-01-01T00:00:00Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .expect_err("links.valid_from must reject second precision");
}

/// The regression that made the precision defect concrete: a valid-time
/// predicate must match an edge asserted at the same instant.
#[tokio::test]
async fn test_valid_time_predicate_matches_edge_at_same_instant() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    let ts = "2026-01-01T00:00:00.000000Z";
    for id in ["c1", "c2"] {
        conn.execute(
            &format!("INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('{id}', 'N', '{ts}', '{ts}')"),
            (),
        ).await.unwrap();
    }
    conn.execute(
        &format!("INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
                  VALUES ('c1', 'c2', 'KNOWS', '{ts}', '9999-12-31T23:59:59.999999Z', 1.0, '{{}}', '{ts}')"),
        (),
    ).await.unwrap();

    // Half-open [valid_from, valid_to): the edge is live exactly at valid_from.
    let edges = macrame::temporal::query_as_of_edges(&conn, ts).await.unwrap();
    assert_eq!(edges.len(), 1, "edge must be live at its own valid_from");
}
