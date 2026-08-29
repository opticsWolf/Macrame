//! The v11 schema, pinned as text, for fixtures that have to start below v12.
//!
//! **Why this module had to exist.** Every migration fixture in the suite used
//! to build its "old" database out of the crate's live `ddl::` constants and
//! stamp an old `user_version` over it. That worked for as long as the live
//! tables were a *superset* of the old ones — an extra index or trigger on a v2
//! fixture is harmless, because the rung that would have added it is written
//! `IF NOT EXISTS`. v12 broke the assumption in both directions at once: the
//! live tables now carry `branch_id` (so the v11 → v12 rung's `ADD COLUMN`
//! meets a column that already exists) and they reference a `branches` table
//! (so the `CREATE` fails outright with `no such table: main.branches`). A
//! fixture cannot be built from a schema that has already had the rung applied
//! to it.
//!
//! **Pinned rather than derived**, for the reason [`v7_schema`] states at
//! greater length: a rung is a statement about the past, and a fixture
//! assembled from today's constants verifies the rung against a shape it will
//! never meet in the field.
//!
//! **Only what v12 changed is pinned.** Four tables gained a column,
//! `links_current` gained a primary-key member, and five triggers had to be
//! redefined to name it. Everything else — the FTS table and its triggers, the
//! annotations table, every index, the delete guards, the monotonicity guard —
//! is byte-identical across the rung and comes from the crate's own DDL.
//! Pinning what did not change would invent a difference to maintain, and the
//! four *new* v12 triggers are excluded rather than pinned because at v11 they
//! do not exist.
//!
//! [`v7_schema`]: ../v7_schema/index.html

#![allow(dead_code)]

use macrame::schema::ddl;

/// The canonical-timestamp GLOB, written out because the macro that builds it
/// is private to the crate. Identical to `canonical_ts_check!`'s expansion.
const TS_GLOB: &str = "[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z";

/// The four triggers v12 adds, which a v11 database must not have.
///
/// Excluded by name rather than by pinning a v11 trigger list, so that a
/// trigger added by some *later* release is a compile-clean surprise here
/// rather than a silent omission: this list says what v12 introduced, and
/// nothing else claims to be complete.
const V12_ONLY_TRIGGERS: &[&str] = &[
    "trg_concepts_cross_lineage",
    "trg_concepts_branch_immutable",
    "trg_branches_frozen_update",
    "trg_branches_frozen_delete",
];

/// The five triggers v12 redefines, by name. Their v11 bodies are below.
const V12_CHANGED_TRIGGERS: &[&str] = &[
    "trg_links_current_sync",
    "trg_links_single_open",
    "trg_concepts_log_insert",
    "trg_concepts_log_update",
    "trg_links_log_insert",
];

/// `concepts` as v11 declared it: no `branch_id`.
pub fn concepts_v11() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS concepts (
    rowid_pk         INTEGER PRIMARY KEY,
    id               TEXT NOT NULL UNIQUE,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    CHECK (valid_from GLOB '{TS_GLOB}' AND valid_to GLOB '{TS_GLOB}' \
     AND recorded_at GLOB '{TS_GLOB}' AND 1)
);"
    )
}

/// `links` as v11 declared it.
pub fn links_v11() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS links (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{{}}',
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at),
    CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
    CHECK (valid_from GLOB '{TS_GLOB}' AND valid_to GLOB '{TS_GLOB}' \
     AND recorded_at GLOB '{TS_GLOB}' AND 1)
);"
    )
}

/// `links_current` as v11 declared it: the **four**-column primary key that
/// v12 widens to five. This is the one shape v12 could not reach by `ALTER`.
pub fn links_current_v11() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS links_current (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    valid_to    TEXT NOT NULL,
    weight      REAL NOT NULL,
    properties  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id, edge_type, valid_from),
    CHECK (valid_from GLOB '{TS_GLOB}' AND valid_to GLOB '{TS_GLOB}' \
     AND recorded_at GLOB '{TS_GLOB}' AND 1)
);"
    )
}

/// `transaction_log` as v11 declared it.
pub fn transaction_log_v11() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS transaction_log (
    seq_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name  TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    operation   TEXT NOT NULL,
    payload     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    CHECK (recorded_at GLOB '{TS_GLOB}' AND 1)
);"
    )
}

