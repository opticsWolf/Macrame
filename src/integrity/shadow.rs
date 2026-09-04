//! Rebuilding `links_current` beside itself, in chunks (T1.2, D-082).
//!
//! # What this is for
//!
//! `rebuild_current` is one `BEGIN IMMEDIATE … COMMIT` holding the write lock
//! for its whole duration — measured at 318 ms for 40,000 rows in `links`
//! (D-077), and D-023 is why it cannot simply be split: the window between the
//! `DELETE` and the `INSERT` is the entire of current belief, and a reader
//! landing in it sees a graph with no edges and no error.
//!
//! Building the replacement *beside* the live table removes that window. The
//! live table stays live and trigger-maintained throughout, so readers and
//! `trg_links_single_open` keep working, and the only moment anything is
//! unavailable is the swap.
//!
//! # Two things about this are easy to get wrong, and one of them is silent
//!
//! **`CREATE TABLE … AS SELECT` does not carry the schema.** The obvious way to
//! make a shadow copies the rows and *nothing else*: no primary key, no `CHECK`
//! constraints, no indexes. The swap then succeeds, the rename succeeds, and the
//! next `INSERT INTO links` fails inside `trg_links_current_sync` with `ON
//! CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint` —
//! because the conflict target no longer exists. Probed on libSQL 0.9.30; the
//! projection had stopped being maintained and the only symptom was an error on
//! an unrelated write. The shadow is therefore created from
//! [`CREATE_LINKS_CURRENT_TABLE`](crate::schema::ddl::CREATE_LINKS_CURRENT_TABLE)
//! with the name substituted, so it cannot drift from the declared table.
//!
//! **The rename reparses the whole schema.** `ALTER TABLE … RENAME` (SQLite
//! ≥ 3.25) re-resolves every trigger body, and both `links` triggers name
//! `links_current` — so the rename fails with `error in trigger
//! trg_links_current_sync: no such table: main.links_current` while they exist.
//! Probed. The order that works, also probed, is `DROP TRIGGER` → `DROP TABLE` →
//! `RENAME` → `CREATE INDEX` → recreate triggers. `PRAGMA legacy_alter_table=ON`
//! also works and is **not** used: it disables the reference fixups the modern
//! rename exists to perform.
//!
//! # Why the indexes are built inside the swap and not on the shadow
//!
//! This is the one place the shape is dictated by SQLite rather than chosen.
//! Index names are global, so the shadow cannot carry `idx_lc_traversal_cover`
//! while the live table still holds that name — and SQLite has no `ALTER INDEX
//! … RENAME`. Building them on the shadow under temporary names would leave
//! `links_current` permanently indexed under names that do not appear in
//! [`CREATE_INDICES`](crate::schema::ddl::CREATE_INDICES), so the next migration
//! would create a **second** copy of each.
//!
//! `DROP TABLE links_current` frees the names, and they are reusable within the
//! same transaction (probed). So the swap pays the index builds, and what the
//! chunking buys is that the *projection* — the window function over all of
//! `links`, which is the O(E log E) term — happens outside the lock. That is a
//! smaller win than "the swap is microseconds", which is what the naive reading
//! of the shadow idea promises, and it is the real one.

use super::projection_where;
use crate::error::{DbError, Result};
use crate::schema::ddl;

/// The table the replacement is built in.
///
/// One fixed name rather than a unique one per attempt: a crashed rebuild must
/// leave something a later attempt can recognise and drop, not an accumulating
/// set of orphans that nothing knows the names of.
pub(crate) const SHADOW_TABLE: &str = "links_current_shadow";

