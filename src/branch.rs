//! Lineage identity and its lifecycle (§15.2, §15.4).
//!
//! The read half of branching shipped first, at 0.14.2 through 0.14.6: the
//! ledger tables carry a `branch_id`, [`crate::graph::TraversalBuilder::on_branch`]
//! resolves along the ancestry, and 0.14.6 bounded that resolution by the fork
//! point ([D-223]). Nothing in that half could *create* a lineage — only raw
//! SQL could, which is why 0.14.6 could repair the read's semantics without
//! breaking anyone. This module is the other half: the name, and the one write
//! that registers it.
//!
//! # Why the read shipped first, and why that ordering is not an accident
//!
//! A write that creates something unreadable is the worse order. Had `fork()`
//! landed at 0.14.2, every branch created between then and 0.14.6 would have
//! been readable only through a query that silently absorbed its parent's later
//! writes — and the repair would have been a semantic break on stored data
//! rather than a correction to an unreachable path. That is [D-160] → [D-174]'s
//! ordering, applied a third time and recorded in [D-223].
//!
//! [D-160]: ../../docs/architecture/s13-decision-register.md#d-160
//! [D-174]: ../../docs/architecture/s13-decision-register.md#d-174
//! [D-223]: ../../docs/architecture/s13-decision-register.md#d-223

use crate::error::{DbError, Result};
use crate::schema::ddl;

/// Longest accepted lineage name.
///
/// A sanity bound rather than a schema limit — `branches.branch_id` is `TEXT`
/// and SQLite would take a megabyte. It is set well above every identifier
/// shape the motivating use case generates (a ULID is 26 characters, a
/// hyphenated UUID 36, a path-like `turn/17/alt/3` shorter still) and well
/// below anything that would make a `branches` listing unreadable.
pub const MAX_BRANCH_ID: usize = 128;

/// A validated lineage name.
///
/// # This is not [`ModelName`](crate::vector::ModelName)'s reason, and the rule
/// is deliberately different
///
/// `ModelName` exists because a model name is spliced into a table identifier
/// and SQLite cannot bind an identifier as a parameter, so the validation is
/// what makes the splice safe. **None of that applies here.** A `branch_id`
/// reaches SQL as a bound value at every one of its call sites; there is no
/// splice to protect. Copying `ModelName`'s `[a-z][a-z0-9_]*` rule would be
/// borrowing a justification that does not hold, and it would reject the two
/// name shapes the use case in §15.5 actually produces — a hyphenated UUID and
/// a path-like turn id.
///
/// What this type is for is narrower and has nothing to do with SQL:
///
/// * `branch_id` is a primary key that four ledger tables hold a foreign key
///   into, and `branches` is append-only under two unconditional triggers
///   ([`CREATE_BRANCHES_GUARD_UPDATE`](crate::schema::ddl::CREATE_BRANCHES_GUARD_UPDATE)).
///   A name is therefore written **once** and can never be corrected — not by
///   the crate, not by raw SQL. Validation has one chance, and this is it.
/// * A name that differs from the caller's intent by a trailing space is not a
///   typo, it is a **second lineage that reads as the first**. Every subsequent
///   `on_branch("release ")` resolves to a different ancestry than
///   `on_branch("release")`, both succeed, and neither reports anything. That
///   is the failure shape this wave keeps finding, and it is cheapest to refuse
///   at [D-034]'s boundary.
///
/// So the rule is: non-empty, at most [`MAX_BRANCH_ID`] bytes, no ASCII control
/// characters, and no leading or trailing whitespace. Refused rather than
/// trimmed, on §4.1's principle — a silent repair here becomes two lineages
/// sharing one intent later.
///
/// # `main` is constructible and not forkable
///
/// [`Database::branches`](crate::Database::branches) returns the trunk and
/// every read may name it, so `BranchId::new("main")` must succeed. What is
/// refused is *creating* it a second time, and that refusal belongs to
/// [`Database::fork`](crate::Database::fork) rather than to this type: the
/// trunk is a lineage like any other to a reader, and only a writer has cause
/// to care that it already exists.
///
/// [D-034]: ../../docs/architecture/s13-decision-register.md#d-034
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchId(String);

