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

// ---------------------------------------------------------------------------
// Lineage is a clause on the second clock (0.14.1, W12.1, D-213)
// ---------------------------------------------------------------------------

/// The doctrine text these read. Foundations is the normative source; §15's
/// framing in the roadmap is prose *about* it, and the two are checked against
/// each other rather than one being trusted.
const FOUNDATIONS: &str = include_str!("../docs/architecture/s0-s3-foundations.md");

/// **A branch is transaction time with a tree order, and W12 has not started
/// building yet** (0.14.1, W12.1, [D-213]).
///
/// [§15.1](../docs/Macrame%20Road%20to%201.0.md) requires the framing to be
/// written down before the schema is, for the reason
/// [D-160](../docs/architecture/s13-decision-register.md#d-160) and
/// [D-174](../docs/architecture/s13-decision-register.md#d-174) were kept one
/// release apart: *a break you cannot state is a break you cannot review.* A
/// framing in a register entry is a framing; a framing a test reads is a
/// framing that survives the four releases between here and the schema.
///
/// Three claims, and the third is the one that will catch a mistake:
///
/// 1. **There are still eight doctrines.** A ninth for branching was the easy
///    edit and the wrong one, and it would also have moved a number
///    [D-211](../docs/architecture/s13-decision-register.md#d-211) froze.
/// 2. **Doctrine II owns lineage**, because it owns the axis lineage is a
///    property of.
/// 3. **No doctrine calls it an axis or a dimension except to refuse it.**
///    That is the specific misreading §15.1 exists to name — it is what
///    produces a `branch_id` column that means nothing precise and a schema
///    nobody can query correctly. A doctrine that refuses a framing has to
///    *say* the framing, so the phrase is permitted where a negation
///    immediately precedes it and forbidden everywhere else. Wording drifts;
///    this pins the part that is not wording.
///
/// [D-213]: ../docs/architecture/s13-decision-register.md
#[test]
fn lineage_belongs_to_the_second_clock_and_is_not_a_third_axis() {
    let doctrines: Vec<&str> = FOUNDATIONS
        .match_indices("<a id=\"doctrine-")
        .map(|(i, _)| {
            let rest = &FOUNDATIONS[i..];
            &rest[..rest.find("\n\n").unwrap_or(rest.len())]
        })
        .collect();

    assert_eq!(
        doctrines.len(),
        8,
        "§0 defines {} doctrines. Branching is a clause on Doctrine II and not a \
         ninth doctrine (D-213), and the count is frozen by the stability \
         contract (D-211)",
        doctrines.len()
    );

    let second = doctrines
        .iter()
        .find(|d| d.starts_with("<a id=\"doctrine-ii\">"))
        .expect("Doctrine II is anchored");
    assert!(
        second.contains("total order within one lineage")
            && second.contains("partial order across lineages"),
        "Doctrine II no longer states that transaction time is a total order \
         within a lineage and a partial order across them. That clause is what \
         makes a branch a fork in the second clock rather than a new axis \
         (D-213), and W12's schema is built against it"
    );

    // Doctrine II says "**not a third axis**", which is the refusal and not the
    // error. A bare substring search cannot tell one from the other, so the
    // negation has to be part of what is checked: the phrase is permitted
    // immediately after one of these and forbidden anywhere else.
    const REFUSALS: &[&str] = &["not a ", "not the ", "never a ", "rather than a ", "nor a "];

    for d in &doctrines {
        let lower = d.to_lowercase();
        for phrase in ["third axis", "third clock", "third temporal", "branch axis"] {
            for (i, _) in lower.match_indices(phrase) {
                let before = &lower[..i];
                assert!(
                    REFUSALS.iter().any(|r| before.ends_with(r)),
                    "a doctrine describes lineage as {phrase:?} without refusing \
                     it. §15.1 refuses that framing by name: a branch is \
                     transaction time with a tree order, and treating it as a \
                     dimension is how the schema acquires a column that means \
                     nothing precise (D-213). Naming the framing is allowed \
                     where a negation precedes it; asserting it is not"
                );
            }
        }
    }
}
