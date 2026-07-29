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
pub const SCHEMA_VERSION: u32 = 4;

type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// One rung of the ladder: takes a database at `from` and leaves it at `to`.
struct Step {
    from: u32,
    to: u32,
    name: &'static str,
    apply: for<'a> fn(&'a libsql::Connection) -> StepFuture<'a>,
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
        apply: |conn| Box::pin(baseline(conn)),
    },
    Step {
        from: 2,
        to: 3,
        name: "analytics-annotations",
        apply: |conn| Box::pin(add_analytics_annotations(conn)),
    },
    Step {
        from: 3,
        to: 4,
        name: "traversal-covering-index",
        apply: |conn| Box::pin(add_traversal_cover(conn)),
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
pub async fn run(conn: &libsql::Connection) -> Result<()> {
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

    verify(conn).await
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
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    let res: Result<()> = async {
        (step.apply)(&tx).await?;
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

    for index_ddl in CREATE_INDICES {
        conn.execute(index_ddl, ()).await?;
    }

    for trigger_ddl in CREATE_TRIGGERS {
        conn.execute(trigger_ddl, ()).await?;
    }

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
            "SELECT type, name FROM sqlite_master WHERE type IN ('table','trigger','index')",
            (),
        )
        .await?;

    let mut present: Vec<(String, String)> = Vec::new();
    while let Some(row) = rows.next().await? {
        present.push((row.get(0)?, row.get(1)?));
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
    Ok(())
}

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
        assert!(triggers.iter().all(|n| n.starts_with("trg_")), "{triggers:?}");

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
}
