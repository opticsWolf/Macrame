#[path = "common/harness.rs"]
mod harness;

/// When the archive session ran, as distinct from the cutoff it used. Any
/// canonical stamp does; the point is that it is not the cutoff (Wave 4.5).
const ARCHIVED_AT: &str = "2026-07-30T12:00:00.000000Z";

/// A canonical `valid_from` for fixtures that write through the public API.
const CTS: &str = "2026-01-01T00:00:00.000000Z";

use harness::TestHarness;
use macrame::graph::AttributeMode;
use macrame::integrity::audit_current;
use macrame::schema::migrations;
use macrame::temporal::{
    archive, hydrate_attributes, load_snapshot, reconstruct, save_snapshot, AsOf, Interval,
    MaterializedState,
};
use std::path::Path;

#[test]
fn test_interval_containment_and_overlap() {
    let open_interval = Interval::new("2026-01-01T00:00:00.000000Z", "9999-12-31T23:59:59.999999Z");
    assert!(open_interval.is_open());
    assert!(open_interval.contains("2026-06-01T00:00:00.000000Z"));

    let closed_interval =
        Interval::new("2026-01-01T00:00:00.000000Z", "2026-06-01T00:00:00.000000Z");
    assert!(!closed_interval.is_open());
    assert!(closed_interval.contains("2026-03-01T00:00:00.000000Z"));
    assert!(!closed_interval.contains("2026-07-01T00:00:00.000000Z"));

    let overlapping = Interval::new("2026-05-01T00:00:00.000000Z", "2026-08-01T00:00:00.000000Z");
    let non_overlapping =
        Interval::new("2026-07-01T00:00:00.000000Z", "2026-09-01T00:00:00.000000Z");

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
    let current_attrs = hydrate_attributes(&conn, &node_ids, &AsOf::now(), AttributeMode::Current)
        .await
        .unwrap();
    assert_eq!(current_attrs[0].title, "Wednesday Title");

    // AttributeMode::AtTime returns Monday title as believed on Tuesday
    let at_time_attrs = hydrate_attributes(
        &conn,
        &node_ids,
        &AsOf::recorded_at(tuesday_ts),
        AttributeMode::AtTime,
    )
    .await
    .unwrap();
    assert_eq!(at_time_attrs[0].title, "Monday Title");

    // AttributeMode::Omit returns no node attributes
    let omit_attrs = hydrate_attributes(&conn, &node_ids, &AsOf::now(), AttributeMode::Omit)
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
        predates_recorded_history: false,
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
    let report = archive(
        &conn,
        "2026-06-01T00:00:00.000000Z",
        ARCHIVED_AT,
        &archive_path,
    )
    .await
    .expect("archive session should succeed");

    assert!(
        archive_path.exists(),
        "cold database file should be created"
    );
    assert_eq!(
        report.links_archived, 1,
        "only the closed interval is archivable"
    );

    let remaining: i64 = conn
        .query("SELECT COUNT(*) FROM links", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(remaining, 1, "the open interval must stay hot");

    let live: i64 = conn
        .query(
            "SELECT COUNT(*) FROM links_current WHERE edge_type = 'LIVE'",
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
    assert_eq!(live, 1);

    // Doctrine VI: the materialization still matches what remains in links.
    assert_eq!(
        audit_current(&conn).await.unwrap(),
        0,
        "archive must not induce drift"
    );

    // The guards re-armed when the session committed.
    assert!(conn.execute("DELETE FROM links", ()).await.is_err());

    // DETACH ran, so a second session can still attach.
    archive(
        &conn,
        "2026-06-01T00:00:00.000000Z",
        ARCHIVED_AT,
        &archive_path,
    )
    .await
    .expect("second archive session should succeed (cold DB detached)");
}

/// **A missing archive is still an error — when there was an archive** (B5,
/// D-121).
///
/// This test used to build a database that had *never been archived*, hand it a
/// path to a file that had never existed, and require an error. It passed, and
/// what it was pinning was the defect: on a young ledger, asking about a time
/// before the first write reported [`DbError::ReplayCorrupt`] — the class
/// meaning *the ledger is damaged* — and named a file the caller had never
/// created. That case now answers with the empty state
/// ([`reconstructing_below_the_log_floor_is_not_a_corruption`]).
///
/// The half worth keeping is the other one, and it needs a fixture that earns
/// it: rows really were moved to cold, and the cold file really is gone. Then
/// the delta is unreachable, "nothing was believed yet" would be an invention,
/// and raising is the only honest answer.
#[tokio::test]
async fn a_missing_archive_is_an_error_when_rows_were_actually_archived() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    // Supersede a concept so there is something archivable: the newest row per
    // entity never moves, so a single write would archive nothing at all.
    for title in ["first", "second", "third"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(CTS))
            .await
            .unwrap();
    }
    let report = db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs log rows to have actually moved: {report:?}"
    );
    db.close().await.unwrap();

    // Now take the cold file away, which is the situation R14 is about.
    let archive_path = harness.db_path.with_file_name("kb_archive.db");
    let archive_path = if archive_path.exists() {
        archive_path
    } else {
        // The handle derives the name; find it rather than assume it.
        std::fs::read_dir(harness.temp_dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().contains("archive"))
            .expect("archive() must have created a cold file")
    };
    std::fs::remove_file(&archive_path).unwrap();

    let conn = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    migrations::run(&conn).await.unwrap();

    let res = reconstruct(
        &conn,
        "2020-01-01T00:00:00.000000Z",
        Some(&archive_path),
        None,
    )
    .await;

    match res {
        Err(DbError::ReplayCorrupt { reason, .. }) => {
            assert!(
                reason.contains("does not exist"),
                "the error should name the missing file: {reason}"
            );
        }
        other => panic!("expected ReplayCorrupt for a missing archive, got {other:?}"),
    }
}

