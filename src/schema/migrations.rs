use std::future::Future;
use std::pin::Pin;

use crate::error::{DbError, Result};
use crate::schema::ddl::*;

/// Schema version this build understands, stored in SQLite's `user_version`.
///
/// The baseline is **2**, not 1, on purpose. Builds before 0.5.4 stamped
/// `user_version = 1` over the pre-canonical schema — no `CHECK` constraints,
/// second-precision timestamps, the narrow sentinel. Had the canonical baseline
/// kept the number 1, one of those files would open silently and every
/// guarantee D-029 buys would be void on it while `user_version` insisted all
/// was well. Reserving 1 as a value this build refuses by name is what makes
/// "no legacy support" an enforced property instead of a README sentence.
pub const SCHEMA_VERSION: u32 = 9;

type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// One rung of the ladder: takes a database at `from` and leaves it at `to`.
struct Step {
    from: u32,
    to: u32,
    name: &'static str,
    apply: for<'a> fn(&'a libsql::Connection) -> StepFuture<'a>,
    /// Suspend foreign-key enforcement **around** this rung's transaction
    /// (0.8.0, B4, D-117).
    ///
    /// # Why a rung would need this, and why the obvious ways do not work
    ///
    /// A rung that rebuilds a table with inbound foreign keys cannot use the
    /// `links`-style recipe. `links` has no inbound keys; `concepts` has two
    /// (`links.source_id`, `links.target_id`). `examples/concepts_rebuild_probe.rs`
    /// measured four approaches on libSQL 0.9.30 and **all four fail**:
    ///
    /// 1. `PRAGMA foreign_keys = OFF` *inside* the transaction — **silently
    ///    ignored**. `execute` returns `Ok`, the value reads back `1`. The
    ///    pragma is a no-op inside a transaction, and [`apply_step`] wraps every
    ///    rung in `BEGIN IMMEDIATE`.
    /// 2. `DROP TABLE concepts` with keys on — `FOREIGN KEY constraint failed`,
    ///    with **or without** the delete guard. The guard is not the obstacle.
    /// 3. `PRAGMA defer_foreign_keys = ON`, which is designed for exactly this —
    ///    every statement succeeds and `foreign_key_check` reports **0
    ///    violations**, and then **COMMIT fails**. SQLite counts deferred
    ///    violation *events*; re-adding an equivalent parent row does not
    ///    decrement the counter.
    /// 4. Rename-around, with `legacy_alter_table` both on and off — the drop
    ///    of the orphaned table fails either way.
    ///
    /// What works is toggling the pragma *outside* the transaction. So the
    /// ladder has to know, and this flag is how a rung says so.
    ///
    /// # Why this does not weaken atomicity
    ///
    /// The rung is still **one transaction and one commit**, with the
    /// `user_version` stamp inside it — [D-032](../../docs/architecture/s13-decision-register.md)'s
    /// property is untouched. `PRAGMA foreign_keys` is per-*connection*, and the
    /// migration connection is created in `open()` and discarded if the
    /// migration fails, so a crash between the toggle and the reset cannot
    /// leave a long-lived connection with enforcement off.
    ///
    /// And the suspension cannot hide a real violation: [`apply_step`] runs
    /// `PRAGMA foreign_key_check` **inside** the transaction before committing,
    /// and any row it reports fails the rung. Enforcement is suspended for the
    /// duration; verification is not.
    suspends_foreign_keys: bool,
}

/// The ladder, in no particular order — `run` walks it by matching `from`.
///
/// The rung out of 0 lays the whole schema; the rung out of 2 adds only what
/// v3 introduced. There is deliberately still no rung out of 1: that is the
/// pre-canonical schema D-032 refuses by name, and v2 is not the same case —
/// it was written by this same 0.5.4 line with canonical timestamps and every
/// CHECK in place, so it is missing a derivative table and nothing else.
const STEPS: &[Step] = &[
    Step {
        from: 0,
        to: SCHEMA_VERSION,
        name: "baseline-0.5.4",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(baseline(conn)),
    },
    Step {
        from: 2,
        to: 3,
        name: "analytics-annotations",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_analytics_annotations(conn)),
    },
    Step {
        from: 3,
        to: 4,
        name: "traversal-covering-index",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_traversal_cover(conn)),
    },
    Step {
        from: 4,
        to: 5,
        name: "concepts-fts",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_concepts_fts(conn)),
    },
    Step {
        from: 5,
        to: 6,
        name: "single-open-interval-index",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_open_interval_index(conn)),
    },
    Step {
        from: 6,
        to: 7,
        name: "links-weight-check",
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_weight_check(conn)),
    },
    Step {
        from: 7,
        to: 8,
        name: "concepts-rowid-pk-and-unread-indices",
        // The only rung that needs it, and the reason the flag exists. See
        // `Step::suspends_foreign_keys` for the four approaches the probe
        // refuted.
        suspends_foreign_keys: true,
        apply: |conn| Box::pin(add_concepts_rowid_pk(conn)),
    },
    Step {
        from: 8,
        to: 9,
        name: "concepts-guard-marker-gated",
        // One trigger replaced. No table is rebuilt and no row moves, so the
        // inbound foreign keys that forced the flag on the rung above are not
        // involved here at all.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(gate_concepts_guard_on_marker(conn)),
    },
];

