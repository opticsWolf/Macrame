//! `concepts.extra` where it crosses a boundary (0.18.0, P1, [D-278]).
//!
//! The ordinary round trip — write it, read it back — is covered by the Python
//! suite and by `graph_tests`. What is here is the set of places a new column
//! has historically been *lost*, each of which is an acceptance gate of the
//! 0.18 plan:
//!
//! * the archive boundary, where six explicit column lists have to agree
//!   ([D-129]: archival is a move, and a move that drops a column is a rewrite);
//! * a cold file written before the column existed, which must be read without
//!   being written to;
//! * a log holding three payload versions at once, which is what the version
//!   marker is *for*;
//! * the upsert's conflict clause, where an omitted argument must not become a
//!   deletion the log records as an intention;
//! * and the per-shape ceiling, whose negative control is the only thing that
//!   distinguishes a gate that refuses from a gate that never fires ([D-282a]).
//!
//! [D-278]: ../docs/architecture/s13-decision-register.md#d-278
//! [D-129]: ../docs/architecture/s13-decision-register.md#d-129
//! [D-282a]: ../docs/architecture/s13-decision-register.md#d-282a

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::{ConceptUpsert, Database};

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-02-01T00:00:00.000000Z";
const OPEN: &str = "9999-12-31T23:59:59.999999Z";
const NOW: &str = "2099-06-01T00:00:00.000000Z";

/// In the future, for the reason `concept_archive_tests` states: `recorded_at`
/// is crate-stamped, so a past cutoff archives nothing at all and every
/// assertion below would hold over the empty set.
const CUTOFF: &str = "2099-01-01T00:00:00.000000Z";

const ATTRS: &str = r#"{"layer":"note","tags":["a","b"],"n":3}"#;

async fn connect(harness: &TestHarness) -> libsql::Connection {
    libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn attach_cold(conn: &libsql::Connection, cold: &std::path::Path) {
    conn.execute(&format!("ATTACH DATABASE '{}' AS cold", cold.display()), ())
        .await
        .unwrap();
}

async fn columns(conn: &libsql::Connection, schema: &str, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA {schema}.table_info({table})"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(1).unwrap());
    }
    out
}

/// Every column of one hot concept, rendered rather than typed.
///
/// `format!("{:?}")` on the `Value` and not a typed `get`, because the
/// comparison this file needs is *byte-equal*, and a typed read would convert
/// both sides through the same lossy step and then find them equal.
async fn hot_row(conn: &libsql::Connection, schema: &str, id: &str) -> Vec<String> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT id, title, content, embedding_model, valid_from, valid_to, \
                 recorded_at, retired, branch_id, extra \
                 FROM {schema}.concepts WHERE id = ?1"
            ),
            libsql::params![id],
        )
        .await
        .unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no concept {id} in {schema}.concepts"));
    (0..10)
        .map(|i| format!("{:?}", row.get_value(i).unwrap()))
        .collect()
}

