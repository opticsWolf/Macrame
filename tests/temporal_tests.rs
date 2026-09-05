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
    macrame::schema::run_migrations(&conn).await.unwrap();

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
    macrame::schema::run_migrations(&conn).await.unwrap();

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
    assert_eq!(state.edges[0].source_id, "c1");
    assert_eq!(state.edges[0].target_id, "c2");
}

#[test]
fn test_snapshot_save_load_roundtrip() {
    let harness = TestHarness::new();
    let snapshots_dir = harness.temp_dir.path().join("snapshots");

    let mut state = MaterializedState::empty("2026-01-01T00:00:00.000000Z");
    state.seq_anchor = 100;
    state.edges = vec![macrame::temporal::EdgeBelief::new(
        "c1",
        "c2",
        "KNOWS",
        "2026-01-01T00:00:00.000000Z",
        "9999-12-31T23:59:59.999999Z",
    )];

    let path = save_snapshot(&snapshots_dir, &state).unwrap();
    assert!(path.exists());

    let loaded = load_snapshot(&path).unwrap();
    assert_eq!(loaded.seq_anchor, 100);
    assert_eq!(loaded.edges.len(), 1);
    assert_eq!(loaded.edges[0].source_id, "c1");
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
    macrame::schema::run_migrations(&conn).await.unwrap();

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
    macrame::schema::run_migrations(&conn).await.unwrap();

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
    // Projected back to the five-tuple `query_as_of_edges` answers with, which
    // is what the caller of this helper compares against. Sound because the
    // fixtures here never fork: on a single-lineage ledger the fold's beliefs
    // and the trunk's resolved view are the same rows. A forked fixture would
    // need the resolution, not a projection — see `branch_read_tests`.
    state
        .edges
        .iter()
        .filter(|e| e.valid_from.as_str() <= t && t < e.valid_to.as_str())
        .map(|e| {
            (
                e.source_id.clone(),
                e.target_id.clone(),
                e.edge_type.clone(),
                e.valid_from.clone(),
                e.valid_to.clone(),
            )
        })
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
    macrame::schema::run_migrations(&conn).await.unwrap();
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
    macrame::schema::run_migrations(&conn).await.unwrap();

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
    macrame::schema::run_migrations(&conn).await.unwrap();

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

/// The valid time everything in [`a_ledger_archived_mid_history`] is asserted
/// over. One instant for all of it: the axis under test is transaction time.
const REACH_VALID_FROM: &str = "1970-01-01T00:00:00.000000Z";
/// Inside the superseded region. The row that won here is the one that goes.
const REACH_EARLY: &str = "1970-01-01T00:30:00.000000Z";
/// After the last superseded row and before nothing: everything below it that
/// has a successor is archivable.
const REACH_CUTOFF: &str = "1970-01-01T02:30:00.000000Z";
/// A valid-time "now" late enough to see every open interval in the fixture.
const REACH_NOW: &str = "1970-01-01T03:00:00.000000Z";

/// The newest `recorded_at` in the hot log — the boundary of the rule that
/// survives archiving, read from the log rather than written down.
///
/// A literal will not do here. `FakeClock` advances a microsecond per reading
/// so that two writes in the same simulated hour still order, which puts
/// sub-second digits on every stamp; a hand-written `02:00:00.000000Z` is
/// *before* the row it means and tests the arm next to the one it names.
async fn newest_recorded_at(conn: &libsql::Connection) -> String {
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

/// A whole topology recorded at once, then one of its three entities
/// superseded twice — **and the other two left alone**.
///
/// That asymmetry is the fixture. `LOG_ARCHIVABLE` takes superseded rows
/// wherever they sit in the sequence, so archiving here moves `c1`'s first two
/// generations and leaves `c2`'s and the edge's original rows in place. The hot
/// log therefore still *reaches back* to `00:00` while no longer being
/// *complete* at `00:30`, which is the exact gap between the two questions
/// 0.5.5 separated — and the state in which a reach test that consults
/// `MIN(recorded_at)` returns a confidently wrong answer.
///
/// A `FakeClock` drives it because the windows are bounds on *transaction*
/// time: with the wall clock supplying `recorded_at`, every write lands in the
/// same microsecond and no cutoff can fall between them.
async fn a_ledger_archived_mid_history(harness: &TestHarness) -> macrame::Database {
    use macrame::prelude::*;

    let db = harness.db_with_fake_clock().await;
    let hour = std::time::Duration::from_secs(3_600);

    // 00:00 — the whole graph, and `c1`'s first generation.
    db.upsert_concept(ConceptUpsert::new("c1", "first").valid_from(REACH_VALID_FROM))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("c2", "other").valid_from(REACH_VALID_FROM))
        .await
        .unwrap();
    db.assert_edge(
        macrame::graph::EdgeAssertion::new("c1", "c2", "KNOWS").valid_from(REACH_VALID_FROM),
    )
    .await
    .unwrap();
    harness.advance(hour);

    // 01:00 and 02:00 — two more generations of `c1` alone.
    for title in ["second", "third"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(REACH_VALID_FROM))
            .await
            .unwrap();
        harness.advance(hour);
    }
    db
}