/// Bring `conn`'s database up to [`SCHEMA_VERSION`], or fail explaining why not.
///
/// Reading `user_version` before writing is the whole point. The previous
/// implementation re-ran every `CREATE … IF NOT EXISTS` unconditionally and then
/// stamped the version it had never read, which meant it could not distinguish a
/// fresh file from a foreign one from a database written by a future build — it
/// simply asserted the schema it wanted and hoped. `IF NOT EXISTS` hides exactly
/// the case that matters: an object that exists with a *different* definition is
/// silently kept, so a legacy table would survive with none of its constraints
/// while the stamp claimed otherwise.
/// What [`run`] did, so a caller can react to the schema having moved.
///
/// The one caller that must is `Database::open`: a `SCHEMA_VERSION` bump
/// invalidates every snapshot on disk (D-043), and until Wave 4.4 nothing
/// noticed — the first `reconstruct` after an upgrade skipped every snapshot as
/// incompatible and folded from genesis, correctly and expensively, with the
/// only trace a `warn!` per skipped file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// The version the file carried on the way in.
    pub from: u32,
    /// [`SCHEMA_VERSION`], always — `run` either reaches it or fails.
    pub to: u32,
}

impl MigrationOutcome {
    /// Whether an **existing** database moved between versions.
    ///
    /// A fresh file (`from == 0`) is deliberately not an upgrade. It has no
    /// snapshots to invalidate, so there is nothing to re-anchor — and treating
    /// it as one made `Database::open` write a snapshot on every first open,
    /// which broke two contracts the suite already pins: an idle database is
    /// never anchored, and a handle opened with no cadence writes nothing until
    /// `close()`. Both are worth keeping. `open()` touching the disk when it was
    /// not asked to is surprising in its own right.
    pub fn upgraded(&self) -> bool {
        self.from != 0 && self.from != self.to
    }
}

pub async fn run(conn: &libsql::Connection) -> Result<MigrationOutcome> {
    let found = read_user_version(conn).await?;

    if found > SCHEMA_VERSION {
        return Err(DbError::Migration {
            to: SCHEMA_VERSION,
            reason: format!(
                "database is at schema v{found}; this build understands v{SCHEMA_VERSION} \
                 and will not operate on a schema it does not know. Upgrade macrame \
                 rather than opening the file with an older build."
            ),
        });
    }

    if found == 0 {
        refuse_if_occupied(conn).await?;
    }

    let mut current = found;
    while current != SCHEMA_VERSION {
        let step = STEPS
            .iter()
            .find(|s| s.from == current)
            .ok_or_else(|| no_path_from(current))?;
        apply_step(conn, step).await?;
        current = step.to;
    }

    verify(conn).await?;
    Ok(MigrationOutcome {
        from: found,
        to: SCHEMA_VERSION,
    })
}

/// Version this build stamps on databases it creates.
pub fn current_version() -> u32 {
    SCHEMA_VERSION
}

/// Refuse to lay the baseline over a database that already holds something.
///
/// `user_version` defaults to 0, so an unrelated SQLite file is indistinguishable
/// from a fresh one by version alone. Without this check, pointing macrame at the
/// wrong path would quietly add four tables and nine triggers to somebody else's
/// database — including delete guards that abort writes the owner never asked to
/// have guarded.
async fn refuse_if_occupied(conn: &libsql::Connection) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            (),
        )
        .await?;
    let objects: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };

    if objects > 0 {
        return Err(DbError::Migration {
            to: SCHEMA_VERSION,
            reason: format!(
                "database carries no macrame schema version but already holds {objects} \
                 object(s); refusing to lay the baseline over an unrelated database. \
                 Point at a new file, or delete this one deliberately."
            ),
        });
    }
    Ok(())
}

/// Explain a version with no rung leading out of it.
fn no_path_from(current: u32) -> DbError {
    let reason = if current < SCHEMA_VERSION {
        format!(
            "database is at schema v{current}, written by a pre-0.5.4 build: its \
             timestamps are second-precision and its tables carry none of the \
             canonical-form CHECK constraints (D-029). This build provides no \
             migration path — create a new database."
        )
    } else {
        format!("no migration step leads out of schema v{current}")
    };
    DbError::Migration {
        to: SCHEMA_VERSION,
        reason,
    }
}

/// Run one rung inside a single transaction, stamp included.
///
/// `user_version` is a database-header field and its write is journalled like
/// any other, so stamping inside the transaction makes "the schema exists" and
/// "the schema is declared to exist" the same commit. A crash mid-step therefore
/// leaves a database that is still honestly at its old version, rather than one
/// stamped for a schema it only partly has.
async fn apply_step(conn: &libsql::Connection, step: &Step) -> Result<()> {
    // Outside the transaction, because inside it the pragma is silently
    // ignored — see `Step::suspends_foreign_keys` for the four approaches that
    // do not work and the probe that measured them.
    if step.suspends_foreign_keys {
        conn.execute("PRAGMA foreign_keys = OFF", ()).await?;
    }

    let res = apply_step_inner(conn, step).await;

    // Restored on **every** path, including the error one. A rung that fails
    // must not leave the connection with enforcement off, even though that
    // connection is about to be discarded: the guarantee should not depend on
    // the caller's disposal habits.
    if step.suspends_foreign_keys {
        conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    }

    res
}

async fn apply_step_inner(conn: &libsql::Connection, step: &Step) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    let res: Result<()> = async {
        (step.apply)(&tx).await?;

        // Suspension is not permission. A rung that ran with enforcement off
        // must still leave a database the engine would accept, so the check
        // runs inside the transaction and its rows fail the rung — which means
        // the rollback below, not a committed database nobody checked.
        if step.suspends_foreign_keys {
            let mut rows = tx.query("PRAGMA foreign_key_check", ()).await?;
            if let Some(row) = rows.next().await? {
                let table: String = row.get(0).unwrap_or_else(|_| "?".to_string());
                return Err(DbError::Migration {
                    to: step.to,
                    reason: format!(
                        "step {:?} suspended foreign keys and left a violation \
                         in {table:?}; the rung is wrong, not the check",
                        step.name
                    ),
                });
            }
        }

        // PRAGMA takes no bind parameters; `to` is a u32 read from a const.
        tx.execute(&format!("PRAGMA user_version = {}", step.to), ())
            .await?;
        Ok(())
    }
    .await;

    match res {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(DbError::Migration {
                to: step.to,
                reason: format!("step {:?}: {e}", step.name),
            })
        }
    }
}

