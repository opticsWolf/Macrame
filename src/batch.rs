//! Mixed atomic writes and pointer edges (0.20.0, M2 and M3; [D-292],
//! [D-293]).
//!
//! # Why a batch carries four kinds of row
//!
//! A conversation turn is one act: the turn's concept, the edges that place
//! it, and the pointer that makes it the branch's head. Before 0.20.0 those
//! were three or four actor round trips with three or four stamps, and two
//! things followed from the stamps rather than from any bug:
//!
//! - a crash between the concept and its edges left an orphan turn (Silk's
//!   `HistoryLedger` says so in its own docstring); and
//! - a fork cut at the turn's stamp (M1) did not inherit the turn's edges,
//!   which were stamped a microsecond later.
//!
//! [`WriteBatch`] is one transaction and one `recorded_at` for concepts,
//! edges, retirements and pointer moves, which is D-014's rule ("the rows were
//! asserted by one act") extended from edges to everything an act writes.
//!
//! # Pointers
//!
//! A pointer is an edge of a kind registered **functional** with
//! [`Database::register_edge_kind`](crate::Database::register_edge_kind): at
//! most one open target per `(source, kind)` in each lineage's view. Moving it
//! closes the open target (by shadow retirement when the open edge is
//! inherited) and opens the new one, under one stamp. Valid time is the
//! caller's (Doctrine II): the old interval is closed at the new pointer's
//! `valid_from`, never at the transaction time.
//!
//! [D-292]: ../docs/architecture/s13-decision-register.md#d-292
//! [D-293]: ../docs/architecture/s13-decision-register.md#d-293

use crate::branch::BranchId;
use crate::graph::EdgeAssertion;
use crate::ConceptUpsert;

/// The most rows one [`WriteBatch`] may carry, counting every concept, edge,
/// retirement and pointer move.
///
/// A batch is one transaction under the write lock, so its size is a hold the
/// caller imposes on every other writer. 2,000 is ten conversation turns'
/// worth of rows a hundred times over; a bulk load belongs on
/// [`Database::bulk_import`](crate::Database::bulk_import), which chunks.
pub const MAX_BATCH_ROWS: usize = 2_000;

/// Move a pointer: close `(source, edge_type)`'s open target on a lineage and
/// open `target`, under one stamp (0.20.0, M3, [D-293]).
///
/// `valid_from` is required — it is where the old target's interval closes and
/// the new one's opens, and deriving it from the transaction time would mix
/// the two clocks (Doctrine II).
///
/// [D-293]: ../docs/architecture/s13-decision-register.md#d-293
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PointerSet {
    pub source: String,
    pub edge_type: String,
    pub target: String,
    pub valid_from: String,
    pub weight: f64,
    pub branch: Option<BranchId>,
    /// When set, the move also asserts an edge of this kind from the target
    /// it closes — or from `source` when nothing was open — to the new one,
    /// at the same `valid_from`. See [`Self::link_previous`].
    pub link_previous: Option<String>,
}

impl PointerSet {
    /// A pointer move with weight 1.0, on the trunk unless
    /// [`Self::on_branch`] says otherwise. `valid_from` must be set before
    /// the move is written.
    pub fn new(
        source: impl Into<String>,
        edge_type: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            edge_type: edge_type.into(),
            target: target.into(),
            valid_from: String::new(),
            weight: 1.0,
            branch: None,
            link_previous: None,
        }
    }

    /// Also link the previous target to the new one with an edge of `kind`
    /// (0.20.0, M3, [D-293]).
    ///
    /// The append primitive a conversation needs: *the new turn follows
    /// whatever the head was*, decided inside the write transaction rather
    /// than by a read the caller made before it. Without it, two writers that
    /// read the same head both link from it and the lineage forks silently;
    /// with it, the second writer links from the first writer's turn. When
    /// nothing was open the edge starts at `source` itself (the session).
    ///
    /// [D-293]: ../docs/architecture/s13-decision-register.md#d-293
    pub fn link_previous(mut self, kind: impl Into<String>) -> Self {
        self.link_previous = Some(kind.into());
        self
    }

    /// Where the new target's interval opens and the old one's closes.
    pub fn valid_from(mut self, ts: impl Into<String>) -> Self {
        self.valid_from = ts.into();
        self
    }

    pub fn weight(mut self, w: f64) -> Self {
        self.weight = w;
        self
    }

    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }

    pub(crate) fn branch_name(&self) -> &str {
        self.branch
            .as_ref()
            .map_or(crate::schema::ddl::MAIN_BRANCH, BranchId::as_str)
    }
}

/// What a pointer move did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PointerChange {
    pub source: String,
    pub edge_type: String,
    /// The target that was open on the lineage before the move, if any.
    pub old: Option<String>,
    /// The target open after it.
    pub new: String,
    /// `false` when `new` was already the open target and nothing was written.
    pub changed: bool,
}

/// One edge closure in a [`WriteBatch`] — [`Database::retire_edge`]'s
/// arguments, as a value.
///
/// [`Database::retire_edge`]: crate::Database::retire_edge
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Retirement {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub valid_from: String,
    pub valid_to: String,
    pub branch: Option<BranchId>,
}

impl Retirement {
    /// Close the edge `(source, target, edge_type)` asserted at `valid_from`,
    /// at `valid_to`.
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        edge_type: impl Into<String>,
        valid_from: impl Into<String>,
        valid_to: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            edge_type: edge_type.into(),
            valid_from: valid_from.into(),
            valid_to: valid_to.into(),
            branch: None,
        }
    }

    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }
}

/// Concepts, edges, retirements and pointer moves written as one act
/// (0.20.0, M2, [D-292]).
///
/// One transaction, one `recorded_at`, all or nothing. Rows that name no
/// lineage take the batch's ([`Self::on_branch`]); a row naming a different
/// lineage is refused with [`DbError::BranchMismatch`], the rule
/// [`BranchView`](crate::BranchView) applies, because it is evidence the
/// caller believed the row was going somewhere else.
///
/// Write order inside the transaction is fixed: concepts, then retirements,
/// then edges, then pointer moves — so an edge may name a concept minted in
/// the same batch, and a pointer may target one.
///
/// [`DbError::BranchMismatch`]: crate::DbError::BranchMismatch
/// [D-292]: ../docs/architecture/s13-decision-register.md#d-292
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteBatch {
    pub concepts: Vec<ConceptUpsert>,
    pub edges: Vec<EdgeAssertion>,
    pub retirements: Vec<Retirement>,
    pub pointers: Vec<PointerSet>,
    pub branch: Option<BranchId>,
}

impl WriteBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The lineage every row that names none is written on.
    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }

    pub fn concept(mut self, c: ConceptUpsert) -> Self {
        self.concepts.push(c);
        self
    }

    pub fn edge(mut self, e: EdgeAssertion) -> Self {
        self.edges.push(e);
        self
    }

    pub fn retire(mut self, r: Retirement) -> Self {
        self.retirements.push(r);
        self
    }

    pub fn pointer(mut self, p: PointerSet) -> Self {
        self.pointers.push(p);
        self
    }

    /// Every row the batch carries.
    pub fn len(&self) -> usize {
        self.concepts.len() + self.edges.len() + self.retirements.len() + self.pointers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a [`WriteBatch`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BatchReport {
    /// The one transaction-time stamp every row in the batch carries.
    pub recorded_at: String,
    pub concepts: usize,
    pub edges: usize,
    pub retirements: usize,
    /// One per pointer move, in the order given.
    pub pointers: Vec<PointerChange>,
}
