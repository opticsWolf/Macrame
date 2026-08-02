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
    /// `None` means *defaulted*, not `Current` (T3.2, D-085).
    ///
    /// The distinction is the whole mechanism. `Current` chosen by a caller who
    /// knows what it means is a legitimate, fast answer; `Current` arrived at by
    /// never touching the setting, on a query about the past, is a wrong answer
    /// nobody asked for. Those two produce identical behaviour and must not be
    /// stored identically, so the field records which happened.
    ///
    /// Public for construction-by-struct-literal, which is why it is an `Option`
    /// here rather than a private `bool` beside the mode: a caller building the
    /// struct directly should have to write down the same thing the builder
    /// method records.
    pub attribute_mode: Option<AttributeMode>,
    /// The valid-time instant to traverse at, if it is not the present.
    ///
    /// Added in 0.6.0 so "as of Tuesday" is *expressible*. Before this, the
    /// instant arrived as `execute`'s `now_ts` parameter and a historical
    /// traversal was indistinguishable from a live one — which is why the
    /// mismatch with `AttributeMode::Current` could only ever be a `warn!`:
    /// nothing in the call had the information needed to raise an error.
    pub as_of: Option<String>,
    /// Whether [`crate::Database::load_subgraph_with`] should fetch
    /// `concepts.content` (0.8.0, B3, D-116).
    ///
    /// **Default `false`, which is a change in what a load returns.** No
    /// algorithm reads document text, and at realistic document sizes it is
    /// most of the byte budget, so the default was spending the budget on bytes
    /// nothing would look at. A caller who needs it asks; one who does not gets
    /// `NodeData::content() == None`, which is distinguishable from an empty
    /// document.
    ///
    /// Ignored by [`crate::Database::load_subgraph`], which has no builder and
    /// never loads content.
    pub content: bool,
}

