//! Concept archival: the move, its partition, and what `reconstruct` does about
//! it (0.9.0, C2, [D-128]).
//!
//! `links` archival has been covered since 0.5.3. These are the tests for the
//! thing v9 made possible — a concept physically leaving the hot table — and the
//! reason they are in their own binary is that almost every assertion here is
//! about a *boundary*: hot against cold, entity data against derived data, and
//! the answer `reconstruct` gives on either side of an archive.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::graph::EdgeAssertion;
use macrame::{Annotation, ConceptUpsert, Database};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-02-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
/// **The cutoff is in the future, and it has to be.**
///
/// `recorded_at` is transaction time and is stamped by the crate, never by the
/// caller (Doctrine II) — so every concept these tests write is recorded at
/// *now*. `CONCEPTS_ARCHIVABLE` requires `recorded_at < :cutoff`, which means a
/// cutoff in the past archives nothing at all no matter what the valid-time
/// columns say. A test that used a past cutoff here would pass a predicate with
/// the `retired` clause deleted, because the answer would be the empty set
/// either way.
const CUTOFF: &str = "2099-01-01T00:00:00.000000Z";
const ARCHIVED_AT: &str = "2099-01-02T00:00:00.000000Z";

/// Three concepts: one archivable, two not, and the two are not-archivable for
/// *different* reasons so a predicate that lost either clause would show.
///
/// Five concepts, and the four unarchivable ones each fail a **different**
/// clause — which is the shape C1's injection run showed is necessary. A fixture
/// where every rejection is caused by the same clause proves only that clause.
///
/// * `gone` — retired, valid time closed before the cutoff, no edges. The only
///   archivable one.
/// * `live` — retired and expired exactly like `gone`, but an edge still names
///   it. Fails **reachability** alone.
/// * `expired` — expired and unreferenced, but not retired. Fails **`retired`**
///   alone.
/// * `retired_open` — retired and unreferenced, but its valid time is still
///   open. Fails **`valid_to`** alone.
/// * `target_only` — retired and expired like `gone`, and named by an edge it is
///   the **target** of and never the source. Fails reachability, but only for a
///   predicate that checks *both* endpoints.
/// * `other` — the far end of `live`'s edge; unremarkable, and there so the edge
///   has somewhere to point.
///
/// `target_only` exists because without it this fixture passed an injection that
/// narrowed the reachability check to `source_id` alone — the same hole, found
/// the same way, as the one `ARCHIVE_NODES` documents in
/// `integrity_property_tests`. Every other concept that an edge names is a
/// source of one, so "appears as a source" and "appears at all" coincided.
///
/// `recorded_at` has no fixture here and cannot have one: transaction time is
/// crate-stamped, so every row in this binary shares it. That clause is covered
/// by injection in `integrity_property_tests` (D-128), which is the right place
/// for it — the generator controls the clocks there and this binary does not.
async fn seeded(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    for (id, retired, valid_to) in [
        ("gone", true, T1),
        ("live", true, T1),
        ("expired", false, T1),
        ("retired_open", true, OPEN),
        ("target_only", true, T1),
        ("other", false, T1),
    ] {
        db.upsert_concept(
            ConceptUpsert::new(id, "Title")
                .content(format!("searchable body of {id}"))
                .valid_from(T0)
                .valid_to(valid_to)
                .retired(retired),
        )
        .await
        .unwrap();
    }
    // `live` keeps an edge, and the edge stays hot: its interval is open, so
    // LINKS_ARCHIVABLE will not take it and the concept stays reachable.
    db.assert_edge(EdgeAssertion::new("live", "other", "KNOWS").valid_from(T0))
        .await
        .unwrap();
    // `target_only` is named here and nowhere else, and never as a source.
    db.assert_edge(EdgeAssertion::new("live", "target_only", "KNOWS").valid_from(T0))
        .await
        .unwrap();
    db
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.read_conn()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

#[tokio::test]
async fn an_archivable_concept_moves_and_the_others_stay() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(
        report.concepts_archived, 1,
        "exactly `gone` should have crossed"
    );

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM concepts WHERE id = 'gone'").await,
        0,
        "the archived concept is still in the hot table"
    );
    for id in ["live", "expired", "retired_open", "target_only", "other"] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM concepts WHERE id = '{id}'")
            )
            .await,
            1,
            "{id} should not have been archived"
        );
    }

    db.close().await.unwrap();
}

