//! The change feed: what the ledger recorded after a sequence number (0.20.0,
//! M7, [D-295]).
//!
//! `transaction_log.seq_id` is `INTEGER PRIMARY KEY AUTOINCREMENT`: strictly
//! increasing in commit order, never reused, and rolled back with a rolled-back
//! transaction (D-049). So a consumer that remembers the last `seq_id` it saw
//! can ask for everything after it, and a **gap** in the range can only mean
//! rows were archived — never that a writer is still in flight, because the
//! actor commits one transaction at a time.
//!
//! That is what makes a gap a refusal rather than a short answer: a consumer
//! told "here are the rows that survive" would silently miss the ones that
//! did not, which is the failure every reach check in this crate exists to
//! prevent. [`DbError::ChangeFeedGap`] tells it to resynchronise from current
//! state instead.
//!
//! Rows carry no payload. The feed says *what changed*; reading the entity
//! through the ordinary reads says what it is now, on whichever lineage the
//! consumer cares about.
//!
//! [D-295]: ../../docs/architecture/s13-decision-register.md#d-295

use crate::error::{DbError, Result};

/// One ledger write, as the change feed reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Change {
    /// The log's sequence number — the cursor a consumer keeps.
    pub seq_id: i64,
    /// The lineage the row was written on.
    pub branch_id: String,
    /// `"concepts"` or `"links"`.
    pub table: String,
    /// The concept id, or the edge key `source|target|type|valid_from`.
    pub entity_id: String,
    /// `"I"` for an insert, `"U"` for an update.
    pub operation: String,
    pub recorded_at: String,
}

/// The highest `seq_id` the log has ever allocated, or 0 for a log never
/// written. Read from `sqlite_sequence`, which an archive does not lower.
pub(crate) async fn head_seq(conn: &libsql::Connection) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'transaction_log'), 0)",
            (),
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

/// Log rows after `after_seq`, oldest first, at most `limit`, optionally only
/// those written on `branch`.
///
/// Refuses with [`DbError::ChangeFeedGap`] when any row in the range the
/// answer covers — `(after_seq, last returned]` when the limit cut it, else
/// `(after_seq, head]` — has been archived. The check counts every lineage's
/// rows, because the sequence is shared.
pub(crate) async fn changes(
    conn: &libsql::Connection,
    after_seq: i64,
    branch: Option<&str>,
    tables: &[&str],
    limit: usize,
) -> Result<Vec<Change>> {
    let tables_sql = if tables.is_empty() {
        String::new()
    } else {
        format!(
            " AND table_name IN ({})",
            (0..tables.len())
                .map(|i| format!("?{}", i + 4))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let sql = format!(
        "SELECT seq_id, branch_id, table_name, entity_id, operation, recorded_at \
           FROM transaction_log \
          WHERE seq_id > ?1 AND (?3 IS NULL OR branch_id = ?3){tables_sql} \
          ORDER BY seq_id LIMIT ?2"
    );
    let mut params: Vec<libsql::Value> = vec![
        after_seq.into(),
        (limit as i64).into(),
        match branch {
            Some(b) => libsql::Value::Text(b.to_string()),
            None => libsql::Value::Null,
        },
    ];
    params.extend(tables.iter().map(|t| libsql::Value::Text((*t).to_string())));

    let mut rows = conn.query(&sql, params).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Change {
            seq_id: row.get(0)?,
            branch_id: row.get(1)?,
            table: row.get(2)?,
            entity_id: row.get(3)?,
            operation: row.get(4)?,
            recorded_at: row.get(5)?,
        });
    }
    drop(rows);

    let upper = if out.len() == limit && limit > 0 {
        out.last().map(|c| c.seq_id).unwrap_or(after_seq)
    } else {
        head_seq(conn).await?
    };
    if upper > after_seq {
        let mut count = conn
            .query(
                "SELECT COUNT(*) FROM transaction_log WHERE seq_id > ?1 AND seq_id <= ?2",
                libsql::params![after_seq, upper],
            )
            .await?;
        let present: i64 = match count.next().await? {
            Some(row) => row.get(0)?,
            None => 0,
        };
        let expected = upper - after_seq;
        if present < expected {
            return Err(DbError::ChangeFeedGap {
                after_seq,
                removed: expected - present,
            });
        }
    }
    Ok(out)
}
