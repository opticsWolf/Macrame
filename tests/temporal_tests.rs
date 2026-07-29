mod harness;

use std::path::Path;
use harness::TestHarness;
use macrame::error::DbError;
use macrame::graph::AttributeMode;
use macrame::schema::migrations;
use macrame::integrity::audit_current;
use macrame::temporal::{
    archive, hydrate_attributes, load_snapshot, reconstruct, save_snapshot, Interval,
    MaterializedState,
};

#[test]
fn test_interval_containment_and_overlap() {
    let open_interval = Interval::new("2026-01-01T00:00:00.000000Z", "9999-12-31T23:59:59.999999Z");
    assert!(open_interval.is_open());
    assert!(open_interval.contains("2026-06-01T00:00:00.000000Z"));

    let closed_interval = Interval::new("2026-01-01T00:00:00.000000Z", "2026-06-01T00:00:00.000000Z");
    assert!(!closed_interval.is_open());
    assert!(closed_interval.contains("2026-03-01T00:00:00.000000Z"));
    assert!(!closed_interval.contains("2026-07-01T00:00:00.000000Z"));

    let overlapping = Interval::new("2026-05-01T00:00:00.000000Z", "2026-08-01T00:00:00.000000Z");
    let non_overlapping = Interval::new("2026-07-01T00:00:00.000000Z", "2026-09-01T00:00:00.000000Z");

    assert!(closed_interval.overlaps(&overlapping));
    assert!(!closed_interval.overlaps(&non_overlapping));
}

#[tokio::test]
async fn test_monday_wednesday_friday_scenario() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // Monday (2026-01-05): Concept created with "Monday Title"
    conn.execute(
        "INSERT INTO concepts (id, title, content, valid_from, recorded_at) \
         VALUES ('c1', 'Monday Title', 'Content', '2026-01-05T00:00:00.000000Z', '2026-01-05T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    // Wednesday (2026-01-07): Concept title updated to "Wednesday Title"
    conn.execute(
        "UPDATE concepts SET title = 'Wednesday Title', recorded_at = '2026-01-07T00:00:00.000000Z' WHERE id = 'c1'",
        (),
    )
    .await
    .unwrap();

    let node_ids = vec!["c1".to_string()];
    let tuesday_ts = "2026-01-06T00:00:00.000000Z";

    // AttributeMode::Current returns Wednesday title
    let current_attrs = hydrate_attributes(&conn, &node_ids, tuesday_ts, AttributeMode::Current)
        .await
        .unwrap();
    assert_eq!(current_attrs[0].title, "Wednesday Title");

    // AttributeMode::AtTime returns Monday title as believed on Tuesday
    let at_time_attrs = hydrate_attributes(&conn, &node_ids, tuesday_ts, AttributeMode::AtTime)
        .await
        .unwrap();
    assert_eq!(at_time_attrs[0].title, "Monday Title");

    // AttributeMode::Omit returns no node attributes
    let omit_attrs = hydrate_attributes(&conn, &node_ids, tuesday_ts, AttributeMode::Omit)
        .await
        .unwrap();
    assert!(omit_attrs.is_empty());
}

#[tokio::test]
async fn test_reconstruct_now_equals_live_table() {
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
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c2', 'Node 2', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'KNOWS', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    let state = reconstruct(&conn, "2026-01-10T00:00:00.000000Z", None, None)
        .await
        .unwrap();

    assert!(state.concepts.contains_key("c1"));
    assert!(state.concepts.contains_key("c2"));
    assert_eq!(state.edges.len(), 1);
    assert_eq!(state.edges[0].0, "c1");
    assert_eq!(state.edges[0].1, "c2");
}

#[test]
fn test_snapshot_save_load_roundtrip() {
    let harness = TestHarness::new();
    let snapshots_dir = harness.temp_dir.path().join("snapshots");

    let state = MaterializedState {
        seq_anchor: 100,
        timestamp: "2026-01-01T00:00:00.000000Z".to_string(),
        concepts: std::collections::HashMap::new(),
        edges: vec![(
            "c1".to_string(),
            "c2".to_string(),
            "KNOWS".to_string(),
            "2026-01-01T00:00:00.000000Z".to_string(),
            "9999-12-31T23:59:59.999999Z".to_string(),
        )],
    };

    let path = save_snapshot(&snapshots_dir, &state).unwrap();
    assert!(path.exists());

    let loaded = load_snapshot(&path).unwrap();
    assert_eq!(loaded.seq_anchor, 100);
    assert_eq!(loaded.edges.len(), 1);
    assert_eq!(loaded.edges[0].0, "c1");
}

