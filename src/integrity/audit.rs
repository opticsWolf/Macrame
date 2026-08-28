use crate::error::{DbError, Result};

/// Audit current belief in links_current against latest-belief projection of links (§5.8).
/// Returns Ok(0) if zero drift, or Err(DbError::CurrentDrift { n }) if drift detected.
///
/// Drift is the *symmetric* difference: rows `links_current` has that the
/// projection does not (stale or spurious materialisation) plus rows the
/// projection has that `links_current` does not (missed materialisation).
/// Both directions must be counted, or the audit certifies a corrupt table.
///
/// The two directions are computed as two independent scalar subqueries and
/// added, rather than as one compound `SELECT`. SQLite gives `EXCEPT` and
/// `UNION` equal precedence and evaluates them left-associatively, so writing
/// the four arms as a flat chain parses as `(((A EXCEPT B) UNION B) EXCEPT A)`,
/// which reduces to `A EXCEPT A` — a constant zero that reports "no drift" no
/// matter how corrupt the table is. That was the pre-0.5.4 defect.
///
/// Parentheses cannot fix it: SQLite rejects a parenthesised compound-`SELECT`
/// operand outright (`near "UNION": syntax error`). Putting each `EXCEPT` alone
/// inside its own scalar subquery makes the grouping structural instead of
/// syntactic, so there is no precedence for a future edit to get wrong.
pub async fn audit_current(conn: &libsql::Connection) -> Result<usize> {
    // `projection` is the latest-belief view of `links`: one row per interval
    // key, the one with the greatest recorded_at. `materialized` is what
    // links_current actually holds. Doctrine VI says they must be equal.
    let query = format!(
        r#"
        WITH materialized AS (
            SELECT source_id, target_id, edge_type, valid_from,
                   valid_to, weight, properties, recorded_at, branch_id
            FROM links_current
        ),
        projection AS ({projection})
        SELECT
            (SELECT COUNT(*) FROM (
                SELECT * FROM materialized EXCEPT SELECT * FROM projection))
          + (SELECT COUNT(*) FROM (
                SELECT * FROM projection EXCEPT SELECT * FROM materialized))
    "#,
        projection = super::LATEST_BELIEF_PROJECTION
    );
    let mut rows = conn.query(&query, ()).await?;
    let count: i64 = if let Some(row) = rows.next().await? {
        row.get(0).unwrap_or(0)
    } else {
        0
    };

    if count > 0 {
        return Err(DbError::CurrentDrift { n: count as usize });
    }
    Ok(0)
}