/// The 0.5.4 schema, applied to an empty database.
async fn baseline(conn: &libsql::Connection) -> Result<()> {
    // concepts first: links declares a foreign key into it.
    conn.execute(CREATE_CONCEPTS_TABLE, ()).await?;
    conn.execute(CREATE_LINKS_TABLE, ()).await?;
    conn.execute(CREATE_LINKS_CURRENT_TABLE, ()).await?;
    conn.execute(CREATE_TRANSACTION_LOG_TABLE, ()).await?;
    // Derivative, and last: every index in CREATE_INDICES must have its table.
    conn.execute(CREATE_ANALYTICS_ANNOTATIONS_TABLE, ()).await?;
    // Before the triggers, not after: `trg_concepts_fts_*` name this table, and
    // SQLite resolves a trigger body's tables at CREATE TRIGGER time.
    conn.execute(CREATE_CONCEPTS_FTS, ()).await?;

    for index_ddl in CREATE_INDICES {
        conn.execute(index_ddl, ()).await?;
    }

    for trigger_ddl in CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await?;
    }

    Ok(())
}

/// v4 → v5: add the FTS5 index over concept text (§5.9, D-051).
///
/// Derivative and additive, so D-036 permits it — an FTS index over `concepts`
/// is Doctrine VI's second category, disposable and reconstructible. The two
/// triggers land on `concepts`, which *is* a frozen ledger table, but a trigger
/// changes neither its columns nor its rows; the compat contract freezes the
/// table's shape, and that is untouched.
///
/// Unlike the v2 → v3 rung this one **does** backfill, and can: the index is a
/// pure function of text the ledger already holds, so `'rebuild'` reconstructs
/// exactly what the triggers would have written had they always existed. That is
/// the difference between this and D-041's annotations, where the old data was
/// destroyed and no recovery existed.
async fn add_concepts_fts(conn: &libsql::Connection) -> Result<()> {
    conn.execute(CREATE_CONCEPTS_FTS, ()).await?;
    for trigger_ddl in CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await?;
    }
    conn.execute(REBUILD_CONCEPTS_FTS, ()).await?;
    Ok(())
}

/// v2 → v3: add the derivative analytics table (D-041).
///
/// Purely additive, and additive on the *periphery* — `analytics_annotations`
/// is Doctrine VI's second category, so D-036's freeze on the ledger tables is
/// not in play. Nothing is backfilled: annotations written before v3 went into
/// `concepts.content`, which is the defect, and there is no way to tell a label
/// that landed there from the document text it replaced. Recomputing is the
/// recovery, and recomputing is what this table exists to make cheap.
async fn add_analytics_annotations(conn: &libsql::Connection) -> Result<()> {
    conn.execute(CREATE_ANALYTICS_ANNOTATIONS_TABLE, ()).await?;
    for index_ddl in CREATE_INDICES {
        conn.execute(index_ddl, ()).await?;
    }
    Ok(())
}

/// v5 → v6: index the single-open-interval probe (D-059).
///
/// Index-only and on a derivative table, so D-036 permits it on the same two
/// grounds the v3 → v4 rung stood on. Nothing is dropped this time: the new
/// index and `idx_lc_traversal_cover` serve different shapes — one needs three
/// equality columns bound, the other leads on `source_id` alone — so neither
/// subsumes the other and keeping both is the point rather than an oversight.
///
/// **This is the largest measured win in the tree and it sat proven and
/// unshipped for a full cycle**, on the stated ground that an index is a schema
/// change wanting its own rung. That was a description of the work rather than
/// an objection to it. See [`CREATE_INDICES`] for the numbers.
///
/// Nothing is backfilled because an index has nothing to backfill; `CREATE
/// INDEX` populates it from the table. That makes this the cheapest rung on the
/// ladder and the only one whose cost is a function of existing row count alone.
async fn add_open_interval_index(conn: &libsql::Connection) -> Result<()> {
    for index_ddl in CREATE_INDICES {
        conn.execute(index_ddl, ()).await?;
    }
    Ok(())
}

/// The v7 shape of `links`, pinned as text (T2.1, D-083).
///
/// **Deliberately not `ddl::CREATE_LINKS_TABLE`.** Every other rung on this
/// ladder reuses the DDL constants, and for those it is right — they create an
/// index or a derivative table, and getting today's definition is the point. A
/// *table rebuild* is different: it produces whatever shape the constant names
/// at the moment it runs, so the day `links` gains a v8 column, this rung would
/// silently take a v6 database straight to the v8 shape and stamp it v7. The
/// ladder would then have two databases both stamped v7 with different columns,
/// and the v7 → v8 rung would run against a table that already had its change.
///
/// A migration rung is a statement about the past. Pinning the text is what
/// makes it one.
const LINKS_V7: &str = r#"
CREATE TABLE links_v7 (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at),
    CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
    -- (the timestamp CHECK, spelled out for the same pinning reason)
    CHECK (valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND valid_to GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND 1)
)
"#;