impl BranchId {
    /// Validate `raw` as a lineage name, or explain why it is not one.
    pub fn new(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref();
        let invalid = || DbError::InvalidBranchId(raw.to_string());

        if raw.is_empty() || raw.len() > MAX_BRANCH_ID {
            return Err(invalid());
        }
        if raw.chars().any(|c| c.is_control()) {
            return Err(invalid());
        }
        // `trim` and not `trim_ascii`: a non-breaking space is invisible in
        // every terminal this name will be read in, which is the whole argument
        // above for refusing the ASCII one.
        if raw.trim() != raw {
            return Err(invalid());
        }
        Ok(Self(raw.to_string()))
    }

    /// The trunk, which every database has from its first migration.
    pub fn main() -> Self {
        Self(ddl::MAIN_BRANCH.to_string())
    }

    /// Adopt a name already stored in `branches`, without revalidating it.
    ///
    /// Infallible on purpose. `branches` may hold rows this type never saw:
    /// written by raw SQL, or by a build older than 0.14.7, both of which the
    /// schema permits and neither of which the append-only guards allow anyone
    /// to repair. A listing that returned `Err` on one such row would be
    /// unusable for the one thing it is for — finding out what is in there —
    /// and would report the *listing* as broken rather than the row.
    pub(crate) fn from_stored(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The name, as it is stored.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this names the trunk.
    pub fn is_main(&self) -> bool {
        self.0 == ddl::MAIN_BRANCH
    }
}

impl std::fmt::Display for BranchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for BranchId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// So a `BranchId` can be handed straight to the 0.14.4 read surface.
///
/// [`TraversalBuilder::on_branch`](crate::graph::TraversalBuilder::on_branch)
/// and [`query_as_of_edges_on`](crate::temporal::query_as_of_edges_on) take
/// `impl Into<String>`, and they shipped two releases before this type existed.
/// This impl is what makes `on_branch(id)` compile rather than
/// `on_branch(id.as_str())` — additive, and cheaper than widening four
/// signatures that are already public.
impl From<BranchId> for String {
    fn from(id: BranchId) -> Self {
        id.0
    }
}

/// One row of `branches`: a lineage, its parent, and where it was cut.
///
/// # `#[non_exhaustive]` costs nothing here, and that is a fact about direction
///
/// [`EdgeBelief`](crate::temporal::EdgeBelief) needed a constructor to go with
/// the attribute, because `save_snapshot` is public and *takes* one — without
/// `EdgeBelief::new` the attribute would not have made the next field additive,
/// it would have made a public function uncallable (0.14.5). This type only
/// ever travels outward: [`Database::branches`](crate::Database::branches)
/// returns it and nothing public accepts it. So the attribute buys the additive
/// field and takes nothing back, and no constructor is owed. §15.4's
/// abandonment arm is the field it is being kept open for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct Branch {
    /// The lineage's own name.
    pub id: BranchId,
    /// The lineage it was cut from, or `None` for the trunk.
    pub parent: Option<BranchId>,
    /// The transaction-time instant it was cut at, or `None` for the trunk.
    ///
    /// This is the visibility cutoff 0.14.6 reads: the branch sees its parent's
    /// history up to and including this instant, and nothing the parent records
    /// after it ([D-223]).
    ///
    /// [D-223]: ../../docs/architecture/s13-decision-register.md#d-223
    pub forked_at: Option<String>,
    /// When the row was written.
    ///
    /// A separate column from `forked_at` rather than a duplicate of it, and
    /// the two are equal for every branch this release can create. They are
    /// separate so that forking from a *past* instant is an additive change
    /// later: a historical fork has a `forked_at` behind its `created_at`, and
    /// the schema has always allowed it — `CHECK (forked_at <= created_at)`.
    pub created_at: String,
}

