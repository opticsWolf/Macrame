use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{DbError, Result};
use crate::graph::builder::AttributeMode;
use crate::temporal::replay::PAYLOAD_VERSION;

/// Node attribute payload hydrated from concepts table or transaction_log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAttributes {
    pub id: String,
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
}

use crate::util::limits::HYDRATE_CHUNK;

/// Query valid-time graph edges under current belief as of `ts` (§5.2).
pub async fn query_as_of_edges(
    conn: &libsql::Connection,
    ts: &str,
) -> Result<Vec<(String, String, String, String, String)>> {
    let sql = r#"
        SELECT source_id, target_id, edge_type, valid_from, valid_to
        FROM links_current
        WHERE valid_from <= ?1 AND ?1 < valid_to
    "#;
    let mut rows = conn.query(sql, libsql::params![ts]).await?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next().await? {
        let src: String = row.get(0)?;
        let tgt: String = row.get(1)?;
        let edge_type: String = row.get(2)?;
        let vf: String = row.get(3)?;
        let vt: String = row.get(4)?;
        edges.push((src, tgt, edge_type, vf, vt));
    }
    Ok(edges)
}

/// `?n, ?n+1, …` for `count` ids starting at `first`.
///
/// The ids are caller data and are bound, never interpolated. Only the
/// *placeholders* are built by hand, which is the one part of the statement that
/// carries no caller input at all.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Hydrate attributes for a list of node IDs based on the specified AttributeMode (§5.2).
///
/// **Retirement is uniform across the three readers as of Wave 1 (defect AB).**
/// The rule is the one `AttributeMode::Current` and
/// [`crate::temporal::reconstruct`]
/// already followed and `AtTime` did not: *a concept retired as of the instant
/// being asked about is not returned*. Retirement is the application axis (§4.1)
/// and a temporal read shows what was visible, so the three modes now disagree
/// about which text they return and agree about which concepts exist. Before
/// this, `AtTime` read the payload and never looked at `retired`, so it returned
/// concepts retired long before `ts` — the one reader that consulted the ledger
/// most faithfully was also the one that answered the visibility question wrong.
///
/// Note what "as of `ts`" means for each: `Current` asks whether the concept is
/// retired *now* and `AtTime` asks whether it was retired *then*. That is not an
/// inconsistency, it is the two clocks — and it is why `Current` on a historical
/// query is worth objecting to. **That objection is no longer made here**
/// (T3.2, D-085): this function receives the mode as a parameter and has no way
/// to tell a historical query from a live one, so it does what it is told.
/// [`crate::graph::TraversalBuilder`] is the layer that knows, and it raises
/// [`DbError::AttributeModeUnstated`].
///
/// Both modes issue **one query per chunk of [`HYDRATE_CHUNK`] ids**, not one
/// per node (defect AE). Results come back in `node_ids` order regardless of the
/// order the rows arrive in, because a graph read that permuted its own output
/// between runs would break the property suite's equality comparisons for a
/// reason that has nothing to do with the property under test.
pub async fn hydrate_attributes(
    conn: &libsql::Connection,
    node_ids: &[String],
    ts: &str,
    mode: AttributeMode,
) -> Result<Vec<NodeAttributes>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let found: HashMap<String, NodeAttributes> = match mode {
        AttributeMode::Omit => return Ok(Vec::new()),
        // No warning here any more (T3.2, D-085). This function takes the mode
        // as a parameter and cannot tell a historical query from a live one —
        // `ts` is just an instant — so the warning fired on *every* `Current`
        // hydrate, which is overwhelmingly the ordinary live case where it is
        // exactly right. Loud where it did not matter and, being a log line,
        // silent where it did. The decision now lives in `TraversalBuilder`,
        // which knows whether `as_of` was set, and is a typed error.
        AttributeMode::Current => hydrate_current(conn, node_ids).await?,
        AttributeMode::AtTime => hydrate_at_time(conn, node_ids, ts).await?,
    };

    // Caller order, and absences simply dropped — the signature returns a Vec
    // rather than a per-id Option, so a node with no visible concept is reported
    // by being missing. That is what both modes did before.
    let mut out = Vec::with_capacity(found.len());
    for id in node_ids {
        if let Some(attrs) = found.get(id) {
            out.push(attrs.clone());
        }
    }
    Ok(out)
}

/// Live attributes, filtered by retirement *now*.
async fn hydrate_current(
    conn: &libsql::Connection,
    node_ids: &[String],
) -> Result<HashMap<String, NodeAttributes>> {
    let mut found = HashMap::new();

    for chunk in node_ids.chunks(HYDRATE_CHUNK) {
        let sql = format!(
            "SELECT id, title, content, embedding_model FROM concepts \
             WHERE retired = 0 AND id IN ({})",
            placeholders(1, chunk.len())
        );
        let params: Vec<libsql::Value> = chunk
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            found.insert(
                id.clone(),
                NodeAttributes {
                    id,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    embedding_model: row.get(3).ok(),
                },
            );
        }
    }

    Ok(found)
}

/// Attributes as recorded at `ts`, filtered by retirement *at* `ts`.
///
/// The window partitions on `entity_id` alone and is sound doing so only because
/// `table_name = 'concepts'` is already in the `WHERE` — the discriminator is
/// applied by the filter instead of by the partition, so the concept/link
/// collision that defect W is about cannot arise here. Stated because the four
/// folds in `replay.rs` now carry the discriminator in the partition and the
/// difference should not read as an oversight.
async fn hydrate_at_time(
    conn: &libsql::Connection,
    node_ids: &[String],
    ts: &str,
) -> Result<HashMap<String, NodeAttributes>> {
    let mut found = HashMap::new();

    for chunk in node_ids.chunks(HYDRATE_CHUNK) {
        let sql = format!(
            r#"
            SELECT entity_id, seq_id, payload FROM (
                SELECT entity_id, seq_id, payload,
                       ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) as rn
                FROM transaction_log
                WHERE table_name = 'concepts'
                  AND recorded_at <= ?1
                  AND entity_id IN ({})
            ) WHERE rn = 1
            "#,
            placeholders(2, chunk.len())
        );

        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(libsql::Value::Text(ts.to_string()));
        params.extend(chunk.iter().map(|id| libsql::Value::Text(id.clone())));

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            let seq_id: i64 = row.get(1)?;
            let payload_str: String = row.get(2)?;

            // Raised rather than skipped. A payload that will not parse is the
            // ledger disagreeing with itself, and the previous version's
            // `if let Ok(..)` turned that into a node quietly missing from the
            // answer — the same shape of silence defect W was.
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).map_err(|e| DbError::ReplayCorrupt {
                    seq: seq_id,
                    reason: format!("Failed to parse payload JSON: {e}"),
                })?;

            let v = payload.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
            if v > PAYLOAD_VERSION as u64 {
                return Err(DbError::PayloadVersion {
                    got: v as u8,
                    max: PAYLOAD_VERSION,
                });
            }

            // Retired as of `ts`: not visible, and not an error either.
            if payload.get("retired").and_then(|r| r.as_i64()).unwrap_or(0) != 0 {
                continue;
            }

            found.insert(
                id.clone(),
                NodeAttributes {
                    id,
                    title: payload
                        .get("title")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    content: payload
                        .get("content")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    // Absent in a v1 payload, which is indistinguishable here
                    // from present-and-null and correctly so: both mean the
                    // concept carries no model.
                    embedding_model: payload
                        .get("embedding_model")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                },
            );
        }
    }

    Ok(found)
}