/// v6 → v7: constrain `links.weight` (§4.7, T2.1, D-083).
///
/// # The only rung that rewrites a ledger table, and what that costs
///
/// SQLite has no `ADD CONSTRAINT`, so this is a full rebuild of `links` — the
/// largest table in the schema — inside [`apply_step`]'s single transaction:
/// create, copy, drop, rename, recreate triggers. Cost is O(rows) in time and
/// roughly 2× `links` in peak disk. Every other rung on this ladder is index
/// work or an additive table; this one is not, and a caller upgrading a large
/// database should expect it to take a while and to need the space.
///
/// **That 2× is an estimate and is still unmeasured**, flagged here in 0.8.0
/// when the *concepts* rung below it was measured properly
/// ([D-125](../../docs/architecture/s13-decision-register.md)). Do not read
/// across from that measurement: the concepts rung peaks at 1.09× the whole
/// file precisely because `concepts` is a small share of it, and this rung
/// rebuilds the share that is large. If anyone needs the real number,
/// `examples/v8_migration_scale_probe.rs` is the shape to copy — it needs a v6
/// fixture instead of a v7 one.
///
/// It is taken **pre-1.0 on purpose**. D-032 makes this a baseline re-issue
/// today, which is cheap; after 1.0 the compat contract (D-036) freezes the
/// ledger tables and the same change becomes an unmigration.
///
/// # Doctrine III is not violated, and the case where it would be is refused
///
/// A rebuild that *altered* an assertion would be exactly what Doctrine III
/// forbids. This one copies every row verbatim — no clamping, no rounding, no
/// dropping. Which means a database already holding a weight the new constraint
/// rejects cannot be migrated at all, and this refuses **before** touching
/// anything, with a count and an example, rather than failing halfway through a
/// copy with a bare `CHECK constraint failed`.
///
/// Such rows are reachable: until this rung, `assert_edge(weight = -1.0)` was
/// accepted by the write API and refused only at load time (§4.7). That was the
/// gap. An operator who has them must decide what those assertions meant, and
/// that is not a decision a migration can take for them.
///
/// # Order, and the trap it avoids
///
/// `DROP TABLE links` first, then rename. Dropping the table takes its four
/// triggers with it, so the rename does not reparse a schema containing trigger
/// bodies that name a table which no longer exists — the failure T1.2 hit from
/// the other direction. All triggers are `IF NOT EXISTS`, so re-running the
/// whole array afterwards recreates the four on `links` and no-ops the rest.
/// No index is defined on `links`, so there is none to rebuild.
async fn add_weight_check(conn: &libsql::Connection) -> Result<()> {
    let offending: i64 = conn
        .query(
            "SELECT COUNT(*) FROM links WHERE NOT (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')",
            (),
        )
        .await?
        .next()
        .await?
        .and_then(|r| r.get(0).ok())
        .unwrap_or(0);

    if offending > 0 {
        let example: Option<String> = conn
            .query(
                "SELECT source_id || ' -> ' || target_id || ' (' || edge_type || \
                 ') weight=' || CAST(weight AS TEXT) FROM links \
                 WHERE NOT (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real') LIMIT 1",
                (),
            )
            .await?
            .next()
            .await?
            .and_then(|r| r.get(0).ok());

        return Err(DbError::Migration {
            to: 7,
            reason: format!(
                "{offending} row(s) in `links` hold a weight the v7 constraint \
                 rejects, e.g. {}. Copying them verbatim is impossible and \
                 altering them would violate Doctrine III, so this migration \
                 refuses rather than choosing on your behalf. These rows were \
                 writable through `assert_edge` before v7 (§4.7) — decide what \
                 they were meant to assert, archive them, and retry.",
                example.as_deref().unwrap_or("<unreadable>")
            ),
        });
    }

    conn.execute(LINKS_V7, ()).await?;
    conn.execute(
        "INSERT INTO links_v7 (source_id, target_id, edge_type, valid_from, \
         recorded_at, valid_to, weight, properties) \
         SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                valid_to, weight, properties FROM links",
        (),
    )
    .await?;
    conn.execute("DROP TABLE links", ()).await?;
    conn.execute("ALTER TABLE links_v7 RENAME TO links", ())
        .await?;

    for trigger_ddl in CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await?;
    }

    Ok(())
}

/// The v8 shape of `concepts`, pinned as text (B4, D-119).
///
/// Pinned for the reason [`LINKS_V7`] states: a rung that rebuilds a table must
/// produce the shape that rung is *about*, not whatever
/// [`CREATE_CONCEPTS_TABLE`] happens to say the day it runs. A migration rung is
/// a statement about the past.
const CONCEPTS_V8: &str = r#"
CREATE TABLE concepts_v8 (
    rowid_pk         INTEGER PRIMARY KEY,
    id               TEXT NOT NULL UNIQUE,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    -- (the timestamp CHECK, spelled out for the same pinning reason)
    CHECK (valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND valid_to GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND 1)
)
"#;

/// The six triggers a v7 `concepts` carries, dropped by name before the rebuild.
///
/// By name and not by discovery: a rung is a statement about the past, and the
/// past is a fixed set. Enumerating what v7 had means a v9 trigger added later
/// cannot be silently swept up by a `DROP` loop over `sqlite_master`.
const CONCEPTS_TRIGGERS_V7: &[&str] = &[
    "trg_concepts_monotonic_ra",
    "trg_concepts_log_insert",
    "trg_concepts_log_update",
    "trg_concepts_guard_delete",
    "trg_concepts_fts_insert",
    "trg_concepts_fts_update",
];

