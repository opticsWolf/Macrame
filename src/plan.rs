//! What a read asks for, stated once (§16, F-34).
//!
//! Three qualifiers appear on every read surface in this crate — the lineage,
//! the valid-time instant, the transaction-time instant — and until 0.15.9
//! each surface spelled them itself. [`TraversalBuilder`] carries three
//! fields, `query_as_of_edges_on` takes two positional arguments and cannot
//! express the third at all, and the Python binding repeats the set as
//! keywords on five entry points. Nothing was wrong with any one of them; what
//! was wrong is that "read `exp` as it stood on Tuesday, under what we believed
//! in March" was a sentence the crate could not hold as a value, so it could
//! not be passed, stored, compared, or given a default.
//!
//! [`ReadPlan`] is that value. It is caller-facing and deliberately dumb: it
//! holds four `Option`s and knows no SQL. The lowering that turns a read into
//! CTEs lives in `graph::plan` and stays crate-private, which is why there are
//! two modules called `plan` and only one of them is a public path. The
//! division is the useful one — this module is *what was asked*, that one is
//! *how it is answered*, and a caller who never reads SQL should never meet
//! the second.
//!
//! # The fourth qualifier, and the promise 0.15.9 made about it
//!
//! `limit` was in [the 0.16.0 plan]'s sketch of this struct and was left out of
//! 0.15.9 on the grounds that a public knob that silently does nothing is the
//! one failure mode a plan value has that three loose arguments do not — a
//! caller can *see* an argument go unused at a call site and cannot see a field
//! go unread. `#[non_exhaustive]` made it additive on the day it meant
//! something, and 0.15.10 ([D-252]) is that day: it bounds the walk from
//! inside the recursive CTE, and every surface that takes a plan reads it.
//!
//! It is not a fourth *temporal* qualifier and it does not compose like one.
//! The other three narrow which rows are true; this one says how much of the
//! answer to pay for, so two reads under the same plan can differ. That is
//! stated on [`ReadPlan::limit()`] rather than smoothed over, and it is why
//! the walk reports whether the ceiling bit
//! ([`WalkOutcome`](crate::graph::WalkOutcome)) where it never had to report
//! on an instant.
//!
//! [the 0.16.0 plan]: ../../docs/Macrame%20Update%20Plan%20v0.16.0.md
//! [D-252]: ../../docs/architecture/s13-decision-register.md#d-252
//! [`TraversalBuilder`]: crate::graph::TraversalBuilder

use crate::branch::BranchId;
use crate::error::Result;
use crate::graph::lineage::lineage_shape;
use crate::graph::plan::{lower, Resolution};
use crate::temporal::EdgeBelief;