/// **Every column crosses, `content` included** — the decision D-129 records.
///
/// Archival is a move, not a rewrite, and a move that drops a column is a
/// rewrite. The log payload for a concept carries its `content` (§4.3), so a
/// cold concept with its text dropped would contradict `cold.transaction_log`
/// about itself and rehydration would return a concept the ledger never
/// recorded — the unexplained absence Doctrine V exists to prevent.
///
/// Asserted column by column rather than by row count, because a `SELECT` that
/// forgot a column produces exactly one row either way.
#[tokio::test]
async fn the_cold_row_carries_every_column_the_hot_row_had() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    let hot: Vec<String> = {
        let mut rows = db
            .read_conn()
            .query(
                "SELECT id, title, content, valid_from, valid_to, recorded_at, retired \
                 FROM concepts WHERE id = 'gone'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        (0..7).map(|i| format!("{:?}", row.get_value(i).unwrap())).collect()
    };

    db.archive(CUTOFF).await.unwrap();

    let conn = db.read_conn();
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![db.archive_path().to_string_lossy().as_ref()],
    )
    .await
    .unwrap();
    let cold: Vec<String> = {
        let mut rows = conn
            .query(
                "SELECT id, title, content, valid_from, valid_to, recorded_at, retired \
                 FROM cold.concepts WHERE id = 'gone'",
                (),
            )
            .await
            .unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("the archived concept should be in cold.concepts");
        (0..7).map(|i| format!("{:?}", row.get_value(i).unwrap())).collect()
    };
    conn.execute("DETACH DATABASE cold", ()).await.unwrap();

    assert_eq!(hot, cold, "the cold row is not the hot row it replaced");
    assert!(
        cold[2].contains("searchable body of gone"),
        "content did not cross: {:?}",
        cold[2]
    );

    db.close().await.unwrap();
}

/// The FTS index follows the concept out — `trg_concepts_fts_delete` firing for
/// real, inside the archive session that made it reachable.
#[tokio::test]
async fn the_search_index_does_not_keep_an_archived_concept() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'gone'"
        )
        .await,
        1,
        "the fixture should be findable before the archive"
    );

    db.archive(CUTOFF).await.unwrap();

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'gone'"
        )
        .await,
        0,
        "the search index still matches a concept that is no longer hot"
    );

    db.close().await.unwrap();
}

/// **`reconstruct` is unaffected, and this test exists to find out whether that
/// is true rather than to confirm that it is.**
///
/// C2 specifies that `reconstruct` should fold cold concepts by the same
/// last-writer-wins `seq_id` rule used for the log. That step assumes the fold
/// reads the `concepts` table. It does not: `reconstruct` is driven entirely by
/// `transaction_log`, whose `'I'` and `'U'` payloads carry the concept's fields,
/// and archival moves no log rows for a concept because concepts have no delete
/// log trigger. So the answer should be **bit-identical across the boundary**,
/// with no new code at all.
///
/// If that is right, C2's step 4 is a no-op the design had already paid for and
/// the finding belongs in the register. If it is wrong, this fails and says so.
#[tokio::test]
async fn reconstruct_gives_the_same_answer_across_an_archive() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    let before = db.reconstruct(ARCHIVED_AT).await.unwrap();
    db.archive(CUTOFF).await.unwrap();
    let after = db.reconstruct(ARCHIVED_AT).await.unwrap();

    assert_eq!(
        before.concepts, after.concepts,
        "archiving a concept changed what the ledger says was believed — the \
         fold is not log-driven after all, and C2 step 4 is real work"
    );
    assert_eq!(before.edges, after.edges, "the edge set moved too");

    db.close().await.unwrap();
}