/// The concepts delete guard **as v8 had it**: unconditional, aborting every
/// physical delete (0.9.0, C2).
///
/// Pinned here for the same reason [`CONCEPTS_V8`] and [`CONCEPTS_TRIGGERS_V7`]
/// are, and the reason is easy to miss. [`add_concepts_rowid_pk`] rebuilds
/// `concepts` and puts the triggers back by looping over [`CREATE_TRIGGERS`] —
/// which is *today's* DDL, and today's guard is marker-gated. Left alone, the
/// `v7 → v8` rung would install a trigger body that did not exist at v8, so a
/// database the ladder reports as v8 would not be a v8 database.
///
/// Harmless in the common path, because `run` never rests at an intermediate
/// version — a v7 file climbs 7 → 8 → 9 in one call and the next rung replaces
/// this body anyway. It is pinned regardless, because a rung is a statement
/// about the past and a rung that quietly writes the present into it cannot be
/// tested against a fixture: `a_v7_database_climbs_to_v8_and_gains_rowid_pk`
/// would have been asserting against whatever the current release happened to
/// think, which is the failure mode this whole file is built to avoid.
const CONCEPTS_GUARD_DELETE_V8: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_guard_delete
    BEFORE DELETE ON concepts
    BEGIN
        SELECT RAISE(ABORT, 'macrame: concepts are never physically archived (D-022)');
    END;
"#;

/// v7 → v8: `concepts` gains `rowid_pk`, the FTS index gains its third trigger,
/// and the two indices with no reader are dropped (B4, D-118, D-119).
///
/// # Why this rung must be taken pre-1.0 or never
///
/// `rowid_pk INTEGER PRIMARY KEY` means `id` stops being the primary key, and
/// SQLite allows exactly one per table. That is a **primary-key change**, which
/// [D-036](../../docs/architecture/s13-decision-register.md) forbids outright
/// after 1.0 and classes as needing a major version with an explicit ETL path.
/// Pre-1.0, D-032 makes it a baseline re-issue. There is no third option and no
/// later cheap moment.
///
/// # What it buys
///
/// `concepts_fts` is external-content keyed on `concepts`'s rowid, which through
/// v7 was **implicit** — and `VACUUM` renumbers implicit rowids, decoupling the
/// index from its rows with no error and no integrity-check failure. D-071
/// showed the hazard unreachable today only because the delete guard is
/// unconditional, so rowids are dense and the renumbering is the identity map.
/// 0.9.0's archival makes them sparse. This installs the fix while the fix is
/// still free, and installs `trg_concepts_fts_delete` in the same rung.
///
/// **What it does not buy, corrected in place.** This paragraph read "*so 0.9.0
/// needs no migration of its own*". That is wrong (D-126). The rung ships C2's
/// steps 1 and 3; step 2 — `trg_concepts_guard_delete` becoming marker-gated —
/// still needs a `v8 → v9` rung of its own, since re-issuing the baseline keeps
/// the old trigger body and `verify` would not notice. It is cheap (a `DROP
/// TRIGGER` and a `CREATE`, no table rebuild) but it is not nothing.
///
/// # Why it needs `suspends_foreign_keys`, and what still checks the result
///
/// `concepts` has inbound foreign keys from `links` (twice),
/// `analytics_annotations` and every registered `embeddings_*` table, so the
/// `links`-style rebuild is not available: the `DROP TABLE` fails with keys on,
/// and the three obvious ways to turn them off inside the transaction all fail
/// differently. See [`Step::suspends_foreign_keys`] for the four measured
/// refutations. [`apply_step`] therefore toggles the pragma around the
/// transaction and runs `PRAGMA foreign_key_check` inside it before committing.
///
/// **One consequence worth stating.** That check reports violations across the
/// whole database, not only ones this rung could have caused. A v7 file that
/// already held an orphaned `links` row — reachable only if it was written with
/// enforcement off — will fail to migrate. That is the right outcome and it is
/// not a silent one: the error names the table.
///
/// # Order, and the two traps in it
///
/// The triggers and `concepts_fts` come down **before** the table is touched,
/// not after. Recreating the triggers while the old FTS table was still present
/// would bind them to an index about to be dropped, and dropping `concepts_fts`
/// while triggers still named it is the schema-reparse failure the `links` rung
/// hit from the other direction. So: indices, triggers, FTS, then the rebuild,
/// then the new FTS, then the triggers, then the rebuild of the index content.
///
/// `rowid` is copied into `rowid_pk` **by value** rather than left to
/// auto-assign. On today's dense numbering the two agree, so this looks
/// redundant; it is what makes the rung correct on a file whose rowids are not
/// dense, and it means the migration preserves row identity rather than merely
/// preserving row order.
///
/// # What it costs, measured (0.8.0, [D-125])
///
/// This rung rewrites a ledger table on somebody's data while holding the write
/// lock, so the operator's two questions are how long they are down and how much
/// free disk they need first. Both are measured rather than estimated —
/// `cargo run --release --example v8_migration_scale_probe`, four scales up to
/// 200k concepts / 600k links / 800k log rows (a 733 MiB file):
///
/// * **Time is linear at ~10–13 µs per concept**, 2.7 s at 200k. It scales with
///   `concepts`, not with the file.
/// * **Peak disk is 1.09× the starting file**, flat across every scale, and it
///   **settles back to 1.00×** after a checkpoint. So the rung wants ~10%
///   headroom transiently and keeps none of it. The intuition that a
///   copy-and-swap needs 2× is right about the *table* and wrong about the
///   *file*, because `concepts` is a small share of a database whose bulk is
///   `links` and `transaction_log`.
/// * **[`suspends_foreign_keys`]'s `PRAGMA foreign_key_check` is 13–17% of the
///   rung**, a stable share. It is a whole-database scan, so unlike the rest of
///   the rung it grows with `links` and the log rather than with `concepts` —
///   on a database with an unusually large ledger relative to its concepts it
///   will dominate.
///
/// The `links` rung above still carries an *estimated* 2×, which this
/// measurement does not transfer to: that one rebuilds the big table, and the
/// ratio that makes this rung cheap is exactly what makes that one expensive.
async fn add_concepts_rowid_pk(conn: &libsql::Connection) -> Result<()> {
    // (a) The two indices with no reader (D-089, completed by D-118).
    conn.execute("DROP INDEX IF EXISTS idx_annotations_label", ())
        .await?;
    conn.execute("DROP INDEX IF EXISTS idx_lc_tgt_active", ())
        .await?;

    // (b) Clear the way: triggers, then the FTS index, then the table.
    for name in CONCEPTS_TRIGGERS_V7 {
        conn.execute(&format!("DROP TRIGGER IF EXISTS {name}"), ())
            .await?;
    }
    conn.execute("DROP TABLE IF EXISTS concepts_fts", ()).await?;

    conn.execute(CONCEPTS_V8, ()).await?;
    conn.execute(
        "INSERT INTO concepts_v8 (rowid_pk, id, title, content, embedding_model, \
         valid_from, valid_to, recorded_at, retired) \
         SELECT rowid, id, title, content, embedding_model, \
                valid_from, valid_to, recorded_at, retired \
         FROM concepts ORDER BY rowid",
        (),
    )
    .await?;
    conn.execute("DROP TABLE concepts", ()).await?;
    conn.execute("ALTER TABLE concepts_v8 RENAME TO concepts", ())
        .await?;

    // (c) Put it back, in the order the trigger bodies require.
    conn.execute(CREATE_CONCEPTS_FTS, ()).await?;
    for trigger_ddl in CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await?;
    }

    // (d) …then correct the one trigger the loop above gets wrong. `CREATE_TRIGGERS`
    // is today's DDL, and today's concepts guard is marker-gated (C2); v8's was
    // unconditional. See `CONCEPTS_GUARD_DELETE_V8`. The `IF NOT EXISTS` in both
    // bodies is why this needs the explicit DROP: without it the loop's version
    // stays, because a re-issue of an existing name keeps the old body.
    conn.execute("DROP TRIGGER IF EXISTS trg_concepts_guard_delete", ())
        .await?;
    conn.execute(CONCEPTS_GUARD_DELETE_V8, ()).await?;

    conn.execute(REBUILD_CONCEPTS_FTS, ()).await?;

    Ok(())
}