/// **Below the log floor is not a corruption** (B5, D-121).
///
/// Reproduced against 0.7.0 as published: an empty database answered
/// `reconstruct("2020-01-01…")` with the empty state, and the *same question*
/// after a single write raised `ReplayCorruptError` naming a `kb_archive.db`
/// nobody had created. Asking what was believed before your data existed is
/// ordinary, and "nothing yet" is the ordinary answer.
///
/// **The boundary is transaction time, not valid time**, which is what makes
/// this worse than a pre-genesis curiosity. `recorded_at` is crate-stamped at
/// write, so on a database written today every `ts` before today was below the
/// floor — including one a caller would reasonably think was well inside the
/// data's own lifetime. The `valid_from` here is deliberately far in the past to
/// pin that: the concept claims to be valid from 2026-01-01, and a
/// reconstruction at 2026-02-01 still predates the *record* of it.
///
/// Both halves of the flag are asserted. An empty answer is not self-describing
/// — everything retired and nothing recorded yet look identical in the concepts
/// and edges — so `predates_recorded_history` has to be checked true here and
/// **false** on a fold that really did have rows, or it is decoration.
#[tokio::test]
async fn reconstructing_below_the_log_floor_is_not_a_corruption() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    // Before any write, this already worked. Asserted so the test states the
    // transition rather than only its second half.
    let before_any_data = db.reconstruct("2020-01-01T00:00:00.000000Z").await.unwrap();
    assert!(before_any_data.concepts.is_empty());
    assert!(before_any_data.predates_recorded_history);

    db.upsert_concept(ConceptUpsert::new("c0", "Title").valid_from(CTS))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("c1", "Other").valid_from(CTS))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("c0", "c1", "KNOWS").valid_from(CTS))
        .await
        .unwrap();

    // The row from the reproduction table that used to raise.
    let after = db.reconstruct("2020-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        after.concepts.is_empty() && after.edges.is_empty(),
        "nothing had been recorded by 2020"
    );
    assert!(
        after.predates_recorded_history,
        "the caller cannot otherwise tell this from a fully retired ledger"
    );

    // Transaction time is the axis: `CTS` is the *valid_from*, and a moment
    // after it is still before anything was recorded.
    let inside_valid_time = db.reconstruct("2026-02-01T00:00:00.000000Z").await.unwrap();
    assert!(
        inside_valid_time.predates_recorded_history,
        "the floor is MIN(recorded_at), not MIN(valid_from)"
    );

    // And the flag is false when there was history to fold, or it says nothing.
    let now = db.reconstruct(&max_recorded_at(&db).await).await.unwrap();
    assert_eq!(now.concepts.len(), 2, "the ordinary fold still works");
    assert!(
        !now.predates_recorded_history,
        "a fold with rows in it must not claim the ledger had not started"
    );

    db.close().await.unwrap();
}