/// The lineage, the two instants, and the ceiling a read is taken under.
///
/// Every field is `None` by default and `None` means the same thing on all of
/// them: **the ordinary read**. No branch is the trunk, no valid instant is
/// now, no recorded instant is current belief, no limit is the whole answer. A
/// default [`ReadPlan`] and no plan at all are the same read, which is what
/// lets [`TraversalBuilder::plan`](crate::graph::TraversalBuilder::plan) be
/// additive over the setters it composes rather than another way to configure a
/// traversal.
///
/// # Why the branch is a [`BranchId`] and the instants are `String`
///
/// Not an oversight, and not symmetry withheld for its own sake. A branch name
/// is validated at construction — length, control characters, surrounding
/// whitespace — and [`BranchId`] is the type that has already asked those
/// questions, so taking a `String` here would move a refusal out of the
/// caller's `BranchId::new` and into somewhere inside a read. A timestamp has
/// no such type in this crate: the canonical form is enforced at the boundary
/// (`util::timestamps`) and carried as a string everywhere below it, and
/// inventing an instant newtype for one struct would give the crate two
/// answers to what a stamp is. So [`Self::on`] cannot fail and neither can
/// [`Self::valid_at`]; a malformed stamp is refused where every other stamp in
/// the crate is refused.
///
/// # Errors, when this is executed
///
/// A plan is inert and returns nothing. The refusals belong to the read that
/// takes one: [`DbError::UnknownBranch`](crate::DbError::UnknownBranch) for a
/// lineage that was never registered, and
/// [`DbError::RecordedInstantUnreachable`](crate::DbError::RecordedInstantUnreachable)
/// for a transaction-time instant the hot log no longer answers for
/// ([D-247](../../docs/architecture/s13-decision-register.md#d-247)).
///
/// ```no_run
/// use macrame::prelude::*;
///
/// # async fn f(db: &Database) -> macrame::Result<()> {
/// let plan = ReadPlan::new()
///     .on(BranchId::new("exp")?)
///     .valid_at("2026-01-06T00:00:00.000000Z")
///     .recorded_at("2026-03-01T00:00:00.000000Z");
///
/// // The same qualifiers, on a whole-ledger read and on a walk.
/// let edges = db.edges(plan.clone()).await?;
/// let reached = TraversalBuilder::new("a")
///     .plan(plan)
///     .execute_ids(db.read_conn(), "2026-06-01T00:00:00.000000Z")
///     .await?;
/// # let _ = (edges, reached);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadPlan {
    /// The lineage to read, or `None` for the trunk (§15.3, D-220).
    pub branch: Option<BranchId>,
    /// The **valid-time** instant: *what was true then*. `None` is now.
    pub valid: Option<String>,
    /// The **transaction-time** instant: *what did we believe then*.
    ///
    /// `None` is current belief, which is a projection read rather than a fold
    /// bounded at the present — the same answer, and only one of them is
    /// cheap. See
    /// [`TraversalBuilder::as_of_recorded`](crate::graph::TraversalBuilder::as_of_recorded).
    pub recorded: Option<String>,
    /// How much of the answer to pay for, or `None` for all of it (0.15.10,
    /// W13.5, C-8).
    ///
    /// **The one field that does not narrow which rows are true.** The other
    /// three name a read; this one bounds it, so a plan carrying a limit
    /// describes an answer that is a prefix of the read rather than the read.
    /// What "prefix" means differs by surface and each says so:
    /// [`TraversalBuilder::limit()`](crate::graph::TraversalBuilder::limit)
    /// bounds the walk's rows and returns the near end of the neighbourhood,
    /// [`Database::edges`](crate::Database::edges) bounds its own statement and
    /// returns an arbitrary `n` of the ledger.
    pub limit: Option<usize>,
}

impl ReadPlan {
    /// The ordinary read: the trunk, now, under current belief.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `branch`'s belief rather than the trunk's.
    pub fn on(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }

    /// Read at a valid-time instant rather than the present.
    pub fn valid_at(mut self, ts: impl Into<String>) -> Self {
        self.valid = Some(ts.into());
        self
    }

    /// Read under the belief held at a transaction-time instant.
    pub fn recorded_at(mut self, ts: impl Into<String>) -> Self {
        self.recorded = Some(ts.into());
        self
    }

    /// Pay for at most `n` rows (0.15.10, W13.5, C-8).
    ///
    /// A ceiling on the read's cost, not on which rows are true, and the two
    /// surfaces that take a plan spend it differently because their statements
    /// are different shapes. On a traversal it becomes
    /// [`TraversalBuilder::limit()`](crate::graph::TraversalBuilder::limit),
    /// where it stops the recursion and yields the nodes nearest the start. On
    /// [`Database::edges`](crate::Database::edges) it becomes a `LIMIT` on one
    /// flat projection, where the rows it keeps are **whichever `n` the engine
    /// reaches first** — that read states no order and adding one to make the
    /// truncation look principled would put a sort on the largest statement in
    /// the crate.
    ///
    /// So a limited plan is honest about being a sample, and both surfaces let
    /// a caller tell that it was one: the traversal by
    /// [`WalkOutcome`](crate::graph::WalkOutcome), and `edges` by returning
    /// exactly `n`, which is the ordinary convention because that statement
    /// drops nothing after the limit applies.
    ///
    /// The method and the field share a name, as they do nowhere else on this
    /// struct. `on`/`branch`, `valid_at`/`valid` and `recorded_at`/`recorded`
    /// read as sentences and `limit_to`/`limit` does not; `p.limit(5)` and
    /// `p.limit` are unambiguous to the compiler and to a reader, and the
    /// spelling matches
    /// [`TraversalBuilder::limit()`](crate::graph::TraversalBuilder::limit),
    /// which is where a caller meets the idea first.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// The branch name the read binds, or `None` for the trunk.
    pub(crate) fn branch_name(&self) -> Option<&str> {
        self.branch.as_ref().map(BranchId::as_str)
    }
}