/// Derived rows are disposed of, not moved (Doctrine VII), and the disposal is
/// what makes the delete legal at all — `analytics_annotations` has a foreign
/// key into `concepts` and would refuse it.
#[tokio::test]
async fn derived_rows_are_disposed_of_rather_than_archived() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    db.write_analytics_annotations(vec![Annotation::new("gone", "louvain.community", "7")])
        .await
        .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM analytics_annotations WHERE concept_id = 'gone'"
        )
        .await,
        1
    );

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.concepts_archived, 1);

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM analytics_annotations WHERE concept_id = 'gone'"
        )
        .await,
        0,
        "the annotation outlived the concept it annotates"
    );

    db.close().await.unwrap();
}

/// **The sparse-rowid case, made sparse by an archive rather than by hand**
/// (C2 exit gate).
///
/// `vacuum_preserves_a_sparse_rowid_pk` seeds its gaps through `raw()` and says
/// so: through v8 there was no supported way to remove a concept, so the gaps
/// had to be written directly and the test was *standing in* for what archival
/// would eventually do. This is that test with the stand-in removed. The gaps
/// here are the real thing — rows that left through the archive path — and what
/// is asserted is that `VACUUM` afterwards leaves both the numbering and the FTS
/// index alone.
///
/// It matters because `concepts_fts` is external-content keyed on `rowid_pk`
/// (D-119). If `VACUUM` renumbered a sparse `rowid_pk`, the index would point at
/// the wrong rows with no error and no integrity-check failure — and until an
/// archive could actually create a gap, nothing in the suite had ever produced
/// the precondition outside a hand-written fixture.
#[tokio::test]
async fn vacuum_after_an_archive_leaves_the_sparse_numbering_and_the_index_alone() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    // Alternate archivable and not, so the survivors are genuinely interleaved
    // rather than a contiguous tail.
    for i in 0..8 {
        let archivable = i % 2 == 0;
        db.upsert_concept(
            ConceptUpsert::new(format!("c{i}"), "Title")
                .content(format!("searchable body {i}"))
                .valid_from(T0)
                .valid_to(if archivable { T1 } else { OPEN })
                .retired(archivable),
        )
        .await
        .unwrap();
    }

    let report = db.archive(CUTOFF).await.unwrap();
    assert_eq!(report.concepts_archived, 4, "four concepts should have left");

    let survivors = |db: &Database| {
        let conn = db.read_conn().clone();
        async move {
            let mut rows = conn
                .query("SELECT rowid_pk, id FROM concepts ORDER BY rowid_pk", ())
                .await
                .unwrap();
            let mut v = Vec::new();
            while let Some(r) = rows.next().await.unwrap() {
                v.push((r.get::<i64>(0).unwrap(), r.get::<String>(1).unwrap()));
            }
            v
        }
    };

    let before = survivors(&db).await;
    assert_eq!(before.len(), 4);
    assert!(
        before.windows(2).any(|w| w[1].0 - w[0].0 > 1),
        "the archive did not leave a gap, so this test proves nothing: {before:?}"
    );

    db.raw()
        .connect()
        .unwrap()
        .execute("VACUUM", ())
        .await
        .unwrap();

    assert_eq!(
        survivors(&db).await,
        before,
        "VACUUM renumbered a rowid_pk left sparse by an archive — concepts_fts \
         is keyed on this column and now points at the wrong rows"
    );

    // The index agrees with the table on both sides of the boundary.
    for i in 0..8 {
        let matched = count(
            &db,
            &format!("SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH 'body AND {i}'"),
        )
        .await;
        assert_eq!(
            matched,
            i64::from(i % 2 != 0),
            "concept c{i} is findable in the index but should not be, or vice versa"
        );
    }

    db.close().await.unwrap();
}