/// The newest `recorded_at` in the hot log, for tests that need a `ts` the fold
/// actually covers.
async fn max_recorded_at(db: &macrame::Database) -> String {
    db.read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
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
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
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
    let db = macrame::prelude::Database::open(&harness.db_path)
        .await
        .unwrap();
    for id in ["c1", "c2"] {
        db.upsert_concept(macrame::prelude::ConceptUpsert::new(id, "N").valid_from(VIII_JAN))
            .await
            .unwrap();
    }

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "REL")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    )
    .await
    .unwrap();
    let believed_open = newest_stamp(db.read_conn()).await;

    // The correction: recorded now, but it changes what was true in February.
    db.retire_edge("c1", "c2", "REL", VIII_JAN, VIII_FEB)
        .await
        .unwrap();
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
        then.len(),
        1,
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
    let db = macrame::prelude::Database::open(&harness.db_path)
        .await
        .unwrap();
    for id in ["c1", "c2"] {
        db.upsert_concept(macrame::prelude::ConceptUpsert::new(id, "N").valid_from(VIII_JAN))
            .await
            .unwrap();
    }

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "EARLY")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    )
    .await
    .unwrap();
    let before_the_late_news = newest_stamp(db.read_conn()).await;

    db.assert_edge(
        macrame::prelude::EdgeAssertion::new("c1", "c2", "LATE")
            .valid_from(VIII_JAN)
            .valid_to(VIII_OPEN),
    )
    .await
    .unwrap();

    let live = as_of_set(db.read_conn(), VIII_MAR).await;
    let then = at_valid_time(
        &db.reconstruct(&before_the_late_news).await.unwrap(),
        VIII_MAR,
    );

    let types = |s: &std::collections::BTreeSet<(String, String, String, String, String)>| {
        s.iter()
            .map(|e| e.2.clone())
            .collect::<std::collections::BTreeSet<_>>()
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
        )
        .await
        .unwrap();
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
        )
        .await
        .unwrap();
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
        (
            "links",
            "CREATE TABLE links (not_the_right_column TEXT)",
            "source_id",
        ),
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
        assert!(
            !before_links.is_empty(),
            "the fixture must have rows to lose"
        );

        let cutoff = "2026-06-01T00:00:00.000000Z";
        let err = archive(&conn, cutoff, ARCHIVED_AT, &cold)
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
            row_set(&conn, HOT_LINK_ROW, "links").await,
            before_links,
            "[{broken_table}] a failed archive session moved or dropped rows from links"
        );
        assert_eq!(
            row_set(&conn, HOT_LOG_ROW, "transaction_log").await,
            before_log,
            "[{broken_table}] a failed archive session moved or dropped ledger rows"
        );
        assert_eq!(
            row_set(&conn, HOT_CURRENT_ROW, "links_current").await,
            before_current,
            "[{broken_table}] a failed archive session left the materialization re-derived"
        );
        assert_eq!(
            audit_current(&conn).await.unwrap(),
            0,
            "[{broken_table}] a failed archive session left links_current drifted from links"
        );

        let marker: i64 = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='macrame_archive_session'",
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
        let report = archive(&conn, cutoff, ARCHIVED_AT, &good)
            .await
            .expect("a later archive session must still succeed after a failed one");
        assert_eq!(
            report.links_archived, 1,
            "[{broken_table}] the closed interval is still archivable"
        );
    }
}

/// **The unreachable-archive error carries what the hot-side marker was wanted
/// for** (C4).
///
/// [D-121] refused a hot-side marker recording *archived at* and *horizon*, and
/// left 0.9.0 to adopt it "only if it wants the richer message". This is the
/// richer message, built from the hot log alone — so the marker is refused
/// outright rather than deferred a second time.
///
/// **The numbers are cross-checked against `ArchiveReport`, not merely present.**
/// A hint that always said "0 log rows have been archived" would satisfy any test
/// that only looked for the sentence, and its failure mode is *always says
/// something reassuring* — the shape [D-030]'s always-zero audit and §8's
/// conjunction rule both exist to catch. So the count in the message must equal
/// what the archive reported moving, and the horizon must equal the horizon the
/// archive recorded. Those are two independent computations of the same fact:
/// the archive counted rows as it moved them, and the message derives the count
/// from `MAX(seq_id) - COUNT(*)` on what survived.
#[tokio::test]
async fn the_unreachable_archive_error_says_how_much_went_and_how_far_back_the_log_reaches() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    for title in ["first", "second", "third", "fourth"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(CTS))
            .await
            .unwrap();
    }
    let report = db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs log rows to have actually moved: {report:?}"
    );
    let horizon = report
        .horizon
        .expect("an archive that moved rows has a horizon");
    db.close().await.unwrap();

    let archive_path = std::fs::read_dir(harness.temp_dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains("archive"))
        .expect("archive() must have created a cold file");
    std::fs::remove_file(&archive_path).unwrap();

    let conn = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    migrations::run(&conn).await.unwrap();

    let res = reconstruct(
        &conn,
        "2020-01-01T00:00:00.000000Z",
        Some(&archive_path),
        None,
    )
    .await;

    let Err(DbError::ReplayCorrupt { reason, .. }) = res else {
        panic!("expected ReplayCorrupt for a missing archive, got {res:?}");
    };

    assert!(
        reason.contains(&format!(
            "{} log rows have been archived",
            report.log_entries_archived
        )),
        "the hint's count disagrees with what the archive reported moving \
         ({}): {reason}",
        report.log_entries_archived
    );
    assert!(
        reason.contains(&format!("begins at seq_id {horizon}")),
        "the hint should name the horizon the archive recorded ({horizon}): {reason}"
    );
}

