#[path = "common/harness.rs"]
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
        .query(
            "SELECT COUNT(*) FROM transaction_log WHERE table_name = 'links'",
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
    assert!(
        err_str.contains("open interval"),
        "Expected single open interval error, got: {err_str}"
    );
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
    assert!(
        err_str.contains("strictly increasing"),
        "Expected monotonic recorded_at error, got: {err_str}"
    );
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

    // The concepts guard is **marker-gated** as of v9 (C2, D-126), matching its
    // two siblings. This assertion used to read `never physically archived` and
    // pin the guard as unconditional, which is what made it the line C2 had to
    // rewrite: an ad-hoc delete is still refused, and that is the property worth
    // keeping, but it is refused for the same reason `links` is rather than
    // because concept archival is impossible.
    let res = conn
        .execute("DELETE FROM concepts WHERE id = 'c1'", ())
        .await;
    assert!(res.is_err());
    let err_str = res.err().unwrap().to_string();
    assert!(
        err_str.contains(macrame::schema::ddl::ABORT_DELETE_GUARD),
        "Expected the marker-gated concepts delete guard, got: {err_str}"
    );
    assert!(
        !err_str.contains("never physically archived"),
        "The v8 guard body survived into v9 — a re-issued baseline keeps the old \
         body and only the rung replaces it (D-126). Got: {err_str}"
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
    tx.execute("CREATE TABLE macrame_archive_session (x)", ())
        .await
        .unwrap();

    // Permitted inside the session.
    tx.execute("DELETE FROM links", ()).await.unwrap();

    // Invisible to a second connection while uncommitted.
    let db2 = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn2 = db2.connect().unwrap();
    let seen: i64 = conn2
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
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
        seen, 0,
        "uncommitted marker must not be visible to other connections"
    );

    tx.execute("DROP TABLE macrame_archive_session", ())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Re-armed after commit, and the marker is not committed state.
    let after: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
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
    tx.execute("CREATE TABLE macrame_archive_session (x)", ())
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let after: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'macrame_archive_session'",
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
    assert_eq!(after, 0, "rollback must discard the marker");
    assert!(conn
        .execute("DELETE FROM transaction_log", ())
        .await
        .is_err());
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
    for (etype, ra) in [
        ("A", "2026-01-01T00:00:00.000000Z"),
        ("B", "2026-01-02T00:00:00.000000Z"),
    ] {
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
    assert_eq!(
        drift_count(audit_current(&conn).await),
        1,
        "missed materialisation must be drift"
    );

    // Direction 2: links_current ALSO has a row the projection does not.
    // Both halves are now non-empty, so the count must be their sum -- catching
    // a formulation that returns only one side.
    conn.execute(
        "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'GHOST', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();
    assert_eq!(
        drift_count(audit_current(&conn).await),
        2,
        "both directions must be summed"
    );

    // A stale row -- right key, wrong payload -- counts once in each direction,
    // because it is simultaneously a row the projection lacks and one it wants.
    conn.execute("DELETE FROM links_current WHERE edge_type = 'GHOST'", ())
        .await
        .unwrap();
    conn.execute(
        "UPDATE links_current SET weight = 99.0 WHERE edge_type = 'B'",
        (),
    )
    .await
    .unwrap();
    assert_eq!(drift_count(audit_current(&conn).await), 3);

    rebuild_current(&conn).await.unwrap();
    assert_eq!(
        audit_current(&conn).await.unwrap(),
        0,
        "rebuild must clear both directions"
    );
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
        "2026-01-01T00:00:00Z",             // second precision
        "2026-01-01T00:00:00.000Z",         // milliseconds
        "2026-01-01T00:00:00.000000",       // no zone
        "2026-01-01T00:00:00.000000+01:00", // offset
        "2026-01-01 00:00:00.000000Z",      // space separator
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
    let edges = macrame::temporal::query_as_of_edges(&conn, ts)
        .await
        .unwrap();
    assert_eq!(edges.len(), 1, "edge must be live at its own valid_from");
}

// ---------------------------------------------------------------------------
// T0.2 / D-077 — the archive no longer audits its own rebuild
// ---------------------------------------------------------------------------

/// `archive()` leaves `links_current` equal to the projection, **audited from
/// outside**.
///
/// D-077 stopped `archive()`'s internal `rebuild_within` from auditing itself:
/// with one definition of the projection (`LATEST_BELIEF_PROJECTION`) that check
/// compares a table against the query that just filled it, in the same
/// transaction, and it was two `EXCEPT` passes over the whole table under the
/// archive's write lock — measured at roughly **half** the entire repair.
///
/// The guarantee it used to provide has to come from somewhere, and this is the
/// somewhere: audit *after* the archive commits, from outside, which is a
/// stronger check than the tautological one it replaces. There is a property
/// test that does this over generated histories, but it lives behind the
/// `property-tests` feature — so plain `cargo test` would otherwise no longer
/// prove that an archive leaves a clean projection at all.
#[tokio::test]
async fn an_archive_leaves_no_drift_although_it_no_longer_checks_itself() {
    let harness = TestHarness::new();
    let conn = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    migrations::run(&conn).await.unwrap();

    let t0 = "2026-01-01T00:00:00.000000Z";
    let t1 = "2026-02-01T00:00:00.000000Z";
    let t2 = "2026-03-01T00:00:00.000000Z";
    let open = "9999-12-31T23:59:59.999999Z";

    for id in ["a", "b", "c"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'n', ?2, ?2)",
            libsql::params![id, t0],
        )
        .await
        .unwrap();
    }

    // Three shapes that must survive an archive differently: a superseded
    // assertion (archivable), the belief that superseded it (not), and a closed
    // interval old enough to go (archivable).
    let rows: [(&str, &str, &str, &str, &str); 3] = [
        ("a", "b", t0, open, t0),
        ("a", "b", t0, open, t1),
        ("b", "c", t0, t1, t0),
    ];
    for (src, tgt, vf, vt, rec) in rows {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) VALUES (?1, ?2, 'KNOWS', ?3, ?4, 1.0, '{}', ?5)",
            libsql::params![src, tgt, vf, vt, rec],
        )
        .await
        .unwrap();
    }

    assert_eq!(
        audit_current(&conn).await.unwrap(),
        0,
        "clean before archive"
    );

    let cold = harness.temp_dir.path().join("cold.db");
    let report = macrame::temporal::archive(&conn, t2, t2, &cold)
        .await
        .unwrap();
    assert!(
        report.links_archived > 0,
        "the fixture must actually archive something, or this proves nothing"
    );

    // The real assertion: the projection is intact, checked by the auditor the
    // rebuild is no longer allowed to call on itself.
    assert_eq!(
        audit_current(&conn).await.unwrap(),
        0,
        "archive left links_current drifted from the projection"
    );
}

