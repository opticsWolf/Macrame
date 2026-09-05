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

/// Every call returns the **same** connection, and per-connection state is
/// therefore shared between diagnostic callers (0.15.14, W15.4, C-9, D-256).
///
/// # What this test used to assert, and why it now asserts the opposite
///
/// Until 0.15.14 this was `each_diagnostic_connection_is_the_callers_own`, and
/// it set `PRAGMA query_only` on one connection to prove the other could not
/// see it. That was the second half of D-091's argument: `read_conn()` hands
/// back a shared `&Connection`, so a long reporting query competes with every
/// traversal and fold, and `diagnostic_conn()` gave each caller their own.
///
/// C-9 asked for the file to be opened once per `Database` instead of once per
/// call, and the measurement behind that ask turned out to be sharper than the
/// ask (`examples/diagnostic_conn_probe.rs`). `Builder::…build()` — the call
/// every document in this crate named as *the open* — costs **0.10 µs and
/// opens nothing**: it succeeds against a path that does not exist. The open
/// is `connect()`, at **51.5 µs of an 82.7 µs call**, and it is also where R15
/// lives on this path: 48 threads through the unlocked Python binding gave
/// **3 bad runs in 30** with a connection per call and **0 in 30** with one
/// shared connection. Caching the *handle* and minting a connection per call —
/// the shape that would have preserved this test — removes 0.10 µs of 82.7 and
/// leaves the crash at 2 in 30.
///
/// So the property is given up deliberately, and the half of D-091 that
/// mattered survives: this is still not `read_conn()`. A reporting query here
/// still does not compete with the traversals and folds on the shared reader,
/// and the `SQLITE_OPEN_READ_ONLY` boundary is untouched — see
/// `turning_the_pragma_off_rescues_the_reader_and_not_the_diagnostic`, which
/// is the assertion D-091 was actually for.
///
/// What is lost is isolation *between diagnostic callers*, and it is asserted
/// here rather than admitted in prose, because `diagnostic_query` is the one
/// arbitrary-SQL surface this crate exposes.
///
/// **Narrowed by 0.15.15 to exactly this** (W15.5, [D-257]). An open
/// transaction, a temp table and an `ATTACH` are scrubbed on entry now; what
/// is still shared is a pragma the crate does not itself set, which
/// `query_only` is. So this test went on asserting the same thing while the
/// three sharper cases stopped being true — which is why the residue has a
/// test of its own next door
/// (`a_pragma_the_crate_does_not_set_is_still_inherited`) rather than
/// resting on this one, whose subject is connection *identity*.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn diagnostic_callers_share_one_connection_and_its_state() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let a = db.diagnostic_conn().await.unwrap();
    let b = db.diagnostic_conn().await.unwrap();

    // `query_only` is per-connection state. One connection, so setting it on
    // `a` is visible on `b` — the inverse of what this asserted before 0.15.14.
    a.execute("PRAGMA query_only = ON", ()).await.unwrap();
    let mut rows = b.query("PRAGMA query_only", ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        v, 1,
        "the two diagnostic connections do not share state, so the connection \
         is being minted per call again — which costs 51.5 us and reopens R15's \
         shape on this path (D-256)"
    );

    // Restore it, so nothing downstream inherits a connection this test armed.
    a.execute("PRAGMA query_only = OFF", ()).await.unwrap();

    db.close().await.unwrap();
}

/// **A transaction one caller leaves open does not reach the next** (0.15.15,
/// W15.5, [D-257]).
///
/// The sharpest thing [D-256]'s shared connection admitted, and the only one
/// whose damage leaves this surface. `BEGIN` on a read-only WAL connection opens
/// a read transaction, and its first read pins a snapshot. Held across calls,
/// that made later diagnostic reads answer from the pinned snapshot — measured
/// at **1 row where the database held 201** — on the one method a caller reaches
/// for when they already distrust the typed answer.
///
/// Scrubbed on *entry* rather than on exit because there is no exit: the method
/// returns a `Connection` clone and is never told the caller is done. So the
/// assertion is what the *next* call sees, which is the thing that matters.
///
/// [D-256]: ../docs/architecture/s13-decision-register.md#d-256
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn a_leaked_transaction_does_not_reach_the_next_caller() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    db.write_concepts(vec![ConceptUpsert::new("a", "A").valid_from(TS)])
        .await
        .unwrap();

    let leaked = db.diagnostic_conn().await.unwrap();
    leaked.execute("BEGIN", ()).await.unwrap();
    // The pin is taken on the first read inside the transaction, not by `BEGIN`
    // itself, so a test that skipped this would assert against an unpinned
    // connection and pass without the scrub.
    let mut rows = leaked
        .query("SELECT count(*) FROM concepts", ())
        .await
        .unwrap();
    let before: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(before, 1);
    drop(rows);
    assert!(!leaked.is_autocommit(), "BEGIN did not open a transaction");

    db.write_concepts(vec![ConceptUpsert::new("b", "B").valid_from(TS)])
        .await
        .unwrap();

    let next = db.diagnostic_conn().await.unwrap();
    assert!(
        next.is_autocommit(),
        "the next caller inherited an open transaction"
    );
    let mut rows = next
        .query("SELECT count(*) FROM concepts", ())
        .await
        .unwrap();
    let after: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        after, 2,
        "the next caller read {after} rows where the database holds 2, so it is \
         answering from the snapshot the leaked transaction pinned"
    );

    drop(rows);
    drop(next);
    drop(leaked);
    db.close().await.unwrap();
}