/// End-to-end archive session: rows move to the cold file, the guards are
/// disarmed only for the duration, and links_current stays drift-free.
#[tokio::test]
async fn test_archive_moves_closed_intervals_and_leaves_no_drift() {
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

    // A closed interval, retired well before the cutoff.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'OLD', '2026-01-01T00:00:00.000000Z', '2026-02-01T00:00:00.000000Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    // An open interval, which must survive.
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
         VALUES ('c1', 'c2', 'LIVE', '2026-01-01T00:00:00.000000Z', '9999-12-31T23:59:59.999999Z', 1.0, '{}', '2026-01-01T00:00:00.000000Z')",
        (),
    ).await.unwrap();

    let archive_path = harness.temp_dir.path().join("test_macrame_archive.db");
    let report = archive(&conn, "2026-06-01T00:00:00.000000Z", &archive_path)
        .await
        .expect("archive session should succeed");

    assert!(archive_path.exists(), "cold database file should be created");
    assert_eq!(report.links_archived, 1, "only the closed interval is archivable");

    let remaining: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(remaining, 1, "the open interval must stay hot");

    let live: i64 = conn
        .query("SELECT COUNT(*) FROM links_current WHERE edge_type = 'LIVE'", ())
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(live, 1);

    // Doctrine VI: the materialization still matches what remains in links.
    assert_eq!(audit_current(&conn).await.unwrap(), 0, "archive must not induce drift");

    // The guards re-armed when the session committed.
    assert!(conn.execute("DELETE FROM links", ()).await.is_err());

    // DETACH ran, so a second session can still attach.
    archive(&conn, "2026-06-01T00:00:00.000000Z", &archive_path)
        .await
        .expect("second archive session should succeed (cold DB detached)");
}

#[tokio::test]
async fn test_cold_db_reconstruct_missing_archive_error() {
    let harness = TestHarness::new();
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();

    // Insert entry with recorded_at = 2026-05-01
    conn.execute(
        "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES ('c1', 'Node 1', '2026-05-01T00:00:00.000000Z', '2026-05-01T00:00:00.000000Z')",
        (),
    )
    .await
    .unwrap();

    // Target a ts older than recorded_at (pre-horizon) with a missing archive file
    let missing_path = Path::new("non_existent_archive.db");
    let res = reconstruct(&conn, "2020-01-01T00:00:00.000000Z", Some(missing_path), None).await;

    assert!(res.is_err());
    if let Err(DbError::ReplayCorrupt { reason, .. }) = res {
        assert!(reason.contains("does not exist"));
    } else {
        panic!("Expected ReplayCorrupt error for missing archive file");
    }
}

// ---------------------------------------------------------------------------
// Phase 5 — Doctrine VIII: the divergence is the point
// ---------------------------------------------------------------------------
//
// The rest of the suite asks whether `as_of` and `reconstruct` *agree*. The
// property test `the_log_fold_and_the_materialization_agree` pins the case where
// they must: nothing recorded after `ts`, so current belief is belief as of `ts`.
//
// These two ask the opposite question. Doctrine VIII exists because the two
// calls answer different questions, and its failure mode is a plausible-looking
// implementation in which they answer the same one — `reconstruct` quietly
// reading the live tables, say, or the fold ranking by valid time instead of
// transaction time. Every agreement test in the suite passes against that
// implementation. Only a test that requires them to *differ* rejects it.
//
// A retroactive correction is what opens the gap, and it opens in two
// directions: belief withdrawn after the fact, and belief added after the fact.
// Both are asserted, because an implementation can lose one and keep the other.