/// v8 → v9: the concepts delete guard becomes marker-gated (C2, D-126).
///
/// The whole rung is two statements, and the first is the one that matters.
/// `CREATE TRIGGER IF NOT EXISTS` on an existing name **keeps the old body** —
/// verified against libSQL 0.9.30, not assumed — so re-issuing the baseline
/// against a v8 database leaves the unconditional guard exactly where it was.
/// The `DROP` is therefore not tidiness; it is the only thing that makes the
/// rung do anything at all. That, plus [`verify`] having compared trigger names
/// and never bodies, is why D-126 could conclude this needs a rung rather than a
/// baseline re-issue: without both, a v8 database opened by 0.9.0 code would
/// carry the old guard, pass verification in silence, and then refuse concept
/// archival at the trigger.
///
/// No table is rebuilt and no row moves, so this costs a schema write and
/// nothing else — a `DROP TRIGGER` and a `CREATE TRIGGER`, independent of how
/// large the database is. It is the cheapest rung this ladder has.
async fn gate_concepts_guard_on_marker(conn: &libsql::Connection) -> Result<()> {
    conn.execute("DROP TRIGGER IF EXISTS trg_concepts_guard_delete", ())
        .await?;
    conn.execute(CREATE_CONCEPTS_GUARD_DELETE, ()).await?;
    Ok(())
}

/// v3 → v4: swap `idx_lc_src_active` for the traversal covering index (D-042).
///
/// Index-only, and on a derivative table, so D-036 permits it twice over. The
/// drop is the point as much as the create: the new index has the same seek
/// column and strictly more payload, so keeping the old one would cost a second
/// index write on every assertion and buy nothing. Order matters only for peak
/// disk — create first so the traversal is never left without an index at all,
/// even though the whole rung is one transaction.
async fn add_traversal_cover(conn: &libsql::Connection) -> Result<()> {
    for index_ddl in CREATE_INDICES {
        conn.execute(index_ddl, ()).await?;
    }
    conn.execute("DROP INDEX IF EXISTS idx_lc_src_active", ())
        .await?;
    Ok(())
}

/// The tables the baseline declares, by name, for [`verify`].
pub(crate) const BASELINE_TABLES: &[&str] = &[
    "concepts",
    "links",
    "links_current",
    "transaction_log",
    "analytics_annotations",
    "concepts_fts",
];