/// One lineage's handle on the ledger (§15.4, 0.14.9, [D-226]).
///
/// A `Database` plus a [`BranchId`], so a caller who forked writes and reads
/// through the fork instead of naming it at every call. Every operation here
/// exists on [`Database`](crate::Database) already and takes a lineage there —
/// **this type buys ergonomics and no capability**, which is what makes it the
/// last piece of §15.4's first bullet rather than a fifth release of it.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use macrame::graph::EdgeAssertion;
/// # use macrame::{Database, BranchId};
/// # async fn f(db: Arc<Database>) -> macrame::Result<()> {
/// let alt = db.fork(BranchId::new("turn/17/alt/1")?, BranchId::main()).await?;
/// let view = db.view(alt.id);
///
/// view.assert_edge(EdgeAssertion::new("a", "b", "CITES").valid_from(ts())).await?;
/// let seen = view.traversal("a").execute_ids(view.read_conn(), ts()).await?;
/// # Ok(())
/// # }
/// # fn ts() -> &'static str { "2020-01-01T00:00:00.000000Z" }
/// ```
///
/// # It holds an `Arc<Database>` and cannot close it
///
/// [`Database::close`](crate::Database::close) takes `self` by value, and an
/// `Arc` cannot give that up while any clone survives. So the borrow is
/// structural rather than a documented request: a caller who forks a view,
/// reads it and drops it is not one call away from stopping the actor everyone
/// else is using. That is why the view is a **separate type** over an
/// `Arc<Database>` and not a `Database` with a field added, and it is the same
/// argument [D-203] made when `Database: Clone` was declined — a handle that can
/// be cloned freely must not carry the right to end the thing it handles.
///
/// `Clone` is therefore free of that concern and is derived: the view owns no
/// lifecycle, so cloning it is cloning an `Arc` and a short string.
///
/// # What it does with an assertion that names a lineage
///
/// It stamps its own on one that names none, and **refuses** one that names a
/// different lineage with [`DbError::BranchMismatch`].
/// Stamping over is the shape a caller building through the view produces and
/// costs nothing; relabelling a write that already named somewhere else would
/// discard a belief rather than contradict it. See that variant for the failure
/// it catches.
///
/// [D-203]: ../../docs/architecture/s13-decision-register.md#d-203
/// [D-226]: ../../docs/architecture/s13-decision-register.md#d-226
#[derive(Clone)]
pub struct BranchView {
    db: std::sync::Arc<crate::Database>,
    branch: BranchId,
}

/// Hand-written because [`Database`](crate::Database) is not `Debug` — it owns
/// a connection, a channel and a clock, none of which prints usefully. What a
/// reader of this type wants is which lineage against which file, so that is
/// what it prints.
impl std::fmt::Debug for BranchView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchView")
            .field("branch", &self.branch)
            .field("path", &self.db.path())
            .finish()
    }
}

impl BranchView {
    /// Bind a lineage to a handle.
    ///
    /// Infallible and does no I/O: the name is already validated by
    /// [`BranchId`], and whether it is *registered* is a question every
    /// operation on this view asks for itself, answering
    /// [`DbError::UnknownBranch`] by name. A
    /// constructor that checked would buy one round trip's worth of earlier
    /// notice and cost the type its `const`-cheapness, and the check would be
    /// stale by the next call anyway — `branches` is append-only, but the view
    /// outlives the answer.
    pub fn new(db: std::sync::Arc<crate::Database>, branch: BranchId) -> Self {
        Self { db, branch }
    }

    /// The lineage this view reads and writes.
    pub fn id(&self) -> &BranchId {
        &self.branch
    }

    /// The handle underneath, for the operations that are not lineage-scoped.
    ///
    /// `archive`, `checkpoint`, `verify` and the rest are properties of the
    /// file rather than of a lineage, so they are reached through here rather
    /// than duplicated onto a view that would answer the same thing for every
    /// branch.
    pub fn database(&self) -> &std::sync::Arc<crate::Database> {
        &self.db
    }

    /// The read connection, so a [`TraversalBuilder`] from
    /// [`Self::traversal`] can be executed without reaching for the handle.
    ///
    /// [`TraversalBuilder`]: crate::graph::TraversalBuilder
    pub fn read_conn(&self) -> &libsql::Connection {
        self.db.read_conn()
    }

    /// A traversal already pointed at this lineage.
    ///
    /// The one read this type needs to wrap, because everything downstream of
    /// it — `execute_ids`, `execute`,
    /// [`load_subgraph_with`](crate::Database::load_subgraph_with) — takes the
    /// lineage *from the builder*. Seeding it here is therefore the whole of
    /// the read side rather than a first method of several.
    pub fn traversal(&self, start_node: impl Into<String>) -> crate::graph::TraversalBuilder {
        crate::graph::TraversalBuilder::new(start_node).on_branch(self.branch.as_str())
    }

