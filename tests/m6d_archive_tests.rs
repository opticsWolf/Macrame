//! M6-d: the archive detector's stories on an overlay store (P-4; Amendment 2
//! §11's M6-d row, §12's red-first names 8–9).
//!
//! The pour is the real-reader oracle's own scenario (the shared furniture),
//! so the ledger this file spreads out carries overlay versions and cuts on
//! three forked lineages beside the trunk's eras — exactly the mixture the
//! archive predicate's questions now have to answer with. The oracle itself
//! speaks after the archive session, too: whatever the cold arm, the walk and
//! the fold resolve, the reader grid re-runs against the same model word for
//! word. The one reader that does not follow is `concepts_by_id` below the
//! hot floor: the 0.15.19 law (D-189) refused connection-only folds before
//! the overlay existed, and refuses them here too — pinned, not changed.
//!
//! Stories the file runs:
//!
//! - `an_archive_keeps_a_concept_whose_branch_version_is_still_reachable`
//!   (row 8: the concept stays resolvable on the lineage whose version still
//!   covers, at current belief, through the never-archived overlay tables;
//!   the archive predicate moves ONLY the classic rows its clauses name —
//!   and the fork-window guard names every row a per-lineage pick needs);
//! - `the_interval_ledger_verifies_clean_with_one_id_versioned_on_two_
//!   lineages` (row 9: the per-`(id, branch_id)` audit sees the overlay's
//!   rows as intervals of the writing lineage alone; a lie raw SQL patches
//!   into one lineage's claim list gets reported by name);
//! - the rehydrate pair: a born-retired concept with a closed window goes
//!   cold whole and comes back whole; a ledger never archived reports nothing
//!   and does not leave a 0-byte cold file behind (0.15.19's C-13 law);
//! - the change feed (P-5's cone): an overlay row is a log row with its
//!   writer's branch_id, so D-311's filter needs no overlay change — pinned.

#[path = "common/m6_fixture.rs"]
mod fixture;
#[path = "common/harness.rs"]
mod harness;

use fixture::*;
use harness::TestHarness;

use macrame::prelude::*;
use macrame::temporal::AsOf;

/// The archive cutoff a whole store of January 2026 windows and microsecond
/// stamps easily clears.
const CUTOFF: &str = "2027-06-01T00:00:00.000000Z";

/// The mirror tables whose rows the cold file must never hold.
const MIRROR_TABLES: &str = "table_name IN ('concept_versions', 'concept_cuts')";

