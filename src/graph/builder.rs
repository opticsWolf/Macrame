use crate::error::Result;
use crate::temporal::as_of::NodeAttributes;

/// Attribute hydration mode for temporal traversals (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttributeMode {
    /// Live attributes from concepts table. Fast. Documented as WRONG for historical text.
    Current,
    /// Attributes as believed at ts, hydrated from transaction_log.
    AtTime,
    /// Topology only; concepts join is omitted.
    Omit,
}

/// Recursive CTE traversal query builder (§5.2).
#[derive(Debug, Clone)]
pub struct TraversalBuilder {
    pub start_node: String,
    pub max_depth: usize,
    pub edge_types: Vec<String>,
    pub min_weight: f64,
    pub attribute_mode: AttributeMode,
}

impl TraversalBuilder {
    pub fn new(start_node: impl Into<String>) -> Self {
        Self {
            start_node: start_node.into(),
            max_depth: 3,
            edge_types: Vec::new(),
            min_weight: 0.0,
            attribute_mode: AttributeMode::Current,
        }
    }

    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn edge_types(mut self, types: Vec<String>) -> Self {
        self.edge_types = types;
        self
    }

    pub fn min_weight(mut self, weight: f64) -> Self {
        self.min_weight = weight;
        self
    }

    pub fn attribute_mode(mut self, mode: AttributeMode) -> Self {
        self.attribute_mode = mode;
        self
    }

    /// Compile the recursive CTE query string as specified in §5.2.
    ///
    /// Edge types become bind placeholders, not quoted literals. An earlier
    /// version spliced them in with `format!("'{t}'")`, which made any caller
    /// string a SQL fragment on the *read* path — and the only validation in the
    /// crate, [`super::edge::validate_edge_type`], runs in
    /// [`super::EdgeAssertion::normalized`] on the *write* path, so a traversal
    /// never passed through it. Binding removes the question rather than
    /// answering it: unlike a table name, an edge type is a value, and values
    /// can be parameters.
    pub fn build_sql(&self) -> String {
        let edge_filter = if self.edge_types.is_empty() {
            String::new()
        } else {
            // ?1..?4 are taken by start, depth, ts and min_weight.
            let placeholders: Vec<String> = (0..self.edge_types.len())
                .map(|i| format!("?{}", i + 5))
                .collect();
            format!(" AND l.edge_type IN ({})", placeholders.join(", "))
        };

        format!(
            r#"
WITH RECURSIVE walk(node_id, depth, path) AS (
    SELECT ?1, 0, CAST(?1 AS BLOB)
    UNION ALL
    SELECT l.target_id, w.depth + 1, w.path || '/' || CAST(l.target_id AS BLOB)
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
      {edge_filter}
      AND INSTR(w.path, CAST(l.target_id AS BLOB)) = 0
)
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
            "#
        )
    }

    /// Node ids reachable under this traversal, in id order (§5.2).
    pub async fn execute_ids(&self, conn: &libsql::Connection, now_ts: &str) -> Result<Vec<String>> {
        let sql = self.build_sql();

        let mut params: Vec<libsql::Value> = vec![
            self.start_node.as_str().into(),
            (self.max_depth as i64).into(),
            now_ts.into(),
            self.min_weight.into(),
        ];
        params.extend(self.edge_types.iter().map(|t| t.as_str().into()));

        let mut rows = conn.query(&sql, params).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    /// Execute the traversal and hydrate attributes per [`Self::attribute_mode`]
    /// (§5.2).
    ///
    /// The hydration is a second step rather than a join in the CTE because the
    /// three modes read from two different places: `Current` and `Omit` from
    /// `concepts`, `AtTime` from `transaction_log`. The previous version always
    /// emitted the `concepts` join, so `attribute_mode` was stored, exposed by a
    /// builder method, and never read — a caller asking for `AtTime` got live
    /// attributes with no indication that the mode had been ignored. That is the
    /// exact failure Doctrine II exists to prevent, arriving as a silent wrong
    /// answer rather than as an error.
    pub async fn execute(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<NodeAttributes>> {
        let ids = self.execute_ids(conn, now_ts).await?;
        crate::temporal::as_of::hydrate_attributes(conn, &ids, now_ts, self.attribute_mode).await
    }
}
