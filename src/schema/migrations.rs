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
pub const SCHEMA_VERSION: u32 = 16;

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
    /// # Why a rung would need this
    ///
    /// Two rungs need it, for reasons that share only the remedy.
    ///
    /// **v7 → v8 rebuilds a table with inbound foreign keys** and so cannot use
    /// the `links`-style recipe; the four approaches below are what
    /// `examples/concepts_rebuild_probe.rs` measured and ruled out.
    ///
    /// **v11 → v12 adds a column carrying a `REFERENCES` clause** to tables
    /// that already hold rows. libSQL applies SQLite's
    /// "a `REFERENCES` column added by `ALTER` must default to NULL" rule
    /// *dynamically*: it refuses only when the table is non-empty **and** keys
    /// are on (probe §15). Being inside a transaction is not the axis — the
    /// pragma is, which is exactly what this flag toggles, and the resulting
    /// key is fully real (see [`BRANCH_COLUMN`]). Note the asymmetry the flag
    /// makes visible: a **fresh** v12 database never needs the suspension,
    /// because there the clause sits in a `CREATE TABLE` with no rows to
    /// validate. The two paths reach the same schema by different routes,
    /// which is the shape D-035 says to say out loud rather than discover.
    ///
    /// # Why the obvious ways do not work
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
        from: 9,
        to: 10,
        name: "concepts-log-insert-marker-gated",
        // Same shape as the rung below and for the same reason: one trigger
        // replaced, no table touched.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(gate_concepts_log_insert_on_marker(conn)),
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
    Step {
        from: 12,
        to: 13,
        name: "branches-archive-gate",
        // One `DROP TRIGGER` and one `CREATE TRIGGER`. No row moves, no table is
        // rebuilt, and the trigger names no table but the one it is on — so the
        // foreign keys that forced the flag on the v7 -> v8 and v11 -> v12 rungs
        // are not involved.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(gate_branches_delete_guard(conn)),
    },
    Step {
        from: 13,
        to: 14,
        name: "lineage-cut-index",
        // One `CREATE INDEX` on an existing derivative table. Nothing is
        // rebuilt and no row moves, which is the same ground the v3 -> v4,
        // v5 -> v6 and v10 -> v11 rungs stood on.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_lineage_cut_index(conn)),
    },
    Step {
        from: 15,
        to: 16,
        name: "log-integrity-bit",
        // Nothing declares a foreign key into or out of the new table, and the
        // seed reads `transaction_log` without writing it.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_log_integrity(conn)),
    },
    Step {
        from: 14,
        to: 15,
        name: "links-lineage-key",
        // The second rung on this ladder to rebuild `links`, and it takes the
        // v6 -> v7 rung's answer to the same question: nothing declares a
        // foreign key *into* `links`, so the drop and rename need no
        // suspension. Its own `REFERENCES concepts(id)` columns are satisfied
        // by every row being copied, because they were satisfied before.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_links_lineage_key(conn)),
    },
    Step {
        from: 11,
        to: 12,
        name: "branch-storage",
        // Not for the rebuild — `links_current` carries no inbound foreign key,
        // so it takes the `links`-style recipe the v7 -> v8 rung could not use.
        // For the three `ADD COLUMN`s: the new column carries a `REFERENCES`
        // clause, and libSQL refuses that on a table that already holds rows
        // while keys are on. Second reason, same flag — see
        // `Step::suspends_foreign_keys`.
        suspends_foreign_keys: true,
        apply: |conn| Box::pin(add_branch_storage(conn)),
    },
    Step {
        from: 10,
        to: 11,
        name: "links-archive-indices",
        // Two `CREATE INDEX`es on an existing table. No row moves and no table
        // is rebuilt, so the inbound foreign keys that forced the flag on the
        // v7 -> v8 rung are not involved.
        suspends_foreign_keys: false,
        apply: |conn| Box::pin(add_links_archive_indices(conn)),
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
    // branches first, and seeded immediately: every ledger table's `branch_id`
    // defaults to `'main'` and declares a foreign key onto this row, so a
    // database without it cannot accept a single write (§15.2, D-214).
    conn.execute(CREATE_BRANCHES_TABLE, ()).await?;
    seed_root_branch(conn).await?;
    // concepts next: links declares a foreign key into it.
    conn.execute(CREATE_CONCEPTS_TABLE, ()).await?;
    conn.execute(CREATE_LINKS_TABLE, ()).await?;
    conn.execute(CREATE_LINKS_CURRENT_TABLE, ()).await?;
    conn.execute(CREATE_TRANSACTION_LOG_TABLE, ()).await?;
    // Derivative, and last: every index in CREATE_INDICES must have its table.
    conn.execute(CREATE_ANALYTICS_ANNOTATIONS_TABLE, ()).await?;
    // v16 (W14.5, D-249). The same seed statement the rung runs, not a
    // literal 0: a baseline log is empty and the answer is 0, but writing the
    // answer here rather than deriving it is how the two paths start to
    // disagree (D-035).
    conn.execute(CREATE_LOG_INTEGRITY_TABLE, ()).await?;
    conn.execute(SEED_LOG_INTEGRITY, ()).await?;
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

/// v15 → v16: the log records whether anything has left it (0.15.7, W14.5, [D-249]).
///
/// Review C-5: the reach guard's intactness test was `COUNT(*)` over the whole
/// hot log, on every recorded-time read below the newest surviving stamp —
/// 32.6 ms at 500,000 rows, in front of an id-bounded hydration that is flat at
/// 0.14 ms.
/// The fact is one bit and the storage knows it the moment it becomes true.
///
/// # The seed is derived, and it is stricter than the query it retires
///
/// A database arriving here may already have been archived, so the initial
/// value cannot be assumed. [`SEED_LOG_INTEGRITY`] derives it from the log's
/// `sqlite_sequence` high-water mark, which counts every id ever allocated and
/// does not fall when rows are deleted ([D-049] for why a rollback is not a
/// deletion). So a database migrated at v16 starts from the truth about its own
/// history, whatever that history was — one statement, once, on the rung
/// instead of a scan on every read.
///
/// It is deliberately *not* the comparison the guard used. `MIN(seq_id) = 1 AND
/// COUNT(*) = MAX(seq_id)` is exact on a log with rows in it and blind on an
/// empty one, where it answers *intact* — right for a database that has never
/// been written, wrong for one archived down to nothing, and those are the two
/// states a bit about archiving most needs to tell apart. Seeding from the
/// high-water mark tells them apart: absent for the first, positive for the
/// second. `the_bit_agrees_with_the_count_it_replaced` pins both halves of that
/// — the agreement everywhere else, and the disagreement here.
///
/// [D-049]: ../../docs/architecture/s13-decision-register.md#d-049
/// [D-249]: ../../docs/architecture/s13-decision-register.md#d-249
async fn add_log_integrity(conn: &libsql::Connection) -> Result<()> {
    conn.execute(CREATE_LOG_INTEGRITY_TABLE, ()).await?;
    conn.execute(SEED_LOG_INTEGRITY, ()).await?;
    // After the seed: the trigger must not fire during it, and it cannot —
    // the seed does not delete — but the ordering is what a reader checks.
    conn.execute(CREATE_TXLOG_MARK_GAP, ()).await?;
    Ok(())
}

/// v11 → v12: the branch storage model (§15.2, W12.2, [D-214]).
///
/// Storage only, in the sense that matters: no write-path changes, and every
/// existing `INSERT` in the crate still omits `branch_id` and takes the
/// default.
///
/// **The public API gate does move, by +18 items and -0**, and the plan's
/// prediction that it would not was wrong rather than nearly right. Every
/// added item is schema text in [`crate::schema::ddl`], whose whole purpose is
/// to publish the schema — thirteen new consts for the `branches` table, its
/// seed, the column, the four guards and the three abort messages; four
/// promotions of trigger bodies that were anonymous entries in
/// [`CREATE_TRIGGERS`] until a rung needed to name them; and three variants on
/// the `#[non_exhaustive]` `AbortKind`, which is what that attribute is for.
/// Nothing removed, nothing narrowed. The gate is still the check that would
/// catch a leak — it simply had something true to report.
///
/// # What the shape is, and what it refuses to be
///
/// Links and the transaction log **branch**; concepts do not. `concepts.id`
/// stays `NOT NULL UNIQUE` and stays the parent of `links.source_id` and
/// `links.target_id`, so `branch_id` on `concepts` is **provenance** — where a
/// concept was minted — and never identity. `examples/branch_identity_probe.rs`
/// measured the two alternatives and both break something the design depends
/// on: widening uniqueness to `(id, branch_id)` leaves today's single-column
/// foreign keys accepting `CREATE` and failing **every insert** with `foreign
/// key mismatch` (§3), and a composite key `(source_id, branch_id)` forbids
/// copy-on-write outright (§4), which is the whole economy of a fork.
///
/// # Why `links_current` is rebuilt and the other three are altered
///
/// `branch_id` has to be **in the primary key** of `links_current`, not merely
/// on it: the table is one row per open belief about an edge, and two lineages
/// believing different things about one edge is two rows. Probe §5 confirmed
/// the split — `links` accepts both rows because `recorded_at` is already in
/// its key, and `links_current` refuses the second. SQLite cannot add a column
/// to a primary key, so the table is re-derived rather than described: it is
/// derivative under Doctrine VI, `rebuild_within` already knows how to
/// reconstruct it from `links`, and re-deriving cannot disagree with the ledger
/// the way a hand-written `INSERT … SELECT` can.
///
/// `branch_id` goes **last** in the key on purpose. The autoindex keeps its
/// leading columns, so D-059's primary-key-versus-covering-index contest is
/// unperturbed by this rung; whether a branch-leading composition reads better
/// is §15.3's measurement to make, not a shape to guess at now (F-33).
///
/// # The triggers, and the one that is easy to miss
///
/// Three log triggers are redefined so the log row carries the lineage the
/// write actually happened on. Without that every entry reads `'main'`, and the
/// fold's new `PARTITION BY … branch_id` would partition on a constant — the
/// widened folds and these triggers are one repair in two files, not two
/// changes. `DROP` then `CREATE`, never a re-issue: `CREATE TRIGGER IF NOT
/// EXISTS` against an existing name keeps the **old body**, which is the lesson
/// [`CONCEPTS_GUARD_DELETE_V8`] already records.
///
/// # Why this rung suspends foreign keys
///
/// Not for the `links_current` rebuild — nothing declares a key into it. For
/// the three `ADD COLUMN`s: libSQL refuses a `REFERENCES` column added to a
/// table that already holds rows while enforcement is on, and every database
/// climbing this rung holds rows by definition. [`Step::suspends_foreign_keys`]
/// toggles the pragma outside the transaction, which is the only placement that
/// works, and `apply_step` re-checks with `PRAGMA foreign_key_check` inside it
/// before committing. The rung is still one transaction and one commit.
///
/// [D-214]: ../../docs/architecture/s13-decision-register.md
async fn add_branch_storage(conn: &libsql::Connection) -> Result<()> {
    // The register and its root, before any column defaults to a row that has
    // to exist for the foreign key to be satisfiable.
    conn.execute(CREATE_BRANCHES_TABLE, ()).await?;
    seed_root_branch(conn).await?;

    // Metadata-only: SQLite records a constant default in the schema header and
    // rewrites no row. Measured at 83-139 microseconds over 20,000 rows (probe
    // §1). The `REFERENCES` clause is why the step suspends foreign keys — on a
    // populated table with keys on, libSQL refuses it. See [`BRANCH_COLUMN`].
    for table in ["concepts", "links", "transaction_log"] {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {BRANCH_COLUMN}"),
            (),
        )
        .await?;
    }

    // `links_current` instead gets the `links` recipe: drop, re-create with the
    // widened key, re-derive. Safe here and not on `concepts` because nothing
    // declares a foreign key into it.
    conn.execute("DROP TABLE links_current", ()).await?;
    conn.execute(CREATE_LINKS_CURRENT_TABLE, ()).await?;

    // `DROP TABLE` took the table's indices with it, and neither the `CREATE`
    // above nor the rebuild below restores them — one declares a table and the
    // other fills one. Without these two the database is stamped v12 and fails
    // its own open-time verification, which is how this was found.
    conn.execute(LC_TRAVERSAL_COVER, ()).await?;
    conn.execute(LC_OPEN_INTERVAL, ()).await?;

    // Triggers whose bodies name columns that just changed. Dropped by name
    // first, because `IF NOT EXISTS` would silently keep the pre-v12 body and
    // leave a database the ladder calls v12 that logs without lineage.
    for (name, ddl) in [
        ("trg_concepts_log_insert", CREATE_CONCEPTS_LOG_INSERT),
        ("trg_concepts_log_update", CREATE_CONCEPTS_LOG_UPDATE),
        ("trg_links_log_insert", CREATE_LINKS_LOG_INSERT),
        ("trg_links_current_sync", CREATE_LINKS_CURRENT_SYNC),
        ("trg_links_single_open", CREATE_LINKS_SINGLE_OPEN),
    ] {
        conn.execute(&format!("DROP TRIGGER IF EXISTS {name}"), ())
            .await?;
        conn.execute(ddl, ()).await?;
    }

    // New guards. `IF NOT EXISTS` is correct for these: no earlier body exists
    // to be kept.
    for ddl in [
        CREATE_CONCEPTS_GUARD_LINEAGE,
        CREATE_CONCEPTS_GUARD_BRANCH,
        CREATE_BRANCHES_GUARD_UPDATE,
        CREATE_BRANCHES_GUARD_DELETE,
    ] {
        conn.execute(ddl, ()).await?;
    }

    // Last, and inside the same transaction: the materialization is re-derived
    // only once every trigger that maintains it speaks v12.
    crate::integrity::rebuild::rebuild_within(conn, crate::integrity::rebuild::Verify::Yes).await?;

    Ok(())
}

