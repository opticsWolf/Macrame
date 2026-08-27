//! Doctrine claims provable without opening a database.
//!
//! Its companion, `doctrine_property_tests.rs`, needs generated histories and
//! therefore a live database per case; that file sits behind the
//! `property-tests` feature because of R15. Anything provable by reading the
//! schema instead belongs here, where it runs on every plain `cargo test`.
//!
//! The split is deliberate: a doctrine check should not become conditional
//! merely because it was written next to one that had to be.

/// **Doctrine VII, the half that is testable before Phase 3.**
///
/// A vector is a derived artifact and never appears in the ledger. This fails
/// the moment a well-meaning change puts one into a `json_object(…)` in the
/// trigger DDL, which is exactly when it is cheap to fix and years before anyone
/// would otherwise notice.
///
/// **The needle was `"embedding"` until Wave 1, and that was coarser than the
/// doctrine.** Doctrine VII excludes the *vector* — the derived artifact that is
/// expensive, model-dependent, and reconstructible. `concepts.embedding_model`
/// is none of those: it is a short scalar naming which model a concept's vector
/// was produced by, it is a column of a ledger table, and a temporal read that
/// omits it answers with a record the live table contradicts. That was defect V,
/// and fixing it meant the payload now legitimately contains the substring the
/// old needle banned.
///
/// So the check is narrowed rather than deleted, and narrowed by naming what is
/// allowed rather than by loosening what is forbidden: `embedding_model` passes,
/// anything else matching `embedding` does not. A future `'embedding', …` or
/// `'embedding_vector', …` still fails here, which is the case the test was
/// written for.
#[test]
fn no_payload_carries_a_vector() {
    /// The one `embedding`-prefixed identifier the ledger is allowed to carry.
    const ALLOWED: &str = "embedding_model";

    for trigger in macrame::schema::ddl::CREATE_TRIGGERS {
        let scrubbed = trigger.replace(ALLOWED, "");
        for needle in ["embedding", "vector", "F32_BLOB", "f32_blob"] {
            assert!(
                !scrubbed.contains(needle),
                "a trigger payload references {needle:?}; Doctrine VII excludes \
                 embeddings from transaction_log. The only permitted exception is \
                 {ALLOWED:?}, which names a model rather than carrying a vector."
            );
        }
    }
}

/// The exception above is an exception, not a door left open.
///
/// If `embedding_model` ever stops being a scalar column of `concepts` — if it
/// becomes a blob, or the vector itself gets folded into that name — the
/// allowance above would silently start permitting what it was carved out to
/// keep excluding. This pins the shape the carve-out depends on.
#[tokio::test]
async fn the_permitted_exception_is_still_a_scalar_column() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = libsql::Builder::new_local(dir.path().join("t.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    macrame::schema::run_migrations(&conn).await.unwrap();

    let mut rows = conn.query("PRAGMA table_info(concepts)", ()).await.unwrap();
    let mut found = None;
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(1).unwrap();
        if name == "embedding_model" {
            found = Some(row.get::<String>(2).unwrap());
        }
    }

    assert_eq!(
        found.as_deref(),
        Some("TEXT"),
        "embedding_model must stay a scalar name, or Doctrine VII's carve-out \
         in no_payload_carries_a_vector stops being sound"
    );
}
