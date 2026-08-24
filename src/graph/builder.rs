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
    /// The **valid-time** instant to traverse at, if it is not the present.
    ///
    /// Added in 0.6.0 as `as_of` so "as of Tuesday" is *expressible*. Before
    /// that, the instant arrived as `execute`'s `now_ts` parameter and a
    /// historical traversal was indistinguishable from a live one — which is why
    /// the mismatch with `AttributeMode::Current` could only ever be a `warn!`:
    /// nothing in the call had the information needed to raise an error.
    ///
    /// **Renamed from `as_of` in 0.13.2 (W7.1, D-174).** The old name carried
    /// one instant onto two clocks; see [`Self::as_of_valid`].
    pub as_of_valid: Option<String>,
    /// The **transaction-time** instant to traverse at: *what did we believe
    /// then* (0.13.2, W7.1, D-174).
    ///
    /// `None` — the default — means current belief, and the walk reads
    /// `links_current` exactly as it always has. `Some(t)` folds
    /// `transaction_log` to `t` instead, so the topology is the one the ledger
    /// held at `t` rather than the one it holds now. See
    /// [`Self::as_of_recorded`].
    pub as_of_recorded: Option<String>,
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
            as_of_valid: None,
            as_of_recorded: None,
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

    /// Traverse the graph as it was **in the world** at `ts` — the valid-time
    /// axis (§5.2, W7.1).
    ///
    /// # This was `as_of`, and the rename is the fix (0.13.2, W7.1, D-174)
    ///
    /// [Doctrine VIII](../docs/architecture/s0-s3-foundations.md#doctrine-viii)
    /// says a query that mixes the two clocks says so in its signature. `as_of`
    /// did not: one timestamp reached `links.valid_from`/`valid_to` on the
    /// **valid-time** axis and `transaction_log.recorded_at` on the
    /// **transaction-time** axis, so `as_of(t).attribute_mode(AtTime)` answered
    /// *"the edges valid at `t`, labelled with what we believed at `t`"* — two
    /// questions under one word. [§3.1](../docs/architecture/s0-s3-foundations.md)
    /// named it; 0.12.17 (W5.6, D-160) wrote the semantics down without changing
    /// them, precisely so this change could be reviewed against a stated
    /// position; this is that change.
    ///
    /// The two axes are now two parameters, and they compose:
    ///
    /// | set | topology comes from | attributes come from |
    /// |---|---|---|
    /// | neither | `links_current`, at `now_ts` | live `concepts` |
    /// | `as_of_valid(v)` | `links_current`, bounded at `v` | `concepts` valid at `v` |
    /// | `as_of_recorded(r)` | `transaction_log` folded to `r`, bounded at `now_ts` | the payload believed at `r` |
    /// | both | folded to `r`, bounded at `v` | believed at `r`, valid at `v` |
    ///
    /// The last row is the cell Jensen and Snodgrass's BCDM defines a bitemporal
    /// database as answering — *what did we believe at `r` about what was true at
    /// `v`* — and before this it was not expressible on any surface in the crate.
    ///
    /// # Setting either instant makes the attribute mode a required decision (T3.2, D-085)
    ///
    /// A historical traversal has two independent questions and until 0.6.0 only
    /// one of them was asked. The topology comes from the instants. The node
    /// *attributes* — titles, content — come from wherever [`AttributeMode`]
    /// says, and the default said `Current`, which is live text. So a historical
    /// traversal returned the past's graph wearing today's titles, and reported
    /// that through a `tracing::warn!` — invisible in any application that has
    /// not configured a subscriber, which is most of them at first run.
    ///
    /// So: with either instant set and no [`Self::attribute_mode`] call,
    /// [`Self::execute`] returns
    /// [`DbError::AttributeModeUnstated`](crate::DbError::AttributeModeUnstated)
    /// rather than guessing. Both answers stay available and neither is silent:
    ///
    /// ```no_run
    /// # use macrame::graph::{AttributeMode, TraversalBuilder};
    /// # async fn f(conn: &libsql::Connection, now: &str) -> macrame::Result<()> {
    /// // What was true on Tuesday, as best we now know.
    /// let then = TraversalBuilder::new("a")
    ///     .as_of_valid("2026-01-06T00:00:00.000000Z")
    ///     .attribute_mode(AttributeMode::AtTime)
    ///     .execute(conn, now)
    ///     .await?;
    ///
    /// // Tuesday's topology with today's titles — legitimate, and now stated.
    /// let mixed = TraversalBuilder::new("a")
    ///     .as_of_valid("2026-01-06T00:00:00.000000Z")
    ///     .attribute_mode(AttributeMode::Current)
    ///     .execute(conn, now)
    ///     .await?;
    /// # Ok(()) }
    /// ```
    ///
    /// A traversal with neither instant is a query about now, where `Current`
    /// and `AtTime` agree about which text to return, so the default stands and
    /// no caller has to change.
    ///
    /// # What the rename buys, concretely
    ///
    /// Suppose a concept's title is corrected today, fixing a typo made in 2020.
    /// Under the old `as_of("2020-06-01")` with `AtTime` the answer was the
    /// **uncorrected** title, because the correction was *recorded* after `ts` —
    /// the right answer to *what did we believe in 2020* and the wrong one to
    /// *what was true in 2020*, which is what the name promised. Now
    /// `as_of_valid("2020-06-01")` alone gives the corrected title,
    /// `as_of_recorded("2020-06-01")` gives the uncorrected one, and a caller
    /// asking for either says which.
    ///
    /// The second, smaller mismatch W5.6 recorded is closed by the same change:
    /// `AtTime` hydration consulted the payload's `retired` flag and never the
    /// concept's **own valid interval**, so a concept whose validity had ended
    /// still hydrated. It is now bounded by whichever instants are set, so the
    /// two halves of the answer agree about what "existed then" means.
    pub fn as_of_valid(mut self, ts: impl Into<String>) -> Self {
        self.as_of_valid = Some(ts.into());
        self
    }

    /// Traverse the graph as the ledger **believed** it at `ts` — the
    /// transaction-time axis (0.13.2, W7.1, D-174).
    ///
    /// Where [`Self::as_of_valid`] asks *what was true*, this asks *what did we
    /// think was true*. Setting it moves the walk off `links_current` and onto a
    /// fold of `transaction_log` bounded at `ts` — the same fold
    /// [`crate::temporal::reconstruct`] performs. That operation still exists
    /// and still returns the whole state; this makes the same instant reachable
    /// from a *traversal*, which is what lets the two axes be set on one query.
    ///
    /// # This reads the hot log, and refuses rather than guessing
    ///
    /// A fold can only answer for instants the hot log still covers.
    /// [`crate::Database::archive`] removes superseded rows, so an instant below
    /// what remains is not *before history*, it is *history that is in the other
    /// file* — and this surface takes a connection, not an archive path, so it
    /// cannot go and get it. It returns
    /// [`DbError::RecordedInstantUnreachable`](crate::DbError::RecordedInstantUnreachable)
    /// naming the instant and pointing at `reconstruct`, which does take the
    /// path. Answering from a partial fold would return *nearly* the right
    /// topology, which is the worst failure available to a ledger.
    ///
    /// # Cost, stated rather than discovered
    ///
    /// `links_current` is a projection maintained for exactly this read and
    /// indexed for it (`idx_lc_traversal_cover`). The fold is a window function
    /// over `transaction_log` with a `json_extract` per column, materialised
    /// once per query and joined per hop. It is not the fast path and is not
    /// meant to be. W10.6 measures it and decides whether anything should be
    /// built for it.
    pub fn as_of_recorded(mut self, ts: impl Into<String>) -> Self {
        self.as_of_recorded = Some(ts.into());
        self
    }

    /// The valid-time instant this traversal reads at: [`Self::as_of_valid`] if
    /// set, else `now_ts`.
    ///
    /// Note the asymmetry with [`Self::as_of_recorded`], which has no `now_ts`
    /// fallback. An unset transaction-time instant means *current belief*, and
    /// current belief is `links_current` rather than a fold bounded at the
    /// present: the two are the same answer and only one of them is cheap.
    pub(crate) fn valid_instant<'a>(&'a self, now_ts: &'a str) -> &'a str {
        self.as_of_valid.as_deref().unwrap_or(now_ts)
    }

    /// The instant pair this traversal reads at, for the hydration layer.
    pub(crate) fn instants(&self, now_ts: &str) -> crate::temporal::as_of::AsOf {
        crate::temporal::as_of::AsOf {
            valid: Some(self.valid_instant(now_ts).to_string()),
            recorded: self.as_of_recorded.clone(),
        }
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

    /// Where edge types start binding: `?5`, or `?6` when the walk carries a
    /// transaction-time instant (0.13.2, W7.1).
    ///
    /// `?1..?4` are start, depth, the valid instant and `min_weight`. A traversal
    /// with [`Self::as_of_recorded`] set binds it at `?5` and pushes the variadic
    /// edge types along by one.
    ///
    /// **This exists so the offset is computed once rather than agreed twice.**
    /// [`Self::bind_params`] and [`Self::edge_filter_sql`] are the only two
    /// places that care, they must agree exactly, and the previous arrangement —
    /// a hard-coded `5` in one file and a comment in the other saying both call
    /// sites push in the same order — is the shape D-030 and D-035 are about.
    pub(crate) fn edge_type_base(&self) -> usize {
        if self.as_of_recorded.is_some() {
            6
        } else {
            5
        }
    }

    /// The `AND l.edge_type IN (…)` fragment, or empty when unfiltered.
    ///
    /// Placeholders start at [`Self::edge_type_base`]. Bound, never spliced: an
    /// edge type is caller data, and the crate's only validation of one runs on
    /// the *write* path (D-039), so a traversal never passes through it.
    pub(crate) fn edge_filter_sql(&self) -> String {
        if self.edge_types.is_empty() {
            String::new()
        } else {
            let base = self.edge_type_base();
            let placeholders: Vec<String> = (0..self.edge_types.len())
                .map(|i| format!("?{}", i + base))
                .collect();
            format!(" AND l.edge_type IN ({})", placeholders.join(", "))
        }
    }

    /// Every parameter the walk and its projections bind, in placeholder order.
    ///
    /// One producer for both consumers ([`Self::execute_ids`] and
    /// `Database::load_subgraph_with`), for the reason [`Self::edge_type_base`]
    /// gives: they previously agreed by comment, and one of them had already
    /// drifted — the subgraph loader bound `now_ts` at `?3` where the builder
    /// bound the traversal's own instant, so **a historical `load_subgraph_with`
    /// silently read the present** (F-35, W7.1).
    pub(crate) fn bind_params(&self, now_ts: &str) -> Vec<libsql::Value> {
        let mut params: Vec<libsql::Value> = vec![
            self.start_node.as_str().into(),
            (self.max_depth as i64).into(),
            self.valid_instant(now_ts).into(),
            self.min_weight.into(),
        ];
        if let Some(recorded) = self.as_of_recorded.as_deref() {
            params.push(recorded.into());
        }
        params.extend(self.edge_types.iter().map(|t| t.as_str().into()));
        params
    }

    /// The relation the walk and the projections read edges from.
    ///
    /// `links_current` under current belief; the `links_at_tx` fold otherwise.
    /// Both expose the same six columns under the same names, which is what lets
    /// the rest of the SQL be written once.
    pub(crate) fn link_source(&self) -> &'static str {
        if self.as_of_recorded.is_some() {
            "links_at_tx"
        } else {
            "links_current"
        }
    }

    /// `links_current` as the ledger believed it at `?5`, or empty (W7.1, D-174).
    ///
    /// `links_current` is a *projection of current belief*: the sync trigger
    /// upserts each corrected edge over its predecessor, so the row that was
    /// there before a correction is not in the table any more. It is in
    /// `transaction_log`, because links are strictly append-only — every
    /// assertion and every correction is an `INSERT`, each logged `'I'` with
    /// `entity_id = source|target|type|valid_from` — so the last log row per
    /// entity at or before `?5` *is* what `links_current` held at `?5`.
    ///
    /// **Partitioning on `entity_id` alone is sound here only because
    /// `table_name = 'links'` is already in the `WHERE`.** The discriminator is
    /// applied by the filter instead of by the partition, so the concept/link
    /// collision defect W is about cannot arise. The four folds in `replay.rs`
    /// carry it in the partition instead; the difference is deliberate and
    /// stated so it does not read as an oversight — the same note
    /// `as_of::hydrate_at_time` carries, for the same reason.
    ///
    /// There is no `'D'` arm because there are no link deletes:
    /// `trg_links_guard_delete` refuses them outside an archive session, and an
    /// archive session removes the *log rows* rather than logging a removal.
    fn links_at_tx_cte(&self) -> String {
        if self.as_of_recorded.is_none() {
            return String::new();
        }
        r#"links_at_tx(source_id, target_id, edge_type, valid_from, valid_to, weight) AS (
    SELECT json_extract(payload, '$.source_id'),
           json_extract(payload, '$.target_id'),
           json_extract(payload, '$.edge_type'),
           json_extract(payload, '$.valid_from'),
           json_extract(payload, '$.valid_to'),
           json_extract(payload, '$.weight')
    FROM (
        SELECT payload,
               ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) AS rn
        FROM transaction_log
        WHERE table_name = 'links' AND recorded_at <= ?5
    ) WHERE rn = 1
),
"#
        .to_string()
    }

    /// Refuse a transaction-time instant the hot log can no longer answer for.
    ///
    /// See [`Self::as_of_recorded`]. Cheap enough to run unconditionally on the
    /// folded path — two aggregates over an indexed column — and it only runs
    /// there, so the ordinary traversal pays nothing.
    pub(crate) async fn check_recorded_reach(&self, conn: &libsql::Connection) -> Result<()> {
        let Some(ts) = self.as_of_recorded.as_deref() else {
            return Ok(());
        };
        if crate::temporal::replay::hot_log_answers_for(conn, ts).await? {
            return Ok(());
        }
        Err(crate::error::DbError::RecordedInstantUnreachable {
            ts: ts.to_string(),
        })
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
        let fold = self.links_at_tx_cte();
        let source = self.link_source();
        format!(
            r#"
WITH RECURSIVE {fold}walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN {source} l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4
      {edge_filter}
)"#
        )
    }

    /// Node ids reachable under this traversal, in id order (§5.2).
    ///
    /// Reads at [`Self::as_of_valid`] when set, else at `now_ts`, and under the
    /// belief [`Self::as_of_recorded`] names when set, else current belief. No
    /// attribute mode is involved, so this never returns
    /// [`DbError::AttributeModeUnstated`](crate::DbError::AttributeModeUnstated):
    /// topology at an instant is unambiguous, and it is only the *pairing* with
    /// live attributes that needed a decision.
    ///
    /// # Errors
    ///
    /// [`DbError::RecordedInstantUnreachable`](crate::DbError::RecordedInstantUnreachable)
    /// when [`Self::as_of_recorded`] is below what the hot log still covers.
    pub async fn execute_ids(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<String>> {
        self.check_recorded_reach(conn).await?;
        let sql = self.build_sql();
        let params = self.bind_params(now_ts);

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
    /// [`DbError::AttributeModeUnstated`](crate::DbError::AttributeModeUnstated)
    /// when either instant is set and [`Self::attribute_mode`] is not — see
    /// [`Self::as_of_valid`] for why that combination is a question rather than
    /// a default (T3.2, D-085).
    ///
    /// [`DbError::RecordedInstantUnreachable`](crate::DbError::RecordedInstantUnreachable)
    /// when [`Self::as_of_recorded`] is below what the hot log still covers.
    ///
    /// `now_ts` is the caller's present, and it is the fallback on *both* axes:
    /// a traversal with neither instant set reads live topology and live text.
    pub async fn execute(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<NodeAttributes>> {
        let mode = self.resolved_mode()?;
        let as_of = self.instants(now_ts);
        let ids = self.execute_ids(conn, now_ts).await?;

        // `Current` hydrates from `concepts` live and ignores both instants, so
        // the pair it receives only matters for `AtTime`. Passing the traversal's
        // own instants rather than `now_ts` is what makes a historical traversal
        // with `AtTime` mean what it says — the whole point of the pairing this
        // method requires the caller to state.
        crate::temporal::as_of::hydrate_attributes(conn, &ids, &as_of, mode).await
    }

    /// The mode to hydrate with, or the error that says the caller must choose.
    ///
    /// Kept separate from [`Self::execute`] so it is unit-testable without a
    /// database: the property under test is a decision about two `Option`s, and
    /// a test that needed a connection to check it would be testing something
    /// else as well.
    pub(crate) fn resolved_mode(&self) -> Result<AttributeMode> {
        // Either instant makes the question live, and the error names whichever
        // was set — valid time first, because a caller who set both is asking a
        // question the message reads correctly either way.
        let instant = self
            .as_of_valid
            .as_deref()
            .or(self.as_of_recorded.as_deref());
        match (instant, self.attribute_mode) {
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
            .as_of_valid(TUE)
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
                .as_of_valid(TUE)
                .attribute_mode(mode)
                .resolved_mode()
                .unwrap();
            assert_eq!(got, mode);
        }
    }

    /// The transaction-time instant raises the same question the valid-time one
    /// does, and asking it on only one axis would be the same gap in a new place.
    #[test]
    fn a_recorded_instant_also_demands_a_stated_mode() {
        let err = TraversalBuilder::new("a")
            .as_of_recorded(TUE)
            .resolved_mode()
            .expect_err("past belief plus present text must not be a default");
        assert!(matches!(err, DbError::AttributeModeUnstated { .. }), "{err:?}");
    }

    /// A traversal about now still defaults, so no existing caller changes.
    ///
    /// This is what keeps the change from being a breaking one for the common
    /// case: with neither instant, `Current` and `AtTime` agree about which text
    /// to return, so there is nothing to decide and nothing to ask.
    #[test]
    fn a_live_traversal_still_defaults_to_current() {
        assert_eq!(
            TraversalBuilder::new("a").resolved_mode().unwrap(),
            AttributeMode::Current
        );
    }

    /// `as_of_valid` supplies the instant the walk reads at; `now_ts` is the
    /// fallback, and `as_of_recorded` never is — see `valid_instant`.
    #[test]
    fn as_of_valid_overrides_the_execute_timestamp() {
        let now = "2026-06-01T00:00:00.000000Z";
        assert_eq!(TraversalBuilder::new("a").valid_instant(now), now);
        assert_eq!(
            TraversalBuilder::new("a").as_of_valid(TUE).valid_instant(now),
            TUE
        );
        assert_eq!(
            TraversalBuilder::new("a")
                .as_of_recorded(TUE)
                .valid_instant(now),
            now,
            "fixing belief must not move the valid-time instant"
        );
    }

    /// The two axes reach the hydration layer separately (W7.1, D-174).
    ///
    /// The property that made the old single parameter wrong was that one
    /// instant arrived on both clocks. This asserts the negation directly, at the
    /// boundary where the split has to survive: what `execute` hands to
    /// `hydrate_attributes`.
    #[test]
    fn the_two_axes_reach_hydration_separately() {
        let now = "2026-06-01T00:00:00.000000Z";
        let mar = "2026-03-01T00:00:00.000000Z";

        let live = TraversalBuilder::new("a").instants(now);
        assert_eq!(live.valid.as_deref(), Some(now));
        assert_eq!(live.recorded, None, "no instant means current belief");

        let valid_only = TraversalBuilder::new("a").as_of_valid(TUE).instants(now);
        assert_eq!(valid_only.valid.as_deref(), Some(TUE));
        assert_eq!(valid_only.recorded, None);

        let recorded_only = TraversalBuilder::new("a").as_of_recorded(mar).instants(now);
        assert_eq!(
            recorded_only.valid.as_deref(),
            Some(now),
            "fixing belief leaves valid time at the present"
        );
        assert_eq!(recorded_only.recorded.as_deref(), Some(mar));

        let both = TraversalBuilder::new("a")
            .as_of_valid(TUE)
            .as_of_recorded(mar)
            .instants(now);
        assert_eq!(both.valid.as_deref(), Some(TUE));
        assert_eq!(both.recorded.as_deref(), Some(mar));
    }

    /// Placeholder arithmetic is the one thing two call sites must agree on.
    ///
    /// `bind_params` and `edge_filter_sql` are separate functions that have to
    /// produce the same layout, and the recorded instant shifts it. A test that
    /// counts is cheaper than the bug, which is an edge type silently compared
    /// against a timestamp.
    #[test]
    fn the_recorded_instant_shifts_the_edge_type_placeholders() {
        let now = "2026-06-01T00:00:00.000000Z";

        let plain = TraversalBuilder::new("a").edge_types(vec!["CITES".into()]);
        assert_eq!(plain.edge_type_base(), 5);
        assert!(plain.edge_filter_sql().contains("?5"), "{}", plain.edge_filter_sql());
        assert_eq!(plain.bind_params(now).len(), 5);

        let folded = plain.clone().as_of_recorded(TUE);
        assert_eq!(folded.edge_type_base(), 6);
        assert!(folded.edge_filter_sql().contains("?6"), "{}", folded.edge_filter_sql());
        assert_eq!(folded.bind_params(now).len(), 6);
    }

    /// The fold replaces the projection, and only when it is asked for.
    #[test]
    fn the_link_source_follows_the_recorded_instant() {
        let plain = TraversalBuilder::new("a");
        assert_eq!(plain.link_source(), "links_current");
        assert!(!plain.walk_cte().contains("transaction_log"));

        let folded = TraversalBuilder::new("a").as_of_recorded(TUE);
        assert_eq!(folded.link_source(), "links_at_tx");
        let sql = folded.walk_cte();
        assert!(sql.contains("links_at_tx"), "{sql}");
        assert!(sql.contains("recorded_at <= ?5"), "{sql}");
        assert!(
            sql.contains("table_name = 'links'"),
            "the partition is only sound with the discriminator filtered: {sql}"
        );
    }
}
