//! The v7 schema, pinned as text, and a builder that lays a genuine v7 database.
//!
//! **Pinned rather than derived, for the reason `LINKS_V7` states in
//! `src/schema/migrations.rs`: a migration rung is a statement about the past.**
//! A fixture assembled from today's `ddl::` constants would be a v8 database
//! wearing a v7 stamp, and the rung under test would then be verified against a
//! shape it will never meet in the field.
//!
//! **Why this is a shared module and not a copy in each caller.** It has two
//! consumers — `tests/migration_tests.rs`, which checks the rung is *correct*,
//! and `examples/v8_migration_scale_probe.rs`, which measures what it *costs* —
//! and a second copy of a pinned schema is a copy that will drift from the
//! first. That is not hypothetical here: [D-124] was written after a fault rate
//! duplicated into four files disagreed with itself and a phantom regression
//! was reported off the stale copy. One pin, two readers.
//!
//! Everything except `concepts` and its FTS index is byte-identical between v7
//! and v8, so those parts come from the crate's own DDL rather than from a copy
//! — pinning what did not change would invent a difference to maintain.

#![allow(dead_code)]

use macrame::schema::ddl;

// Reached rather than declared. A `#[path]` module is *loaded* where it is
// declared, so declaring it here as well as in the test root loads the file
// twice into one binary — two types named `Tables`, two pinned trigger lists,
// and a diagnostic that names neither. Every binary that pulls this module in
// declares `v11_schema` at its own root, which is where a `#[path]` belongs.
use crate::v11_schema;

pub const TS: &str = "2026-01-01T00:00:00.000000Z";

/// `concepts` as v7 declared it: `id` is the primary key and the rowid is
/// implicit. v8's whole change is the explicit `rowid_pk` this lacks.
pub const CONCEPTS_V7: &str = r#"
CREATE TABLE concepts (
    id               TEXT PRIMARY KEY,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    CHECK (valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND valid_to GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND 1)
)
"#;

/// v7's FTS index tracked the *implicit* rowid. v8 repoints it at `rowid_pk`,
/// which is why the rung has to drop and rebuild it rather than leave it alone.
pub const CONCEPTS_FTS_V7: &str = r#"
CREATE VIRTUAL TABLE concepts_fts USING fts5(
    title, content, content='concepts', content_rowid='rowid'
)
"#;

pub const FTS_TRIGGERS_V7: &[&str] = &[
    r#"
    CREATE TRIGGER trg_concepts_fts_insert AFTER INSERT ON concepts
    BEGIN
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid, NEW.title, NEW.content);
    END;
    "#,
    r#"
    CREATE TRIGGER trg_concepts_fts_update AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO concepts_fts (concepts_fts, rowid, title, content)
        VALUES ('delete', OLD.rowid, OLD.title, OLD.content);
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid, NEW.title, NEW.content);
    END;
    "#,
];

/// The two indices v8 drops, as v7 declared them.
pub const UNREAD_INDICES_V7: &[&str] = &[
    "CREATE INDEX idx_annotations_label ON analytics_annotations (label)",
    "CREATE INDEX idx_lc_tgt_active ON links_current (target_id, valid_to)",
];

/// Lay the v7 schema — tables, indices and triggers — and stamp nothing.
///
/// The caller stamps `user_version` after seeding, because a database stamped
/// v7 before its rows exist is a database the ladder could legally migrate
/// mid-seed if anything opened it.
pub async fn v7_schema(conn: &libsql::Connection) {
    conn.execute(CONCEPTS_V7, ()).await.unwrap();
    // v11's shapes, not today's: since v12 the live constants reference a
    // `branches` table and carry a `branch_id` column, neither of which a v7
    // database has ever seen. The module doc's "byte-identical between v7 and
    // v8" reasoning still holds — it is just that "today's DDL" stopped being
    // the right source for the parts v7 and v8 agree on.
    conn.execute(&v11_schema::links_v11(), ()).await.unwrap();
    conn.execute(&v11_schema::links_current_v11(), ())
        .await
        .unwrap();
    conn.execute(&v11_schema::transaction_log_v11(), ())
        .await
        .unwrap();
    conn.execute(ddl::CREATE_ANALYTICS_ANNOTATIONS_TABLE, ())
        .await
        .unwrap();
    conn.execute(CONCEPTS_FTS_V7, ()).await.unwrap();

    for index_ddl in ddl::CREATE_INDICES.iter().chain(UNREAD_INDICES_V7) {
        conn.execute(index_ddl, ()).await.unwrap();
    }
    // v8's FTS triggers name `rowid_pk`, which this table does not have, so the
    // two FTS ones come from the v7 copy and the delete trigger does not exist
    // yet — it is part of what the rung installs.
    for trigger_ddl in v11_schema::triggers_v11() {
        if trigger_ddl.contains("trg_concepts_fts_") {
            continue;
        }
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
    for trigger_ddl in FTS_TRIGGERS_V7 {
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
}

/// Build a genuine v7 database, seeded with the named concepts, and stamp it v7.
///
/// `links` rows are **not** optional garnish. The rung's finding is that a naive
/// rebuild fails on `concepts`'s *inbound* foreign keys, and a `concepts` with
/// nothing pointing at it exercises none of that: the rung would pass with the
/// suspension removed. So every fixture here carries links.
pub async fn seeded_v7(conn: &libsql::Connection, concepts: &[&str]) {
    v7_schema(conn).await;

    for id in concepts {
        conn.execute(
            "INSERT INTO concepts (id, title, content, valid_from, recorded_at) \
             VALUES (?1, 'N', 'findable body text', ?2, ?2)",
            libsql::params![*id, TS],
        )
        .await
        .unwrap();
    }
    for pair in concepts.windows(2) {
        conn.execute(
            "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
             weight, properties, recorded_at) \
             VALUES (?1, ?2, 'KNOWS', ?3, '9999-12-31T23:59:59.999999Z', 1.0, '{}', ?3)",
            libsql::params![pair[0], pair[1], TS],
        )
        .await
        .unwrap();
    }

    conn.execute("PRAGMA user_version = 7", ()).await.unwrap();
}