/// Distinct `source_id`s projected per chunk.
///
/// Sized against [`CHUNK_BUDGET`](crate::CHUNK_BUDGET) rather than derived from
/// it, and the unit is sources rather than rows for a reason that is also a
/// limitation: the chunk boundary has to be a range the window function can be
/// restricted to, and `PARTITION BY (source_id, target_id, edge_type,
/// valid_from)` means a partition never spans a `source_id`. So a source is the
/// smallest safe unit — and a single hub node with a very large out-degree is
/// one chunk however long it takes. That case is bounded by the graph, not by
/// this constant, and no chunking of this shape can fix it.
pub(crate) const SOURCES_PER_CHUNK: usize = 256;

/// One step of a chunked rebuild, as sent to the actor.
///
/// Three commands rather than one because each must be its own **turn** — the
/// whole point is that the actor returns to its `select!` between chunks, so a
/// high-priority assertion can jump ahead. A loop inside one command would
/// produce the same small transactions inside one hold and buy nothing, which is
/// the same trap [`Database::archive_windowed`](crate::Database::archive_windowed)
/// avoids.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShadowStep {
    /// Drop any orphan shadow, create a fresh one from the declared DDL.
    Begin,
    /// Project the next `SOURCES_PER_CHUNK` sources into the shadow.
    Fill { after: Option<String> },
    /// Catch up on writes since `build_start`, then swap. One transaction.
    ///
    /// `epoch` is the archive count [`ShadowOutcome::Started`] reported. It
    /// travels out to the caller and back rather than being remembered by the
    /// actor: the actor is stateless per command by construction, and a single
    /// remembered slot would be shared — and silently corrupted — by two
    /// rebuilds running at once.
    Swap { build_start: String, epoch: u64 },
}

/// What a [`ShadowStep`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShadowOutcome {
    /// `build_start`, and the actor's archive epoch as of the start.
    Started { build_start: String, epoch: u64 },
    /// The last `source_id` projected, or `None` when the table is exhausted.
    Filled { last: Option<String> },
    /// Rows in the new `links_current`.
    Swapped { rows: usize },
}

const SHADOW_COLUMNS: &str = "(source_id, target_id, edge_type, valid_from, \
                              valid_to, weight, properties, recorded_at, branch_id)";

/// Create the shadow, and report the transaction time the build starts from.
///
/// `build_start` is `MAX(recorded_at)` **before** any chunk runs, so every write
/// that lands during the build is at or after it and the catch-up pass can find
/// them all by that one column. Taking it after the first chunk would leave a
/// gap no later pass could name.
pub(crate) async fn begin(conn: &libsql::Connection) -> Result<String> {
    // An orphan from a crashed attempt is dropped rather than reused: its
    // contents are a projection of a `links` that has since moved on, and there
    // is no way to tell how far.
    conn.execute(&format!("DROP TABLE IF EXISTS {SHADOW_TABLE}"), ())
        .await?;
    conn.execute(&shadow_ddl(), ()).await?;

    let build_start: Option<String> = conn
        .query("SELECT MAX(recorded_at) FROM links", ())
        .await?
        .next()
        .await?
        .and_then(|row| row.get(0).ok());

    // An empty `links` still needs a stamp the catch-up can compare against.
    // The epoch sentinel is below every canonical timestamp, so the catch-up
    // sees every row — which on an empty table is none, and on a table written
    // to during the build is all of them. Correct in both directions.
    Ok(build_start.unwrap_or_else(|| "0001-01-01T00:00:00.000000Z".to_string()))
}

/// [`CREATE_LINKS_CURRENT_TABLE`](ddl::CREATE_LINKS_CURRENT_TABLE) with the
/// table name substituted — primary key, `CHECK`s and all.
///
/// Substituted rather than written out, so the shadow cannot drift from the
/// declared table. See the module header for what happens when it does.
fn shadow_ddl() -> String {
    ddl::CREATE_LINKS_CURRENT_TABLE.replacen("links_current", SHADOW_TABLE, 1)
}