/// **Rehydration does not change what the hint says, and that is the fact a
/// hot-side marker would have had to get right too** (C4).
///
/// The question that decides whether an archive record can be trusted after a
/// round trip is whether rehydration invalidates it. It does not, and the reason
/// is structural rather than lucky: rehydration moves **concept rows** and never
/// log rows ([D-131]), so `MAX(seq_id) - COUNT(*)` and `MIN(seq_id)` are
/// untouched by it. A marker recording *archived at* and *horizon* would have
/// stayed accurate for the same reason — which is precisely why it earns no
/// rung: it would be paying a schema change for a fact the log already keeps
/// correct for free.
///
/// Asserted against the *same* fixture before and after the round trip, so a
/// hint that had quietly become a constant would still have to match a number
/// derived from `ArchiveReport`.
#[tokio::test]
async fn a_rehydration_leaves_the_archive_hint_saying_exactly_what_it_said_before() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    // An archivable concept: retired, its valid interval closed before the
    // cutoff, and named by no edge (D-128). Superseded first, so the log has
    // rows to archive as well as the row itself.
    for title in ["first", "second"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(CTS))
            .await
            .unwrap();
    }
    db.upsert_concept(
        ConceptUpsert::new("c1", "last")
            .valid_from(CTS)
            .valid_to("2027-01-01T00:00:00.000000Z")
            .retired(true),
    )
    .await
    .unwrap();

    let report = db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();
    assert_eq!(
        report.concepts_archived, 1,
        "the fixture needs the concept itself to have gone cold: {report:?}"
    );
    assert!(report.log_entries_archived >= 2, "{report:?}");
    let horizon = report
        .horizon
        .expect("an archive that moved rows has a horizon");

    let rehydrated = db.rehydrate(&["c1"]).await.unwrap();
    assert_eq!(rehydrated.concepts_rehydrated, 1);
    db.close().await.unwrap();

    let archive_path = std::fs::read_dir(harness.temp_dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains("archive"))
        .expect("archive() must have created a cold file");
    std::fs::remove_file(&archive_path).unwrap();

    let conn = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    migrations::run(&conn).await.unwrap();

    let res = reconstruct(
        &conn,
        "2020-01-01T00:00:00.000000Z",
        Some(&archive_path),
        None,
    )
    .await;

    let Err(DbError::ReplayCorrupt { reason, .. }) = res else {
        panic!("expected ReplayCorrupt for a missing archive, got {res:?}");
    };
    assert!(
        reason.contains(&format!(
            "{} log rows have been archived",
            report.log_entries_archived
        )),
        "the round trip changed the archived-row count the hint reports \
         (expected {}): {reason}",
        report.log_entries_archived
    );
    assert!(
        reason.contains(&format!("begins at seq_id {horizon}")),
        "the round trip moved the horizon the hint reports (expected {horizon}): {reason}"
    );
}