/// **A leaked transaction no longer defeats `checkpoint()`** (0.15.15, W15.5,
/// [D-257]).
///
/// The half of the leak that has nothing to do with diagnostics. A pinned WAL
/// read snapshot is a floor the checkpointer cannot truncate past, so one
/// forgotten `BEGIN` on the diagnostic connection made
/// [`Database::checkpoint`] — a typed public method — a no-op for the life of
/// the handle, with the WAL measured stuck at 8.5 MB until the transaction was
/// rolled back.
///
/// Asserted on the file rather than on the report because the report says what
/// the checkpointer attempted and the file says what it achieved, and it was the
/// second that was wrong.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn a_leaked_transaction_no_longer_pins_the_wal() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    let wal = harness.db_path.with_extension("db-wal");

    let leaked = db.diagnostic_conn().await.unwrap();
    leaked.execute("BEGIN", ()).await.unwrap();
    let mut rows = leaked
        .query("SELECT count(*) FROM concepts", ())
        .await
        .unwrap();
    let _: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    drop(rows);

    for i in 0..40 {
        db.write_concepts(vec![ConceptUpsert::new(format!("c{i}"), "C")
            .content("padding, so the WAL is worth truncating")
            .valid_from(TS)])
            .await
            .unwrap();
    }

    // The scrub runs here, on the way in — this call is what releases the pin.
    let next = db.diagnostic_conn().await.unwrap();
    drop(next);

    db.checkpoint().await.unwrap();
    let after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        after, 0,
        "the WAL is {after} bytes after a checkpoint, so a diagnostic caller's \
         leaked read transaction is still pinning it and checkpoint() cannot \
         truncate past it"
    );

    drop(leaked);
    db.close().await.unwrap();
}

/// **`scrub_diagnostic_conn()` releases the pin without handing anything out**
/// (0.15.15, W15.5, [D-257]).
///
/// The exit-side half. `diagnostic_conn()` scrubs on the way in, which is all
/// the crate can do for a caller who keeps the clone; it leaves the case of
/// somebody who leaks a transaction and then never calls again, holding the WAL
/// snapshot until the handle drops. This is what the Python binding calls after
/// each `diagnostic_query` to close that gap, and it is asserted here rather
/// than only through the binding because it is public Rust API.
///
/// The distinguishing assertion is that **no `diagnostic_conn()` call happens
/// between the leak and the check** — otherwise the entry-side scrub would be
/// doing the work and this method could be a no-op and still pass.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn scrubbing_releases_the_pin_without_handing_out_a_connection() {
    let harness = TestHarness::new();
    let db = db(&harness).await;
    let wal = harness.db_path.with_extension("db-wal");

    let leaked = db.diagnostic_conn().await.unwrap();
    leaked.execute("BEGIN", ()).await.unwrap();
    let mut rows = leaked
        .query("SELECT count(*) FROM concepts", ())
        .await
        .unwrap();
    let _: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    drop(rows);

    for i in 0..40 {
        db.write_concepts(vec![ConceptUpsert::new(format!("c{i}"), "C")
            .content("padding, so the WAL is worth truncating")
            .valid_from(TS)])
            .await
            .unwrap();
    }

    db.scrub_diagnostic_conn().await;

    db.checkpoint().await.unwrap();
    let after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        after, 0,
        "the WAL is {after} bytes after scrubbing and checkpointing, so the          scrub did not release the leaked read transaction"
    );

    drop(leaked);
    db.close().await.unwrap();
}