const VIII_JAN: &str = "2026-01-01T00:00:00.000000Z";
const VIII_FEB: &str = "2026-02-01T00:00:00.000000Z";
const VIII_MAR: &str = "2026-03-01T00:00:00.000000Z";
const VIII_OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// Newest transaction-time stamp in the ledger, i.e. "now" in belief terms.
async fn newest_stamp(conn: &libsql::Connection) -> String {
    conn.query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap()
}

/// The edges a materialized state holds at valid time `t`, as a set.
fn at_valid_time(
    state: &MaterializedState,
    t: &str,
) -> std::collections::BTreeSet<(String, String, String, String, String)> {
    state
        .edges
        .iter()
        .filter(|(_, _, _, vf, vt)| vf.as_str() <= t && t < vt.as_str())
        .cloned()
        .collect()
}

async fn as_of_set(
    conn: &libsql::Connection,
    t: &str,
) -> std::collections::BTreeSet<(String, String, String, String, String)> {
    macrame::temporal::query_as_of_edges(conn, t)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

/// **Doctrine VIII, withdrawal.** A retroactive retirement makes `as_of` forget
/// an interval that `reconstruct` at the earlier stamp must still report.
///
/// The edge is asserted open, then retired back to February. Asked about March:
/// current belief says it was already over; belief as of the first stamp says it
/// was open and had no end. Both are correct answers, to different questions,
/// and requiring them to differ is what makes the transaction-time axis
/// load-bearing rather than decorative.
#[tokio::test]
async fn a_retroactive_retirement_makes_as_of_and_reconstruct_diverge() {
    let harness = TestHarness::new();
    let db = macrame::prelude::Database::open(&harness.db_path).await.unwrap();
    for id in ["c1", "c2"] {
        db.upsert_concept(macrame::prelude::ConceptUpsert::new(id, "N").valid_from(VIII_JAN))
            .await.unwrap();
    }

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "REL")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    ).await.unwrap();
    let believed_open = newest_stamp(db.read_conn()).await;

    // The correction: recorded now, but it changes what was true in February.
    db.retire_edge("c1", "c2", "REL", VIII_JAN, VIII_FEB).await.unwrap();
    let believed_closed = newest_stamp(db.read_conn()).await;
    assert!(
        believed_open < believed_closed,
        "the correction must be recorded strictly later, or there is no gap to test"
    );

    let live = as_of_set(db.read_conn(), VIII_MAR).await;
    let then = at_valid_time(&db.reconstruct(&believed_open).await.unwrap(), VIII_MAR);
    let now = at_valid_time(&db.reconstruct(&believed_closed).await.unwrap(), VIII_MAR);

    assert!(
        live.is_empty(),
        "current belief has the interval closed in February, so March is outside it: {live:?}"
    );
    assert_eq!(
        then.len(), 1,
        "belief at the first stamp had the interval open, so March is inside it: {then:?}"
    );
    assert_ne!(
        live, then,
        "as_of and reconstruct agreed across a retroactive retirement; \
         Doctrine VIII requires the gap to be visible, not smoothed over"
    );
    assert_eq!(
        live, now,
        "once the correction is itself in the past, the two questions have the same answer"
    );

    db.close().await.unwrap();
}

