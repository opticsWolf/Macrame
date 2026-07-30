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
    ///
    /// **Use [`TraversalBuilder::execute_ids`], not [`TraversalBuilder::execute`].**
    /// `execute` returns `Vec<NodeAttributes>`, and there are no attributes to
    /// return under this mode, so it answers `Ok(vec![])` — which a caller
    /// cannot tell apart from a traversal that reached nothing. `execute_ids`
    /// returns exactly what this mode is for, and distinguishes the two cases by
    /// construction.
    ///
    /// Kept rather than removed (Wave 4.5) because it is meaningful where the
    /// mode is a *parameter* — `hydrate_attributes` and `FilteredVectorSearch`
    /// both take one and are right to accept "no attributes" as a choice. It is
    /// only `execute`'s return type that cannot express it.
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
        format!(
            "{}{}",
            self.walk_cte(),
            r#"
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
            "#
        )
    }

    /// The `AND l.edge_type IN (…)` fragment, or empty when unfiltered.
    ///
    /// `?1..?4` are start, depth, `now_ts` and `min_weight`, so edge types bind
    /// from `?5`. Both call sites push them in the same order after those four,
    /// which is why this lives beside the CTE rather than at either of them.
    pub(crate) fn edge_filter_sql(&self) -> String {
        if self.edge_types.is_empty() {
            String::new()
        } else {
            let placeholders: Vec<String> = (0..self.edge_types.len())
                .map(|i| format!("?{}", i + 5))
                .collect();
            format!(" AND l.edge_type IN ({})", placeholders.join(", "))
        }
    }

    /// The recursive `walk` CTE — **the one copy** (T0.1).
    ///
    /// [`Self::build_sql`] and `Database::load_subgraph_with` append their own
    /// projections to this. They previously carried byte-identical copies of the
    /// recursion in two files, and had already drifted once: D-073 found the
    /// subgraph loader taking neither `edge_types` nor `min_weight` while this
    /// builder took both. Two copies of a query that must agree is the same
    /// failure class as [D-030](../../docs/architecture/s13-decision-register.md)
    /// and D-035, applied to SQL.
    ///
    /// **`UNION`, not `UNION ALL`, and no `path` column (T0.1).** The shipped
    /// form carried a `path` of visited ids and refused a target already in it,
    /// which restricts the walk to *simple paths* — so `walk` held one row per
    /// distinct path to each node rather than one row per node, and the trailing
    /// `SELECT DISTINCT` collapsed the duplication only after the work was done.
    /// On a tree that costs nothing, because a tree has exactly one path to each
    /// node; on a graph the row count is multiplicative in branching factor per
    /// hop. Measured on libSQL 0.9.30 over a layered fixture (root, then *L*
    /// layers of *W*, each fully joined to the next): a **328-edge** graph at
    /// depth 6 produced **299,593** walk rows and took **428 ms**. The same
    /// traversal here produces 49 rows in 0.1 ms.
    ///
    /// `UNION` dedupes on `(node_id, depth)` as rows enter the queue, so `walk`
    /// is bounded by `V × (depth+1)` and termination comes from the depth bound
    /// rather than from inspecting the path. The projections keep their
    /// `DISTINCT`, because a node still legitimately appears at several depths.
    ///
    /// **Equivalence, argued rather than only measured.** The old form admits
    /// only simple paths; this one admits any walk. The reachable sets are the
    /// same: if a walk of length `k ≤ D` reaches `X`, excising its cycles yields
    /// a simple path of length `≤ k` that also reaches `X`. So simple-path
    /// reachability within `D` equals walk reachability within `D`, and the two
    /// forms differed only in how much redundant work they did to establish it.
    /// A property test over generated graphs — cycles, self-loops, diamonds and
    /// expired edges, the four shapes the proof steps over — compares this form
    /// against the old one at depths 1–4 and requires identical node *and* edge
    /// sets (`integrity_property_tests`, 512 cases).
    ///
    /// **It is not free on a tree, and the plan that proposed it said it was.**
    /// `UNION` maintains a dedupe b-tree over every row entering the queue; on a
    /// tree nothing is ever deduped, so that is pure overhead. Measured on the
    /// star-of-stars fixture at depth 3, best of 15, stable across runs:
    /// 1,011 nodes 1.6 ms either way, 5,051 nodes 8.9 → 9.5 ms, 10,101 nodes
    /// 17.8 → 19.6 ms — roughly **8–10% slower** where the old form was already
    /// optimal, against ~2,000× faster where it was not. Recorded rather than
    /// smoothed over: the trade is overwhelmingly worth taking and it is still a
    /// trade, and "within noise" was a claim from a different engine's numbers.
    pub(crate) fn walk_cte(&self) -> String {
        let edge_filter = self.edge_filter_sql();
        format!(
            r#"
WITH RECURSIVE walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN links_current l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
      {edge_filter}
)"#
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
    ///
    /// **[`AttributeMode::Omit`] returns `Ok(vec![])` here**, which is
    /// indistinguishable from a traversal that reached nothing. That is a
    /// limitation of this method's return type rather than of the mode; callers
    /// wanting topology only should use [`Self::execute_ids`], which says what it
    /// found.
    pub async fn execute(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<NodeAttributes>> {
        let ids = self.execute_ids(conn, now_ts).await?;
        crate::temporal::as_of::hydrate_attributes(conn, &ids, now_ts, self.attribute_mode).await
    }
}
