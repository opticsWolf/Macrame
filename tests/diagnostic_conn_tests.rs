//! `diagnostic_conn()` is a boundary, `read_conn()` is a guardrail (T5.1, D-091).
//!
//! §4.7 invariant 2 says all writes are serialised through one connection, and
//! names the holes. `read_conn()`'s `PRAGMA query_only = ON` was cited as the
//! read-only diagnostic path, and T5.1's objection to that is exact: the pragma
//! is per-connection and its holder can turn it off in one statement.
//!
//! These tests assert the difference **in both directions**, because only the
//! pair means anything. Asserting that `diagnostic_conn()` refuses a write would
//! pass just as well if it were a `query_only` connection; what distinguishes
//! the two is what happens after `PRAGMA query_only = OFF`, and that is
//! [`turning_the_pragma_off_rescues_the_reader_and_not_the_diagnostic`].

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::prelude::*;

const TS: &str = "2026-01-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// A write the schema accepts, so a refusal is about permissions and not shape.
const INSERT: &str = "INSERT INTO concepts \
     (id, title, content, valid_from, valid_to, recorded_at, retired) \
     VALUES ('probe','P','','2026-01-01T00:00:00.000000Z', \
             '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)";

async fn db(harness: &TestHarness) -> Database {
    Database::open_with_cadence(&harness.db_path, None)
        .await
        .unwrap()
}

/// It reads, which is the capability it exists to provide.
#[tokio::test]
async fn a_diagnostic_connection_reads_and_explains() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    db.write_concepts(vec![ConceptUpsert::new("a", "A")
        .content("body")
        .valid_from(TS)])
        .await
        .unwrap();

    let conn = db.diagnostic_conn().await.unwrap();

    let mut rows = conn
        .query("SELECT COUNT(*) FROM concepts", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        n, 1,
        "the diagnostic connection sees the actor's committed write"
    );

    // The use T5.1 names first, and the one `raw()`'s old doc listed first.
    conn.query("EXPLAIN QUERY PLAN SELECT * FROM links_current", ())
        .await
        .expect("EXPLAIN QUERY PLAN must work on a diagnostic connection");

    db.close().await.unwrap();
}

/// It refuses writes, and so does the reader — this alone proves nothing.
#[tokio::test]
async fn both_read_paths_refuse_a_write() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    let diag = db.diagnostic_conn().await.unwrap();

    assert!(
        diag.execute(INSERT, ()).await.is_err(),
        "a read-only connection accepted an INSERT"
    );
    assert!(
        db.read_conn().execute(INSERT, ()).await.is_err(),
        "query_only stopped refusing writes"
    );

    db.close().await.unwrap();
}

/// **The test that distinguishes the two.**
///
/// `PRAGMA query_only = OFF` succeeds on both connections — the statement is
/// accepted either way, which is itself worth pinning, since a reader might
/// expect the read-only connection to reject the pragma rather than ignore it.
/// What differs is whether the *next write* lands.
///
/// If this ever passes in both arms, `diagnostic_conn()` has silently become a
/// second `read_conn()` and §4.7 would be describing a boundary that is a
/// pragma.
#[tokio::test]
async fn turning_the_pragma_off_rescues_the_reader_and_not_the_diagnostic() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    let diag = db.diagnostic_conn().await.unwrap();

    // The reader: the guardrail comes off, and the write lands.
    db.read_conn()
        .execute("PRAGMA query_only = OFF", ())
        .await
        .unwrap();
    db.read_conn().execute(INSERT, ()).await.expect(
        "query_only is a guardrail: turning it off must restore writes, \
                 and §4.7 invariant 2 depends on that being true",
    );
    // Re-arm, so `close()` does not run against a reader this test disarmed.
    db.read_conn()
        .execute("PRAGMA query_only = ON", ())
        .await
        .unwrap();

    // The diagnostic connection: the pragma is accepted and changes nothing.
    diag.execute("PRAGMA query_only = OFF", ()).await.unwrap();
    let err = diag
        .execute(
            "INSERT INTO concepts \
             (id, title, content, valid_from, valid_to, recorded_at, retired) \
             VALUES ('probe2','P','','2026-01-01T00:00:00.000000Z', \
                     '9999-12-31T23:59:59.999999Z','2026-01-01T00:00:00.000000Z',0)",
            (),
        )
        .await
        .expect_err(
            "SQLITE_OPEN_READ_ONLY was defeated by a PRAGMA, so diagnostic_conn() \
             is a guardrail and not a boundary",
        );
    assert!(
        err.to_string().contains("readonly"),
        "refused for the wrong reason: {err}"
    );

    db.close().await.unwrap();
}

/// Each call returns the caller's **own** connection.
///
/// The other half of the need `read_conn()` does not serve: it hands back a
/// shared `&Connection`, so a long reporting query competes with every
/// traversal and fold in the process. Two diagnostic connections must be able to
/// hold independent per-connection state.
#[tokio::test]
async fn each_diagnostic_connection_is_the_callers_own() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let a = db.diagnostic_conn().await.unwrap();
    let b = db.diagnostic_conn().await.unwrap();

    // `query_only` is per-connection state; setting it on one must not be
    // visible on the other. (Neither can write regardless — this is about
    // whether they are the same handle.)
    a.execute("PRAGMA query_only = ON", ()).await.unwrap();
    let mut rows = b.query("PRAGMA query_only", ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        v, 0,
        "the two diagnostic connections share per-connection state"
    );

    db.close().await.unwrap();
}