/// **Doctrine VIII, addition.** The other direction, and the one the doctrine
/// names outright: "a caller should never receive yesterday's graph with today's
/// text."
///
/// An edge valid from January is asserted only now. `as_of(March)` reports it,
/// because current belief includes it. `reconstruct` at the earlier stamp must
/// not, because the database did not know it yet.
#[tokio::test]
async fn a_retroactive_assertion_is_invisible_to_the_earlier_belief() {
    let harness = TestHarness::new();
    let db = macrame::prelude::Database::open(&harness.db_path).await.unwrap();
    for id in ["c1", "c2"] {
        db.upsert_concept(macrame::prelude::ConceptUpsert::new(id, "N").valid_from(VIII_JAN))
            .await.unwrap();
    }

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "EARLY")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    ).await.unwrap();
    let before_the_late_news = newest_stamp(db.read_conn()).await;

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "LATE")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    ).await.unwrap();

    let live = as_of_set(db.read_conn(), VIII_MAR).await;
    let then = at_valid_time(
        &db.reconstruct(&before_the_late_news).await.unwrap(),
        VIII_MAR,
    );

    let types = |s: &std::collections::BTreeSet<(String, String, String, String, String)>| {
        s.iter().map(|e| e.2.clone()).collect::<std::collections::BTreeSet<_>>()
    };
    assert!(
        types(&live).contains("LATE"),
        "current belief must include the retroactively asserted edge: {live:?}"
    );
    assert!(
        !types(&then).contains("LATE"),
        "belief at the earlier stamp cannot contain something recorded after it: {then:?}"
    );
    assert_ne!(
        live, then,
        "as_of and reconstruct agreed across a retroactive assertion; \
         reconstruct is reading current state rather than folding the ledger"
    );
    assert!(
        types(&then).contains("EARLY"),
        "the earlier belief must still hold what it did know: {then:?}"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Phase 5 — D-012: an archive session is atomic, or it never happened
// ---------------------------------------------------------------------------

/// Every row of a table as one delimited string, as a set — "these rows did not
/// change", immune to column-order drift in a way positional `SELECT *` is not.
async fn row_set(
    conn: &libsql::Connection,
    expr: &str,
    table: &str,
) -> std::collections::BTreeSet<String> {
    let mut rows = conn
        .query(&format!("SELECT {expr} FROM {table}"), ())
        .await
        .unwrap();
    let mut out = std::collections::BTreeSet::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.insert(r.get::<String>(0).unwrap());
    }
    out
}

const HOT_LINK_ROW: &str = "source_id||'|'||target_id||'|'||edge_type||'|'||valid_from||'|'||\
                            valid_to||'|'||weight||'|'||properties||'|'||recorded_at";
const HOT_LOG_ROW: &str = "seq_id||'|'||table_name||'|'||entity_id||'|'||operation||'|'||\
                           payload||'|'||recorded_at";
const HOT_CURRENT_ROW: &str = "source_id||'|'||target_id||'|'||edge_type||'|'||valid_from||'|'||\
                               valid_to||'|'||recorded_at";

/// Build a hot database holding both archivable and non-archivable rows.
async fn archivable_fixture(path: &Path) -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn).await.unwrap();
    for id in ["c1", "c2"] {
        conn.execute(
            &format!(
                "INSERT INTO concepts (id, title, valid_from, recorded_at) \
                 VALUES ('{id}', 'N', '2026-01-01T00:00:00.000000Z', '2026-01-01T00:00:00.000000Z')"
            ),
            (),
        ).await.unwrap();
    }
    for (etype, valid_to) in [
        ("OLD", "2026-02-01T00:00:00.000000Z"),
        ("LIVE", "9999-12-31T23:59:59.999999Z"),
    ] {
        conn.execute(
            &format!(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
                 weight, properties, recorded_at) VALUES \
                 ('c1', 'c2', '{etype}', '2026-01-01T00:00:00.000000Z', '{valid_to}', \
                  1.0, '{{}}', '2026-01-01T00:00:00.000000Z')"
            ),
            (),
        ).await.unwrap();
    }
    (db, conn)
}