/// Confirm the database actually holds what the DDL claims to create.
///
/// Cheap insurance against the failure mode `IF NOT EXISTS` is built to hide: a
/// statement that no-ops instead of creating. It also catches the DDL arrays and
/// reality drifting apart — add a trigger to [`CREATE_TRIGGERS`] that fails to
/// compile as written and it is missing here rather than at the first write that
/// needed it.
///
/// **Presence by name, not a count of everything present.** The original
/// counted `sqlite_master` and required exactly four tables, which made
/// verification fail on any database carrying an object the baseline did not
/// create — and this schema now has three legitimate sources of those. A
/// registered embedding model adds `embeddings_<model>` (§4.1); libSQL's vector
/// index adds `libsql_vector_meta_shadow`, a shadow table and a shadow index of
/// its own; and D-036 explicitly permits post-1.0 migrations to add indexes. A
/// count treats all three as corruption and refuses to open a healthy file. What
/// verification is actually for is the absence of something required, so that is
/// what it now checks.
async fn verify(conn: &libsql::Connection) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT type, name, COALESCE(sql, '') FROM sqlite_master \
             WHERE type IN ('table','trigger','index')",
            (),
        )
        .await?;

    let mut present: Vec<(String, String)> = Vec::new();
    let mut bodies: Vec<(String, String)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let (kind, name, sql): (String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        if kind == "trigger" {
            bodies.push((name.clone(), sql));
        }
        present.push((kind, name));
    }
    let has = |kind: &str, name: &str| {
        present
            .iter()
            .any(|(k, n)| k == kind && n.eq_ignore_ascii_case(name))
    };

    let mut missing: Vec<String> = Vec::new();
    for table in BASELINE_TABLES {
        if !has("table", table) {
            missing.push(format!("table {table}"));
        }
    }
    for name in trigger_names() {
        if !has("trigger", &name) {
            missing.push(format!("trigger {name}"));
        }
    }
    for name in index_names() {
        if !has("index", &name) {
            missing.push(format!("index {name}"));
        }
    }

    if !missing.is_empty() {
        return Err(DbError::Migration {
            to: SCHEMA_VERSION,
            reason: format!(
                "schema verification failed: the database is stamped v{SCHEMA_VERSION} \
                 but is missing {}: {}",
                missing.len(),
                missing.join(", ")
            ),
        });
    }

    // The three delete guards are checked by *body*, not only by name (0.9.0,
    // C2, D-126). Presence was never the property that mattered for these: a
    // guard with the right name and the wrong body is a guard that refuses a
    // legal archive or permits an illegal delete, and the check above cannot
    // see the difference. That is not hypothetical — it is exactly what
    // `CREATE TRIGGER IF NOT EXISTS` produces when a baseline is re-issued
    // against a database whose guard predates a change, and it is the reason
    // the concepts guard needed a rung instead.
    //
    // The probe is `macrame_archive_session`, not the trigger's whole text.
    // Comparing full bodies would fail on whitespace and would have to be
    // updated by hand every time a guard is reworded, which makes it the kind
    // of check people disable. What is asserted is the one property all three
    // share and none may lose: **this guard is gated on the archive session.**
    let ungated: Vec<&str> = DELETE_GUARDS
        .iter()
        .filter(|name| {
            bodies
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .is_none_or(|(_, sql)| !sql.contains(ARCHIVE_SESSION_MARKER))
        })
        .copied()
        .collect();

    if !ungated.is_empty() {
        return Err(DbError::Migration {
            to: SCHEMA_VERSION,
            reason: format!(
                "schema verification failed: the database is stamped v{SCHEMA_VERSION} \
                 but {} delete guard(s) do not probe the archive-session marker: {}. \
                 A guard with the right name and a pre-v9 body refuses archival it \
                 should permit; upgrading through the ladder replaces it.",
                ungated.len(),
                ungated.join(", ")
            ),
        });
    }

    Ok(())
}

/// The delete guards, whose bodies [`verify`] checks rather than only their
/// names.
///
/// Listed rather than discovered, on the same reasoning as
/// [`CONCEPTS_TRIGGERS_V7`]: the property being asserted is that *these three*
/// tables cannot lose rows outside an archive session, and a loop over whatever
/// happens to be named `*_guard_delete` would assert whatever the schema
/// happens to contain.
const DELETE_GUARDS: &[&str] = &[
    "trg_concepts_guard_delete",
    "trg_links_guard_delete",
    "trg_txlog_guard_delete",
];

/// The object names the DDL creates, recovered from the DDL itself.
///
/// Parsed rather than listed separately so that adding a trigger to
/// [`CREATE_TRIGGERS`] extends what `verify` requires, with no second list to
/// remember. A hand-kept list of names beside the statements that create them is
/// the drift D-035 is about.
fn names_after(ddl: &[&str], keyword: &str) -> Vec<String> {
    ddl.iter()
        .filter_map(|stmt| {
            let lower = stmt.to_ascii_lowercase();
            let at = lower.find(keyword)? + keyword.len();
            Some(
                stmt[at..]
                    .split_whitespace()
                    .next()?
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                    .to_string(),
            )
        })
        .filter(|n| !n.is_empty())
        .collect()
}

fn trigger_names() -> Vec<String> {
    names_after(CREATE_TRIGGERS, "create trigger if not exists ")
}

fn index_names() -> Vec<String> {
    names_after(CREATE_INDICES, "create index if not exists ")
}