/// The five trigger bodies v12 redefines, as v11 declared them.
///
/// The three log triggers are the ones that matter to the rung under test:
/// none of them names `branch_id`, which is precisely the defect v12 repairs —
/// a v12 fold partitions on a column that every pre-v12 trigger would leave
/// reading `'main'`.
pub const TRIGGERS_V11: &[&str] = &[
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_current_sync
    AFTER INSERT ON links
    BEGIN
        INSERT INTO links_current
            (source_id, target_id, edge_type, valid_from, valid_to,
             weight, properties, recorded_at)
        VALUES
            (NEW.source_id, NEW.target_id, NEW.edge_type, NEW.valid_from,
             NEW.valid_to, NEW.weight, NEW.properties, NEW.recorded_at)
        ON CONFLICT(source_id, target_id, edge_type, valid_from) DO UPDATE SET
            valid_to    = excluded.valid_to,
            weight      = excluded.weight,
            properties  = excluded.properties,
            recorded_at = excluded.recorded_at
        WHERE excluded.recorded_at > links_current.recorded_at;
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_single_open
    BEFORE INSERT ON links
    WHEN NEW.valid_to = '9999-12-31T23:59:59.999999Z'
         AND EXISTS (
             SELECT 1 FROM links_current
             WHERE source_id  = NEW.source_id
               AND target_id  = NEW.target_id
               AND edge_type  = NEW.edge_type
               AND valid_from <> NEW.valid_from
               AND valid_to   = '9999-12-31T23:59:59.999999Z'
         )
    BEGIN
        SELECT RAISE(ABORT, 'macrame: edge already has an open interval; retire it first');
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_insert
    AFTER INSERT ON concepts
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = 'macrame_archive_session'
    )
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('concepts', NEW.id, 'I',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at);
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_update
    AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('concepts', NEW.id, 'U',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at);
    END;
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_log_insert
    AFTER INSERT ON links
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('links',
                NEW.source_id || '|' || NEW.target_id || '|' || NEW.edge_type || '|' || NEW.valid_from,
                'I',
                json_object('v', 1, 'source_id', NEW.source_id, 'target_id', NEW.target_id,
                            'edge_type', NEW.edge_type, 'valid_from', NEW.valid_from,
                            'valid_to', NEW.valid_to, 'weight', NEW.weight,
                            'properties', json(NEW.properties)),
                NEW.recorded_at);
    END;
    "#,
];

/// Every trigger a v11 database has: the crate's list, minus what v12 added,
/// with the five v12 redefined back to their v11 bodies.
pub fn triggers_v11() -> Vec<&'static str> {
    ddl::CREATE_TRIGGERS
        .iter()
        .copied()
        .filter(|t| {
            !V12_ONLY_TRIGGERS.iter().any(|n| t.contains(n))
                && !V12_CHANGED_TRIGGERS.iter().any(|n| t.contains(n))
        })
        .chain(TRIGGERS_V11.iter().copied())
        .collect()
}

/// The four ledger tables as v11 declared them, in dependency order.
pub fn tables_v11() -> Vec<String> {
    vec![
        concepts_v11(),
        links_v11(),
        links_current_v11(),
        transaction_log_v11(),
    ]
}

/// Lay a complete v11 schema — tables, indices and triggers — and stamp
/// nothing.
///
/// The indices that existed at v11, which is every one `ddl` declares except
/// the v14 addition (0.14.14, D-231).
///
/// Three fixtures build a pre-v12 database — v2 and v7 in `migration_tests`,
/// v11 here — and all three used to scan `CREATE_INDICES` whole. That was
/// exact for as long as the index set had not changed since v11, and it broke
/// the moment v14 declared one over `links_current.branch_id`: a column those
/// fixtures deliberately do not have, so the fixture died with `no such
/// column` before the rung under test ever ran.
///
/// Filtered from the live declaration rather than pinned as text. The six
/// entries really are byte-identical to v11's, and copying them into a fixture
/// would trade a list that cannot drift for six that can — which is the trap
/// `wind_back_to_v11` accepts deliberately for the two `links_current` indices
/// it must recreate *after* dropping the table, and which is worth avoiding
/// wherever the DDL can simply be read.
///
/// The exclusion is by name and not by position, so a v15 index appended after
/// this one does not silently rejoin the v11 set.
pub fn indices_v11() -> Vec<&'static str> {
    ddl::CREATE_INDICES
        .iter()
        .copied()
        .filter(|sql| !sql.contains("idx_lc_lineage_cut"))
        .collect()
}

/// The caller stamps `user_version` after seeding, for the reason
/// [`v7_schema::v7_schema`] gives: a database stamped before its rows exist is
/// one the ladder could legally migrate mid-seed.
pub async fn v11_schema(conn: &libsql::Connection) {
    for table in tables_v11() {
        conn.execute(&table, ()).await.unwrap();
    }
    conn.execute(ddl::CREATE_ANALYTICS_ANNOTATIONS_TABLE, ())
        .await
        .unwrap();
    conn.execute(ddl::CREATE_CONCEPTS_FTS, ()).await.unwrap();

    for index_ddl in indices_v11() {
        conn.execute(index_ddl, ()).await.unwrap();
    }
    for trigger_ddl in triggers_v11() {
        conn.execute(trigger_ddl, ()).await.unwrap();
    }
}