// ---------------------------------------------------------------------------
// The marker as *committed* state: the disarm switch (0.10.0, W2)
// ---------------------------------------------------------------------------
//
// The tests above establish that the marker works — it gates the guards, it is
// invisible to other connections while uncommitted, and a rollback re-arms.
// These four cover the case none of them does: the table being present when no
// archive is running. While it is, all three delete guards are disarmed and
// `trg_concepts_log_insert` writes nothing, so Doctrine IV and Doctrine V are
// suspended together, with no error and no counter.
//
// Nothing checked for it before 0.10.0. The safety argument on record is about
// crashes and is correct about them; it says nothing about a writer that
// creates the table directly, and §4.7 concedes raw writers exist.

const W2_T0: &str = "2026-01-01T00:00:00.000000Z";
const W2_T1: &str = "2026-02-01T00:00:00.000000Z";
const W2_CUTOFF: &str = "2099-01-01T00:00:00.000000Z";

/// Seed one archivable edge, so an archive has real work to do.
async fn w2_seeded(path: &std::path::Path) -> macrame::Database {
    let db = macrame::Database::open(path).await.unwrap();
    for id in ["c1", "c2"] {
        db.upsert_concept(macrame::ConceptUpsert::new(id, "N").valid_from(W2_T0))
            .await
            .unwrap();
    }
    db.assert_edge(
        macrame::graph::EdgeAssertion::new("c1", "c2", "KNOWS")
            .valid_from(W2_T0)
            .valid_to(W2_T1),
    )
    .await
    .unwrap();
    db
}

/// Write the marker as committed state, the way a raw writer would.
async fn w2_leak_marker(db: &macrame::Database) {
    // Outside the actor and outside any transaction. `raw()` is
    // `#[doc(hidden)]` and provoking a guard is its documented legitimate use
    // (D-091).
    db.raw()
        .connect()
        .unwrap()
        .execute("CREATE TABLE macrame_archive_session (x)", ())
        .await
        .unwrap();
}

/// `Database` is not `Debug`, so `expect_err` is unavailable on an open.
async fn w2_open_err(path: &std::path::Path) -> DbError {
    match macrame::Database::open(path).await {
        Ok(_) => panic!("open must be refused while the marker is present"),
        Err(e) => e,
    }
}

/// Committed marker, fresh open, refused.
#[tokio::test]
async fn a_leaked_archive_session_marker_is_refused_at_open() {
    let harness = TestHarness::new();

    let db = macrame::Database::open(&harness.db_path).await.unwrap();
    w2_leak_marker(&db).await;
    db.close().await.unwrap();

    let err = w2_open_err(&harness.db_path).await;
    assert!(
        matches!(err, DbError::ArchiveSessionLeaked { ref marker }
                 if marker == "macrame_archive_session"),
        "expected ArchiveSessionLeaked, got {err:?}"
    );
}

/// The remedy is in the message, because the message is where it is looked for.
///
/// A `DROP TABLE` is the whole fix and the user can run it. An error that
/// diagnoses a condition without saying how to clear it sends the reader to the
/// source; D-069's rule is that the caller is told what to do.
#[tokio::test]
async fn the_marker_check_names_the_remedy() {
    let harness = TestHarness::new();

    let db = macrame::Database::open(&harness.db_path).await.unwrap();
    w2_leak_marker(&db).await;
    db.close().await.unwrap();

    let msg = w2_open_err(&harness.db_path).await.to_string();

    assert!(
        msg.contains("DROP TABLE macrame_archive_session"),
        "the message must carry the runnable remedy, got: {msg}"
    );
    assert!(
        msg.contains("audit"),
        "the message must say the damage needs auditing, got: {msg}"
    );
}

