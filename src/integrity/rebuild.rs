use crate::error::{DbError, Result};
use crate::integrity::audit::audit_current;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildReport {
    pub rows_rebuilt: usize,
    pub drift_after: usize,
}

/// Rebuild the materialized current-belief table from `links` (§5.8).
///
/// One `BEGIN IMMEDIATE … COMMIT`. The empty window between the `DELETE` and
/// the `INSERT` is the whole of current belief, so a failure across it — or a
/// concurrent reader landing in it — sees a graph with no edges and no error.
/// The transaction is what makes the repair a repair rather than a second way
/// to lose the table.
pub async fn rebuild_current(conn: &libsql::Connection) -> Result<RebuildReport> {
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    match rebuild_within(&tx, Verify::Yes).await {
        Ok(report) => {
            tx.commit().await?;
            Ok(report)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Whether a rebuild audits itself when it is finished (T0.2, D-077).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verify {
    /// Run `audit_current` afterwards and fail with `RebuildFailed` on drift.
    ///
    /// For the operator-facing repair. The post-check is what makes
    /// `RebuildReport::drift_after` and `DbError::RebuildFailed` mean anything,
    /// and a repair somebody invoked deliberately can afford to prove itself.
    Yes,
    /// Skip it.
    ///
    /// For `archive()`, which calls this **inside its own write transaction**.
    /// The audit compares `links_current` against
    /// [`LATEST_BELIEF_PROJECTION`](super::LATEST_BELIEF_PROJECTION); the insert
    /// above fills `links_current` *from* that same projection, in the same
    /// transaction, with nothing else able to write in between. So the check is
    /// tautological — it verifies that `INSERT … SELECT` inserted what it
    /// selected — and it is two `EXCEPT` passes over the whole table, O(E log E)
    /// each, under the archive's lock.
    ///
    /// This was only safe to say once the projection had **one** definition. It
    /// had two, byte-identical, in this file and `audit.rs`, and against two
    /// copies the post-rebuild audit was a real check: that they still agreed.
    No,
}

/// The rebuild itself, without a transaction of its own.
///
/// Exists so a caller that already holds one can reuse it — `archive()` does,
/// to re-derive `links_current` after moving rows out of `links`. Opening a
/// nested transaction there would simply fail, and doing the work outside the
/// archive transaction would leave a window where the materialization does not
/// match the ledger.
pub(crate) async fn rebuild_within(
    conn: &libsql::Connection,
    verify: Verify,
) -> Result<RebuildReport> {
    conn.execute("DELETE FROM links_current", ()).await?;

    let insert_query = format!(
        "INSERT INTO links_current \
         (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, \
          recorded_at, branch_id) \
         {projection}",
        projection = super::LATEST_BELIEF_PROJECTION
    );
    let rows_inserted = conn.execute(&insert_query, ()).await?;

    if verify == Verify::No {
        return Ok(RebuildReport {
            rows_rebuilt: rows_inserted as usize,
            drift_after: 0,
        });
    }

    match audit_current(conn).await {
        Ok(0) => Ok(RebuildReport {
            rows_rebuilt: rows_inserted as usize,
            drift_after: 0,
        }),
        Ok(n) => Err(DbError::RebuildFailed { n }),
        Err(DbError::CurrentDrift { n }) => Err(DbError::RebuildFailed { n }),
        Err(e) => Err(e),
    }
}