    /// [`Database::load_subgraph`](crate::Database::load_subgraph) on this
    /// lineage.
    ///
    /// The sugar form has no builder to carry the branch, so it is wrapped;
    /// `load_subgraph_with` is not, because a builder from [`Self::traversal`]
    /// already carries it.
    pub async fn load_subgraph(
        &self,
        start_node: &str,
        max_hops: u32,
        now_ts: &str,
        byte_budget: usize,
    ) -> Result<crate::graph::Subgraph> {
        self.db
            .load_subgraph_with(
                &self
                    .traversal(start_node)
                    .max_depth(max_hops as usize)
                    .min_weight(f64::NEG_INFINITY),
                now_ts,
                byte_budget,
            )
            .await
    }

    /// Every edge this lineage believes in at `ts`.
    #[allow(clippy::type_complexity)]
    pub async fn query_as_of_edges(
        &self,
        ts: &str,
    ) -> Result<Vec<(String, String, String, String, String)>> {
        crate::temporal::query_as_of_edges_on(self.read_conn(), ts, Some(self.branch.as_str()))
            .await
    }

    /// Assert an edge on this lineage.
    pub async fn assert_edge(&self, edge: crate::graph::EdgeAssertion) -> Result<()> {
        self.db.assert_edge(self.claim_edge(edge)?).await
    }

    /// Retire an edge on this lineage — [`Database::retire_edge_on`] without
    /// the argument.
    ///
    /// An inherited edge is retired by writing this lineage's **own** closed
    /// row at the ancestor's key; the parent's row is never touched.
    ///
    /// [`Database::retire_edge_on`]: crate::Database::retire_edge_on
    pub async fn retire_edge(
        &self,
        source: impl Into<String>,
        target: impl Into<String>,
        edge_type: impl Into<String>,
        valid_from: &str,
        valid_to: &str,
    ) -> Result<()> {
        self.db
            .retire_edge_on(
                source,
                target,
                edge_type,
                valid_from,
                valid_to,
                self.branch.clone(),
            )
            .await
    }

    /// Mint a concept on this lineage.
    ///
    /// A branch **inherits** its parent's concepts and may not restate one:
    /// `concepts` is keyed by identity, so that is
    /// [`DbError::CrossLineage`].
    pub async fn upsert_concept(&self, concept: crate::ConceptUpsert) -> Result<()> {
        self.db.upsert_concept(self.claim_concept(concept)?).await
    }

    /// [`Database::write_bulk_atomic`](crate::Database::write_bulk_atomic) with
    /// every edge on this lineage.
    pub async fn write_bulk_atomic(
        &self,
        edges: Vec<crate::graph::EdgeAssertion>,
    ) -> Result<usize> {
        self.db.write_bulk_atomic(self.claim_edges(edges)?).await
    }

    /// [`Database::bulk_import`](crate::Database::bulk_import) with every edge
    /// on this lineage.
    pub async fn bulk_import(
        &self,
        edges: Vec<crate::graph::EdgeAssertion>,
    ) -> crate::error::BulkResult<usize> {
        let edges = self
            .claim_edges(edges)
            .map_err(|cause| crate::error::BulkInterrupted { written: 0, cause })?;
        self.db.bulk_import(edges).await
    }

    /// [`Database::write_concepts`](crate::Database::write_concepts) with every
    /// concept on this lineage.
    pub async fn write_concepts(
        &self,
        concepts: Vec<crate::ConceptUpsert>,
    ) -> crate::error::BulkResult<usize> {
        let concepts = concepts
            .into_iter()
            .map(|c| self.claim_concept(c))
            .collect::<Result<Vec<_>>>()
            .map_err(|cause| crate::error::BulkInterrupted { written: 0, cause })?;
        self.db.write_concepts(concepts).await
    }

    /// Refuse a foreign lineage, stamp an unnamed one.
    fn claim_edge(&self, edge: crate::graph::EdgeAssertion) -> Result<crate::graph::EdgeAssertion> {
        match &edge.branch {
            Some(named) if named != &self.branch => Err(DbError::BranchMismatch {
                view: self.branch.as_str().to_string(),
                named: named.as_str().to_string(),
            }),
            Some(_) => Ok(edge),
            None => Ok(edge.on_branch(self.branch.clone())),
        }
    }

