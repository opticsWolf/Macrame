//! What does libSQL actually report for a `RAISE(ABORT, …)`? (review C-20)
//!
//! `abort_kind` classifies a schema guard by looking for the crate's own abort
//! message inside `err.to_string()`. The review asks for the extended result
//! code where one identifies the guard. Whether that is available at all, and
//! what it is on this build, is a measurement rather than a lookup.
#[tokio::main]
async fn main() {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .unwrap();
    let c = db.connect().unwrap();

    c.execute("CREATE TABLE t (a TEXT)", ()).await.unwrap();
    c.execute(
        "CREATE TRIGGER trg BEFORE INSERT ON t WHEN NEW.a = 'no' \
         BEGIN SELECT RAISE(ABORT, 'macrame: the guard fired'); END",
        (),
    )
    .await
    .unwrap();

    let e = c
        .execute("INSERT INTO t (a) VALUES ('no')", ())
        .await
        .unwrap_err();
    println!("RAISE(ABORT)");
    println!("  Display : {e}");
    println!("  Debug   : {e:?}");
    match &e {
        libsql::Error::SqliteFailure(code, msg) => {
            println!("  variant : SqliteFailure");
            println!("  code    : {code}");
            println!("  msg     : {msg:?}");
        }
        other => println!("  variant : {other:?} (not SqliteFailure)"),
    }

    // For contrast: an ordinary constraint the engine raises itself.
    c.execute("CREATE TABLE u (a TEXT NOT NULL)", ())
        .await
        .unwrap();
    let e2 = c
        .execute("INSERT INTO u (a) VALUES (NULL)", ())
        .await
        .unwrap_err();
    println!();
    println!("NOT NULL");
    println!("  Display : {e2}");
    if let libsql::Error::SqliteFailure(code, msg) = &e2 {
        println!("  code    : {code}");
        println!("  msg     : {msg:?}");
    }
}
