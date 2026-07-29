use serde::{Deserialize, Serialize};
use crate::error::Result;
use crate::graph::builder::AttributeMode;

/// Node attribute payload hydrated from concepts table or transaction_log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAttributes {
    pub id: String,
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
}

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

/// Hydrate attributes for a list of node IDs based on the specified AttributeMode (§5.2).
pub async fn hydrate_attributes(
    conn: &libsql::Connection,
    node_ids: &[String],
    ts: &str,
    mode: AttributeMode,
) -> Result<Vec<NodeAttributes>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    match mode {
        AttributeMode::Omit => Ok(Vec::new()),
        AttributeMode::Current => {
            tracing::warn!(
                "AttributeMode::Current requested for as_of({}) query: returning live attributes which may reflect post-{} edits",
                ts, ts
            );
            let mut results = Vec::new();
            for id in node_ids {
                let mut rows = conn
                    .query(
                        "SELECT id, title, content, embedding_model FROM concepts WHERE id = ?1 AND retired = 0",
                        libsql::params![id.as_str()],
                    )
                    .await?;
                if let Some(row) = rows.next().await? {
                    results.push(NodeAttributes {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        content: row.get(2)?,
                        embedding_model: row.get(3).ok(),
                    });
                }
            }
            Ok(results)
        }
        AttributeMode::AtTime => {
            let mut results = Vec::new();
            for id in node_ids {
                let sql = r#"
                    SELECT payload FROM (
                        SELECT payload, ROW_NUMBER() OVER (ORDER BY seq_id DESC) as rn
                        FROM transaction_log
                        WHERE table_name = 'concepts' AND entity_id = ?1 AND recorded_at <= ?2
                    ) WHERE rn = 1
                "#;
                let mut rows = conn.query(sql, libsql::params![id.as_str(), ts]).await?;
                if let Some(row) = rows.next().await? {
                    let payload_str: String = row.get(0)?;
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload_str) {
                        let title = v.get("title").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        let content = v.get("content").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        let embedding_model = v.get("embedding_model").and_then(|s| s.as_str()).map(|s| s.to_string());
                        results.push(NodeAttributes {
                            id: id.clone(),
                            title,
                            content,
                            embedding_model,
                        });
                    }
                }
            }
            Ok(results)
        }
    }
}