/// `path()` reports the file the handle opened, which is what
/// `diagnostic_conn()` reopens.
///
/// The accessor is the whole input to the read-only open, so a wrong `path()`
/// would give a diagnostic connection to some other database while every
/// assertion above still passed.
#[tokio::test]
async fn the_handle_reports_the_file_it_opened() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    assert_eq!(db.path(), harness.db_path.as_path());
    db.close().await.unwrap();
}

/// The missing-file error names the file and says why.
///
/// **This asserts the rendering, not the branch, and the distinction is stated
/// rather than glossed.** The branch is `!self.path.exists()`, and it is not
/// reachable from a live handle on Windows: deleting the file underneath an open
/// `Database` fails with OS error 32 (verified), and `close()` consumes the
/// handle, so no in-process sequence reaches it. It is reachable in the case it
/// was written for — a file removed by something outside this process between
/// open and the diagnostic call — which a test cannot stage here.
///
/// What *is* worth pinning is the part D-069 is about: an error that names the
/// wrong subject. `NotFound` renders "node {0} not found", and a caller handed
/// that for a missing database file would look for a concept. This fails if the
/// variant is ever collapsed into a generic one.
#[test]
fn the_missing_file_error_names_the_file_and_the_reason() {
    let err = macrame::DbError::DiagnosticConn {
        path: r"C:\somewhere\nope.db".to_string(),
        reason: "the file does not exist, and a read-only open cannot create it".into(),
    };
    let text = err.to_string();
    assert!(text.contains("nope.db"), "does not name the file: {text}");
    assert!(text.contains("read-only"), "does not say why: {text}");
    assert!(
        !text.contains("node"),
        "reads as an error about a node, which is D-069's defect: {text}"
    );
}

/// The actor still owns writes: a diagnostic connection sees them, after.
///
/// Guards the read-your-writes property a diagnostic tool depends on. WAL means
/// a reader can hold an older snapshot; this asserts a connection opened *after*
/// a committed write observes it, which is the case a person running a
/// diagnostic is actually in.
#[tokio::test]
async fn a_diagnostic_connection_opened_after_a_write_sees_it() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    db.write_concepts(vec![
        ConceptUpsert::new("a", "A").content("x").valid_from(TS),
        ConceptUpsert::new("b", "B").content("y").valid_from(TS),
    ])
    .await
    .unwrap();
    db.assert_edge(
        EdgeAssertion::new("a", "b", "LINKS")
            .valid_from(TS)
            .valid_to(OPEN),
    )
    .await
    .unwrap();

    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM links_current", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 1);

    db.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// It is configured now, which it was not until 0.12.16 (W5.5, D-159)
// ---------------------------------------------------------------------------

/// **A diagnostic connection waits like every other connection in the process.**
///
/// Until 0.12.16 this method opened a connection and returned it untouched, so
/// it ran with SQLite's default `busy_timeout` of **0** — fail immediately with
/// `SQLITE_BUSY` — while the writer, the shared reader and the cadence
/// connection all waited 5 s. That put the shortest fuse in the process on the
/// one surface whose stated job is to answer questions when the typed path is
/// already suspect, and it failed under precisely the contention that would
/// prompt someone to reach for it.
///
/// Asserted on the pragma rather than by racing a lock, because the value *is*
/// the behaviour and a contention test for a 5 s timeout is a 5 s test.
#[tokio::test]
async fn a_diagnostic_connection_carries_the_crates_busy_timeout() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn.query("PRAGMA busy_timeout", ()).await.unwrap();
    let timeout: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        timeout, 5000,
        "a diagnostic connection reports busy_timeout={timeout}; 0 means it is \
         being returned unconfigured again, which is the W5.5 finding"
    );

    db.close().await.unwrap();
}

/// **And it carries the reader's cache size, not the writer's.**
///
/// `diagnostic_conn` mints a connection per call, so it is on the plural side
/// of W5.4's split (D-158) — a caller opening several must not be multiplying
/// the writer's cache.
#[tokio::test]
async fn a_diagnostic_connection_carries_the_reader_cache_size() {
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

    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn.query("PRAGMA cache_size", ()).await.unwrap();
    let size: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        size, -8_000,
        "a diagnostic connection reports cache_size={size}; -64000 is the \
         writer's and -2000 means it was never configured"
    );

    db.close().await.unwrap();
}

/// **It still cannot write**, which is the property the configuration must not
/// have widened.
///
/// The whole value of this method over `read_conn()` is that
/// `SQLITE_OPEN_READ_ONLY` is an OS-level boundary rather than a pragma its
/// holder can turn off. Running pragmas on it is exactly the kind of change
/// that could quietly reopen it read-write, so the refusal is re-asserted here
/// beside the configuration rather than left to the tests above.
#[tokio::test]
async fn configuring_it_did_not_make_it_writable() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    let conn = db.diagnostic_conn().await.unwrap();

    assert!(
        conn.execute(INSERT, ()).await.is_err(),
        "a configured diagnostic connection accepted a write"
    );
    let _ = conn.query("PRAGMA query_only = OFF", ()).await;
    assert!(
        conn.execute(INSERT, ()).await.is_err(),
        "turning query_only off rescued the write, so the connection is not \
         actually read-only at the OS level"
    );

    db.close().await.unwrap();
}
