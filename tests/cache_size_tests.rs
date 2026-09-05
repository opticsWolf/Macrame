//! The writer's page cache and the readers' are two knobs, not one (W5.4, D-158).
//!
//! `PRAGMA cache_size` is per-connection, so the claim being tested is that the
//! two values land on *different* connections — not merely that a pragma ran.
//! The failure a single shared field would produce is invisible to any test
//! that sets one value and reads it back somewhere: the value would be right
//! everywhere, which is the bug.
//!
//! The writer is deliberately unnameable from outside the actor, so it cannot
//! be read back directly. What can be asserted is the half that would break:
//! that a large writer cache does **not** appear on the shared reader.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

/// SQLite's own default, in its own units: −2000 KiB, i.e. 2 MB.
const SQLITE_DEFAULT: i64 = -2000;

async fn cache_size(conn: &libsql::Connection) -> i64 {
    let mut rows = conn.query("PRAGMA cache_size", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// **Absence runs no pragma**, so the default stays SQLite's to define.
#[tokio::test]
async fn the_default_tuning_leaves_the_cache_where_sqlite_put_it() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning::default().cadence(CadencePolicy::Disabled),
    )
    .await
    .unwrap();

    assert_eq!(
        cache_size(db.read_conn()).await,
        SQLITE_DEFAULT,
        "a default Tuning changed the page cache; the default is SQLite's, and \
         restating it here would freeze a number this crate did not choose"
    );

    db.close().await.unwrap();
}

/// **The reader's value reaches the reader, and the writer's does not.**
///
/// This is the whole of W5.4. Two distinct values are passed and the shared
/// read connection is asked which one it got; a single shared field, or a
/// wiring mistake that passes the writer's value to both, fails here and
/// nowhere else in the suite.
#[tokio::test]
async fn the_reader_gets_the_reader_value_and_not_the_writer_value() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning::default()
            .cadence(CadencePolicy::Disabled)
            .writer_cache_size(-64_000)
            .reader_cache_size(-8_000),
    )
    .await
    .unwrap();

    let seen = cache_size(db.read_conn()).await;
    assert_eq!(
        seen, -8_000,
        "the shared reader reports {seen}; -64000 means the writer's value was \
         applied to both connections, and -2000 means the reader's never ran"
    );

    db.close().await.unwrap();
}

/// **Positive means pages and negative means KiB**, unchanged from SQLite.
///
/// Asserted because the temptation to normalise the two into one signed
/// "bytes" field is real, and a caller who knows the pragma would then be
/// silently wrong by a factor of the page size.
#[tokio::test]
async fn a_positive_value_is_passed_through_as_pages() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning::default()
            .cadence(CadencePolicy::Disabled)
            .reader_cache_size(4_000),
    )
    .await
    .unwrap();

    assert_eq!(
        cache_size(db.read_conn()).await,
        4_000,
        "a positive cache_size was reinterpreted; SQLite's units are the units"
    );

    db.close().await.unwrap();
}

/// **The database still works with a deliberately tiny cache.**
///
/// A cache of ten pages forces SQLite to spill and re-read constantly, which is
/// a fine way to find out that a pragma was applied somewhere it breaks
/// correctness rather than only performance.
#[tokio::test]
async fn a_tiny_cache_is_a_performance_choice_and_not_a_correctness_one() {
    let harness = TestHarness::new();
    let db = Database::open_tuned(
        &harness.db_path,
        Tuning::default()
            .cadence(CadencePolicy::Disabled)
            .writer_cache_size(10)
            .reader_cache_size(10),
    )
    .await
    .unwrap();

    let concepts: Vec<_> = (0..200)
        .map(|i| {
            ConceptUpsert::new(format!("c{i}"), format!("Concept {i}"))
                .content("x".repeat(512))
                .valid_from("2026-01-01T00:00:00.000000Z")
        })
        .collect();
    db.write_concepts(concepts).await.unwrap();

    let mut rows = db
        .read_conn()
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 200);

    db.close().await.unwrap();
}