/// **A temp table one caller creates does not reach the next** (0.15.15, W15.5,
/// [D-257]).
///
/// `CREATE TEMP TABLE` succeeds on this connection — the temp database is
/// writable however `main` was opened, which is the section above headed *One
/// way this is more permissive*. Shared, that turned into two callers colliding
/// on a name (`table foo already exists`) and one reading the other's rows.
///
/// The connection is discarded rather than cleaned: dropping the objects one by
/// one would be a second guess at what SQLite considers state, and a fresh
/// connection costs 56.5 µs on the call that dirtied it and nothing on any
/// other.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn a_temp_table_does_not_reach_the_next_caller() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let first = db.diagnostic_conn().await.unwrap();
    first
        .execute("CREATE TEMP TABLE leftover(x)", ())
        .await
        .unwrap();
    first
        .execute("INSERT INTO leftover VALUES (42)", ())
        .await
        .unwrap();

    let next = db.diagnostic_conn().await.unwrap();
    let err = next
        .query("SELECT x FROM leftover", ())
        .await
        .expect_err("the next caller can see a temp table the previous one left");
    assert!(
        err.to_string().contains("no such table"),
        "refused, but not because the table is gone: {err}"
    );

    drop(first);
    db.close().await.unwrap();
}

/// **An `ATTACH` one caller makes does not reach the next** (0.15.15, W15.5,
/// [D-257]).
///
/// An attachment is a schema name on the connection, so a shared connection
/// meant the second caller to want `aux` got `database aux is already in use`
/// and, worse, that a caller who did not attach anything could read through
/// someone else's attachment. The read-only boundary is not what is at stake —
/// an attachment inherits the flags — the *namespace* is.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn an_attachment_does_not_reach_the_next_caller() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    // A second real database to attach: the harness's own file, named again.
    let aux = harness.db_path.display().to_string().replace('\\', "/");

    let first = db.diagnostic_conn().await.unwrap();
    first
        .execute(&format!("ATTACH DATABASE '{aux}' AS aux"), ())
        .await
        .unwrap();

    let next = db.diagnostic_conn().await.unwrap();
    let err = next
        .query("SELECT count(*) FROM aux.concepts", ())
        .await
        .expect_err("the next caller can read through an attachment it did not make");
    assert!(
        err.to_string().contains("no such table"),
        "refused, but not because the attachment is gone: {err}"
    );

    drop(first);
    db.close().await.unwrap();
}

/// **The crate's own pragmas are restored, not merely set once** (0.15.15,
/// W15.5, [D-257]).
///
/// No dirt check can see a pragma — `temp.schema_version` and `database_list`
/// both sit still while one is set — so the crate restates the two it sets
/// rather than detecting that they moved. `busy_timeout` is the one that
/// matters: [D-159] put 5 s on this connection precisely because it is the
/// surface that answers when the typed path is already suspect, and a caller
/// who set it to 0 was, before 0.15.15, setting it to 0 for everyone after them.
///
/// [D-159]: ../docs/architecture/s13-decision-register.md#d-159
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn the_crates_busy_timeout_is_restored_after_a_caller_moves_it() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let first = db.diagnostic_conn().await.unwrap();
    let mut rows = first.query("PRAGMA busy_timeout = 0", ()).await.unwrap();
    let set: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        set, 0,
        "the pragma did not take, so the test proves nothing"
    );
    drop(rows);

    let next = db.diagnostic_conn().await.unwrap();
    let mut rows = next.query("PRAGMA busy_timeout", ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        v, 5000,
        "the next caller inherited a busy_timeout of {v} ms, so D-159's 5 s \
         margin lasts only until someone runs one pragma"
    );

    drop(rows);
    drop(first);
    db.close().await.unwrap();
}

/// **A pragma the crate does not set is still inherited, and that is the
/// documented residue** (0.15.15, W15.5, [D-257]).
///
/// The limit of the scrub, pinned so that it stays a known quantity rather than
/// drifting into a claim the docs no longer make. SQLite offers no cheap
/// enumeration of connection-scoped pragma state, so the crate restores what it
/// set and nothing else. `case_sensitive_like` is the sharp instance: it changes
/// what `LIKE` *means* for every later diagnostic query on this handle, and no
/// dirt check can see it.
///
/// What bounds it is scope rather than detection: the crate's readers and writer
/// are different connections, so nothing here can move a typed answer. If this
/// test ever fails because the value no longer carries, the scrub grew — which
/// is welcome, and this test and the rustdoc's table both need rewriting rather
/// than deleting.
///
/// [D-257]: ../docs/architecture/s13-decision-register.md#d-257
#[tokio::test]
async fn a_pragma_the_crate_does_not_set_is_still_inherited() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let first = db.diagnostic_conn().await.unwrap();
    first
        .execute("PRAGMA case_sensitive_like = ON", ())
        .await
        .unwrap();

    let next = db.diagnostic_conn().await.unwrap();
    let mut rows = next.query("SELECT 'A' LIKE 'a'", ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        v, 0,
        "the pragma no longer carries between callers, so the scrub covers more \
         than D-257 says it does -- rewrite the residue row in diagnostic_conn's \
         rustdoc, do not delete this test"
    );

    drop(rows);
    drop(first);
    db.close().await.unwrap();
}

