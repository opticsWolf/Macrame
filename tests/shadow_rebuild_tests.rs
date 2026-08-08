//! The chunked rebuild must be indistinguishable from the atomic one (T1.2, D-082).
//!
//! `rebuild_current` is one transaction and is the reference. This path splits
//! it across many, builds the answer in a second table, and replaces the schema
//! object underneath a live database — so it has far more ways to be subtly
//! wrong, and every one of them ends with a `links_current` that merely *looks*
//! right until something else touches it.
//!
//! That last part is why several of these tests do a write *after* the rebuild
//! and check again. The swap's most dangerous failure mode is not a wrong row
//! count — it is a correct row count on a table whose primary key, checks or
//! triggers did not survive, which is silent until the next assertion.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::integrity::audit_current;
use macrame::{ConceptUpsert, Database, DbError};

const T0: &str = "1970-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// A star of `keys` edges, each re-asserted `generations` times so `links` holds
/// superseded history and the projection has real work to do.
async fn seed(db: &Database, keys: usize, generations: usize) {
    let ids: Vec<String> = (0..=keys).map(|i| format!("c{i:05}")).collect();
    for chunk in ids.chunks(2_000) {
        db.write_concepts(
            chunk
                .iter()
                .map(|id| ConceptUpsert::new(id, "n").valid_from(T0))
                .collect(),
        )
        .await
        .unwrap();
    }

    for generation in 0..generations {
        let batch: Vec<_> = (0..keys)
            .map(|k| {
                EdgeAssertion::new(&ids[k], &ids[k + 1], "LINKS")
                    .valid_from(T0)
                    .valid_to(OPEN)
                    .weight(1.0 + generation as f64)
            })
            .collect();
        db.bulk_import(batch).await.unwrap();
    }
}