/// Take a v12 database back to v11 — schema *and* stamp are the caller's to
/// pair, this touches only the schema.
///
/// **Why a wind-back rather than a fresh v11 build.** Two callers need a v11
/// database that has been through the real write path: the rung test wants
/// rows written by v11 code before lineage existed anywhere, and the
/// re-anchoring test wants a database that `Database::open` actually populated,
/// snapshots and all. Neither can be assembled by laying DDL and inserting.
///
/// **Why rolling the stamp back alone is not enough**, which is the trap this
/// function exists to close: the ladder is not re-entrant. Stamping a v12
/// database `user_version = 5` and reopening does not replay history — the
/// v7 -> v8 rung rebuilds `links` from its *pinned v7* definition, which has no
/// `branch_id`, and the live `trg_links_current_sync` then fails with
/// `no such column: NEW.branch_id`. A rung is a statement about a shape, so the
/// shape has to be there.
pub async fn wind_back_to_v11(conn: &libsql::Connection) {
    for trigger in V12_ONLY_TRIGGERS.iter().chain(V12_CHANGED_TRIGGERS) {
        conn.execute(&format!("DROP TRIGGER IF EXISTS {trigger}"), ())
            .await
            .unwrap();
    }

    for table in ["concepts", "transaction_log"] {
        conn.execute(&format!("ALTER TABLE {table} DROP COLUMN branch_id"), ())
            .await
            .unwrap_or_else(|e| panic!("DROP COLUMN on {table}: {e}"));
    }

    // `links` is not in that loop since v15, and the reason is the same one
    // that made v15 a release: `branch_id` is now **in its primary key**, and
    // SQLite refuses `DROP COLUMN` on a key member. So undoing v12 on this
    // table means the same thing undoing it on `links_current` has always
    // meant — rebuild from the pinned shape and carry the rows across.
    //
    // The rename comes first so the copy has a source, and the delete guard is
    // dropped explicitly: `RENAME` rewrites trigger bodies to follow the table,
    // so the guard would otherwise end up attached to the table about to be
    // dropped. The other three `links` triggers are already gone — the loop
    // above this one drops every trigger v12 touched, and all three are in it.
    conn.execute("ALTER TABLE links RENAME TO links_wound_back", ())
        .await
        .unwrap();
    conn.execute("DROP TRIGGER IF EXISTS trg_links_guard_delete", ())
        .await
        .unwrap();
    conn.execute(&links_v11(), ()).await.unwrap();
    conn.execute(
        "INSERT INTO links (source_id, target_id, edge_type, valid_from, \
         recorded_at, valid_to, weight, properties) \
         SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                valid_to, weight, properties FROM links_wound_back",
        (),
    )
    .await
    .unwrap();
    conn.execute("DROP TABLE links_wound_back", ())
        .await
        .unwrap();
    conn.execute(ddl::CREATE_LINKS_GUARD_DELETE, ())
        .await
        .unwrap();
    // The drop took these two with it. v11 has both — they arrived at the
    // v10 -> v11 rung — so a wind-back that stopped here would be describing
    // v10. Pinned rather than taken from `ddl` for the reason the two
    // `links_current` indices below are.
    for index in [
        "CREATE INDEX idx_links_recorded_at ON links (recorded_at);",
        "CREATE INDEX idx_links_target ON links (target_id);",
    ] {
        conn.execute(index, ()).await.unwrap();
    }

    // `links_current` is the one v12 could not reach by `ALTER`, so undoing it
    // means rebuilding — and re-deriving, because a `links_current` that
    // disagrees with `links` is not a v11 database, it is a broken one.
    conn.execute("DROP TABLE links_current", ()).await.unwrap();
    conn.execute(&links_current_v11(), ()).await.unwrap();
    // Pinned rather than taken from `ddl`, which keeps them crate-internal.
    // v12 did not change either definition, so these are byte-identical
    // to what the rung restores — which is the point: if a later release
    // does change one, this fixture keeps describing v11 and the rung
    // for that release describes the change.
    for index in [
        "CREATE INDEX idx_lc_traversal_cover ON links_current \
         (source_id, valid_from, valid_to, weight, edge_type, target_id);",
        "CREATE INDEX idx_lc_open_interval ON links_current \
         (source_id, target_id, edge_type, valid_to, valid_from);",
    ] {
        conn.execute(index, ()).await.unwrap();
    }

    conn.execute("DROP TABLE branches", ()).await.unwrap();

    // All five v11 bodies, not the three the first version of this restored.
    // The two it left out were `trg_concepts_log_insert` and
    // `trg_links_single_open`, so the fixture it built was a v11 database that
    // logged no concept inserts and permitted two open intervals on one edge —
    // which is not the shape the rung will meet.
    for body in TRIGGERS_V11 {
        conn.execute(body, ()).await.unwrap();
    }

    conn.execute(
        "INSERT INTO links_current (source_id, target_id, edge_type, valid_from, \
         valid_to, weight, properties, recorded_at) \
         SELECT source_id, target_id, edge_type, valid_from, valid_to, weight, \
                properties, recorded_at FROM links \
         WHERE true ON CONFLICT DO NOTHING",
        (),
    )
    .await
    .unwrap();
}