/// **The control arm, and it is not optional.**
///
/// A check that refuses healthy databases is worse than no check, and this
/// project has that shape on record: `verify` chose presence-by-name over a
/// count of `sqlite_master` precisely because the count refused files carrying
/// legitimate extra objects.
///
/// The false-positive analysis rests on one property: **the marker is never
/// committed state.** Both archive paths bracket it inside the session
/// transaction (`archive.rs:302/401` and `583/683`), so a commit drops it and a
/// rollback discards it; and `verify()` reads *committed* state at open, so it
/// cannot observe an in-flight session.
///
/// That sentence is here rather than only in the register because the obvious
/// "simplification" of this check is to move it somewhere cheaper or more
/// central — and on any path that runs *during* a session it would refuse a
/// healthy database mid-archive. The check is safe where it is, not safe in
/// general.
#[tokio::test]
async fn a_normal_archive_leaves_no_marker_and_reopens_clean() {
    let harness = TestHarness::new();

    let db = w2_seeded(&harness.db_path).await;
    let report = db.archive(W2_CUTOFF).await.unwrap();
    assert!(
        report.links_archived > 0,
        "the archive must actually run, or the control proves nothing about a \
         path that opened a session"
    );
    db.close().await.unwrap();

    macrame::Database::open(&harness.db_path)
        .await
        .expect("a database that has completed an archive must still open")
        .close()
        .await
        .unwrap();
}

/// The second control arm: an **aborted** archive must still reopen.
///
/// **This is deliberately not "the crash-safety claim, asserted rather than
/// argued".** W2 was planned on the premise that the claim was only ever
/// argued; that premise is false.
/// `temporal_tests::a_failed_archive_session_leaves_the_hot_database_untouched_and_the_guards_armed`
/// already asserts it, with a stronger fixture than anything proposed here: two
/// cases, the second breaking `cold.transaction_log` so the session dies *after*
/// rows have been deleted, which is where rollback is load-bearing rather than
/// incidental. It counts the marker in `sqlite_master` directly and requires it
/// to be zero. Duplicating that, more weakly, would add a test and no coverage.
///
/// What is genuinely new is the **refusal added in 0.10.0**, and the risk a new
/// refusal carries is the false positive. An aborted archive is the most
/// plausible way for a healthy database to end up looking suspicious, so it is
/// the arm worth having: the check must not turn a recoverable failed archive
/// into a database that will not open.
///
/// The abort is real and needs no `#[cfg(test)]` seam. `COLD_SCHEMA` creates
/// the cold tables with `IF NOT EXISTS`, and its own comment records the
/// consequence: an existing cold database keeps whatever definition it was
/// created with. A cold file pre-created with a truncated `links` table
/// therefore survives `COLD_SCHEMA` untouched, and the `INSERT … SELECT` that
/// follows — inside the session, after the marker is created — fails on the
/// missing columns.
#[tokio::test]
async fn an_aborted_archive_still_reopens_under_the_marker_check() {
    let harness = TestHarness::new();

    // The cold path `Database::open` derives, pre-created with a `links` table
    // carrying one column instead of eight.
    let cold_path = harness.temp_dir.path().join("test_macrame_archive.db");
    {
        let cold = libsql::Builder::new_local(&cold_path)
            .build()
            .await
            .unwrap();
        cold.connect()
            .unwrap()
            .execute("CREATE TABLE links (source_id TEXT)", ())
            .await
            .unwrap();
    }

    let db = w2_seeded(&harness.db_path).await;
    let err = db
        .archive(W2_CUTOFF)
        .await
        .expect_err("the rigged cold file must abort the session");
    // **This assertion is what stops the test being vacuous.** If the archive
    // failed *before* `CREATE TABLE {MARKER}`, the reopen below would succeed
    // for a reason having nothing to do with the marker, and the test would
    // pass while asserting nothing — the shape D-133 records finding in C5's
    // column check. Naming the missing column pins the failure to the
    // `INSERT … SELECT`, which is after the marker. The same technique, and
    // the same reason for it, as the `missing_column` assertion in
    // `temporal_tests`.
    let msg = format!("{err}");
    assert!(
        msg.contains("target_id"),
        "the abort must come from the rigged cold table, i.e. from *after* the \
         marker was created — got: {msg}"
    );

    db.close().await.unwrap();

    // The point of the test: the new refusal does not fire here. Asserted
    // *through* `Database::open`, so this and the check cannot drift apart —
    // `temporal_tests` covers the marker's absence directly, this covers what
    // 0.10.0 now does about it.
    macrame::Database::open(&harness.db_path)
        .await
        .expect("an aborted archive must not leave a database that refuses to open")
        .close()
        .await
        .unwrap();
}