async fn read_user_version(conn: &libsql::Connection) -> Result<u32> {
    // PRAGMA user_version yields a row, so it must go through query(), not
    // execute() -- libsql rejects a statement that returns rows from execute().
    let mut rows = conn.query("PRAGMA user_version", ()).await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<u32>(0)?),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rung that does not advance the version would spin `run`'s loop forever.
    #[test]
    fn every_step_advances() {
        for step in STEPS {
            assert!(
                step.to > step.from,
                "step {:?} does not advance ({} -> {})",
                step.name,
                step.from,
                step.to
            );
        }
    }

    /// Two rungs out of the same version make the ladder ambiguous: `run` takes
    /// whichever comes first in the array, which is not a decision anyone made.
    #[test]
    fn no_two_steps_share_an_origin() {
        for (i, a) in STEPS.iter().enumerate() {
            for b in &STEPS[i + 1..] {
                assert_ne!(a.from, b.from, "two steps start at v{}", a.from);
            }
        }
    }

    /// The ladder has to actually reach the version this build stamps, or every
    /// fresh open fails with "no migration step leads out of v0".
    #[test]
    fn the_ladder_reaches_the_current_version() {
        let mut current = 0;
        for _ in 0..STEPS.len() {
            match STEPS.iter().find(|s| s.from == current) {
                Some(step) => current = step.to,
                None => break,
            }
        }
        assert_eq!(current, SCHEMA_VERSION);
    }

    /// `verify` requires every name this returns, so a parse that silently
    /// yielded nothing would turn verification into a no-op that passes on an
    /// empty database.
    #[test]
    fn every_trigger_and_index_yields_a_name() {
        let triggers = super::trigger_names();
        assert_eq!(triggers.len(), CREATE_TRIGGERS.len());
        assert!(
            triggers.iter().all(|n| n.starts_with("trg_")),
            "{triggers:?}"
        );

        let indices = super::index_names();
        assert_eq!(indices.len(), CREATE_INDICES.len());
        assert!(indices.iter().all(|n| n.starts_with("idx_")), "{indices:?}");
    }

    /// v1 belongs to the pre-canonical schema and must stay unreachable, or a
    /// 0.5.3 database silently becomes a supported input again.
    #[test]
    fn legacy_v1_has_no_rung() {
        assert!(
            !STEPS.iter().any(|s| s.from == 1),
            "a step out of v1 reintroduces pre-0.5.4 databases as a supported input"
        );
    }

    // -- the foreign-key suspension mechanism (0.8.0, B4, D-117) -------------
    //
    // No shipped rung sets `suspends_foreign_keys` yet — v7 → v8 is what will.
    // The mechanism lands first and is tested first, because the thing that
    // makes it safe is not that the rebuild works (the probe measured that) but
    // that a rung which suspends enforcement **still cannot commit a violation**.
    // A flag that turns checking off is only acceptable if something else turns
    // verification on, and that is what these two tests hold.

    async fn scratch() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
        conn.execute("CREATE TABLE parent (id TEXT PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute(
            "CREATE TABLE child (id TEXT PRIMARY KEY, p TEXT NOT NULL \
             REFERENCES parent(id))",
            (),
        )
        .await
        .unwrap();
        conn.execute("INSERT INTO parent VALUES ('a')", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO child VALUES ('c', 'a')", ())
            .await
            .unwrap();
        conn
    }

    /// The shape the probe found: rebuild a table that has inbound foreign keys
    /// by dropping and renaming, which is impossible with enforcement on.
    #[tokio::test]
    async fn a_suspending_rung_can_rebuild_a_table_with_inbound_keys() {
        let conn = scratch().await;
        let step = Step {
            from: 0,
            to: 99,
            name: "test-rebuild",
            suspends_foreign_keys: true,
            apply: |tx| {
                Box::pin(async move {
                    tx.execute("CREATE TABLE parent_new (rowid_pk INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE)", ()).await?;
                    tx.execute("INSERT INTO parent_new (id) SELECT id FROM parent ORDER BY rowid", ()).await?;
                    tx.execute("DROP TABLE parent", ()).await?;
                    tx.execute("ALTER TABLE parent_new RENAME TO parent", ()).await?;
                    Ok(())
                })
            },
        };

        apply_step(&conn, &step).await.expect("the rung must apply");

        // The rebuild happened, the child still resolves, and — the part that
        // matters — enforcement is back on afterwards.
        let mut rows = conn
            .query("SELECT rowid_pk FROM parent WHERE id = 'a'", ())
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some(), "parent lost its row");

        let orphan = conn
            .execute("INSERT INTO child VALUES ('d', 'nonexistent')", ())
            .await;
        assert!(
            orphan.is_err(),
            "foreign keys were not restored after the rung"
        );
    }

    /// **The reason the flag is safe.** A rung that suspends enforcement and
    /// leaves a genuine violation must not commit: `foreign_key_check` runs
    /// inside the transaction and its rows fail the rung.
    ///
    /// Without this, `suspends_foreign_keys` would be a way to write a corrupt
    /// database on purpose and have the ladder call it a success.
    #[tokio::test]
    async fn a_suspending_rung_that_leaves_a_violation_is_refused() {
        let conn = scratch().await;
        let step = Step {
            from: 0,
            to: 99,
            name: "test-orphan",
            suspends_foreign_keys: true,
            // Drops the parent and puts nothing back: `child.p` now points at
            // nothing. With enforcement on this could not even be attempted,
            // which is exactly why the check has to exist.
            apply: |tx| {
                Box::pin(async move {
                    tx.execute("DROP TABLE parent", ()).await?;
                    tx.execute("CREATE TABLE parent (id TEXT PRIMARY KEY)", ())
                        .await?;
                    Ok(())
                })
            },
        };

        let err = apply_step(&conn, &step)
            .await
            .expect_err("a rung that orphans a row must not commit");
        // Pinned to *this* wording, not to "foreign key" generally. A DDL
        // statement failing for its own reasons would also produce an error
        // mentioning foreign keys, and the test would then pass while proving
        // nothing about the check that is the point of the flag.
        let text = err.to_string();
        assert!(
            text.contains("suspended foreign keys and left a violation"),
            "the rung must fail at `foreign_key_check`, not merely fail: {text}"
        );
        assert!(text.contains("test-orphan"), "the error should name the rung: {text}");

        // Rolled back, and enforcement restored despite the failure.
        let mut rows = conn.query("PRAGMA user_version", ()).await.unwrap();
        let v: u32 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(v, 0, "a failed rung must not stamp its version");

        let orphan = conn
            .execute("INSERT INTO child VALUES ('d', 'nonexistent')", ())
            .await;
        assert!(
            orphan.is_err(),
            "foreign keys must be restored even when the rung failed"
        );
    }
}
