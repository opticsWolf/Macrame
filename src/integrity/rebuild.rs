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
    /// [`latest_belief_projection`](super::latest_belief_projection); the insert
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

/// The five columns `links_current` is keyed by, in the order the primary key
/// declares them.
///
/// Spelled once because the keyed repair names them four times — the temp
/// table, the `DELETE`'s tuple, its subquery, and the re-projection's — and a
/// list that has to agree with itself four times is a list that will not.
pub(crate) const PROJECTION_KEY: &str = "source_id, target_id, edge_type, valid_from, branch_id";

/// Re-derive `links_current` **at named keys only** (0.15.3, [D-245]).
///
/// `keys` is a table holding [`PROJECTION_KEY`] — the archive collects it from
/// `links` before its `DELETE`, inside the same transaction, so it names every
/// key the session is about to disturb and nothing else.
///
/// # Why this is exact, and not an approximation of the rebuild
///
/// `links_current` is a function of `links`, one row per key (Doctrine VI), and
/// the function is *pointwise*: the row at a key depends on the `links` rows at
/// that key and on nothing else. So a change confined to a set of keys can only
/// change the projection at those keys, and re-deriving there is not a cheaper
/// estimate of the full rebuild — it is the same answer with the untouched
/// partitions left alone. Both halves matter and both are derived from the
/// definition rather than described: the `DELETE` removes what was there, the
/// `INSERT` puts back whatever the surviving rows project to, which is **no
/// row** when the session archived the last belief at that key. That case is
/// the one a hand-written compensation gets wrong, and it is why the repair is
/// two statements against the projection instead of one `DELETE` with a
/// predicate.
///
/// The full [`rebuild_within`] stays for `rebuild_current`, where the caller is
/// asking for exactly that and has no key set to offer.
///
/// [D-245]: ../../docs/architecture/s13-decision-register.md#d-245
pub(crate) async fn repair_keys_within(conn: &libsql::Connection, keys: &str) -> Result<usize> {
    conn.execute(
        &format!("DELETE FROM links_current WHERE ({PROJECTION_KEY}) IN (SELECT {PROJECTION_KEY} FROM {keys})"),
        (),
    )
    .await?;

    let rows = conn
        .execute(
            &format!(
                "INSERT INTO links_current \
                 (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, \
                  recorded_at, branch_id) \
                 {projection}",
                projection = super::projection_where(&format!(
                    "({PROJECTION_KEY}) IN (SELECT {PROJECTION_KEY} FROM {keys})"
                ))
            ),
            (),
        )
        .await?;

    Ok(rows as usize)
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
        projection = super::latest_belief_projection()
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