/// Every row of `links_current`, ordered, as one comparable value.
async fn snapshot(db: &Database) -> Vec<(String, String, String, f64, String)> {
    let mut out = Vec::new();
    let mut rows = db
        .read_conn()
        .query(
            "SELECT source_id, target_id, edge_type, weight, recorded_at \
             FROM links_current ORDER BY source_id, target_id, edge_type, valid_from",
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((
            r.get(0).unwrap(),
            r.get(1).unwrap(),
            r.get(2).unwrap(),
            r.get(3).unwrap(),
            r.get(4).unwrap(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// The obligation
// ---------------------------------------------------------------------------

/// The chunked path produces exactly the table the atomic path produces.
///
/// The load-bearing test. `keys` is well past [`SOURCES_PER_CHUNK`], so the fill
/// really does run many chunks and the keyset boundaries are exercised rather
/// than skipped — a version that projected only the first chunk and stopped
/// would pass a smaller fixture.
///
/// # Both rebuilds run against **one** seeding (0.12.0, W3)
///
/// This used to seed two databases identically and compare their results, which
/// worked only because `seed`'s `bulk_import` was deterministic: a fixed chunk
/// size meant a fixed number of chunks, and therefore the same `recorded_at` on
/// the same row in both. W3 made the chunk size a function of measured hold, so
/// the two seedings now disagree about the stamps — legitimately, and it is what
/// §5.1.6's note about machine-dependent boundaries means in practice.
///
/// Seeding once and running both rebuilds over the same rows removes the
/// dependence and is the stronger test anyway: it compares the two paths on
/// identical input rather than on input that was merely supposed to be
/// identical. The atomic rebuild runs second, so the chunked table has to
/// survive being reprojected over.
#[tokio::test]
async fn the_chunked_rebuild_produces_what_the_atomic_one_does() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, 900, 3).await;

    let report = db.rebuild_current_chunked().await.unwrap();
    assert_eq!(report.rows_rebuilt, 900);
    let chunked_first = snapshot(&db).await;

    db.rebuild_current().await.unwrap();
    let atomic = snapshot(&db).await;
    assert_eq!(
        chunked_first, atomic,
        "the two repairs disagree about current belief"
    );

    // And again from the atomic table, so the comparison is not an artifact of
    // which one ran first.
    db.rebuild_current_chunked().await.unwrap();

    let chunked = snapshot(&db).await;
    assert_eq!(
        chunked, atomic,
        "the two repairs disagree about current belief"
    );
    assert_eq!(
        audit_current(db.read_conn()).await.unwrap(),
        0,
        "the chunked rebuild left drift"
    );

    db.close().await.unwrap();
}

/// The swapped-in table is a *schema* object, not just rows.
///
/// This is the failure the probe found: `CREATE TABLE … AS SELECT` copies rows
/// and drops the primary key, so the swap succeeds, the row count is right, and
/// the next `INSERT INTO links` dies inside `trg_links_current_sync` with "ON
/// CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint". The
/// only way to catch it is to write afterwards.
#[tokio::test]
async fn writing_after_a_swap_still_maintains_the_projection() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, 40, 2).await;

    db.rebuild_current_chunked().await.unwrap();
    let before = snapshot(&db).await.len();

    // The sync trigger's ON CONFLICT target must exist...
    db.upsert_concept(ConceptUpsert::new("fresh", "f").valid_from(T0))
        .await
        .unwrap();
    db.assert_edge(
        EdgeAssertion::new("c00000", "fresh", "CITES")
            .valid_from(T0)
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot(&db).await.len(),
        before + 1,
        "trg_links_current_sync did not survive the swap"
    );

    // ...and re-asserting one key must update in place rather than duplicate,
    // which is the primary key doing its job.
    db.assert_edge(
        EdgeAssertion::new("c00000", "c00001", "LINKS")
            .valid_from(T0)
            .valid_to("2026-01-01T00:00:00.000000Z")
            .weight(99.0),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot(&db).await.len(),
        before + 1,
        "a re-assertion added a row: the swapped table has no primary key"
    );

    // The other trigger has to be back too, or D-060's guard is gone.
    let err = db
        .assert_edge(
            EdgeAssertion::new("c00000", "fresh", "CITES")
                .valid_from("2027-01-01T00:00:00.000000Z")
                .valid_to(OPEN),
        )
        .await
        .expect_err("trg_links_single_open did not survive the swap");
    assert!(
        matches!(err, DbError::SingleOpenViolation { .. }),
        "got {err:?}"
    );

    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
    db.close().await.unwrap();
}

/// The indexes come back, or every traversal silently gets slow.
///
/// Asserted as a plan shape rather than a duration (D-042): the recursive walk
/// must still be served by `idx_lc_traversal_cover`. A swap that forgot the
/// indexes leaves a correct table that scans, and nothing else in this file
/// would notice.
#[tokio::test]
async fn the_indexes_come_back_with_their_declared_names() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, 60, 2).await;
    db.rebuild_current_chunked().await.unwrap();

    let conn = db.read_conn();
    let mut names = Vec::new();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND tbl_name = 'links_current' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        names.push(r.get::<String>(0).unwrap());
    }

    assert_eq!(
        names,
        // `idx_lc_tgt_active` was here through v7 and was dropped by the v7 → v8
        // rung: no query in the crate seeks on `target_id` as a leading column
        // (D-089, D-118), so it was an index write per assertion buying nothing.
        vec![
            "idx_lc_open_interval".to_string(),
            "idx_lc_traversal_cover".to_string(),
        ],
        "the swap left links_current indexed under different names than \
         CREATE_INDICES declares, so the next migration would create a second \
         copy of each"
    );

    // And nothing is left behind under the shadow's name.
    let leftover: i64 = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%shadow%'",
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
    assert_eq!(leftover, 0, "the shadow table outlived the swap");

    db.close().await.unwrap();
}