/// **D-012, the failure path.** An archive session that dies after opening is
/// indistinguishable, from the hot database's side, from one that never ran.
///
/// The session is failed on purpose, and at both points where failure is
/// dangerous — after the marker table exists, so the delete guards are
/// *disarmed*, and before the commit. A cold file pre-created with an
/// incompatible table is what does it: `CREATE TABLE IF NOT EXISTS` no-ops over
/// it and the copy then fails on a missing column.
///
/// Which table is broken decides *where* the session dies, and the two cases
/// are not interchangeable:
///
/// * `links` broken — the session dies on the first copy, before anything has
///   been deleted. Only the marker's lifetime is under test.
/// * `transaction_log` broken — the session dies after `links` has been copied
///   **and deleted from** and `links_current` re-derived. Rollback is now
///   load-bearing: without it the hot graph has lost rows that were never
///   committed anywhere. Running only the first case would leave every
///   "rows are still here" assertion below satisfied by construction.
///
/// Four distinct claims are checked. Hot tables untouched is atomicity. The
/// marker being gone is the guards re-arming, which D-008's revision rests on
/// and which fails *silently* — a disarmed guard produces no error, only a
/// database that has quietly stopped refusing deletes. That an outside `DELETE`
/// still aborts tests the guard's behaviour rather than the marker's absence,
/// which is not the same statement. And that a later session still succeeds is
/// the `DETACH`-on-error path (D-044): a leaked attachment makes every
/// subsequent archive fail with "database cold is already in use", days later
/// and nowhere near the cause.
#[tokio::test]
async fn a_failed_archive_session_leaves_the_hot_database_untouched_and_the_guards_armed() {
    // The broken cold table has to be broken *precisely*. `COLD_SCHEMA` builds
    // indexes on `cold.transaction_log` before the transaction opens, so a table
    // missing `entity_id` fails there — before the marker, before any delete,
    // making the second case a duplicate of the first. It must therefore carry
    // every column the indexes name and be missing only one the copy needs.
    let cases: [(&str, &str, &str); 2] = [
        ("links", "CREATE TABLE links (not_the_right_column TEXT)", "source_id"),
        (
            "transaction_log",
            "CREATE TABLE transaction_log (seq_id INTEGER PRIMARY KEY, table_name TEXT, \
             entity_id TEXT, operation TEXT, recorded_at TEXT)",
            "payload",
        ),
    ];

    for (broken_table, broken_ddl, missing_column) in cases {
        let harness = TestHarness::new();
        let (_db, conn) = archivable_fixture(&harness.db_path).await;

        let cold = harness
            .temp_dir
            .path()
            .join(format!("incompatible_{broken_table}.db"));
        {
            let d = libsql::Builder::new_local(&cold).build().await.unwrap();
            let c = d.connect().unwrap();
            c.execute(broken_ddl, ()).await.unwrap();
        }

        let before_links = row_set(&conn, HOT_LINK_ROW, "links").await;
        let before_log = row_set(&conn, HOT_LOG_ROW, "transaction_log").await;
        let before_current = row_set(&conn, HOT_CURRENT_ROW, "links_current").await;
        assert!(!before_links.is_empty(), "the fixture must have rows to lose");

        let cutoff = "2026-06-01T00:00:00.000000Z";
        let err = archive(&conn, cutoff, &cold)
            .await
            .expect_err(
                "the session was supposed to fail on the incompatible cold table; \
                 if it succeeded this case tests nothing",
            )
            .to_string();

        // Pin *where* it died. Without this the `transaction_log` case can
        // silently regress into a second copy of the `links` case — which is
        // exactly what happened when this test was first written, and it left
        // the row-preservation assertions below proving nothing.
        assert!(
            err.contains(missing_column),
            "[{broken_table}] expected the failure to be the copy missing {missing_column:?}, \
             so the session dies at the phase this case exists to cover; got: {err}"
        );

        assert_eq!(
            row_set(&conn, HOT_LINK_ROW, "links").await, before_links,
            "[{broken_table}] a failed archive session moved or dropped rows from links"
        );
        assert_eq!(
            row_set(&conn, HOT_LOG_ROW, "transaction_log").await, before_log,
            "[{broken_table}] a failed archive session moved or dropped ledger rows"
        );
        assert_eq!(
            row_set(&conn, HOT_CURRENT_ROW, "links_current").await, before_current,
            "[{broken_table}] a failed archive session left the materialization re-derived"
        );
        assert_eq!(
            audit_current(&conn).await.unwrap(), 0,
            "[{broken_table}] a failed archive session left links_current drifted from links"
        );

        let marker: i64 = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='macrame_archive_session'",
                (),
            )
            .await.unwrap().next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(
            marker, 0,
            "[{broken_table}] the archive-session marker outlived the session: the \
             delete guards are disarmed and will stay that way"
        );
        assert!(
            conn.execute("DELETE FROM links", ()).await.is_err(),
            "[{broken_table}] an ad-hoc DELETE succeeded after a failed archive \
             session (Doctrine V)"
        );

        // No leaked ATTACH and no leaked transaction: a real session still runs.
        let good = harness.temp_dir.path().join("good_cold.db");
        let report = archive(&conn, cutoff, &good)
            .await
            .expect("a later archive session must still succeed after a failed one");
        assert_eq!(
            report.links_archived, 1,
            "[{broken_table}] the closed interval is still archivable"
        );
    }
}