    /// [`Self::claim_edge`] for a batch, refusing on the **first** foreign
    /// lineage rather than reporting all of them.
    ///
    /// A batch that names two lineages is a caller error about the batch, not a
    /// list of independent mistakes, and the first one names the confusion as
    /// well as the tenth would.
    fn claim_edges(
        &self,
        edges: Vec<crate::graph::EdgeAssertion>,
    ) -> Result<Vec<crate::graph::EdgeAssertion>> {
        edges.into_iter().map(|e| self.claim_edge(e)).collect()
    }

    /// [`Self::claim_edge`] for a concept.
    fn claim_concept(&self, concept: crate::ConceptUpsert) -> Result<crate::ConceptUpsert> {
        match &concept.branch {
            Some(named) if named != &self.branch => Err(DbError::BranchMismatch {
                view: self.branch.as_str().to_string(),
                named: named.as_str().to_string(),
            }),
            Some(_) => Ok(concept),
            None => Ok(concept.on_branch(self.branch.clone())),
        }
    }
}

/// Register a lineage, refusing the three things a `CHECK` cannot (§15.2).
///
/// Runs inside the write actor, so the three reads and the insert are one turn
/// against one connection and nothing can register a colliding name between the
/// check and the write.
///
/// # What the schema already refuses, and what is left over
///
/// `branches` carries a foreign key on `parent_id`, a primary key on
/// `branch_id`, and two row-local `CHECK`s. So a missing parent and a duplicate
/// name would both fail at the engine anyway — they are checked here to be
/// *named*, because `classify` would otherwise surface a duplicate fork as a
/// constraint violation naming a column, and the caller's question was about a
/// branch.
///
/// # The third refusal, and the invariant that turned out not to be checkable
///
/// [`CREATE_BRANCHES_TABLE`]'s comment left this to `fork()`: *"the fork point
/// is at or after the parent's creation" is not [row-local], and a `CHECK`
/// cannot see another row. The cross-row half is `fork()`'s to enforce at
/// D-034's boundary.* Enforcing it as written **refuses every fork on every
/// injected-clock database in the crate**, and the reason is not the clock the
/// test chose. `seed_root_branch` stamps `main.created_at` from
/// `SystemTime::now()` inside `migrations::run`, which runs *before* the
/// database's clock is resolved — the floor that clock is raised against is
/// read from tables the migration has to create first, so the order cannot
/// simply be swapped. **`branches.created_at` is therefore not on the ledger's
/// timeline**, and on a [`FakeClock`](crate::util::FakeClock) database it sits
/// years in the future of every row.
///
/// What *is* comparable is `forked_at`: every one of them is issued by this
/// function from the same clock as every `recorded_at`. So the check is against
/// the parent's fork point, and the trunk — whose `forked_at` is `NULL` because
/// it was cut from nothing — constrains nothing, which is right: as far as any
/// ledger row is concerned the trunk has always existed.
///
/// The narrower rule is also the one worth having. It makes fork points
/// **non-decreasing down any root path**, which is precisely the property
/// [`ancestry_cte`](crate::graph::lineage) clamps for defensively. The clamp
/// stays — `branches` accepts raw-SQL rows this function never saw — but for
/// rows the crate wrote it is now a belt beside braces rather than the only
/// thing holding the shape.
///
/// What it refuses is a fork point earlier than its parent's. That branch would
/// inherit **nothing whatever from the parent it names** — every row the parent
/// wrote is after the parent's own fork point, so all of them fall past the
/// child's cutoff — leaving a lineage whose `parent_id` says one thing and
/// whose visible history says another. A sibling wearing a child's parent
/// pointer, and silent about it.
///
/// [`CREATE_BRANCHES_TABLE`]: crate::schema::ddl::CREATE_BRANCHES_TABLE
pub(crate) async fn fork(
    conn: &libsql::Connection,
    name: &BranchId,
    parent: &BranchId,
    stamp: &str,
) -> Result<Branch> {
    // `EXISTS` separately from `forked_at`, because the trunk's `forked_at` is
    // legitimately NULL and a single nullable column cannot tell "no such
    // branch" apart from "the root".
    let mut rows = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM branches WHERE branch_id = ?1), \
                    (SELECT COUNT(*) FROM branches WHERE branch_id = ?2), \
                    (SELECT forked_at FROM branches WHERE branch_id = ?2)",
            libsql::params![name.as_str(), parent.as_str()],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| {
        // A three-aggregate SELECT always returns a row; if it did not, the
        // honest report is that the parent could not be established.
        DbError::UnknownBranch(parent.as_str().to_string())
    })?;
    let taken: i64 = row.get(0)?;
    let parent_exists: i64 = row.get(1)?;
    let parent_forked_at: Option<String> = row.get(2)?;

    if taken > 0 {
        return Err(DbError::BranchExists(name.as_str().to_string()));
    }
    if parent_exists == 0 {
        return Err(DbError::UnknownBranch(parent.as_str().to_string()));
    }
    if let Some(parent_forked_at) = parent_forked_at {
        if stamp < parent_forked_at.as_str() {
            return Err(DbError::ForkPrecedesParent {
                branch: name.as_str().to_string(),
                parent: parent.as_str().to_string(),
                forked_at: stamp.to_string(),
                parent_forked_at,
            });
        }
    }

    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES (?1, ?2, ?3, ?3)",
        libsql::params![name.as_str(), parent.as_str(), stamp],
    )
    .await?;

    Ok(Branch {
        id: name.clone(),
        parent: Some(parent.clone()),
        forked_at: Some(stamp.to_string()),
        created_at: stamp.to_string(),
    })
}