/// Insert the root lineage, shared by the baseline and the rung.
///
/// One helper rather than two call sites composing the same statement, because
/// `'main'` spliced twice is `'main'` spelled two ways eventually.
async fn seed_root_branch(conn: &libsql::Connection) -> Result<()> {
    let now = crate::util::timestamp::format(std::time::SystemTime::now());
    conn.execute(SEED_MAIN_BRANCH, libsql::params![now]).await?;
    Ok(())
}

/// Every trigger v12 introduced or redefined, by name (§15.2, D-214).
///
/// Consulted by [`triggers_before_v12`] and by nothing else. Kept as names
/// rather than folded into that function so the two halves of the rule — what
/// v12 touched, and what a pre-v12 rung installs instead — are separately
/// readable.
const V12_TRIGGERS: &[&str] = &[
    // New at v12: three of these sit on `branches`, which is why a pre-v12 rung
    // installing them fails outright rather than merely installing the wrong
    // body.
    "trg_concepts_cross_lineage",
    "trg_concepts_branch_immutable",
    "trg_branches_frozen_update",
    "trg_branches_frozen_delete",
    // Redefined at v12 to name `branch_id`. Their v11 bodies are in
    // [`TRIGGERS_V11`].
    "trg_links_current_sync",
    "trg_links_single_open",
    "trg_concepts_log_insert",
    "trg_concepts_log_update",
    "trg_links_log_insert",
];