/// One archivable concept carrying attributes, plus one that stays hot.
///
/// Retired with a closed valid time from the start: valid time is the caller's
/// and transaction time is not, so archivability has to be expressed there.
async fn seeded(harness: &TestHarness) -> Database {
    let db = Database::open(&harness.db_path).await.unwrap();
    db.upsert_concept(
        ConceptUpsert::new("gone", "Gone")
            .content("body of gone")
            .valid_from(T0)
            .valid_to(T1)
            .retired(true)
            .extra(ATTRS),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("stays", "Stays")
            .valid_from(T0)
            .extra(r#"{"layer":"live"}"#),
    )
    .await
    .unwrap();
    db
}

// ───────────────────────────────────────────────────────────────────────────
// Gate 1 — round trip under archive
// ───────────────────────────────────────────────────────────────────────────

/// **Acceptance gate 1: a concept carrying `extra` archives, rehydrates, and
/// compares byte-equal.**
///
/// The test [D-129] implies and the six explicit column lists make necessary.
/// Archival is a *move*: the cold row has to be the hot row it replaced, or the
/// cold file contradicts `cold.transaction_log` about itself and rehydration
/// returns a concept the ledger never recorded — the unexplained absence
/// Doctrine V exists to prevent.
///
/// Column by column and not by row count, for the reason its sibling in
/// `concept_archive_tests` states: a `SELECT` that forgot a column produces
/// exactly one row either way.
#[tokio::test]
async fn a_concept_carrying_attributes_survives_the_archive_round_trip() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;

    let before = hot_row(db.read_conn(), "main", "gone").await;
    assert!(
        before[9].contains("note"),
        "the fixture did not store the attributes it is about: {:?}",
        before[9]
    );

    assert_eq!(db.archive(CUTOFF).await.unwrap().concepts_archived, 1);

    // The cold row, while it is cold.
    {
        let conn = db.read_conn();
        attach_cold(conn, db.archive_path()).await;
        let cold = hot_row(conn, "cold", "gone").await;
        assert_eq!(
            cold, before,
            "the cold row is not the hot row it replaced -- a column list \
             somewhere in the move has fallen behind the schema"
        );
        conn.execute("DETACH DATABASE cold", ()).await.unwrap();
    }

    assert_eq!(
        db.rehydrate(&["gone"]).await.unwrap().concepts_rehydrated,
        1
    );

    let after = hot_row(db.read_conn(), "main", "gone").await;
    assert_eq!(
        after, before,
        "the rehydrated row differs from the one that was archived"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Gate 2 — a cold file written before the column existed
// ───────────────────────────────────────────────────────────────────────────

/// **Acceptance gate 2: a pre-0.18 cold file attaches, reads and folds with no
/// `extra` column, and is upgraded only by the archive writer.**
///
/// The asymmetry is deliberate and is the same one `branch_storage_tests`
/// records for `branch_id`: the *writer* upgrades the cold schema because it is
/// about to put a value there, and the *reader* does not, because a cold file
/// may sit on read-only media or a share and a reader that wrote to what it
/// read would be a new failure class rather than a convenience.
///
/// The column's absence reads as `'{}'`, not as NULL. There is no third state
/// below the empty object, and a NULL would be refused by the `NOT NULL` on the
/// way back in — a rehydration that failed on an old archive would make the
/// archive a one-way door.
#[tokio::test]
async fn a_cold_file_from_before_the_column_is_read_without_being_written_to() {
    let harness = TestHarness::new();
    let db = seeded(&harness).await;
    db.archive(CUTOFF).await.unwrap();
    let cold_path = db.archive_path().to_path_buf();
    db.close().await.unwrap();

    // Make the cold file look like one 0.17 wrote.
    {
        let conn = connect(&harness).await;
        attach_cold(&conn, &cold_path).await;
        conn.execute("ALTER TABLE cold.concepts DROP COLUMN extra", ())
            .await
            .unwrap();
    }

    let db = Database::open(&harness.db_path).await.unwrap();
    assert_eq!(
        db.rehydrate(&["gone"]).await.unwrap().concepts_rehydrated,
        1
    );

    let back = hot_row(db.read_conn(), "main", "gone").await;
    assert!(
        back[9].contains("{}"),
        "a concept rehydrated from a cold file that predates the column must \
         come back with the empty object, not a null the NOT NULL would have \
         refused: {:?}",
        back[9]
    );

    {
        let conn = db.read_conn();
        attach_cold(conn, &cold_path).await;
        assert!(
            !columns(conn, "cold", "concepts")
                .await
                .contains(&"extra".into()),
            "rehydration wrote to the cold file it was only supposed to read"
        );
        conn.execute("DETACH DATABASE cold", ()).await.unwrap();
    }

    // And the writer is what upgrades it. `stays` is hot and unarchivable as
    // seeded; retire and close it so this second session has something to move.
    db.upsert_concept(
        ConceptUpsert::new("stays", "Stays")
            .valid_from(T0)
            .valid_to(T1)
            .retired(true)
            .extra(r#"{"layer":"live"}"#),
    )
    .await
    .unwrap();
    // Two, not one: `gone` was rehydrated above and is archivable again, which
    // is the archive being a door rather than a one-way trip.
    assert_eq!(db.archive(CUTOFF).await.unwrap().concepts_archived, 2);

    let conn = db.read_conn();
    attach_cold(conn, &cold_path).await;
    assert!(
        columns(conn, "cold", "concepts")
            .await
            .contains(&"extra".into()),
        "the archive writer did not grow the cold schema it was about to write \
         a value into"
    );
    let cold = hot_row(conn, "cold", "stays").await;
    assert!(
        cold[9].contains("live"),
        "the upgraded column is there but empty, so the ALTER ran and the \
         write did not use it: {:?}",
        cold[9]
    );
    conn.execute("DETACH DATABASE cold", ()).await.unwrap();

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Gate 3 — a log holding three payload versions
// ───────────────────────────────────────────────────────────────────────────

/// **Acceptance gate 3: v1, v2 and v3 concept entries fold to the state they
/// describe, with `extra` present only where it was written.**
///
/// This is what the version marker is for, and the property is *tolerance*
/// rather than translation: an older entry is missing a field, and missing has
/// a value — the column default — rather than being an error. A ledger whose
/// oldest entries stopped folding the day a column was added would make every
/// schema change a quiet amnesia about everything before it.
///
/// Each row is hand-minted because nothing writes v1 or v2 any more, which is
/// precisely why they need a test: the only v1 payloads left in the world are
/// in other people's databases.
#[tokio::test]
async fn a_log_holding_three_payload_versions_folds() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    // v1: before Wave 1, so no `embedding_model` and no `extra`.
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'one', 'I', \
                 json_object('v', 1, 'title', 'One', 'content', 'B1', \
                             'valid_from', ?1, 'valid_to', ?2, 'retired', 0), ?1)",
        libsql::params![T0, OPEN],
    )
    .await
    .unwrap();

    // v2: Wave 1 through 0.17 — the model, still no attributes.
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'two', 'I', \
                 json_object('v', 2, 'title', 'Two', 'content', 'B2', \
                             'valid_from', ?1, 'valid_to', ?2, 'retired', 0, \
                             'embedding_model', 'nomic_v1'), ?1)",
        libsql::params![T0, OPEN],
    )
    .await
    .unwrap();

    // v3: 0.18 and after.
    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'three', 'I', \
                 json_object('v', 3, 'title', 'Three', 'content', 'B3', \
                             'valid_from', ?1, 'valid_to', ?2, 'retired', 0, \
                             'embedding_model', null, 'extra', json(?3)), ?1)",
        libsql::params![T0, OPEN, ATTRS],
    )
    .await
    .unwrap();

    let state = macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap();
    assert_eq!(state.concepts.len(), 3, "every version must fold");

    let one = &state.concepts["one"];
    assert_eq!(one.title, "One");
    assert_eq!(one.embedding_model, None, "absent, not an error");
    assert_eq!(one.extra, "{}", "absent is the default, not a null");

    let two = &state.concepts["two"];
    assert_eq!(two.embedding_model.as_deref(), Some("nomic_v1"));
    assert_eq!(two.extra, "{}");

    let three = &state.concepts["three"];
    assert!(
        three.extra.contains("\"layer\""),
        "the v3 entry lost the field it exists to carry: {}",
        three.extra
    );
    // Re-rendered from the decoded JSON rather than compared to the literal:
    // the fold reads an object out of the payload and writes it back as text,
    // and `json_object` does not promise to preserve spacing.
    let got: serde_json::Value = serde_json::from_str(&three.extra).unwrap();
    assert_eq!(
        got,
        serde_json::from_str::<serde_json::Value>(ATTRS).unwrap()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Gate 7 — the upsert preserves what it was not told about
// ───────────────────────────────────────────────────────────────────────────

/// **Acceptance gate 7: `extra` set on the first upsert survives a second that
/// does not mention it — and the log says so.**
///
/// `COALESCE(?10, concepts.extra)` in the conflict clause, and the second half
/// of this gate is why it matters more than an ergonomic preference. If an
/// omitted argument wiped the column, a title fixer written against 0.17 would
/// erase a caller's attributes *and the log would record the erasure as a
/// deliberate belief change*: irreversible by design, indistinguishable from an
/// intention, and about the past.
///
/// Deliberately unlike `branch_id`, which the same statement excludes from the
/// conflict clause outright. A lineage is identity and a bag of attributes is
/// not, so the two columns want opposite answers to the same question.
#[tokio::test]
async fn an_upsert_that_says_nothing_about_attributes_preserves_them() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();

    db.upsert_concept(
        ConceptUpsert::new("c1", "C1")
            .valid_from(T0)
            .extra(r#"{"layer":"note"}"#),
    )
    .await
    .unwrap();
    db.upsert_concept(ConceptUpsert::new("c1", "C1 renamed").valid_from(T0))
        .await
        .unwrap();

    let row = hot_row(db.read_conn(), "main", "c1").await;
    assert!(
        row[1].contains("C1 renamed"),
        "the rename should have landed"
    );
    assert!(
        row[9].contains("note"),
        "the second upsert wiped attributes it never mentioned: {:?}",
        row[9]
    );

    // And the log agrees, which is the half that makes the row trustworthy: a
    // row the log does not describe is a state no reconstruction can reach.
    let mut rows = db
        .read_conn()
        .query(
            "SELECT json_extract(payload, '$.extra.layer') FROM transaction_log \
             WHERE table_name = 'concepts' AND entity_id = 'c1' ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    let mut layers: Vec<Option<String>> = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        layers.push(r.get(0).unwrap());
    }
    assert_eq!(
        layers,
        vec![Some("note".to_string()), Some("note".to_string())],
        "the update entry must carry the preserved value too -- a log that \
         recorded the second write without `extra` would describe a state the \
         table contradicts"
    );

    let state = db.reconstruct(NOW).await.unwrap();
    assert!(
        state.concepts["c1"].extra.contains("note"),
        "and the fold agrees with both"
    );

    db.close().await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Gate 9 — the per-shape ceiling, both negatives
// ───────────────────────────────────────────────────────────────────────────

/// **Acceptance gate 9: each shape refuses above its own ceiling, and the two
/// ceilings are different numbers.**
///
/// Gate 3 asserts only the positive — that mixed versions fold — and a gate
/// that never fires is indistinguishable from one that cannot. This is its
/// negative control, which is gate 6's lesson applied to the payload gate.
///
/// The ceiling is checked *after* the `table_name` dispatch ([D-282a]). A
/// single global constant gated two shapes that version independently: from the
/// day Wave 1 took concepts to v2 and left links at 1, a links row stamped 2
/// passed the gate and decoded under v1 field names. 0.18 does not open that
/// gap — it widens it, since bumping one constant to 3 would admit links rows
/// stamped 3 as well.
///
/// The `max` in each refusal is the shape's own ceiling and is what names the
/// shape: 1 is unreachable for concepts and 3 is unreachable for links, so the
/// pairing of the row and the number identifies which gate fired.
///
/// Both rows are hand-minted because no version of this crate ever wrote
/// either. That is the point rather than a caveat — the ceiling exists for the
/// log's other writers, the ones §4.7 concedes, and it can only ever fire on a
/// row this crate did not mint.
///
/// [D-282a]: ../docs/architecture/s13-decision-register.md#d-282a
#[tokio::test]
async fn a_links_payload_above_the_links_ceiling_is_refused() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('links', 'a|b|KNOWS', 'I', \
                 json_object('v', 3, 'source_id', 'a', 'target_id', 'b', \
                             'edge_type', 'KNOWS', 'valid_from', ?1, \
                             'valid_to', ?2, 'weight', 1.0, \
                             'properties', json('{}')), ?1)",
        libsql::params![T0, OPEN],
    )
    .await
    .unwrap();

    match macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap_err()
    {
        DbError::PayloadVersion { got, max } => {
            assert_eq!(got, 3);
            assert_eq!(
                max, 1,
                "the refusal must report the LINKS ceiling. A 3 here would mean \
                 the concepts ceiling gated a links row, which is the drift \
                 D-282a closed"
            );
        }
        other => panic!("expected PayloadVersion, got {other:?}"),
    }
}