/// Every lineage the ledger knows about, trunk first, then by creation.
///
/// Read through the read connection rather than the actor: `branches` is
/// append-only, so a listing cannot be torn by a concurrent write in any way a
/// caller could act on — the worst a racing `fork` can do is not appear yet.
///
/// Ordered rather than left to the engine, and ordered by `created_at` rather
/// than by name, because the useful reading of this list is the shape of the
/// tree over time. The trunk is pinned first because it is the one row that is
/// always there and is nobody's child.
pub(crate) async fn list(conn: &libsql::Connection) -> Result<Vec<Branch>> {
    let mut rows = conn
        .query(
            "SELECT branch_id, parent_id, forked_at, created_at FROM branches \
             ORDER BY (parent_id IS NOT NULL), created_at, branch_id",
            (),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Branch {
            id: BranchId::from_stored(row.get::<String>(0)?),
            parent: row.get::<Option<String>>(1)?.map(BranchId::from_stored),
            forked_at: row.get(2)?,
            created_at: row.get(3)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_name_shapes_the_use_case_generates() {
        for ok in [
            "main",
            "b9",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",           // ULID
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301", // hyphenated UUID
            "turn/17/alt/3",                        // path-like turn id
            "explore Kant's second critique",       // a human sentence
        ] {
            assert_eq!(BranchId::new(ok).unwrap().as_str(), ok);
        }
    }

    /// The two that matter are the whitespace pair: each is a second lineage
    /// that reads as the first everywhere it is printed.
    #[test]
    fn refuses_what_cannot_be_corrected_later() {
        for bad in [
            "",
            " release", // leading space
            "release ", // trailing space
            "release\n",
            "rel\tease", // control character in the middle
            "rel\0ease",
        ] {
            assert!(
                BranchId::new(bad).is_err(),
                "{bad:?} was accepted, and `branches` is append-only"
            );
        }
        assert!(BranchId::new("x".repeat(MAX_BRANCH_ID)).is_ok());
        assert!(BranchId::new("x".repeat(MAX_BRANCH_ID + 1)).is_err());
    }

    /// `ModelName`'s rule is the one this type is most likely to be "fixed" to
    /// match. Pinned so the fix has to argue with a test.
    #[test]
    fn the_rule_is_not_model_names_rule() {
        for accepted_here_rejected_there in ["Release-1", "3f2504e0-4f89", "a.b"] {
            assert!(BranchId::new(accepted_here_rejected_there).is_ok());
            assert!(crate::vector::ModelName::new(accepted_here_rejected_there).is_err());
        }
    }

    #[test]
    fn the_trunk_knows_itself() {
        assert!(BranchId::main().is_main());
        assert!(BranchId::new("main").unwrap().is_main());
        assert!(!BranchId::new("mains").unwrap().is_main());
    }
}