/// `as_of_recorded` refuses once the hot log has been archived (W7.1, D-174).
///
/// A transaction-time traversal folds `transaction_log`, and `archive` removes
/// superseded rows from it. A traversal takes a `Connection` and no archive path,
/// so it cannot go and get what was moved — and a fold missing its superseded
/// rows returns *nearly* the right topology, which is the failure a ledger can
/// least afford and the one an assertion on non-emptiness will not catch.
///
/// The valid-time axis is unaffected, and that half of the assertion is the
/// point: the refusal is scoped to the mechanism that actually reads the log,
/// not applied to every historical query on an archived database.
#[tokio::test]
async fn a_recorded_instant_is_refused_once_rows_have_been_archived() {
    use macrame::graph::TraversalBuilder;
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = macrame::Database::open(&harness.db_path).await.unwrap();

    // Supersede a concept so there is something archivable: the newest row per
    // entity never moves, so a single write would archive nothing at all.
    for title in ["first", "second", "third"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(CTS))
            .await
            .unwrap();
    }
    db.upsert_concept(ConceptUpsert::new("c2", "other").valid_from(CTS))
        .await
        .unwrap();
    db.assert_edge(macrame::graph::EdgeAssertion::new("c1", "c2", "KNOWS").valid_from(CTS))
        .await
        .unwrap();

    let now = "2029-01-01T00:00:00.000000Z";
    let instant = "2028-01-01T00:00:00.000000Z";

    // Before archiving, the fold is answerable and the traversal works.
    let before = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_recorded(instant)
        .execute_ids(db.read_conn(), now)
        .await
        .expect("an intact hot log answers for any instant");
    assert_eq!(before, vec!["c1".to_string(), "c2".to_string()]);

    let report = db.archive("2030-01-01T00:00:00.000000Z").await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs log rows to have actually moved: {report:?}"
    );

    // After archiving the same call refuses, by name, and names the instant.
    let err = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_recorded(instant)
        .execute_ids(db.read_conn(), now)
        .await
        .expect_err("a short hot log must refuse rather than fold what is left");
    match &err {
        macrame::DbError::RecordedInstantUnreachable { ts } => assert_eq!(ts, instant),
        other => panic!("got {other:?}"),
    }
    // And it must send the caller somewhere that can answer.
    assert!(err.to_string().contains("reconstruct"), "{err}");

    // The valid-time axis is untouched: it reads live tables and never the log,
    // so archiving has nothing to do with it.
    let by_valid = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_valid(now)
        .execute_ids(db.read_conn(), now)
        .await
        .expect("valid time does not read the log and must be unaffected");
    assert_eq!(by_valid, vec!["c1".to_string(), "c2".to_string()]);

    db.close().await.unwrap();
}

/// **`AttributeMode::AtTime` must not answer from a log the archive has taken
/// rows out of** (W9.1, §3.2).
///
/// The fixture is the one shape that makes the failure visible: three
/// generations of one concept an hour of transaction time apart, an archive
/// cutoff between the second and the third, and an instant back in the *first*
/// generation. `LOG_ARCHIVABLE` keeps the newest row per entity, so the hot log
/// is left holding only `third` — and a fold bounded at `00:30` finds nothing
/// at all, because the row that was true then is in the other file.
///
/// The pre-fix behaviour was `Ok(vec![])`: a concept that exists, was not
/// retired, and had a title at the instant asked about, reported as absent by
/// being missing from a `Vec`. Indistinguishable at the call site from retired
/// and from never having existed.
#[tokio::test]
async fn hydrating_at_a_recorded_instant_refuses_once_rows_have_been_archived() {
    use macrame::prelude::*;
    use std::time::Duration;

    const EPOCH: &str = "1970-01-01T00:00:00.000000Z";
    /// Inside the first generation: what was believed here is archived.
    const INSTANT: &str = "1970-01-01T00:30:00.000000Z";
    /// Between the second generation and the third.
    const CUTOFF: &str = "1970-01-01T01:30:00.000000Z";

    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;

    for title in ["first", "second", "third"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(EPOCH))
            .await
            .unwrap();
        harness.advance(Duration::from_secs(3_600));
    }

    let ids = vec!["c1".to_string()];

    // Before archiving, the hot log holds all three and the instant is
    // answerable — the assertion that keeps the fix from being "always refuse".
    let before = hydrate_attributes(
        db.read_conn(),
        &ids,
        &AsOf::recorded_at(INSTANT),
        AttributeMode::AtTime,
    )
    .await
    .expect("an intact hot log answers for any instant");
    assert_eq!(
        before.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
        vec!["first"],
        "at {INSTANT} the ledger held the first generation"
    );

    let report = db.archive(CUTOFF).await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs the superseded rows to have actually moved: {report:?}"
    );

    // After archiving the same call refuses, by name, and names the instant.
    let err = hydrate_attributes(
        db.read_conn(),
        &ids,
        &AsOf::recorded_at(INSTANT),
        AttributeMode::AtTime,
    )
    .await
    .expect_err("a short hot log must refuse rather than return a shorter Vec");
    match &err {
        macrame::DbError::RecordedInstantUnreachable { ts } => assert_eq!(ts, INSTANT),
        other => panic!("got {other:?}"),
    }
    assert!(err.to_string().contains("reconstruct"), "{err}");

    // The other three cells of the dispatch table read live `concepts` and
    // never the log, so archiving has nothing to do with them.
    for as_of in [AsOf::now(), AsOf::valid_at(INSTANT)] {
        let live = hydrate_attributes(db.read_conn(), &ids, &as_of, AttributeMode::AtTime)
            .await
            .expect("the live arms do not read the log and must be unaffected");
        assert_eq!(
            live.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
            vec!["third"],
            "live text, whichever instant the valid axis names"
        );
    }

    db.close().await.unwrap();
}