/// A local handle for the raw patch the negative control writes. The read
/// connection is query-only, and a ledger written outside the write law is
/// exactly what the interval verifier claims to catch — the manual write is
/// how the patch gets past the write guards.
async fn local_conn(path: &std::path::Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

/// One scalar read against a connection.
async fn scalar_of(conn: &libsql::Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

// ---------------------------------------------------------------------------
// Row shapes the archive statements name, as both files read them
// ---------------------------------------------------------------------------

/// One stored row of the classic log, seen by file.
fn concept_row_shape(entity: &str, start: &str, end: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM transaction_log \
         WHERE table_name = 'concepts' AND entity_id = '{entity}' \
           AND json_extract(payload, '$.valid_from') = '{start}' \
           AND json_extract(payload, '$.valid_to')   = '{end}'",
    )
}

// ---------------------------------------------------------------------------
// Story 8
// ---------------------------------------------------------------------------

/// After an archive session the ledger's law is where it always was: the
/// overlay's tables never moved, the classic rows moved by their own clauses
/// keep answering through the cold fold, per-lineage picks are protected by
/// the archive predicate's fork-window guard, and the branch's own version
/// still reaches it at current belief.
#[tokio::test]
async fn an_archive_keeps_a_concept_whose_branch_version_is_still_reachable() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    let report = db.archive(CUTOFF).await.unwrap();
    assert!(
        report.log_entries_archived > 0,
        "the session had classic rows to move: {report:?}"
    );

    // The overlay's rows never move: the scanner's table arm and the cold
    // fold's §5.3 channels read them in place, forever.
    let hot_mirrors = scalar_of(
        db.read_conn(),
        &format!("SELECT COUNT(*) FROM transaction_log WHERE {MIRROR_TABLES}"),
    )
    .await;
    assert_eq!(
        hot_mirrors as usize,
        m.versions.len() + m.cuts.len(),
        "every mirror row the pour wrote stays hot: the never-archived law"
    );
    let cold = local_conn(db.archive_path()).await;
    let cold_mirrors = scalar_of(
        &cold,
        &format!("SELECT COUNT(*) FROM transaction_log WHERE {MIRROR_TABLES}"),
    )
    .await;
    assert_eq!(cold_mirrors, 0, "the cold file holds no mirror rows");

    // era one stays hot, and the file holds none of it — the mint row is
    // the row b1's read picks (its fork point is the mint's own stamp: the
    // fork-window guard is inclusive at the row, because a lineage forked at
    // the instant a row was recorded reads exactly that row), and the close
    // is the pair's own newest write. What "moves" in this store is under
    // the reach law rid of nothing a per-lineage pick reads.
    let era_one = concept_row_shape("c", &v(1), OPEN);
    assert_eq!(
        scalar_of(db.read_conn(), &era_one).await,
        1,
        "the mint a fork-at-the-instant read picks stays hot"
    );
    assert_eq!(scalar_of(&cold, &era_one).await, 0, "it never moved");
    let close = concept_row_shape("c", &v(1), &v(7));
    assert_eq!(
        scalar_of(db.read_conn(), &close).await,
        1,
        "the close is the pair's newest write and stays"
    );

    // era three moves: its start's rewrite (the pivot at V13) landed with no
    // fork between them, and era three's open row is nobody's answer above
    // the fork points — the trunk's window reads past it, the branches' caps
    // stop before it.
    let era_three = concept_row_shape("c", &v(12), OPEN);
    assert_eq!(
        scalar_of(&cold, &era_three).await,
        1,
        "era three's open row moved: no fork between its write and its rewrite"
    );
    assert_eq!(
        scalar_of(db.read_conn(), &era_three).await,
        0,
        "and left the hot log"
    );
    let pivot = concept_row_shape("c", &v(12), &v(13));
    assert_eq!(
        scalar_of(db.read_conn(), &pivot).await,
        1,
        "the rewrite that superseded it stays UTC-hot"
    );

    // d's mint stays hot, and the cold file has none of it: b0's fork lands
    // between the mint and its pair's rewrite, which the fork-window guard
    // refuses to cross — a per-lineage pick reads it.
    let d_mint = concept_row_shape("d", &v(3), OPEN);
    assert_eq!(
        scalar_of(db.read_conn(), &d_mint).await,
        1,
        "the fork-window guard keeps d's mint row hot"
    );
    assert_eq!(scalar_of(&cold, &d_mint).await, 0, "it never moved");

    // The oracle speaks again, at every cell, either arm.
    for r in act_bounds(&m) {
        for viewer in VIEWERS {
            let state = db
                .reconstruct_on(&r, viewer)
                .await
                .unwrap_or_else(|e| panic!("reconstruct_on({r}, {viewer}) after the archive: {e}"));
            for id in IDS {
                let want = m.on(viewer, id, &r);
                let got = state.concepts.get(id).map(|n| n.title.clone());
                state_cell(
                    got,
                    want.as_ref(),
                    format!("after the archive: reconstruct_on({r}, {viewer}) :: {id}"),
                );
            }
        }
        let state = db
            .reconstruct(&r)
            .await
            .unwrap_or_else(|e| panic!("reconstruct({r}) after the archive: {e}"));
        for id in IDS {
            let want = m.whole_recorded(id, &r);
            let got = state.concepts.get(id).map(|n| n.title.clone());
            state_cell(
                got,
                want.as_ref(),
                format!("after the archive: reconstruct({r}) :: {id}"),
            );
        }
        for k in 1..=13 {
            let at = v(k);
            let state = db
                .reconstruct_as_of(AsOf::bitemporal(at.clone(), r.clone()))
                .await
                .unwrap_or_else(|e| panic!("reconstruct_as_of({at}, {r}) after the archive: {e}"));
            for id in IDS {
                let want = m.whole_valid(id, &at, Some(&r));
                let got = state.concepts.get(id).map(|n| n.title.clone());
                state_cell(
                    got,
                    want.as_ref(),
                    format!("after the archive: reconstruct_as_of({at}, {r}) :: {id}"),
                );
            }
        }
    }

    // ...including the valid-only cells ...
    for k in 1..=13 {
        let at = v(k);
        let state = db
            .reconstruct_as_of(AsOf::valid_at(at.clone()))
            .await
            .unwrap_or_else(|e| panic!("reconstruct_as_of(valid {at}) after the archive: {e}"));
        for id in IDS {
            let want = m.whole_valid(id, &at, None);
            let got = state.concepts.get(id).map(|n| n.title.clone());
            state_cell(
                got,
                want.as_ref(),
                format!("after the archive: reconstruct_as_of(valid {at}) :: {id}"),
            );
        }
    }

    // D-189's law, now with an overlay in the ledger: the connection-only
    // per-lineage reader refuses below the hot floor rather than folding a
    // short state — and answers whole at the newest surviving stamp, where
    // the fold is complete without the cold file.
    let bounds = act_bounds(&m);
    let last = bounds.last().expect("act stamps").clone();
    for r in &bounds {
        if *r == last {
            continue;
        }
        let plan = ReadPlan::new()
            .valid_at(v(7).clone())
            .recorded_at(r.clone());
        let got = db.concepts_by_id(&["c".to_string()], plan).await;
        assert!(
            matches!(
                got,
                Err(macrame::DbError::RecordedInstantUnreachable { .. })
            ),
            "concepts_by_id at r={r}, below the horizon after the archive, must refuse: {got:?}"
        );
    }
    let bounds_hot: Vec<&String> = bounds.iter().filter(|r| **r == last).collect();
    for r in bounds_hot {
        for k in 1..=13 {
            let at = v(k);
            for viewer in VIEWERS {
                for id in IDS {
                    by_id_cell(&db, &m, viewer, id, Some(&at), Some(r)).await;
                }
            }
        }
    }

    // Current belief, no recorded bound: the branch's own version still
    // reaches it (b0's open window answers its cells; the retired trunk ones
    // stay absent; b1's cut keeps its walk stopped) — the same r=None grid
    // the oracle file ran, now from an archived ledger.
    for k in 1..=13 {
        let at = v(k);
        for viewer in VIEWERS {
            for id in IDS {
                by_id_cell(&db, &m, viewer, id, Some(&at), None).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Story 9
// ---------------------------------------------------------------------------

/// One id carried versioned on three lineages verifies clean; a lie patched
/// into one lineage's claim list by raw SQL is reported by name.
#[tokio::test]
async fn the_interval_ledger_verifies_clean_with_one_id_versioned_on_two_lineages() {
    let harness = TestHarness::new();
    let (db, _m) = hold(&harness).await;

    let overlaps = db.verify_concept_intervals().await.unwrap();
    assert!(
        overlaps.is_empty(),
        "the write law's own ledger verifies clean: {overlaps:?}"
    );

    // The lie: era one's close row is patched, on the log it lives in, to
    // overlap era two. The verifier reads the fold's own claim list (one
    // current belief per valid start), so the patched row is `c`'s existing
    // belief at V1 and the report names both intervals.
    let conn = local_conn(&harness.db_path).await;
    let patch = format!(
        "UPDATE transaction_log \
         SET payload = json_set(payload, '$.valid_to', '{leak}') \
         WHERE table_name = 'concepts' AND entity_id = 'c' AND branch_id = 'main' \
           AND json_extract(payload, '$.valid_from') = '{start}' \
           AND json_extract(payload, '$.valid_to')   = '{end}'",
        leak = v(10),
        start = v(1),
        end = v(7),
    );
    let patched = conn.execute(&patch, ()).await.unwrap();
    assert_eq!(
        patched, 1,
        "exactly the close row was patched, not the mint"
    );

    let overlaps = db.verify_concept_intervals().await.unwrap();
    assert_eq!(
        overlaps.len(),
        1,
        "one overlap, from the one patched pair: {overlaps:?}"
    );
    let ov = &overlaps[0];
    assert_eq!(ov.id, "c", "the lie is about c");
    // the claimed (younger) row: era two's current belief at V7
    assert_eq!(ov.valid_from, v(7), "the claimed row's own start");
    assert_eq!(ov.valid_to, v(12), "the claimed row's own end");
    // the existing (patched) row: the close of era one, as the patch left it
    assert_eq!(ov.existing_from, v(1), "the patched row's start");
    assert_eq!(ov.existing_to, v(10), "the patched row's end under the lie");
}

// ---------------------------------------------------------------------------
// Rehydrate, both ways
// ---------------------------------------------------------------------------

/// A born-retired concept with a closed window is the one shape the archive
/// predicate removes physically; rehydrate brings the row back, and the
/// readers speak its absence either way (the law the oracle names: retired
/// rows show nowhere). No overlay rows ever leave, so nothing about the pair
/// of them changes.
#[tokio::test]
async fn a_rehydrated_concept_comes_back_and_the_reads_still_speak_the_law() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(
        ConceptUpsert::new("f", "f born retired")
            .retired(true)
            .valid_from(v(4))
            .valid_to(v(10)),
    )
    .await
    .unwrap();

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(
        report.concepts_archived, 1,
        "the born-retired concept went cold: {report:?}"
    );
    let cold = local_conn(db.archive_path()).await;
    assert_eq!(
        scalar_of(&cold, "SELECT COUNT(*) FROM concepts WHERE id = 'f'").await,
        1,
        "the row is kept, not dropped"
    );
    // ... and the projection no longer names it
    assert_eq!(
        scalar_of(
            db.read_conn(),
            "SELECT COUNT(*) FROM concepts WHERE id = 'f'"
        )
        .await,
        0,
        "the hot projection row went with it"
    );

    // The recorded-axis fold still answers from the log: f is retired, so it
    // shows nowhere.
    let state = db.reconstruct("2027-06-01T00:00:00.000000Z").await.unwrap();
    assert!(
        !state.concepts.contains_key("f"),
        "a retired concept answers absent, archived or not"
    );

    let rep = db.rehydrate(&["f"]).await.unwrap();
    assert_eq!(rep.concepts_rehydrated, 1, "the row came back: {rep:?}");
    assert_eq!(
        scalar_of(
            db.read_conn(),
            "SELECT COUNT(*) FROM concepts WHERE id = 'f'"
        )
        .await,
        1,
        "the projection names it again"
    );

    // And the law is unmoved: retired, still absent at current belief.
    let state = db.reconstruct("2027-06-01T00:00:00.000000Z").await.unwrap();
    assert!(
        !state.concepts.contains_key("f"),
        "rehydration does not resurrect retirement"
    );

    // ... and the valid-axis fold's claim list is clean again
    let overlaps = db.verify_concept_intervals().await.unwrap();
    assert!(
        overlaps.is_empty(),
        "the verifier stays clean: {overlaps:?}"
    );

    db.close().await.unwrap();
}

/// Rehydrate on a ledger that was never archived: nothing moved, and no
/// 0-byte cold file was left behind (0.15.19, review C-13's law — the create
/// on ATTACH would have broken every later read in the crate).
#[tokio::test]
async fn a_rehydrate_on_a_never_archived_ledger_reports_nothing_and_leaves_no_file() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    let rep = db
        .rehydrate(&["x"])
        .await
        .expect("rehydrate without an archive answers, not errors");
    assert_eq!(
        (rep.concepts_rehydrated, rep.rowids_reassigned),
        (0, 0),
        "nothing was rehydrated, by the law's own report"
    );
    assert!(
        !db.archive_path().exists(),
        "the 0-byte cold file stayed away: the whole point of archive_present"
    );

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// The change feed (P-5's cone, pinned)
// ---------------------------------------------------------------------------

/// An overlay row is a log row with its writer's branch_id: the feed's own
/// D-311 filter needs no overlay change, so the branch's windows and cuts are
/// its rows to ferry.
#[tokio::test]
async fn a_change_feed_sees_a_version_row_cut_to_the_writer_s_branch() {
    let harness = TestHarness::new();
    let (db, _m) = hold(&harness).await;

    let rows = db
        .changes(0, Some("b0"), &["concept_versions", "concept_cuts"], 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "b0's takeover wrote one pair: {rows:?}");
    assert!(
        rows.iter().all(|row| row.branch_id == "b0"),
        "the mirrors carry the writer's lineage: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.table == "concept_versions"),
        "the version mirror is on the feed: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.table == "concept_cuts"),
        "the cut that rides the pair is on the feed: {rows:?}"
    );

    // The filter cuts siblings out: b1's windowed pair and b2's pair plus
    // its standalone cut are none of b0's feed.
    for (branch, want) in [("b1", 2), ("b2", 3)] {
        let rows = db
            .changes(0, Some(branch), &["concept_versions", "concept_cuts"], 100)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            want,
            "{branch}'s own rows stay on {branch}'s feed: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.branch_id == branch),
            "{branch}'s feed carries only its rows: {rows:?}"
        );
    }

    let trunk = db
        .changes(0, Some("main"), &["concept_versions", "concept_cuts"], 100)
        .await
        .unwrap();
    assert!(
        trunk.is_empty(),
        "the trunk's classic supersede wrote nothing to the overlay: {trunk:?}"
    );
}

// ---------------------------------------------------------------------------
// The per-lineage hint arm (M6-f; the amendment §5.4 cost 5's fourth cold
// reader: `archive_hint` answers per lineage)
// ---------------------------------------------------------------------------

/// A reader that named a lineage is told where that lineage's own surviving
/// log rows begin — the whole-ledger sentence answers neither branch's
/// question, because two lineages archive differently. The trunk's error
/// carries only the whole-ledger sentence.
#[tokio::test]
async fn the_archive_hint_answers_per_lineage_for_a_reader_naming_one() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;
    db.archive(CUTOFF).await.unwrap();

    // Move the archive file away so the reach refusals cannot attach (the
    // "does not exist" arm is where the hint is spoken).
    let moved = db.archive_path().with_extension("moved-away");
    std::fs::rename(db.archive_path(), &moved).unwrap();

    // A branch caller below the hot floor gets its own floor named.
    let r = act_bounds(&m)[0].clone();
    let bound = r.clone();
    let err = db
        .reconstruct_on(&bound, "b0")
        .await
        .expect_err("below the floor, with the archive gone, the fold refuses");
    let text = err.to_string();
    assert!(
        text.contains("lineage b0's own"),
        "the per-lineage hint names the writer's own surviving rows: {text}"
    );

    // The whole-ledger reader's error does not pretend to.
    let err = db
        .reconstruct(&bound)
        .await
        .expect_err("same refusal, no lineage named");
    let text = err.to_string();
    assert!(
        !text.contains("lineage "),
        "the whole-ledger hint carries no per-lineage sentence: {text}"
    );

    std::fs::rename(&moved, db.archive_path()).unwrap();
}