impl TraversalBuilder {
    pub fn new(start_node: impl Into<String>) -> Self {
        Self {
            start_node: start_node.into(),
            max_depth: 3,
            edge_types: Vec::new(),
            min_weight: 0.0,
            attribute_mode: None,
            as_of: None,
            content: false,
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

    /// State the attribute mode explicitly.
    ///
    /// Calling this is what turns `Current` from a default into a decision, and
    /// [`Self::execute`] treats the two differently on a historical traversal —
    /// see [`Self::as_of`].
    pub fn attribute_mode(mut self, mode: AttributeMode) -> Self {
        self.attribute_mode = Some(mode);
        self
    }

    /// Fetch `concepts.content` into every hydrated node (0.8.0, B3, D-116).
    ///
    /// Off by default. Turning it on is what the byte budget is then spent on:
    /// at 20 KB per concept, document text is the large majority of a loaded
    /// graph, and none of the six algorithms reads it.
    pub fn content(mut self, content: bool) -> Self {
        self.content = content;
        self
    }

    /// Traverse the graph as it was at `ts` rather than at the present (§5.2).
    ///
    /// # Setting this makes the attribute mode a required decision (T3.2, D-085)
    ///
    /// A historical traversal has two independent temporal questions, and until
    /// 0.6.0 only one of them was asked. The topology comes from `ts`. The node
    /// *attributes* — titles, content — come from wherever
    /// [`AttributeMode`] says, and the default said `Current`, which is live
    /// text. So `as_of(Tuesday)` returned Tuesday's graph wearing today's
    /// titles, and reported that through a `tracing::warn!` — invisible in any
    /// application that has not configured a subscriber, which is most of them
    /// at first run.
    ///
    /// So: with `as_of` set and no [`Self::attribute_mode`] call,
    /// [`Self::execute`] returns [`DbError::AttributeModeUnstated`] rather than
    /// guessing. Both answers stay available and neither is silent:
    ///
    /// ```no_run
    /// # use macrame::graph::{AttributeMode, TraversalBuilder};
    /// # async fn f(conn: &libsql::Connection, now: &str) -> macrame::Result<()> {
    /// // Tuesday's graph with Tuesday's titles — usually what was meant.
    /// let then = TraversalBuilder::new("a")
    ///     .as_of("2026-01-06T00:00:00.000000Z")
    ///     .attribute_mode(AttributeMode::AtTime)
    ///     .execute(conn, now)
    ///     .await?;
    ///
    /// // Tuesday's topology with today's titles — legitimate, and now stated.
    /// let mixed = TraversalBuilder::new("a")
    ///     .as_of("2026-01-06T00:00:00.000000Z")
    ///     .attribute_mode(AttributeMode::Current)
    ///     .execute(conn, now)
    ///     .await?;
    /// # Ok(()) }
    /// ```
    ///
    /// A traversal with no `as_of` is a query about now, where `Current` and
    /// `AtTime` agree about which text to return, so the default stands and no
    /// caller has to change.
    pub fn as_of(mut self, ts: impl Into<String>) -> Self {
        self.as_of = Some(ts.into());
        self
    }

    /// The instant this traversal reads at: [`Self::as_of`] if set, else `now_ts`.
    fn instant<'a>(&'a self, now_ts: &'a str) -> &'a str {
        self.as_of.as_deref().unwrap_or(now_ts)
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
    ///
    /// Reads at [`Self::as_of`] when set, else at `now_ts`. No attribute mode is
    /// involved, so this never returns [`DbError::AttributeModeUnstated`]:
    /// topology at an instant is unambiguous, and it is only the *pairing* with
    /// live attributes that needed a decision.
    pub async fn execute_ids(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<String>> {
        let sql = self.build_sql();

        let mut params: Vec<libsql::Value> = vec![
            self.start_node.as_str().into(),
            (self.max_depth as i64).into(),
            self.instant(now_ts).into(),
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
    ///
    /// # Errors
    ///
    /// [`DbError::AttributeModeUnstated`] when [`Self::as_of`] is set and
    /// [`Self::attribute_mode`] is not — see `as_of` for why that combination is
    /// a question rather than a default (T3.2, D-085).
    ///
    /// `now_ts` is the caller's present. A traversal with `as_of` set reads
    /// topology at that instant instead; `now_ts` is still what an
    /// `AttributeMode::Current` hydrate means by "current".
    pub async fn execute(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<NodeAttributes>> {
        let mode = self.resolved_mode()?;
        let instant = self.instant(now_ts);
        let ids = self.execute_ids(conn, now_ts).await?;

        // `Current` hydrates from `concepts`, which is live regardless of the
        // instant, so the ts it receives only matters for `AtTime`. Passing the
        // traversal's instant rather than `now_ts` is what makes
        // `as_of(t) + AtTime` mean "as believed at t" — the whole point of the
        // pairing this method now requires the caller to state.
        crate::temporal::as_of::hydrate_attributes(conn, &ids, instant, mode).await
    }

    /// The mode to hydrate with, or the error that says the caller must choose.
    ///
    /// Kept separate from [`Self::execute`] so it is unit-testable without a
    /// database: the property under test is a decision about two `Option`s, and
    /// a test that needed a connection to check it would be testing something
    /// else as well.
    pub(crate) fn resolved_mode(&self) -> Result<AttributeMode> {
        match (self.as_of.as_deref(), self.attribute_mode) {
            (Some(as_of), None) => Err(crate::error::DbError::AttributeModeUnstated {
                as_of: as_of.to_string(),
            }),
            (_, Some(mode)) => Ok(mode),
            (None, None) => Ok(AttributeMode::Current),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DbError;

    const TUE: &str = "2026-01-06T00:00:00.000000Z";

    /// The only combination that is a question, and it is now asked.
    #[test]
    fn as_of_without_a_stated_mode_is_an_error() {
        let err = TraversalBuilder::new("a")
            .as_of(TUE)
            .resolved_mode()
            .expect_err("past topology plus present text must not be a default");

        match err {
            DbError::AttributeModeUnstated { as_of } => assert_eq!(as_of, TUE),
            other => panic!("got {other:?}"),
        }

        // And the message has to be actionable: a caller who reads only this
        // should know which two calls resolve it.
        let text = DbError::AttributeModeUnstated {
            as_of: TUE.to_string(),
        }
        .to_string();
        assert!(
            text.contains("AtTime") && text.contains("Current"),
            "{text}"
        );
    }

    /// Stating `Current` on a historical traversal is legitimate and stays so.
    ///
    /// The fix must not be "forbid the fast path". Past topology with live text
    /// is a real query — a caller rendering a historical diagram with today's
    /// labels wants exactly it — and the objection was always to getting it
    /// without asking, never to asking for it.
    #[test]
    fn a_stated_mode_is_honoured_on_a_historical_traversal() {
        for mode in [
            AttributeMode::Current,
            AttributeMode::AtTime,
            AttributeMode::Omit,
        ] {
            let got = TraversalBuilder::new("a")
                .as_of(TUE)
                .attribute_mode(mode)
                .resolved_mode()
                .unwrap();
            assert_eq!(got, mode);
        }
    }

    /// A traversal about now still defaults, so no existing caller changes.
    ///
    /// This is what keeps the change from being a breaking one for the common
    /// case: with no `as_of`, `Current` and `AtTime` agree about which text to
    /// return, so there is nothing to decide and nothing to ask.
    #[test]
    fn a_live_traversal_still_defaults_to_current() {
        assert_eq!(
            TraversalBuilder::new("a").resolved_mode().unwrap(),
            AttributeMode::Current
        );
    }

    /// `as_of` supplies the instant the walk reads at; `now_ts` is the fallback.
    #[test]
    fn as_of_overrides_the_execute_timestamp() {
        let now = "2026-06-01T00:00:00.000000Z";
        assert_eq!(TraversalBuilder::new("a").instant(now), now);
        assert_eq!(TraversalBuilder::new("a").as_of(TUE).instant(now), TUE);
    }
}