/// The shared connection is still **not** the shared reader.
///
/// The half of D-091 that C-9 does not touch, pinned on its own now that the
/// test above no longer implies it: `read_conn()` and `diagnostic_conn()` are
/// two different connections, so a caller holding the diagnostic one cannot
/// change what the crate's own readers see.
#[tokio::test]
async fn the_diagnostic_connection_is_not_the_shared_reader() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    let diag = db.diagnostic_conn().await.unwrap();
    diag.execute("PRAGMA query_only = OFF", ()).await.unwrap();

    let mut rows = db.read_conn().query("PRAGMA query_only", ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        v, 1,
        "disarming the diagnostic connection disarmed the shared reader, so \
         they are the same connection and D-091's boundary is gone"
    );

    diag.execute("PRAGMA query_only = ON", ()).await.unwrap();

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
/// open and the diagnostic call — which a test cannot stage on Windows, where
/// the file cannot be unlinked while this process holds it open. It can be
/// staged on POSIX, and since 0.15.14 it must be: see
/// [`a_deleted_file_is_refused_rather_than_answered_from_the_cached_connection`].
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

/// **A file deleted under a live handle is refused, not answered** (0.15.14,
/// W15.4, [D-256]).
///
/// # Why this exists, and why it is `cfg(unix)`
///
/// `diagnostic_conn` runs `path.exists()` on every call and not only the
/// first. Before 0.15.14 that was a rounding error beside the 51.5 µs
/// `connect()` it sat in front of; the connection is minted once now, so the
/// `stat` is **19.9 µs of a 19.9 µs call** — the whole of it. A cost that is
/// the whole of a call needs an assertion rather than a paragraph, and it did
/// not have one: a mutation deleting the check passed the entire suite.
///
/// What it buys is the worst failure a *diagnostic* surface can have. Delete
/// the file and put another one in its place, and a cached connection keeps
/// answering from the unlinked inode — silently, correctly-looking, on the one
/// method a caller reaches for when they already doubt the typed answer. The
/// `stat` turns that into [`macrame::DbError::DiagnosticConn`].
///
/// The hazard is POSIX's, and so is the staging: Windows refuses to unlink a
/// file this process holds open. The skip is at **run time rather than behind
/// `cfg(unix)`**, so the body is compiled on every platform and cannot rot on
/// the one that does not run it — which is how the sibling test above came to
/// say a test *cannot* stage this, a claim that was true of the box it was
/// written on rather than of the crate.
///
/// [D-256]: ../docs/architecture/s13-decision-register.md#d-256
#[tokio::test]
async fn a_deleted_file_is_refused_rather_than_answered_from_the_cached_connection() {
    let harness = TestHarness::new();
    let db = db(&harness).await;

    // Mint the cached connection, so the deletion happens against a handle
    // that is already open — which is the case the check exists for. Without
    // this first call the test would only be re-asserting the cold path.
    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn.query("SELECT 1", ()).await.unwrap();
    let _: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    drop(rows);
    drop(conn);

    if std::fs::remove_file(&harness.db_path).is_err() {
        // Windows: the file cannot be unlinked while this process holds it,
        // so the case this test is about cannot arise on this platform.
        eprintln!("skipped: this platform will not unlink an open file");
        db.close().await.unwrap();
        return;
    }

    let err = db.diagnostic_conn().await.expect_err(
        "the file is gone and the call succeeded, so it answered from the \
         cached connection's unlinked inode -- which is a diagnostic surface \
         reporting on a database that no longer exists",
    );
    assert!(
        matches!(err, macrame::DbError::DiagnosticConn { .. }),
        "refused, but not as DiagnosticConn: {err}"
    );

    // No `close()`: the ledger's file is gone, so the shutdown path has
    // nothing to write to and its failure is not what this test is about.
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