/// The concepts half of the same gate, at the version after this release's.
#[tokio::test]
async fn a_concepts_payload_above_the_concepts_ceiling_is_refused() {
    let harness = TestHarness::new();
    let conn = connect(&harness).await;
    macrame::schema::run_migrations(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at) \
         VALUES ('concepts', 'future', 'I', \
                 json_object('v', 4, 'title', 'From a later build', 'content', '', \
                             'valid_from', ?1, 'valid_to', ?2, 'retired', 0, \
                             'embedding_model', null, 'extra', json('{}')), ?1)",
        libsql::params![T0, OPEN],
    )
    .await
    .unwrap();

    match macrame::temporal::reconstruct(&conn, NOW, None, None)
        .await
        .unwrap_err()
    {
        DbError::PayloadVersion { got, max } => {
            assert_eq!(got, 4);
            assert_eq!(
                max, 3,
                "the concepts ceiling is this release's payload version"
            );
        }
        other => panic!("expected PayloadVersion, got {other:?}"),
    }
}

/// And the row this crate *does* write is not refused, so the two tests above
/// are about the ceiling rather than about the check being on at all.
#[tokio::test]
async fn a_links_payload_at_its_own_ceiling_still_folds() {
    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    for id in ["a", "b"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(T0))
            .await
            .unwrap();
    }
    db.assert_edge(macrame::graph::EdgeAssertion::new("a", "b", "KNOWS").valid_from(T0))
        .await
        .unwrap();

    let state = db.reconstruct(NOW).await.unwrap();
    assert_eq!(state.edges.len(), 1, "a v1 links row must still fold");
    assert_eq!(state.concepts.len(), 2);

    db.close().await.unwrap();
}