/// Triggers introduced *after* v12, which a pre-v12 rung must also leave out
/// (v16, W14.5, [D-249](../../docs/architecture/s13-decision-register.md#d-249)).
///
/// A second list rather than more entries in [`V12_TRIGGERS`], because the two
/// exclusions are excluded for opposite reasons and a reader has to be able to
/// tell them apart: v12's are left out because the rung must install their
/// *older* bodies, listed in [`TRIGGERS_V11`]; these are left out because at
/// that point on the ladder they have no older body — they do not exist yet,
/// and `trg_txlog_mark_gap` names a table three rungs away from being created.
/// It failed loudly rather than quietly, which is the one mercy: the v6 → v7
/// rung deletes log rows, so the trigger fired against a missing table and the
/// climb stopped.
const LATER_TRIGGERS: &[&str] = &["trg_txlog_mark_gap"];

/// The five redefined triggers **as v11 had them** (§15.2, D-214).
///
/// Pinned for the reason [`CONCEPTS_LOG_INSERT_V9`] states, and the reason has
/// now bitten twice: a rung that restores triggers from today's
/// [`CREATE_TRIGGERS`] installs *today's* bodies on a database several versions
/// short of them. There it produced a v8 database that stopped logging concept
/// inserts; here it would produce a v5 database whose sync trigger writes a
/// column `links_current` does not have.
///
/// `trg_concepts_log_insert` is the marker-gated v10 body, not the v9 one —
/// [`add_concepts_rowid_pk`] still corrects it back to [`CONCEPTS_LOG_INSERT_V9`]
/// afterwards, because that rung is about v8 and this list is about v11.
const TRIGGERS_V11: &[&str] = &[
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
    // The abort text is written out rather than spliced from
    // `abort_single_open!()`, which is private to `ddl`. Held to the const by
    // `the_pinned_v11_triggers_carry_the_messages_the_crate_declares` below,
    // so a divergence is a red test rather than a message that reads wrong.
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
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_insert
    AFTER INSERT ON concepts
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = '"#,
        "macrame_archive_session",
        r#"'
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
    "#
    ),
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