/// Project one chunk of sources into the shadow.
///
/// Returns the last `source_id` written, or `None` when there is nothing left.
pub(crate) async fn fill_chunk(
    conn: &libsql::Connection,
    after: Option<&str>,
) -> Result<Option<String>> {
    // Keyset pagination over the *distinct* sources, so the boundary is a real
    // source and a chunk never splits one. `links`'s primary key leads on
    // `source_id`, so this is an index range scan of at most SOURCES_PER_CHUNK
    // distinct values rather than a pass over the table.
    let low = after.unwrap_or("");
    let high: Option<String> = conn
        .query(
            &format!(
                "SELECT MAX(source_id) FROM ( \
                     SELECT DISTINCT source_id FROM links \
                     WHERE source_id > ?1 ORDER BY source_id LIMIT {SOURCES_PER_CHUNK} \
                 )"
            ),
            libsql::params![low],
        )
        .await?
        .next()
        .await?
        .and_then(|row| row.get::<Option<String>>(0).ok())
        .flatten();

    let Some(high) = high else {
        return Ok(None);
    };

    conn.execute(
        &format!(
            "INSERT INTO {SHADOW_TABLE} {SHADOW_COLUMNS} {projection}",
            projection = projection_where("source_id > ?1 AND source_id <= ?2")
        ),
        libsql::params![low, high.as_str()],
    )
    .await?;

    Ok(Some(high))
}