/// Where the branch binds in [`edges_at`]'s statement, when the shape has one.
///
/// `?1` is the valid instant, which every shape binds. This is the
/// [`TraversalBuilder`](crate::graph::TraversalBuilder)'s layout with four
/// slots removed — no start node, no depth, no weight floor — and it is a named
/// constant for the same reason it is one there: the SQL and the parameter
/// vector must agree exactly, and D-030's failure mode is two places agreeing
/// by comment.
const BRANCH_SLOT: usize = 2;

/// Every edge one plan names, as the ledger held them.
///
/// The whole projection filtered to an instant — this is not a neighbourhood
/// read and there is no budget on it. `load_subgraph_with` is the bounded one.
///
/// **The order is unspecified**, as it is for
/// [`query_as_of_edges`](crate::temporal::query_as_of_edges), whose statement
/// this is. Adding an `ORDER BY` would put a sort on the largest read in the
/// crate to make its result look tidy; a caller who needs an order knows which
/// one, and sorting a `Vec` they already own is cheaper than sorting a relation
/// SQLite has to spill.
///
/// `limit` is a plain `LIMIT` on that projection, and it interacts with the
/// unspecified order exactly as badly as it sounds: the rows kept are whichever
/// the engine reaches first. It is offered anyway because the alternative —
/// leaving [`ReadPlan::limit`] unread on this surface — is the silently
/// ignored field 0.15.9 refused to ship, and because bounding a whole-ledger
/// read is worth having even when the sample is arbitrary. Unlike the walk it
/// needs nothing to report truncation: nothing drops rows after the limit
/// applies, so `out.len() == n` is exact.
pub(crate) async fn edges_at(
    conn: &libsql::Connection,
    valid: &str,
    recorded: Option<&str>,
    branch: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<EdgeBelief>> {
    // Refuses an unregistered lineage, and picks between the three shapes for
    // D-219's measured reason: the resolved form is 3x on a database with
    // nothing to resolve.
    let shape = lineage_shape(conn, branch).await?;

    // The traversal's reach guard, asked here for the same reason and at the
    // same cost (D-247): a fold below the hot log's newest surviving stamp is
    // a wrong answer rather than a slow one, and refusing it costs one index
    // seek at every instant a caller is actually likely to ask about.
    if let Some(ts) = recorded {
        if !crate::temporal::replay::hot_log_answers_for(conn, ts).await? {
            return Err(crate::error::DbError::RecordedInstantUnreachable { ts: ts.to_string() });
        }
    }

    let recorded_slot = BRANCH_SLOT + usize::from(shape.binds_branch());
    let lowered = lower(&Resolution {
        shape,
        branch_slot: BRANCH_SLOT,
        recorded_slot: recorded.map(|_| recorded_slot),
        tag: "",
        // A whole-projection read discovers its edges; there is no key to push
        // down. See `Resolution::key`.
        key: None,
    });

    // Spliced rather than bound, which is the one place this file departs
    // from its own rule. A `LIMIT` placeholder would have to sit after the
    // lineage and recorded slots, whose presence varies by shape, so the
    // number would be computed in a third place that has to agree with the
    // other two — D-030's failure mode, bought for nothing. The value is a
    // `usize` this function was handed, so there is no string to carry SQL.
    let limit_clause = match limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };

    let sql = format!(
        "{}SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.branch_id \
         FROM {} l WHERE l.valid_from <= ?1 AND ?1 < l.valid_to{}{}",
        lowered.with_clause(),
        lowered.source,
        lowered.filter,
        limit_clause,
    );

    // Pushed in placeholder order, and only when the emitted SQL names the
    // slot: a `Trunk` read emits no `lineage` CTE and binds no branch, so
    // everything after it moves back by one.
    let mut params: Vec<libsql::Value> = vec![valid.into()];
    if shape.binds_branch() {
        params.push(branch.unwrap_or(crate::schema::ddl::MAIN_BRANCH).into());
    }
    if let Some(ts) = recorded {
        params.push(ts.into());
    }

    let mut rows = conn.query(&sql, params).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(EdgeBelief {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            edge_type: row.get(2)?,
            valid_from: row.get(3)?,
            valid_to: row.get(4)?,
            branch_id: row.get(5)?,
        });
    }
    Ok(out)
}