/// `as_of_recorded` refuses at an instant the archive really did take the
/// answer from (W7.1, D-174; narrowed 0.15.4, W14.2, review C-2).
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
///
/// **The instant now has to earn the refusal, which the old fixture's did
/// not.** Through 0.15.3 this test wrote on the wall clock and asked at
/// `2028-01-01` — an instant *after* every row in the log, which
/// [`reach_with_rows_removed`] answers exactly, and it passed only because the
/// guard discarded the timestamp. It is paired with
/// [`an_instant_at_or_after_the_newest_hot_row_survives_the_archive`], which
/// takes the other side of the same boundary; neither is meaningful alone.
#[tokio::test]
async fn a_recorded_instant_is_refused_once_rows_have_been_archived() {
    use macrame::graph::TraversalBuilder;

    let harness = TestHarness::new();
    let db = a_ledger_archived_mid_history(&harness).await;

    // Before archiving, the fold is answerable and the traversal works.
    let before = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_recorded(REACH_EARLY)
        .execute_ids(db.read_conn(), REACH_NOW)
        .await
        .expect("an intact hot log answers for any instant");
    assert_eq!(before, vec!["c1".to_string(), "c2".to_string()]);

    let report = db.archive(REACH_CUTOFF).await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs log rows to have actually moved: {report:?}"
    );

    // After archiving the same call refuses, by name, and names the instant.
    let err = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_recorded(REACH_EARLY)
        .execute_ids(db.read_conn(), REACH_NOW)
        .await
        .expect_err("a short hot log must refuse rather than fold what is left");
    match &err {
        macrame::DbError::RecordedInstantUnreachable { ts } => assert_eq!(ts, REACH_EARLY),
        other => panic!("got {other:?}"),
    }
    // And it must send the caller somewhere that can answer.
    assert!(err.to_string().contains("reconstruct"), "{err}");

    // The valid-time axis is untouched: it reads live tables and never the log,
    // so archiving has nothing to do with it.
    let by_valid = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_valid(REACH_NOW)
        .execute_ids(db.read_conn(), REACH_NOW)
        .await
        .expect("valid time does not read the log and must be unaffected");
    assert_eq!(by_valid, vec!["c1".to_string(), "c2".to_string()]);

    // **The second entry point onto the same fold, added 0.15.9 (W13.4,
    // D-251).** `Database::edges` folds `links_at_tx` exactly as the traversal
    // does, so it has to refuse exactly where the traversal refuses; asserted
    // on this fixture rather than on a copy of it, because the fixture is the
    // expensive half and the guard is the cheap one.
    let err = db
        .edges(
            macrame::ReadPlan::new()
                .valid_at(REACH_NOW)
                .recorded_at(REACH_EARLY),
        )
        .await
        .expect_err("a plan reader that skipped the guard would fold a short log");
    match &err {
        macrame::DbError::RecordedInstantUnreachable { ts } => assert_eq!(ts, REACH_EARLY),
        other => panic!("got {other:?}"),
    }
    // Without the recorded instant it is a projection read and answers.
    db.edges(macrame::ReadPlan::new().valid_at(REACH_NOW))
        .await
        .expect("current belief never reads the log");

    db.close().await.unwrap();
}