/// Catch up, then swap. One transaction, and the only moment `links_current` is
/// not the live table.
///
/// `epoch` is the actor's archive count from [`begin`]. If an archive committed
/// during the build, the shadow is a projection of rows some of which no longer
/// exist — a *deletion* the catch-up cannot see, because the catch-up finds work
/// by `recorded_at` and a deleted row has no `recorded_at` to find. Rather than
/// making the swap verify itself (O(E log E) under the lock, which is the cost
/// this exists to remove), the rebuild is abandoned and the caller told to
/// retry. Archives are rare and a retry is cheap; a silently wrong `links_current`
/// is neither.
pub(crate) async fn swap(
    conn: &libsql::Connection,
    build_start: &str,
    epoch: u64,
    epoch_now: u64,
) -> Result<usize> {
    if epoch != epoch_now {
        conn.execute(&format!("DROP TABLE IF EXISTS {SHADOW_TABLE}"), ())
            .await?;
        return Err(DbError::RebuildInterrupted {
            reason: format!(
                "{} archive session(s) committed during the shadow build; their \
                 deletions are invisible to a catch-up keyed on recorded_at. \
                 Re-run rebuild_current_chunked.",
                epoch_now - epoch
            ),
        });
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // --- catch-up: only the keys written since the build began ---
    //
    // Bounded by writes during the rebuild, not by the size of `links`. The
    // `DELETE` and the re-`INSERT` are both restricted by the same subquery, so
    // a key written during the build is replaced rather than duplicated.
    //
    // `branch_id` is deliberately *not* in this key, and leaving it out is what
    // keeps the pass correct rather than what breaks it (v12, §15.2). The
    // `DELETE` clears every lineage's row for a touched edge and the `INSERT`
    // re-derives every lineage's winner for the same edges, so the pair stays
    // symmetric. Narrowing the delete by lineage without narrowing the
    // projection would insert a second lineage's row beside one never removed.
    let touched = "(source_id, target_id, edge_type, valid_from) IN ( \
                   SELECT source_id, target_id, edge_type, valid_from \
                   FROM links WHERE recorded_at >= ?1)";
    tx.execute(
        &format!("DELETE FROM {SHADOW_TABLE} WHERE {touched}"),
        libsql::params![build_start],
    )
    .await?;
    tx.execute(
        &format!(
            "INSERT INTO {SHADOW_TABLE} {SHADOW_COLUMNS} {projection}",
            projection = projection_where(
                "(source_id, target_id, edge_type, valid_from) IN ( \
                 SELECT source_id, target_id, edge_type, valid_from \
                 FROM links WHERE recorded_at >= ?1)"
            )
        ),
        libsql::params![build_start],
    )
    .await?;

    // --- the swap, in the one order that works ---
    for stmt in [
        "DROP TRIGGER IF EXISTS trg_links_current_sync",
        "DROP TRIGGER IF EXISTS trg_links_single_open",
        "DROP TABLE links_current",
    ] {
        tx.execute(stmt, ()).await?;
    }
    tx.execute(
        &format!("ALTER TABLE {SHADOW_TABLE} RENAME TO links_current"),
        (),
    )
    .await?;

    // The names are free now that the old table is gone, and reusable in this
    // same transaction (probed). Taken from the crate's own DDL so the rebuilt
    // indexes cannot differ from the declared ones.
    for stmt in ddl::CREATE_INDICES {
        if stmt.contains("links_current") {
            tx.execute(stmt, ()).await?;
        }
    }
    for trigger in ddl::CREATE_TRIGGERS {
        if trigger.contains("trg_links_current_sync") || trigger.contains("trg_links_single_open") {
            tx.execute(trigger, ()).await?;
        }
    }

    let rows: i64 = tx
        .query("SELECT COUNT(*) FROM links_current", ())
        .await?
        .next()
        .await?
        .and_then(|row| row.get(0).ok())
        .unwrap_or(0);

    tx.commit().await?;
    Ok(rows as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shadow's DDL must be the declared table's, with only the name changed.
    ///
    /// This is the silent failure from the module header, pinned at the cheapest
    /// possible level. If the substitution ever stops producing a primary key,
    /// the swap still succeeds and `trg_links_current_sync` breaks on the next
    /// write — a failure whose symptom appears on an unrelated operation.
    #[test]
    fn the_shadow_carries_the_declared_schema_not_just_the_columns() {
        let ddl = shadow_ddl();
        assert!(ddl.contains(SHADOW_TABLE), "{ddl}");
        // Not a pinned literal. The property that matters is that the
        // shadow's key and the sync trigger's `ON CONFLICT` target are the
        // *same* columns, so the check reads the target out of the trigger
        // and asks the shadow for it. Pinning the text instead meant that
        // widening the key at v12 produced a red test whose fix was to
        // retype the new spelling — which proves the two were edited
        // together once, and nothing about whether they still agree.
        let target = ddl::CREATE_LINKS_CURRENT_SYNC
            .split_once("ON CONFLICT(")
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(cols, _)| cols.to_string())
            .expect("the sync trigger declares an ON CONFLICT target");
        assert!(
            ddl.contains(&format!("PRIMARY KEY ({target})")),
            "the shadow's primary key is not the sync trigger's ON CONFLICT \
             target ({target}), so the trigger breaks on the first write \
             after the swap: {ddl}"
        );
        assert!(
            ddl.contains("CHECK"),
            "the shadow dropped the canonical-timestamp checks: {ddl}"
        );
        // Only the table name changed — `links_current` must not survive
        // anywhere in the shadow's own DDL.
        assert!(
            !ddl.replace(SHADOW_TABLE, "").contains("links_current"),
            "the substitution left a reference to the live table: {ddl}"
        );
    }

    /// The chunk restriction has to sit inside the window function's subquery.
    ///
    /// Outside it, the projection still ranks every partition in `links` and a
    /// chunk costs what the whole rebuild costs — the query would be correct and
    /// the chunking pointless, which is the kind of thing that only shows up in
    /// a benchmark nobody ran.
    #[test]
    fn the_chunk_restriction_is_inside_the_window() {
        let sql = projection_where("source_id > ?1 AND source_id <= ?2");
        let inner = sql.find("FROM links").unwrap();
        let outer = sql.rfind("WHERE rn = 1").unwrap();
        let clause = sql.find("source_id > ?1").unwrap();
        assert!(
            clause > inner && clause < outer,
            "the restriction landed outside the subquery:\n{sql}"
        );
    }
}