/// The trigger set a rung *below* v12 installs: today's, minus what v12
/// introduced, plus the v11 bodies of what v12 redefined.
///
/// Three rungs rebuild a table and put the triggers back, and each has to put
/// back the triggers **of its own era**. The alternative — a pinned list per
/// rung — was rejected because those three eras are identical in every trigger
/// that matters here, and three copies of one list is the shape [D-124] names.
///
/// [D-124]: ../../docs/architecture/s13-decision-register.md
fn triggers_before_v12() -> impl Iterator<Item = &'static str> {
    CREATE_TRIGGERS
        .iter()
        .copied()
        .filter(|t| {
            !V12_TRIGGERS
                .iter()
                .chain(LATER_TRIGGERS.iter())
                .any(|name| t.contains(name))
        })
        .chain(TRIGGERS_V11.iter().copied())
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
    for trigger_ddl in triggers_before_v12() {
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
/// **No index.** This rung used to re-issue the whole of [`CREATE_INDICES`],
/// which it needed for exactly one entry — `idx_annotations_label`, the index
/// on the table it creates. [D-089](../../docs/architecture/s13-decision-register.md#d-089)
/// found nothing seeks on that index and the v7 → v8 rung dropped it, which
/// left this loop re-issuing six indices belonging to other rungs and owning
/// none of them. See [`create_indices`] for why that shape had to go.
async fn add_analytics_annotations(conn: &libsql::Connection) -> Result<()> {
    conn.execute(CREATE_ANALYTICS_ANNOTATIONS_TABLE, ()).await?;
    Ok(())
}

/// v10 → v11: index the archive cutoff and the reverse-reachability arm
/// (0.12.6, W3.1/W3.2, D-151).
///
/// # The first rung to index a frozen table, which is the case D-036 named
///
/// Every index rung before this one landed on `links_current`, a derivative
/// table [D-036](../../docs/architecture/s13-decision-register.md#d-036) gives
/// no stability guarantee at all. These two land on **`links`**, which is a
/// normative ledger table and frozen. That is not an exception being taken:
/// D-036's freeze restricts post-1.0 change on the core to *additive*
/// operations and names `ADD COLUMN` and **new indexes** as the two that
/// qualify. An index adds no column, moves no row, and changes no bitemporal
/// semantics — `CREATE INDEX` reads the table and writes a b-tree beside it. A
/// v10 database and a v11 database hold identical `links` rows.
///
/// So this rung is doing the thing the freeze was drafted to permit, and it is
/// worth saying once, here, because the *next* one to touch `links` may not be.
///
/// # Cost
///
/// Two b-trees built from an existing table, so proportional to the row count
/// and nothing else, with nothing to backfill. The standing cost is two extra
/// index writes per ledger insert, forever, which is what
/// [`ddl::CREATE_INDICES`](crate::schema::ddl::CREATE_INDICES) records the
/// measured before/after plans for and what
/// `tests/index_plan_tests.rs` holds registry entries against.
/// v12 → v13: the `branches` delete guard becomes marker-gated (0.14.13,
/// §15.4, [D-230](../../docs/architecture/s13-decision-register.md#d-230)).
///
/// The cheapest kind of rung and the most necessary: `CREATE TRIGGER IF NOT
/// EXISTS` on an existing name keeps the **old body**, so nothing short of an
/// explicit drop replaces the unconditional v12 guard. Without this rung every
/// database not created by this build would refuse
/// [`crate::Database::archive_branch`] with a trigger abort, and `verify` —
/// which now carries `trg_branches_frozen_delete` in [`DELETE_GUARDS`] — is
/// what turns that into a sentence at open time instead.
///
/// See [`CREATE_BRANCHES_GUARD_DELETE`] for why the guard changed at all. Only
/// the delete half moves; `trg_branches_frozen_update` is left exactly as it
/// was, because archival is a move and not an edit.
async fn gate_branches_delete_guard(conn: &libsql::Connection) -> Result<()> {
    conn.execute("DROP TRIGGER IF EXISTS trg_branches_frozen_delete", ())
        .await?;
    conn.execute(CREATE_BRANCHES_GUARD_DELETE, ()).await?;
    Ok(())
}

async fn add_links_archive_indices(conn: &libsql::Connection) -> Result<()> {
    create_indices(conn, &["idx_links_recorded_at", "idx_links_target"]).await
}

/// v13 → v14: the lineage read gets an index to seek on (0.14.14, §15.4,
/// [D-231](../../docs/architecture/s13-decision-register.md#d-231)).
///
/// Index-only and on a derivative table, so [D-036] permits it on the same two
/// grounds every index rung before it stood on. Nothing is dropped:
/// [`LC_LINEAGE_CUT`] leads on `branch_id` and the two indices already here
/// lead on `source_id`, so no pair subsumes another.
///
/// **This is not the rung §15.4 owes, and the difference is the release.** The
/// plan asked for `idx_lc_traversal_cover` to *gain* `branch_id`, and
/// [D-219](../../docs/architecture/s13-decision-register.md#d-219) measured
/// three placements of it. Both were reasoning about a reader that resolved
/// with `branch_id IN (ancestry)` — the form the same probe run then showed is
/// not a resolution at all, and which 0.14.4 consequently did not ship. Under
/// the reader that did ship, that index is not on the branched path and the
/// folded shape buys nothing measurable; and any shape leading on `branch_id`
/// evicts the *trunk* walk from its covering index. See [`CREATE_INDICES`] for
/// the numbers and the plans.
///
/// Nothing is backfilled, because an index has nothing to backfill — `CREATE
/// INDEX` populates it from the table — so the cost is a function of existing
/// row count alone.
///
/// [D-036]: ../../docs/architecture/s13-decision-register.md#d-036
async fn add_lineage_cut_index(conn: &libsql::Connection) -> Result<()> {
    create_indices(conn, &["idx_lc_lineage_cut"]).await
}

/// The v15 shape of `links`, pinned as text (0.14.15, [D-232]).
///
/// Pinned for the reason [`LINKS_V7`] states in full, and this is the second
/// rung to need it. Note what the pinning buys *here specifically*: the two
/// rungs that rebuild this table now sit on the same ladder, and they must
/// produce different shapes — `LINKS_V7` has no `branch_id` at all, because at
/// v7 there was none. A rung reading `ddl::CREATE_LINKS_TABLE` would make both
/// of them produce today's, and a v6 database would arrive at v7 already
/// carrying a v15 key.
///
/// The `REFERENCES concepts(id)` clauses are spelled out rather than dropped
/// and re-added: `links` is being rebuilt, not altered, so the new table
/// declares them from the start and the copy satisfies them row for row.
const LINKS_V15: &str = r#"
CREATE TABLE links_v15 (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    branch_id   TEXT NOT NULL DEFAULT 'main' REFERENCES branches(branch_id),
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at, branch_id),
    CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real'),
    -- (the timestamp CHECK, spelled out for the same pinning reason)
    CHECK (valid_from GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND valid_to GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z' AND 1)
)
"#;

/// The four triggers a v14 `links` carries, by name, dropped with the table.
///
/// Enumerated for [`CONCEPTS_TRIGGERS_V7`]'s reason: a rung is a statement
/// about a fixed past, so a v16 trigger added to this table later cannot be
/// swept into a rung that predates it. They are not `DROP`ped explicitly —
/// `DROP TABLE` takes them — but the rung has to put exactly these back, and
/// the list is what says which.
const LINKS_TRIGGERS_V15: &[&str] = &[
    "trg_links_current_sync",
    "trg_links_single_open",
    "trg_links_log_insert",
    "trg_links_guard_delete",
];

/// v14 → v15: `links` is keyed by lineage (0.14.15, §15.4, [D-232]).
///
/// # What was actually broken
///
/// Two lineages asserting one edge key at one `recorded_at` collided on
/// `PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)`.
/// §15.4 called this "unreachable through the crate until branch-scoped writes
/// exist, which is 0.14.5", and left it to a later rung "to widen or to decline
/// in writing". **It became reachable at 0.14.8 and nothing noticed**, because
/// the reasoning that made it look unreachable is about the clock — successive
/// calls return strictly increasing values, so two sequential assertions cannot
/// share a stamp — and the batch paths do not make successive calls. They take
/// **one stamp for the whole batch** ([D-014]), deliberately, because the rows
/// were asserted by one act.
///
/// `reject_overlaps_within` then groups candidates by `(source, target,
/// edge_type, branch_id)`, so a trunk row and a branch row about one edge are
/// in different groups, are not an overlap, and are handed to the insert as a
/// legal pair. `examples/links_key_reach_probe.rs` reproduces it on both batch
/// surfaces and shows the caller receiving raw engine text.
///
/// Widening rather than refusing, and the choice is not close: the two
/// assertions are *legitimate*. Two lineages are allowed to believe different
/// things about one edge — that is what a lineage is — and rejecting the pair
/// would let a storage key decide what a caller may assert in one transaction.
///
/// # Why this is its own release
///
/// §15.4 assigned it to the same rung as an index. It is not the same size: an
/// index is one `CREATE INDEX` on a derivative table, and this is a rebuild of
/// the ledger's largest table — the operation [`LINKS_V7`] exists because of
/// and [D-119] had to suspend foreign keys for. Bundling the two would have
/// made one revert undo both.
///
/// **Measured**, since the v6 → v7 rung's cost estimate is on record as
/// unmeasured: create, copy, drop, rename runs in **2.7 ms at 1,000 rows,
/// 14.4 ms at 10,000 and 122.9 ms at 50,000** — linear, and cheap because no
/// trigger fires. The insert targets `links_v15`, and every trigger on this
/// table names `links`.
///
/// # Order, and the two things that get taken with the table
///
/// `DROP TABLE links` before the rename, for [`add_weight_check`]'s reason: the
/// drop takes the four triggers with it, so the rename does not reparse a
/// schema whose trigger bodies name a table that no longer exists.
///
/// It takes **the two indices** as well, which the v6 → v7 rung did not have to
/// think about — its docstring says in as many words that "no index is defined
/// on `links`", and that stopped being true at v11. They are put back by name
/// through [`create_indices`], which is also what makes this rung's failure
/// mode a panic naming the index rather than a database stamped v15 that fails
/// its own open-time verification.
///
/// [D-014]: ../../docs/architecture/s13-decision-register.md#d-014
/// [D-119]: ../../docs/architecture/s13-decision-register.md#d-119
/// [D-232]: ../../docs/architecture/s13-decision-register.md#d-232
async fn add_links_lineage_key(conn: &libsql::Connection) -> Result<()> {
    conn.execute(LINKS_V15, ()).await?;
    conn.execute(
        "INSERT INTO links_v15 (source_id, target_id, edge_type, valid_from, \
         recorded_at, valid_to, weight, properties, branch_id) \
         SELECT source_id, target_id, edge_type, valid_from, recorded_at, \
                valid_to, weight, properties, branch_id FROM links",
        (),
    )
    .await?;
    conn.execute("DROP TABLE links", ()).await?;
    conn.execute("ALTER TABLE links_v15 RENAME TO links", ())
        .await?;

    // The triggers the drop took. By const and not by copy — the bodies do not
    // change at v15, and `CREATE_LINKS_CURRENT_SYNC` states the rule: a rung
    // with its own copy of a trigger is a copy that drifts. `LINKS_TRIGGERS_V15`
    // is what pins *which four*, which is the half a later rung will need.
    for ddl in [
        CREATE_LINKS_CURRENT_SYNC,
        CREATE_LINKS_SINGLE_OPEN,
        CREATE_LINKS_LOG_INSERT,
        CREATE_LINKS_GUARD_DELETE,
    ] {
        conn.execute(ddl, ()).await?;
    }
    debug_assert_eq!(LINKS_TRIGGERS_V15.len(), 4);

    // And the two indices, which `DROP TABLE` took with equally little noise.
    create_indices(conn, &["idx_links_recorded_at", "idx_links_target"]).await
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
    create_indices(conn, &["idx_lc_open_interval"]).await
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

    for trigger_ddl in triggers_before_v12() {
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
    conn.execute("DROP TABLE IF EXISTS concepts_fts", ())
        .await?;

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
    for trigger_ddl in triggers_before_v12() {
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
    conn.execute("DROP TRIGGER IF EXISTS trg_concepts_log_insert", ())
        .await?;
    conn.execute(CONCEPTS_LOG_INSERT_V9, ()).await?;

    conn.execute(REBUILD_CONCEPTS_FTS, ()).await?;

    Ok(())
}

/// `trg_concepts_log_insert` **as v9 had it**: unconditional (0.9.0, C3).
///
/// Pinned for the same reason as [`CONCEPTS_GUARD_DELETE_V8`], and the reason
/// bites harder here: [`add_concepts_rowid_pk`] restores triggers from
/// [`CREATE_TRIGGERS`], so without this the v7 → v8 rung would install the v10
/// body — a database the ladder calls v8 whose concept inserts stop logging
/// inside a session, three versions before that behaviour was decided.
const CONCEPTS_LOG_INSERT_V9: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_insert
    AFTER INSERT ON concepts
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
        VALUES ('concepts', NEW.id, 'I',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at);
    END;
"#;

/// v9 → v10: the concepts insert log trigger becomes marker-gated (C3).
///
/// Two statements, like the rung below, and necessary for a reason that is not
/// tidiness. See [`CREATE_CONCEPTS_LOG_INSERT`]: an unlogged insert is what makes
/// rehydration a *move* rather than a write, and without it a rehydrated concept
/// outranks its own retirement in the fold and comes back alive.
async fn gate_concepts_log_insert_on_marker(conn: &libsql::Connection) -> Result<()> {
    conn.execute("DROP TRIGGER IF EXISTS trg_concepts_log_insert", ())
        .await?;
    conn.execute(CREATE_CONCEPTS_LOG_INSERT, ()).await?;
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

/// The entries of [`CREATE_INDICES`] a rung names, created in declaration
/// order (0.14.14, D-231).
///
/// # Why a rung names its indices instead of running the list
///
/// Every index rung before v14 ran the whole of [`CREATE_INDICES`], which was
/// correct for eleven versions and stopped being correct silently. A rung is a
/// statement about the schema at *its* version, and that loop makes it a
/// statement about the schema at **today's** — so the moment v14 declared an
/// index over `links_current.branch_id`, the v3 → v4, v5 → v6 and v10 → v11
/// rungs all began failing with `no such column: branch_id` on databases that
/// legitimately had no such column yet. Ten migration tests, one cause.
///
/// The blanket loop was never load-bearing: each of those rungs owes exactly
/// the indices its own decision record names, and the extras it re-issued were
/// already there under `IF NOT EXISTS`. So this takes the DDL from the one
/// place that declares it and lets the rung say which of it applies, which is
/// what [`ddl::LC_TRAVERSAL_COVER`](crate::schema::ddl::CREATE_INDICES)'s own
/// note already argued for: *"a rung should state which indices it owes rather
/// than derive the list from a definition that will keep changing after it."*
///
/// A name that matches no declaration is a panic rather than a silent no-op,
/// because the failure it prevents — a rung that creates nothing and stamps a
/// version anyway — is exactly the one `verify` had to be written to catch.
async fn create_indices(conn: &libsql::Connection, names: &[&str]) -> Result<()> {
    for name in names {
        let ddl = CREATE_INDICES
            .iter()
            .find(|sql| sql.contains(name))
            .unwrap_or_else(|| panic!("{name} is not declared in ddl::CREATE_INDICES"));
        conn.execute(ddl, ()).await?;
    }
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
    create_indices(conn, &["idx_lc_traversal_cover"]).await?;
    conn.execute("DROP INDEX IF EXISTS idx_lc_src_active", ())
        .await?;
    Ok(())
}

/// The tables the baseline declares, by name, for [`verify`].
pub(crate) const BASELINE_TABLES: &[&str] = &[
    "branches",
    "concepts",
    "links",
    "links_current",
    "transaction_log",
    "analytics_annotations",
    "concepts_fts",
    // v16 (W14.5, D-249).
    "log_integrity",
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
    let mut links_sql = String::new();
    while let Some(row) = rows.next().await? {
        let (kind, name, sql): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        if kind == "table" && name.eq_ignore_ascii_case("links") {
            links_sql = sql.clone();
        }
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

    // `links` is checked by **key**, not only by name (0.14.15, D-232), and the
    // reason is the one D-126 gives below for the delete guards: presence was
    // never the property that mattered.
    //
    // A table's primary key has no name for the loop above to look for. So a
    // database stamped v15 whose `links` still carries the v14 key — a stamp
    // written by hand, a restore from a file that never climbed, a rung that
    // silently no-opped — opens cleanly, reads correctly, and then refuses one
    // legal batch write in a hundred with raw engine text. That is precisely
    // the shape v15 exists to remove, and every other v15 object is present, so
    // nothing else here would notice.
    //
    // The probe is the column name inside the `PRIMARY KEY` clause and not the
    // table's whole text, for D-126's reason: a full-text comparison fails on
    // whitespace and has to be re-pinned every time a comment moves, which
    // makes it the kind of check people disable.
    let keyed_by_lineage = links_sql
        .split_once("PRIMARY KEY")
        .and_then(|(_, rest)| rest.split_once(')'))
        .is_some_and(|(key, _)| key.contains("branch_id"));
    if !links_sql.is_empty() && !keyed_by_lineage {
        return Err(DbError::Migration {
            to: SCHEMA_VERSION,
            reason: format!(
                "schema verification failed: the database is stamped \
                 v{SCHEMA_VERSION} but `links` is not keyed by lineage. Its \
                 primary key must end in `branch_id` (v15); without it a batch \
                 asserting one edge key on two lineages collides, because the \
                 batch paths share one `recorded_at` by contract. Upgrading \
                 through the ladder rebuilds the table."
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

    // The guards above are checked for being *gated*. This checks that the gate
    // is not currently open (0.10.0, W2).
    //
    // A committed `macrame_archive_session` disarms all three delete guards and
    // silences the concepts log-insert trigger — Doctrine IV and Doctrine V
    // suspended at once, with no error and no counter. Nothing checked for it,
    // and the safety argument on record (§5.7) is about *crashes*: it is correct
    // about those, because both archive paths bracket the marker inside the
    // session transaction, so a rollback discards it. It says nothing about a
    // writer that creates the table directly, and §4.7 concedes raw writers.
    //
    // Free to check here: `present` is already built, so this is one more scan
    // of a vector, not another query. And it is safe *here specifically* —
    // `verify` reads committed state at open, so it cannot observe an in-flight
    // session and refuse a healthy database mid-archive. Moving it onto a path
    // that runs during a session would break that.
    if has("table", ARCHIVE_SESSION_MARKER) {
        return Err(DbError::ArchiveSessionLeaked {
            marker: ARCHIVE_SESSION_MARKER.to_string(),
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
    // v13 (0.14.13, §15.4, D-230). The list was three names and one sentence —
    // *branches are never archived* — for eight releases; `archive_branch` is
    // what made the fourth name belong here, and carrying it is what makes a
    // v12 database's stale unconditional guard a refusal at open rather than a
    // trigger abort in the middle of the first abandonment.
    "trg_branches_frozen_delete",
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
    ///
    /// **Starting at v2 and not at v0, which is the whole test** (0.14.14,
    /// [D-231](../../docs/architecture/s13-decision-register.md#d-231)). The
    /// baseline step is `from: 0, to: SCHEMA_VERSION`, so a walk beginning at
    /// v0 takes it, lands on the top, and reports success — *on any `STEPS`
    /// array whatsoever*, including one with every incremental rung deleted.
    /// The previous version of this test did exactly that: removing the v13 →
    /// v14 rung left it green while ten integration tests went red. It was
    /// checking that the baseline is the baseline.
    ///
    /// The walk that means something starts at the lowest version a *stored*
    /// database can hold. That is v2: v1 is pre-canonical and refused
    /// deliberately, which [`legacy_v1_has_no_rung`]
    /// pins from the other side.
    #[test]
    fn the_ladder_reaches_the_current_version() {
        let mut current = 2;
        for _ in 0..STEPS.len() {
            match STEPS.iter().find(|s| s.from == current) {
                Some(step) => current = step.to,
                None => break,
            }
        }
        assert_eq!(
            current, SCHEMA_VERSION,
            "the incremental ladder stops at v{current} and this build stamps \
             v{SCHEMA_VERSION}: a database stored at v{current} has no rung out \
             of it and cannot be opened"
        );
    }

    /// Every version a stored database can hold has a rung out of it.
    ///
    /// The chain walk above finds the *first* break and stops. This says the
    /// same thing per version, so the failure names which rung is missing
    /// rather than which version the walk happened to stall on — and it also
    /// refuses a gap the walk would jump over, because a step is free to skip
    /// versions and none of them does.
    #[test]
    fn every_stored_version_has_a_rung_out_of_it() {
        for v in 2..SCHEMA_VERSION {
            assert!(
                STEPS.iter().any(|s| s.from == v),
                "no rung leads out of v{v}, so a database stored at v{v} cannot \
                 be opened by this build"
            );
        }
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
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
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
                    tx.execute(
                        "INSERT INTO parent_new (id) SELECT id FROM parent ORDER BY rowid",
                        (),
                    )
                    .await?;
                    tx.execute("DROP TABLE parent", ()).await?;
                    tx.execute("ALTER TABLE parent_new RENAME TO parent", ())
                        .await?;
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
        assert!(
            text.contains("test-orphan"),
            "the error should name the rung: {text}"
        );

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