/// The other side of the boundary: an archived ledger still answers for the
/// instants it never lost (0.15.4, W14.2, review C-2).
///
/// The newest row per entity is never archivable, so at an instant at or after
/// the newest stamp the hot log still holds, every entity's winning row is its
/// newest row and every one of those is hot. The fold is complete, and refusing
/// it costs a deployment `AttributeMode::AtTime` and every `as_of_recorded`
/// traversal for its whole history the first time it archives anything —
/// including `as_of_recorded(now)`, the instant the archive is *guaranteed* to
/// answer.
///
/// **Asked at the boundary itself** — the newest stamp in the log, read from
/// it — rather than an instant comfortably past it, so an off-by-one in the
/// comparison is a failure here and not a silence.
///
/// **Both readers, and the answer compared rather than restated.** The truth is
/// taken while the log is intact and required back afterwards, so a guard that
/// admitted the instant and a fold that then answered it wrongly cannot pass
/// together — the mistake a test asserting `vec!["c1", "c2"]` in both places
/// would let through.
#[tokio::test]
async fn an_instant_at_or_after_the_newest_hot_row_survives_the_archive() {
    use macrame::graph::TraversalBuilder;

    let harness = TestHarness::new();
    let db = a_ledger_archived_mid_history(&harness).await;
    let ids = vec!["c1".to_string()];
    let late = newest_recorded_at(db.read_conn()).await;

    let topology = TraversalBuilder::new("c1")
        .max_depth(1)
        .as_of_recorded(&late)
        .execute_ids(db.read_conn(), REACH_NOW)
        .await
        .expect("an intact hot log answers for any instant");
    let text = hydrate_attributes(
        db.read_conn(),
        &ids,
        &AsOf::recorded_at(&late),
        AttributeMode::AtTime,
    )
    .await
    .expect("an intact hot log answers for any instant");

    let report = db.archive(REACH_CUTOFF).await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs log rows to have actually moved: {report:?}"
    );
    assert_eq!(
        newest_recorded_at(db.read_conn()).await,
        late,
        "the newest row per entity is never archivable, and the whole rule rests on it"
    );

    assert_eq!(
        TraversalBuilder::new("c1")
            .max_depth(1)
            .as_of_recorded(&late)
            .execute_ids(db.read_conn(), REACH_NOW)
            .await
            .expect("the newest row per entity is hot, so this instant is answerable"),
        topology,
        "the archive moved superseded rows, it did not change the topology at {late}"
    );
    assert_eq!(
        hydrate_attributes(
            db.read_conn(),
            &ids,
            &AsOf::recorded_at(&late),
            AttributeMode::AtTime,
        )
        .await
        .expect("the second reader takes the same verdict"),
        text,
        "the archive moved superseded rows, it did not change the text at {late}"
    );

    db.close().await.unwrap();
}