/// Writes landing during the build are caught up, not lost.
///
/// The fill is chunked by `source_id`, so a write to a source the fill has
/// *already passed* is the case that needs the catch-up pass — it would
/// otherwise be missing from the shadow and vanish at the swap. Driven here by
/// interleaving on the low-priority channel, which is what a real background
/// rebuild competes with.
#[tokio::test]
async fn a_write_during_the_build_survives_the_swap() {
    let harness = TestHarness::new();
    let db = std::sync::Arc::new(harness.db_with_fake_clock().await);
    seed(&db, 900, 2).await;

    let writer = {
        let db = std::sync::Arc::clone(&db);
        tokio::spawn(async move {
            // The very first source, which the fill reaches immediately.
            db.assert_edge(
                EdgeAssertion::new("c00000", "c00001", "LINKS")
                    .valid_from(T0)
                    .valid_to("2030-01-01T00:00:00.000000Z")
                    .weight(1234.0),
            )
            .await
        })
    };

    db.rebuild_current_chunked().await.unwrap();
    writer.await.unwrap().unwrap();

    // Whether the write landed before or after the fill passed c00000, the
    // result must be current belief either way — that is what the catch-up is
    // for, and the race is the test.
    assert_eq!(
        audit_current(db.read_conn()).await.unwrap(),
        0,
        "a write during the shadow build left links_current diverged"
    );

    std::sync::Arc::into_inner(db)
        .unwrap()
        .close()
        .await
        .unwrap();
}

/// An orphan shadow from a crashed attempt does not poison the next one.
#[tokio::test]
async fn a_leftover_shadow_table_is_discarded_not_reused() {
    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, 30, 2).await;

    // Stand in for a crash: a shadow table with the right name and wrong rows.
    let raw = db.raw().connect().unwrap();
    raw.execute(
        "CREATE TABLE links_current_shadow (source_id TEXT, target_id TEXT)",
        (),
    )
    .await
    .unwrap();
    raw.execute(
        "INSERT INTO links_current_shadow VALUES ('junk', 'junk')",
        (),
    )
    .await
    .unwrap();

    db.rebuild_current_chunked().await.unwrap();

    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);
    assert_eq!(snapshot(&db).await.len(), 30);
    db.close().await.unwrap();
}

/// An archive during the build abandons the rebuild rather than swapping in a
/// stale table.
///
/// The archive's deletions are invisible to a catch-up keyed on `recorded_at` —
/// a deleted row has no `recorded_at` left to be found by — so the shadow would
/// resurrect archived history. Refusing is the conservative answer, and the
/// error says so: `RebuildInterrupted` means the repair did *not* run, which is
/// a different fact from `RebuildFailed`.
#[tokio::test]
async fn an_archive_during_the_build_abandons_the_rebuild() {
    use macrame::integrity::{ShadowOutcome, ShadowStep};

    let harness = TestHarness::new();
    let db = harness.db_with_fake_clock().await;
    seed(&db, 30, 3).await;

    // Driven step by step, because the point is what happens *between* steps
    // and `rebuild_current_chunked` deliberately gives no seam to interleave at.
    let ShadowOutcome::Started { build_start, epoch } =
        db.shadow_step(ShadowStep::Begin).await.unwrap()
    else {
        panic!("Begin returned the wrong outcome")
    };
    db.shadow_step(ShadowStep::Fill { after: None })
        .await
        .unwrap();

    harness.advance(std::time::Duration::from_secs(3_600));
    db.archive(&harness.clock.peek()).await.unwrap();

    let err = db
        .shadow_step(ShadowStep::Swap { build_start, epoch })
        .await
        .expect_err("an archive committed during the build");
    assert!(
        matches!(err, DbError::RebuildInterrupted { .. }),
        "got {err:?}"
    );

    // Untouched, not half-repaired: that is what RebuildInterrupted promises.
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);

    // And a retry now succeeds.
    db.rebuild_current_chunked().await.unwrap();
    assert_eq!(audit_current(db.read_conn()).await.unwrap(), 0);

    db.close().await.unwrap();
}
