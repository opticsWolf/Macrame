use crate::error::{Result, StatedInstants};
use crate::graph::lineage::{lineage_shape, LineageShape};
use crate::graph::plan::{lower, Resolution};
use crate::schema::ddl;
use crate::temporal::as_of::NodeAttributes;

/// Attribute hydration mode for temporal traversals (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
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
    /// one instant onto two clocks; the method's own docs are where that is
    /// argued out.
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
    /// The lineage this traversal reads (§15.3, D-220).
    ///
    /// `None` is what every traversal written before v12 meant and what every
    /// database without a fork still holds: the trunk. `Some(id)` reads that
    /// branch's belief — one row per edge key, taken from the **nearest**
    /// branch on the path from it to the root, so a branch that corrects or
    /// retires an inherited edge is seen to have done so.
    ///
    /// **An unregistered lineage is refused rather than defaulted**, by
    /// `graph::lineage::lineage_shape`. Answering it
    /// with the trunk's view is the answer a caller is least able to detect,
    /// because on a database that has never forked it is the answer they
    /// expected anyway.
    ///
    /// Set through [`Self::on_branch`].
    pub branch: Option<String>,
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
            branch: None,
            content: false,
        }
    }

    /// Read one lineage's belief rather than the trunk's (§15.3, D-220).
    ///
    /// When this shipped, the only way to put a second lineage into a database
    /// was raw SQL. The read went first because it is the half that had to be
    /// measured (D-219), and because a write that creates something unreadable
    /// is the worse order to ship the two halves in. `fork()` arrived at
    /// **0.14.7** — this comment said 0.14.5 until 0.14.9, which is a stale
    /// prediction rather than a record, and the kind D-223 and D-224 were both
    /// found by reading. [`BranchView::traversal`](crate::BranchView::traversal)
    /// seeds this from a lineage the caller already holds.
    pub fn on_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
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
    /// see [`Self::as_of_valid`].
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
    /// **The refusal is scoped to the instants the archive actually took**
    /// (0.15.4, W14.2, D-246). The newest row per entity is never archivable, so
    /// an instant at or after the newest stamp still in the log — `now`
    /// included — folds completely and is answered. Through 0.15.3 the guard
    /// discarded the instant and refused everything on any archived database.
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
    /// # This function cannot ask the database, and the shape depends on it
    ///
    /// It emits the resolved form when a branch is named and the trunk form
    /// when one is not, which is the shape this
    /// builder's own configuration implies. That is exact on every database
    /// holding a single lineage — which is every database this crate has
    /// written — and it is *not* exact on a forked one, where an unbranched
    /// traversal reads the ancestor's and the descendant's rows alike.
    ///
    /// The execution paths do not have that gap: [`Self::execute_ids`],
    /// [`Self::execute`] and `Database::load_subgraph_with` each ask
    /// `graph::lineage::lineage_shape` and pass the answer to the
    /// shape-taking form of this method. This method stays for inspecting and
    /// explaining the query — which is what its callers in `tests/` do — and
    /// says so rather than quietly returning the shape that is usually right.
    pub fn build_sql(&self) -> String {
        self.build_sql_with(self.implied_shape())
    }

    /// The shape [`Self::build_sql`] assumes when nobody has asked the database.
    pub(crate) fn implied_shape(&self) -> LineageShape {
        if self.branch.is_some() {
            LineageShape::Resolved
        } else {
            LineageShape::Trunk
        }
    }

    /// [`Self::build_sql`] against a shape the caller has already established.
    pub(crate) fn build_sql_with(&self, shape: LineageShape) -> String {
        format!(
            "{}{}",
            self.walk_cte(shape),
            r#"
SELECT DISTINCT w.node_id
FROM walk w JOIN concepts c ON c.id = w.node_id
WHERE c.retired = 0
ORDER BY w.node_id;
            "#
        )
    }

    /// Where the reading branch binds, when the shape has one: `?5`.
    ///
    /// A [`LineageShape::Trunk`] read emits no `lineage` CTE and binds nothing
    /// here, so everything after it moves back by one. That is why the shape has
    /// to reach every method that lays out a placeholder rather than only the
    /// one that emits the CTE.
    pub(crate) const BRANCH_SLOT: usize = 5;

    /// Where the transaction-time instant binds, when the traversal has one.
    pub(crate) fn recorded_slot(shape: LineageShape) -> usize {
        Self::BRANCH_SLOT + usize::from(shape.binds_branch())
    }

    /// Where edge types start binding (0.13.2, W7.1; lineage slot 0.14.4).
    ///
    /// `?1..?4` are start, depth, the valid instant and `min_weight`. A
    /// resolved read binds its branch next; a traversal with
    /// [`Self::as_of_recorded`] set binds that next again; the variadic edge
    /// types follow whatever is there.
    ///
    /// **This exists so the offset is computed once rather than agreed twice.**
    /// [`Self::bind_params`] and [`Self::edge_filter_sql`] are the only two
    /// places that care, they must agree exactly, and the previous arrangement —
    /// a hard-coded `5` in one file and a comment in the other saying both call
    /// sites push in the same order — is the shape D-030 and D-035 are about.
    /// The lineage slot is the second thing to shift this layout and it shifted
    /// it in one place.
    pub(crate) fn edge_type_base(&self, shape: LineageShape) -> usize {
        Self::recorded_slot(shape) + usize::from(self.as_of_recorded.is_some())
    }

    /// The `AND l.edge_type IN (…)` fragment, or empty when unfiltered.
    ///
    /// Placeholders start at [`Self::edge_type_base`]. Bound, never spliced: an
    /// edge type is caller data, and the crate's only validation of one runs on
    /// the *write* path (D-039), so a traversal never passes through it.
    pub(crate) fn edge_filter_sql(&self, shape: LineageShape) -> String {
        if self.edge_types.is_empty() {
            String::new()
        } else {
            let base = self.edge_type_base(shape);
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
    pub(crate) fn bind_params(&self, now_ts: &str, shape: LineageShape) -> Vec<libsql::Value> {
        let mut params: Vec<libsql::Value> = vec![
            self.start_node.as_str().into(),
            (self.max_depth as i64).into(),
            self.valid_instant(now_ts).into(),
            self.min_weight.into(),
        ];
        // Pushed only when the emitted SQL names `BRANCH_SLOT`, which is every
        // shape but `Trunk`. An unbranched traversal on a forked database still
        // reaches this arm — `lineage_shape` answers for the database, not for
        // the builder — and reads `main`'s own lineage, which is the trunk's
        // belief and not the union of everything stored.
        if shape.binds_branch() {
            params.push(self.branch.as_deref().unwrap_or(ddl::MAIN_BRANCH).into());
        }
        if let Some(recorded) = self.as_of_recorded.as_deref() {
            params.push(recorded.into());
        }
        params.extend(self.edge_types.iter().map(|t| t.as_str().into()));
        params
    }

    /// What this traversal has decided about its lineage read, for
    /// [`lower`] to spell (0.15.1, W13.1).
    ///
    /// The builder owns the placeholder layout — [`Self::BRANCH_SLOT`] and
    /// [`Self::recorded_slot`] — and the lowering owns the SQL; this is the
    /// seam between them. `recorded_slot` is `Some` exactly when
    /// [`Self::as_of_recorded`] is, and the layout it names is the one
    /// [`Self::bind_params`] fills.
    pub(crate) fn resolution(&self, shape: LineageShape) -> Resolution<'static> {
        Resolution {
            shape,
            branch_slot: Self::BRANCH_SLOT,
            recorded_slot: self
                .as_of_recorded
                .as_ref()
                .map(|_| Self::recorded_slot(shape)),
            tag: "",
        }
    }

    /// The relation the walk and the projections read edges from.
    ///
    /// Under [`LineageShape::Resolved`] that is always `visible`, which holds
    /// one row per edge key from the nearest lineage that has one *and was
    /// entitled to be seen*; the walk and the projection do not need to know
    /// which relation it reduced, nor that the reduction had two arms
    /// (D-223). Under `Trunk` it is that relation directly: `links_current`
    /// under current belief, the `links_at_tx` fold otherwise. See [`lower`]
    /// for why the two shapes do not pick from the same pair.
    pub(crate) fn link_source(&self, shape: LineageShape) -> String {
        lower(&self.resolution(shape)).source
    }

    /// The lineage predicate the walk and the projections append to their
    /// own `WHERE`, or empty (0.15.2, D-244).
    ///
    /// Non-empty only under [`LineageShape::TrunkOnForked`] under current
    /// belief; see `Lowered::filter`. Spliced in both places the edge-type
    /// filter is, and for the same reason (D-073): a projection that skipped
    /// it would populate the trunk's subgraph with every lineage's edges
    /// between the nodes the trunk reached.
    pub(crate) fn lineage_filter_sql(&self, shape: LineageShape) -> String {
        lower(&self.resolution(shape)).filter
    }

    /// Refuse a transaction-time instant the hot log can no longer answer for.
    ///
    /// See [`Self::as_of_recorded`]. It only runs on the folded path, so the
    /// ordinary traversal pays nothing.
    ///
    /// **Two aggregates since 0.15.4, where it was one** — intactness over
    /// `seq_id`, then one stamp over `recorded_at`, both served as one-row index
    /// scans. The second is the price of consulting the instant rather than
    /// discarding it, and it is paid on every recorded-time read including the
    /// unarchived ones. It could be skipped on an intact log, where the verdict
    /// is *answerable* whatever the instant — and is not, because that puts "an
    /// intact log always answers" in a second place, which is the failure class
    /// this whole area keeps producing (D-035). The fold this guards is a window
    /// function over the bounded slice of `transaction_log`; an index scan of one
    /// row does not register against it.
    pub(crate) async fn check_recorded_reach(&self, conn: &libsql::Connection) -> Result<()> {
        let Some(ts) = self.as_of_recorded.as_deref() else {
            return Ok(());
        };
        if crate::temporal::replay::hot_log_answers_for(conn, ts).await? {
            return Ok(());
        }
        Err(crate::error::DbError::RecordedInstantUnreachable { ts: ts.to_string() })
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
    /// **The recursion is one copy across both lineage shapes too (0.14.4).**
    /// `shape` changes what the prelude holds and what `{source}` names; the
    /// walk itself is the same text either way, because
    /// `visible` exposes the columns `links_current` does. A second copy
    /// of the recursion for the resolved read would have been the T0.1 defect
    /// re-introduced by a feature rather than inherited from one.
    ///
    /// **And the prelude is one copy across the three readers (0.15.1).**
    /// What goes before `walk` is [`lower`]'s output, which
    /// `query_as_of_edges_on` and `diff_sql` splice too; this method chooses
    /// nothing about the lineage read beyond where its placeholders sit.
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
    pub(crate) fn walk_cte(&self, shape: LineageShape) -> String {
        let edge_filter = self.edge_filter_sql(shape);
        // The prelude and the source come from one lowering, shared with
        // `query_as_of_edges_on` and `diff_sql` (0.15.1, W13.1). The walk
        // splices what it is handed and knows nothing about what it holds,
        // which is the point: a shape that lands in `graph::plan` lands here.
        let lowered = lower(&self.resolution(shape));
        let source = &lowered.source;
        let lineage_filter = &lowered.filter;
        let prelude = lowered.prelude();

        format!(
            r#"
WITH RECURSIVE {prelude}walk(node_id, depth) AS (
    SELECT ?1, 0
    UNION
    SELECT l.target_id, w.depth + 1
    FROM walk w
    JOIN {source} l ON l.source_id = w.node_id
    WHERE w.depth < ?2
      AND l.valid_from <= ?3 AND ?3 < l.valid_to
      AND l.weight >= ?4{lineage_filter}
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
    ///
    /// [`DbError::UnknownBranch`](crate::DbError::UnknownBranch), naming the
    /// branch, when [`Self::on_branch`] names a lineage that is not registered
    /// (0.14.4; `NotFound` until 0.14.7).
    pub async fn execute_ids(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<String>> {
        self.check_recorded_reach(conn).await?;
        // The database decides the shape, not the builder: an unbranched
        // traversal on a forked ledger must still resolve, or it reads every
        // lineage's rows at once. See `build_sql` for why the pure function
        // cannot answer this and does not pretend to.
        let shape = lineage_shape(conn, self.branch.as_deref()).await?;
        let sql = self.build_sql_with(shape);
        let params = self.bind_params(now_ts, shape);

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
        // Either instant makes the question live, and the error names *which*
        // (0.13.10, W7.7, D-183). This was an `.or()` picking valid time first,
        // which answered the caller with an axis they might not have asked
        // about and dropped the other one when they had asked about both.
        let instants =
            StatedInstants::new(self.as_of_valid.as_deref(), self.as_of_recorded.as_deref());
        match (instants, self.attribute_mode) {
            (Some(instants), None) => {
                Err(crate::error::DbError::AttributeModeUnstated { instants })
            }
            (_, Some(mode)) => Ok(mode),
            (None, None) => Ok(AttributeMode::Current),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DbError;
    use crate::graph::lineage::ancestry_cte;

    const TUE: &str = "2026-01-06T00:00:00.000000Z";

    /// The only combination that is a question, and it is now asked.
    #[test]
    fn as_of_without_a_stated_mode_is_an_error() {
        let err = TraversalBuilder::new("a")
            .as_of_valid(TUE)
            .resolved_mode()
            .expect_err("past topology plus present text must not be a default");

        match &err {
            DbError::AttributeModeUnstated { instants } => {
                assert_eq!(instants.valid(), Some(TUE));
                assert_eq!(instants.recorded(), None, "no belief instant was set");
            }
            other => panic!("got {other:?}"),
        }

        // And the message has to be actionable: a caller who reads only this
        // should know which axis they asked about and which two calls resolve
        // it. `as_of(…)` named a method that has not existed since 0.12.17.
        let text = err.to_string();
        assert!(
            text.contains(&format!("as_of_valid({TUE})")),
            "the message must name the call the caller made: {text}"
        );
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
        assert!(
            matches!(err, DbError::AttributeModeUnstated { .. }),
            "{err:?}"
        );
        // The axis reaches the caller. Until 0.13.10 this said `as_of(…)`,
        // which is the valid-time method's old name and not what was called.
        let text = err.to_string();
        assert!(text.contains(&format!("as_of_recorded({TUE})")), "{text}");
        assert!(!text.contains("as_of("), "no dead method name: {text}");
    }

    /// Both axes set is the bitemporal cell, and dropping half of it was the
    /// second half of the defect: the `.or()` reported valid time and said
    /// nothing about the belief instant the caller had also stated.
    #[test]
    fn both_instants_are_reported_when_both_were_stated() {
        const WED: &str = "2026-01-07T00:00:00.000000Z";
        let err = TraversalBuilder::new("a")
            .as_of_valid(TUE)
            .as_of_recorded(WED)
            .resolved_mode()
            .expect_err("the cell needs a stated mode as much as either axis");

        match &err {
            DbError::AttributeModeUnstated { instants } => {
                assert_eq!(instants.valid(), Some(TUE));
                assert_eq!(instants.recorded(), Some(WED));
            }
            other => panic!("got {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains(TUE) && text.contains(WED), "{text}");
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
            TraversalBuilder::new("a")
                .as_of_valid(TUE)
                .valid_instant(now),
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
    /// produce the same layout, and both the recorded instant and the lineage
    /// slot shift it. A test that counts is cheaper than the bug, which is an
    /// edge type silently compared against a timestamp — or, since 0.14.4, a
    /// branch id compared against one.
    #[test]
    fn the_recorded_instant_shifts_the_edge_type_placeholders() {
        let now = "2026-06-01T00:00:00.000000Z";
        let trunk = LineageShape::Trunk;

        let plain = TraversalBuilder::new("a").edge_types(vec!["CITES".into()]);
        assert_eq!(plain.edge_type_base(trunk), 5);
        assert!(
            plain.edge_filter_sql(trunk).contains("?5"),
            "{}",
            plain.edge_filter_sql(trunk)
        );
        assert_eq!(plain.bind_params(now, trunk).len(), 5);

        let folded = plain.clone().as_of_recorded(TUE);
        assert_eq!(folded.edge_type_base(trunk), 6);
        assert!(
            folded.edge_filter_sql(trunk).contains("?6"),
            "{}",
            folded.edge_filter_sql(trunk)
        );
        assert_eq!(folded.bind_params(now, trunk).len(), 6);
    }

    /// The lineage slot shifts everything after it, in both functions (0.14.4).
    ///
    /// This is the same property as the test above and it is written twice on
    /// purpose: the layout now has two independent shifts, and a test that only
    /// varied one of them would pass on a `recorded_slot` that ignored the shape
    /// entirely.
    #[test]
    fn the_lineage_slot_shifts_everything_after_it() {
        let now = "2026-06-01T00:00:00.000000Z";
        let (trunk, resolved) = (LineageShape::Trunk, LineageShape::Resolved);

        let plain = TraversalBuilder::new("a").edge_types(vec!["CITES".into()]);
        assert_eq!(plain.edge_type_base(resolved), 6);
        assert!(plain.edge_filter_sql(resolved).contains("?6"));
        assert_eq!(plain.bind_params(now, resolved).len(), 6);

        let folded = plain.clone().as_of_recorded(TUE);
        assert_eq!(folded.edge_type_base(resolved), 7);
        assert!(folded.edge_filter_sql(resolved).contains("?7"));
        assert_eq!(folded.bind_params(now, resolved).len(), 7);

        // The branch lands in the slot the CTE reads it from, and it is the
        // *builder's* branch — not a positional accident that happens to hold a
        // string. `?5` is `BRANCH_SLOT`; `?6` is the recorded instant.
        let named = folded.clone().on_branch("b9");
        let params = named.bind_params(now, resolved);
        assert_eq!(
            params[TraversalBuilder::BRANCH_SLOT - 1],
            libsql::Value::from("b9")
        );
        assert!(ancestry_cte(TraversalBuilder::BRANCH_SLOT, "").contains("?5"));
        assert!(named.walk_cte(resolved).contains("recorded_at <= ?6"));

        // And an unnamed traversal that still has to resolve reads the trunk's
        // own lineage rather than the union of every lineage stored.
        assert_eq!(
            folded.bind_params(now, resolved)[TraversalBuilder::BRANCH_SLOT - 1],
            libsql::Value::from(ddl::MAIN_BRANCH)
        );

        // Nothing is bound for a slot the SQL never names.
        assert!(!folded.walk_cte(trunk).contains("lineage"));
    }

    /// The fold replaces the projection, and only when it is asked for.
    #[test]
    fn the_link_source_follows_the_recorded_instant() {
        let trunk = LineageShape::Trunk;

        let plain = TraversalBuilder::new("a");
        assert_eq!(plain.link_source(trunk), "links_current");
        assert!(!plain.walk_cte(trunk).contains("transaction_log"));

        let folded = TraversalBuilder::new("a").as_of_recorded(TUE);
        assert_eq!(folded.link_source(trunk), "links_at_tx");
        let sql = folded.walk_cte(trunk);
        assert!(sql.contains("links_at_tx"), "{sql}");
        assert!(sql.contains("recorded_at <= ?5"), "{sql}");
        assert!(
            sql.contains("table_name = 'links'"),
            "the partition is only sound with the discriminator filtered: {sql}"
        );
    }

    /// The fold partitions by lineage, and the resolution can therefore see it.
    ///
    /// The defect this pins is not that the SQL was ugly: `PARTITION BY
    /// entity_id` put an ancestor's assertion and a descendant's correction of
    /// it into one group and kept the higher `seq_id`, so a transaction-time
    /// traversal on a forked ledger lost one of the two before the resolution
    /// ever ran. D-216 fixed the same shape in `replay.rs` one release earlier
    /// and this fold was not in that sweep.
    #[test]
    fn the_folded_source_partitions_by_lineage() {
        let folded = TraversalBuilder::new("a").as_of_recorded(TUE);
        let sql = folded.walk_cte(LineageShape::Resolved);

        assert!(
            sql.contains("PARTITION BY transaction_log.entity_id, transaction_log.branch_id"),
            "two lineages' assertions collapse to one without this: {sql}"
        );
        // Qualified by table name rather than by an alias, and that is a plan
        // decision rather than a style one: 0.14.6's cutoff join brings a
        // second `branch_id` into scope so the columns must be qualified, and
        // `EXPLAIN QUERY PLAN` prints whatever the FROM clause named the table.
        // An alias would rewrite every plan guard that names `transaction_log`
        // — including the Trunk-shape seek assertion in `bitemporal_plan_tests`,
        // which is about a query this release does not otherwise touch.
        assert!(
            sql.contains("FROM transaction_log\n"),
            "the fold's table must keep its own name in the plan: {sql}"
        );
        // Carried out of the fold as well as partitioned on, because it is the
        // column the ancestry joins against.
        assert!(
            sql.contains("valid_to, weight, branch_id) AS MATERIALIZED ("),
            "the fold must expose what `visible` joins on: {sql}"
        );
        assert!(
            sql.contains("JOIN lineage g ON g.branch_id = l.branch_id"),
            "{sql}"
        );
    }

    /// The resolution is one row per edge *key*, and the walk reads only that.
    #[test]
    fn the_resolved_shape_puts_the_resolution_between_the_walk_and_the_rows() {
        let walk = TraversalBuilder::new("a");

        let resolved = walk.walk_cte(LineageShape::Resolved);
        assert_eq!(walk.link_source(LineageShape::Resolved), "visible");
        assert!(resolved.contains("JOIN visible l ON l.source_id = w.node_id"));
        assert!(
            resolved.contains("PARTITION BY l.source_id, l.target_id, l.edge_type, l.valid_from"),
            "the partition is the edge key, not the edge: {resolved}"
        );

        // And the trunk shape is byte-for-byte what shipped before 0.14.4: no
        // ancestry, no window function, the walk reading the table directly.
        let trunk = walk.walk_cte(LineageShape::Trunk);
        assert!(!trunk.contains("lineage"), "{trunk}");
        assert!(!trunk.contains("ROW_NUMBER"), "{trunk}");
        assert!(trunk.contains("JOIN links_current l ON l.source_id = w.node_id"));
    }

    /// `build_sql` answers for the configuration; only a connection knows more.
    #[test]
    fn the_pure_builder_takes_the_shape_its_configuration_implies() {
        assert_eq!(
            TraversalBuilder::new("a").implied_shape(),
            LineageShape::Trunk
        );
        assert_eq!(
            TraversalBuilder::new("a").on_branch("b9").implied_shape(),
            LineageShape::Resolved
        );
        assert!(TraversalBuilder::new("a")
            .on_branch("b9")
            .build_sql()
            .contains("lineage"));
        assert!(!TraversalBuilder::new("a").build_sql().contains("lineage"));
    }

    /// The trunk on a forked ledger binds its name and filters on it, and
    /// resolves nothing (0.15.2, D-244).
    #[test]
    fn the_forked_trunk_walk_is_the_trunk_walk_plus_one_predicate() {
        let shape = LineageShape::TrunkOnForked;
        let walk = TraversalBuilder::new("a");
        let sql = walk.walk_cte(shape);
        assert!(sql.contains("JOIN links_current l ON l.source_id = w.node_id"));
        assert!(sql.contains("AND l.weight >= ?4 AND +l.branch_id = ?5\n"));
        assert!(!sql.contains("lineage"), "a root resolves nothing: {sql}");
        assert!(!sql.contains("ROW_NUMBER"), "{sql}");
        assert_eq!(walk.link_source(shape), "links_current");
        assert_eq!(walk.lineage_filter_sql(shape), " AND +l.branch_id = ?5");

        // And the layout after the branch is the resolved layout: the branch
        // is bound, so the recorded instant and the edge types move by one.
        assert_eq!(TraversalBuilder::recorded_slot(shape), 6);
        assert_eq!(walk.edge_type_base(shape), 6);
        let params = walk.bind_params(TUE, shape);
        assert_eq!(params.len(), 5, "start, depth, valid, weight, branch");
        assert_eq!(
            params[4],
            libsql::Value::from(crate::schema::ddl::MAIN_BRANCH),
            "an unbranched traversal on the forked trunk reads main"
        );

        let folded = TraversalBuilder::new("a").as_of_recorded(TUE);
        let sql = folded.walk_cte(shape);
        assert!(sql.contains("JOIN links_at_tx l ON l.source_id = w.node_id"));
        assert!(sql.contains("AND +transaction_log.branch_id = ?5"));
        assert!(sql.contains("recorded_at <= ?6"));
        assert!(
            !sql.contains("l.branch_id"),
            "the fold already narrowed: {sql}"
        );
        assert!(!sql.contains("lineage"), "{sql}");
        assert_eq!(folded.bind_params(TUE, shape).len(), 6);
        assert_eq!(folded.edge_type_base(shape), 7);
    }

    /// The plan the third shape gets, pinned where the walk's other plans are
    /// pinned: it still seeks `idx_lc_traversal_cover` on `source_id`, and no
    /// materialised lineage relation appears anywhere in it.
    #[tokio::test]
    async fn the_forked_trunk_walk_seeks_the_traversal_index_and_materialises_nothing() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('b', 'main', ?1, ?1)",
            libsql::params![TUE],
        )
        .await
        .unwrap();

        for (label, builder) in [
            ("unfiltered", TraversalBuilder::new("a").max_depth(3)),
            (
                "edge-typed",
                TraversalBuilder::new("a")
                    .max_depth(3)
                    .edge_types(vec!["CITES".into()]),
            ),
        ] {
            let shape = LineageShape::TrunkOnForked;
            let sql = format!("EXPLAIN QUERY PLAN {}", builder.build_sql_with(shape));
            let mut rows = conn
                .query(&sql, builder.bind_params(TUE, shape))
                .await
                .unwrap();
            let mut plan = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                plan.push(row.get::<String>(3).unwrap());
            }
            let text = plan.join("\n");
            assert!(
                plan.iter()
                    .any(|s| s.contains("SEARCH l USING") && s.contains("idx_lc_traversal_cover")),
                "{label}: the forked trunk's walk left the traversal index:\n{text}"
            );
            assert!(
                !text.contains("MATERIALIZE") && !text.contains("lineage"),
                "{label}: the forked trunk's walk resolves an ancestry it does not have:\n{text}"
            );
        }
    }

    /// **The fold is materialised once per query, not re-run once per walk
    /// row** (0.15.2, D-244).
    ///
    /// `links_at_tx` is referenced once, by the walk's recursive step, and
    /// SQLite's default for a single-reference CTE is a co-routine — which for
    /// a CTE joined *inside a recursive step* means the whole fold, window and
    /// all, runs again for every row the walk produces. Measured on 11,110
    /// trunk edges at depth 4: **10.6 s** as a co-routine, **59 ms**
    /// materialised. The resolved shape never showed it because its `visible`
    /// window forces materialisation on its own, which is how a 180× defect
    /// on the trunk's transaction-time read hid behind the branched read being
    /// the slower-looking one. Pinned on every shape that emits the fold; the
    /// two trunk shapes also keep the seek the transaction-time bound has
    /// always had (`bitemporal_plan_tests`).
    #[tokio::test]
    async fn the_fold_is_materialised_once_per_query_on_every_shape() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('b', 'main', ?1, ?1)",
            libsql::params![TUE],
        )
        .await
        .unwrap();
        let folded = TraversalBuilder::new("a").max_depth(3).as_of_recorded(TUE);
        for shape in [
            LineageShape::Trunk,
            LineageShape::TrunkOnForked,
            LineageShape::Resolved,
        ] {
            let sql = format!("EXPLAIN QUERY PLAN {}", folded.build_sql_with(shape));
            let mut rows = conn
                .query(&sql, folded.bind_params(TUE, shape))
                .await
                .unwrap();
            let mut plan = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                plan.push(row.get::<String>(3).unwrap());
            }
            let text = plan.join("\n");
            assert!(
                text.contains("MATERIALIZE links_at_tx"),
                "{shape:?}: the fold went back to being a co-routine inside the \
                 recursive step, which is the 180x defect:\n{text}"
            );
            // The resolved fold joins the ancestry, and the planner takes the
            // equality over the range: an automatic index on
            // `(table_name, branch_id)` built by scanning the log once per
            // query, then a seek per lineage row. That is the fold as it has
            // been since 0.10.0, measured at 121 ms against the trunk's 59 ms
            // on the same fixture, and it is not this release's to change —
            // the point pinned here is that no shape re-runs the fold per row.
            if shape != LineageShape::Resolved {
                assert!(
                    text.contains("SEARCH transaction_log USING INDEX idx_txlog_time"),
                    "{shape:?}: the transaction-time bound stopped seeking:\n{text}"
                );
            }
        }
    }
}
