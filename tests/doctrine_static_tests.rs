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
/// A vector is a derived artifact and never appears in the ledger. Today no
/// payload could carry one; this fails the moment a well-meaning change adds an
/// embedding field to a `json_object(…)` in the trigger DDL, which is exactly
/// when it is cheap to fix and years before anyone would otherwise notice.
#[test]
fn no_payload_carries_a_vector() {
    for trigger in macrame::schema::ddl::CREATE_TRIGGERS {
        for needle in ["embedding", "vector", "F32_BLOB", "f32_blob"] {
            assert!(
                !trigger.contains(needle),
                "a trigger payload references {needle:?}; Doctrine VII excludes \
                 embeddings from transaction_log"
            );
        }
    }
}