/// `reconstruct` with no archive path refuses at an instant the hot log cannot
/// complete, even where the hot log still reaches back past it (0.15.4, W14.2,
/// review C-2).
///
/// This is the arm 0.5.5 did not reach. That release replaced
/// `MIN(recorded_at) <= ts` — *does the log stretch back to `ts`* — with the
/// completeness test, and kept `MIN` for the case where **no archive file is
/// present**, on the reasoning that nothing can have been removed then. The
/// reasoning holds for a database that was never archived and not for one whose
/// cold file the caller simply did not pass, which is the ordinary way to reach
/// it: `reconstruct`'s `archive_path` is an `Option`.
///
/// [`a_ledger_archived_mid_history`] is the state that separates the two
/// questions — `c2` and the edge keep their original rows, so `MIN` still sits
/// at `00:00` while `c1`'s winning row at `00:30` is in the cold file. Before
/// this release the fold ran and returned, with no error, a state holding the
/// edge `c1 -> c2` and **no `c1` at all** — a graph carrying an edge out of a
/// concept the same state says did not exist. Exactly D-189's silent short
/// answer, at the one reader that has an archive path and was handed `None`.
///
/// The assertion is on the error, and separately on the fact that a fold *would*
/// have been wrong — because a refusal that is right for the wrong reason is
/// what this whole area keeps producing.
#[tokio::test]
async fn reconstructing_without_the_archive_path_refuses_rather_than_folding_a_gap() {
    let harness = TestHarness::new();
    let db = a_ledger_archived_mid_history(&harness).await;

    let truth = reconstruct(db.read_conn(), REACH_EARLY, None, None)
        .await
        .expect("an intact hot log needs no archive");
    assert!(
        truth.concepts.contains_key("c1"),
        "the fixture must have something at {REACH_EARLY} for the archive to take"
    );

    db.archive(REACH_CUTOFF).await.unwrap();

    // The hot log still reaches back past the instant — this is the premise,
    // and without it the old rule would have refused anyway and the test would
    // be measuring nothing.
    let floor: String = db
        .read_conn()
        .query("SELECT MIN(recorded_at) FROM transaction_log", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert!(
        floor.as_str() <= REACH_EARLY,
        "the hot log must still stretch back past {REACH_EARLY} for this to be the case \
         a `MIN(recorded_at)` reach test gets wrong; floor is {floor}"
    );

    let err = reconstruct(db.read_conn(), REACH_EARLY, None, None)
        .await
        .expect_err("the log reaches back to the instant but no longer completes it");
    assert!(
        matches!(err, macrame::DbError::ReplayCorrupt { .. }),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("no archive path was given"),
        "the refusal must name the missing ingredient: {err}"
    );

    // And with the path, the same instant answers what it answered before.
    assert_eq!(
        db.reconstruct(REACH_EARLY)
            .await
            .expect("reconstruct takes the archive path and must answer")
            .concepts
            .get("c1"),
        truth.concepts.get("c1"),
        "the archive moved the row, it did not change what was believed"
    );

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
/// The valid time every generation of the fixture concept is asserted over.
const HORIZON_VALID_FROM: &str = "1970-01-01T00:00:00.000000Z";
/// Inside the first generation: what was believed here goes to the cold file.
const HORIZON_INSTANT: &str = "1970-01-01T00:30:00.000000Z";
/// Between the second generation and the third.
const HORIZON_CUTOFF: &str = "1970-01-01T01:30:00.000000Z";

/// Three generations of one concept an hour of transaction time apart, with an
/// archive cutoff between the second and the third — the one shape that puts
/// [`HORIZON_INSTANT`] on the far side of the horizon.
///
/// A `FakeClock` drives it because the windows are bounds on *transaction*
/// time: with the wall clock supplying `recorded_at`, three writes land in the
/// same microsecond, no cutoff can fall between them, and the fixture would
/// pass while demonstrating nothing.
async fn a_ledger_archived_across_a_horizon(harness: &TestHarness) -> macrame::Database {
    use macrame::prelude::*;

    let db = harness.db_with_fake_clock().await;
    for title in ["first", "second", "third"] {
        db.upsert_concept(ConceptUpsert::new("c1", title).valid_from(HORIZON_VALID_FROM))
            .await
            .unwrap();
        harness.advance(std::time::Duration::from_secs(3_600));
    }
    db
}

#[tokio::test]
async fn hydrating_at_a_recorded_instant_refuses_once_rows_have_been_archived() {
    const INSTANT: &str = HORIZON_INSTANT;

    let harness = TestHarness::new();
    let db = a_ledger_archived_across_a_horizon(&harness).await;
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

    let report = db.archive(HORIZON_CUTOFF).await.unwrap();
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

/// **The refusal names `reconstruct`, and `reconstruct` must actually answer**
/// (W9.2, §3.2).
///
/// W9.1 chose the error over unioning the cold log into the fold, and an
/// error is only the right choice if the operation it redirects to gives the
/// caller the same answer. Nothing checked that. This is the finding the plan
/// calls *the one most likely to be "fixed" by a change nobody can
/// demonstrate*, and the demonstration is the round trip rather than the
/// branch: the fixture is archived across the horizon, `hydrate_attributes`
/// refuses, and `reconstruct` with the archive path returns exactly what
/// `hydrate_attributes` returned before the archive ran.
///
/// **The pre-archive reading is taken first and compared against, not
/// asserted twice.** A test that hard-codes `"first"` in both places passes if
/// both readers are wrong in the same way. Taking the live answer while the hot
/// log is still intact and then requiring the cold path to reproduce *that
/// value* is what makes the archive the only variable.
///
/// The last arm is the horizon itself: without the archive path the same
/// `reconstruct` fails rather than folding what is left, so the refusal above
/// is a property of the ledger and not an artifact of one reader.
#[tokio::test]
async fn what_the_hot_log_refuses_the_archive_path_still_answers() {
    let harness = TestHarness::new();
    let db = a_ledger_archived_across_a_horizon(&harness).await;
    let ids = vec!["c1".to_string()];

    // The truth, read while everything is still hot. Not a literal: see above.
    let truth = hydrate_attributes(
        db.read_conn(),
        &ids,
        &AsOf::recorded_at(HORIZON_INSTANT),
        AttributeMode::AtTime,
    )
    .await
    .expect("an intact hot log answers for any instant")
    .pop()
    .expect("the concept existed at the instant");

    let before = reconstruct(db.read_conn(), HORIZON_INSTANT, None, None)
        .await
        .expect("an intact hot log needs no archive");
    assert_eq!(
        before.concepts.get("c1"),
        Some(&truth),
        "the two readers must already agree before the archive splits them"
    );

    let report = db.archive(HORIZON_CUTOFF).await.unwrap();
    assert!(
        report.log_entries_archived >= 2,
        "the fixture needs the superseded rows to have actually moved: {report:?}"
    );

    // What the hot log can no longer answer.
    let err = hydrate_attributes(
        db.read_conn(),
        &ids,
        &AsOf::recorded_at(HORIZON_INSTANT),
        AttributeMode::AtTime,
    )
    .await
    .expect_err("the instant is past the horizon now");
    assert!(err.to_string().contains("reconstruct"), "{err}");

    // ...and what the operation it names does answer, from the same connection
    // plus the path the error says is the missing ingredient.
    let after = db
        .reconstruct(HORIZON_INSTANT)
        .await
        .expect("reconstruct takes the archive path and must answer");
    assert_eq!(
        after.concepts.get("c1"),
        Some(&truth),
        "the archive moved the row, it did not change what was believed"
    );

    // The horizon is real: the same call without the path refuses rather than
    // folding what the archive left behind.
    let err = reconstruct(db.read_conn(), HORIZON_INSTANT, None, None)
        .await
        .expect_err("a short hot log and no archive path is not answerable");
    assert!(
        matches!(err, macrame::DbError::ReplayCorrupt { .. }),
        "got {err:?}"
    );

    db.close().await.unwrap();
}
