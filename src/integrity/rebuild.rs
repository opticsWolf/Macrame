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
    match rebuild_within(&tx).await {
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

/// The rebuild itself, without a transaction of its own.
///
/// Exists so a caller that already holds one can reuse it — `archive()` does,
/// to re-derive `links_current` after moving rows out of `links`. Opening a
/// nested transaction there would simply fail, and doing the work outside the
/// archive transaction would leave a window where the materialization does not
/// match the ledger.
pub(crate) async fn rebuild_within(conn: &libsql::Connection) -> Result<RebuildReport> {
    conn.execute("DELETE FROM links_current", ()).await?;

    let insert_query = r#"
        INSERT INTO links_current (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at)
        SELECT source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at
        FROM (
            SELECT source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at,
                   ROW_NUMBER() OVER (
                       PARTITION BY source_id, target_id, edge_type, valid_from
                       ORDER BY recorded_at DESC
                   ) as rn
            FROM links
        ) WHERE rn = 1
    "#;
    let rows_inserted = conn.execute(insert_query, ()).await?;

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
