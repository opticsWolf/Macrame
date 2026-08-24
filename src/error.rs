use thiserror::Error;

/// The two intervals of a [`DbError::OverlappingInterval`], boxed out of the
/// error enum (D-075).
///
/// Both are reported because neither alone identifies the conflict: the caller
/// knows what they asserted and not what it collided with, and a message naming
/// only the other interval reads as though the assertion were the innocent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    /// The interval the caller asserted.
    pub valid_from: String,
    pub valid_to: String,
    /// The interval it collides with — see [`Overlap::within_batch`] for where
    /// that one is, because it is not always in the database.
    pub existing_from: String,
    pub existing_to: String,
    /// Whether the collision is with another edge in the *same call* (0.13.7,
    /// D-180).
    ///
    /// Two guards raise this one error. `reject_overlapping_interval` compares
    /// the assertion against committed rows; `reject_overlaps_within` compares
    /// a batch against itself, before the transaction opens, and nothing it
    /// names is in the database — the batch is refused whole, so nothing it
    /// names ever will be. A caller told an edge "already holds" an interval
    /// goes looking for a row that is not there.
    pub within_batch: bool,
}

impl Overlap {
    /// The message's closing clause: where the second interval came from.
    ///
    /// A method rather than two `#[error]` strings, because one variant gets
    /// one format string, and rather than a `String` because this is on a
    /// `Display` path.
    pub fn provenance(&self) -> &'static str {
        if self.within_batch {
            "this same batch also asserts"
        } else {
            "is already recorded"
        }
    }
}

/// Which instants a traversal stated, for the one error that has to name them
/// (0.13.10, W7.7, D-183).
///
/// Three cases and never zero. [`DbError::AttributeModeUnstated`] exists
/// *because* an instant was set, so a fourth case carrying neither would be a
/// state no construction site can reach — [D-177]'s objection to a `Result`
/// that cannot fail, in a different shape. [`Self::new`] returns an `Option`
/// and the `None` is the ordinary live traversal, resolved before any error
/// exists.
///
/// [D-177]: ../docs/architecture/s13-decision-register.md#d-177
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatedInstants {
    /// `as_of_valid` alone — *what was true then*.
    Valid(String),
    /// `as_of_recorded` alone — *what we believed then*.
    Recorded(String),
    /// Both, which is the bitemporal cell: *what did we believe at `recorded`
    /// about what was true at `valid`*.
    Both {
        /// The valid-time instant.
        valid: String,
        /// The transaction-time instant.
        recorded: String,
    },
}

impl StatedInstants {
    /// `None` when neither axis was set, which is not an error and not this
    /// type's business to describe.
    pub fn new(valid: Option<&str>, recorded: Option<&str>) -> Option<Self> {
        match (valid, recorded) {
            (Some(v), Some(r)) => Some(Self::Both {
                valid: v.to_string(),
                recorded: r.to_string(),
            }),
            (Some(v), None) => Some(Self::Valid(v.to_string())),
            (None, Some(r)) => Some(Self::Recorded(r.to_string())),
            (None, None) => None,
        }
    }

    /// The valid-time instant, if this traversal stated one.
    pub fn valid(&self) -> Option<&str> {
        match self {
            Self::Valid(v) | Self::Both { valid: v, .. } => Some(v),
            Self::Recorded(_) => None,
        }
    }

    /// The transaction-time instant, if this traversal stated one.
    pub fn recorded(&self) -> Option<&str> {
        match self {
            Self::Recorded(r) | Self::Both { recorded: r, .. } => Some(r),
            Self::Valid(_) => None,
        }
    }
}

/// Rendered as the **calls that produce them**, which is the whole point.
///
/// A message reading `as_of(2020-06-01)` names a method that has not existed
/// since 0.12.17 ([D-174](../docs/architecture/s13-decision-register.md#d-174)),
/// so a caller who goes looking for it finds nothing — and, worse, is not told
/// which of the two axes the instant they set landed on. `as_of_valid(…)` and
/// `as_of_recorded(…)` are what a caller typed and what a caller can change.
impl std::fmt::Display for StatedInstants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid(v) => write!(f, "as_of_valid({v})"),
            Self::Recorded(r) => write!(f, "as_of_recorded({r})"),
            Self::Both { valid, recorded } => {
                write!(f, "as_of_valid({valid}) with as_of_recorded({recorded})")
            }
        }
    }
}

/// Central error type for the Macrame bitemporal ledger database.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("engine: {0}")]
    Engine(#[from] libsql::Error),

    #[error("migration to v{to} failed: {reason}")]
    Migration { to: u32, reason: String },

    #[error("invalid edge type {0} (must match [A-Z0-9]+)")]
    InvalidEdgeType(String),

    // NOTE: the spec (§7) names these fields `source` / `target`. `source` is a
    // reserved field name for thiserror (it is inferred as the error source and
    // requires `std::error::Error`), so the schema column names are used instead.
    #[error(
        "{source_id} -> {target_id} ({edge_type}) already has an open interval; retire it first"
    )]
    SingleOpenViolation {
        source_id: String,
        target_id: String,
        edge_type: String,
    },

    #[error("node {0} not found")]
    NotFound(String),

    #[error("embedding dim {got}, expected {expected} for model {model}")]
    DimMismatch {
        got: usize,
        expected: usize,
        model: String,
    },

    /// A model name is spliced into DDL and queries as a table identifier, and
    /// identifiers cannot be bound as parameters. Validating the name is what
    /// makes that splice safe, so an invalid one is refused rather than escaped.
    #[error("invalid embedding model name {0:?}: expected [a-z][a-z0-9_]* up to 48 characters")]
    InvalidModelName(String),

    #[error("embedding model {model} is not registered (no {table} table)")]
    ModelNotRegistered { model: String, table: String },

    #[error("subgraph exceeds budget ({n} > {budget})")]
    SubgraphTooLarge { n: usize, budget: usize },

    /// Dijkstra and A* settle a node permanently the first time they pop it,
    /// which is only sound when no later edge can reduce the distance — that is,
    /// when weights are non-negative. `links.weight` is a bare `REAL NOT NULL`
    /// with no CHECK, so the guarantee has to be established at load time. The
    /// alternative is a shortest-path result that is quietly just a path.
    #[error("edge {source_id} -> {target_id} has weight {weight}, which shortest-path analytics cannot use")]
    NegativeEdgeWeight {
        source_id: String,
        target_id: String,
        weight: f64,
    },

    #[error("replay corrupt at seq {seq}: {reason}")]
    ReplayCorrupt { seq: i64, reason: String },

    /// A snapshot this build cannot read. Distinct from [`Self::ReplayCorrupt`]
    /// on purpose: corruption is a fault to report, an incompatible snapshot is
    /// the ordinary consequence of an upgrade, and the correct response is to
    /// discard the file and fold from the log instead (D-043).
    #[error("snapshot {path} is not readable by this build: {reason}")]
    SnapshotIncompatible { path: String, reason: String },

    #[error("payload v{got} unsupported (max {max})")]
    PayloadVersion { got: u8, max: u8 },

    #[error("physical delete blocked outside archive session ({table})")]
    ArchiveViolation { table: String },

    /// The archive-session marker exists as **committed** state (0.10.0, W2).
    ///
    /// [`ArchiveViolation`] is this guard working. This variant is the guard
    /// having been silently switched off: while
    /// `macrame_archive_session` is present, `trg_concepts_guard_delete`,
    /// `trg_links_guard_delete` and `trg_txlog_guard_delete` all evaluate their
    /// `WHEN` to false and permit the deletes they exist to refuse, and
    /// `trg_concepts_log_insert` writes no `transaction_log` row for a concept
    /// insert. [Doctrine IV] and [Doctrine V] are both suspended, with no error
    /// and no counter — which is why the condition needs a name of its own.
    ///
    /// **It cannot be produced by an archive session, crashed or otherwise.**
    /// `archive()` and `archive_windowed()` create and drop the marker inside
    /// the same transaction that does the work, so a commit drops it and a
    /// rollback discards it; and the check that raises this error —
    /// `verify` in `src/schema/migrations.rs`, which is private, hence the file
    /// reference rather than a link — reads committed state, so it cannot see an
    /// in-flight session. Reaching this
    /// error therefore means something wrote the table outside the write actor
    /// — the raw-writer case §4.7 concedes exists.
    ///
    /// Not a [`Migration`] error: the schema is intact. What is wrong is the
    /// database's *contents*, and saying "your schema is wrong" would send the
    /// reader to the migration ladder for a fault a `DROP TABLE` fixes.
    ///
    /// [Doctrine IV]: ../../docs/architecture/s0-s3-foundations.md#doctrine-iv
    /// [Doctrine V]: ../../docs/architecture/s0-s3-foundations.md#doctrine-v
    /// [`ArchiveViolation`]: DbError::ArchiveViolation
    /// [`Migration`]: DbError::Migration
    #[error(
        "the archive-session marker table {marker:?} is present as committed \
         state. While it exists, the delete guards on concepts, links and \
         transaction_log are disarmed and concept inserts write no \
         transaction_log row. An archive session creates and drops this table \
         inside one transaction, so it should never be visible here — \
         something wrote it outside the write actor. Drop it (DROP TABLE \
         {marker}) and audit for deletions and missing log rows since it \
         appeared"
    )]
    ArchiveSessionLeaked { marker: String },

    /// A traversal asked about the past without saying which text it wanted
    /// (T3.2, D-085).
    ///
    /// An instant on either axis fixes the *topology*. Node attributes are a
    /// second, independent question, and the default answer —
    /// `AttributeMode::Current` — is live text. That combination returns the
    /// past's graph wearing the present's titles, which is a legitimate thing to
    /// want and a terrible thing to get by accident.
    ///
    /// It used to be a `tracing::warn!`, which is invisible in any application
    /// that has not configured a subscriber. This is the same statement as a
    /// value the caller cannot miss.
    ///
    /// Fix by stating the mode: `.attribute_mode(AttributeMode::AtTime)` for the
    /// past's text, or `.attribute_mode(AttributeMode::Current)` to affirm that
    /// live text is what was meant.
    ///
    /// # It carries [`StatedInstants`] rather than one string (0.13.10, W7.7, D-183)
    ///
    /// The field was `as_of: String` and the message rendered it as
    /// `as_of(…)` — a method removed in 0.12.17 when
    /// [D-174](../docs/architecture/s13-decision-register.md#d-174) split the
    /// axes. Both instants collapsed into it through an `.or()`, so a caller who
    /// set `as_of_recorded` was told about `as_of`, a caller who set both was
    /// told about one of them, and neither was told which clock they had asked
    /// about. Naming the axis is the whole remedy this error offers.
    #[error(
        "traversal {instants} did not state an attribute mode: that topology \
         would be returned with attributes as they are *now*. Call \
         .attribute_mode(AttributeMode::AtTime) for attributes as believed at \
         the stated instant, or .attribute_mode(AttributeMode::Current) to \
         confirm live attributes are intended"
    )]
    AttributeModeUnstated { instants: StatedInstants },
    /// [`crate::Database::diagnostic_conn`] could not open the file read-only
    /// (T5.1, D-091).
    ///
    /// Its own variant rather than `NotFound`, which renders "node {0} not
    /// found" — naming the wrong subject is the defect [D-069] was written to
    /// correct, and a file is not a node.
    ///
    /// The case worth the sentence is a missing file:
    /// `SQLITE_OPEN_READ_ONLY` drops `SQLITE_OPEN_CREATE` with it, so a path
    /// that does not exist is `SQLITE_CANTOPEN` rather than a fresh empty
    /// database. That is the right behaviour and an opaque error to receive.
    ///
    /// [D-069]: ../../docs/architecture/s13-decision-register.md#d-069
    #[error("cannot open {path} read-only for diagnostics: {reason}")]
    DiagnosticConn { path: String, reason: String },
    /// [`crate::Database::archive_windowed`] was given a window it cannot use
    /// (T1.1, D-080).
    ///
    /// Carries a `reason` rather than the numbers as fields because the two
    /// cases it covers are not the same shape — a zero-length window never
    /// advances at all, while a merely narrow one produces a session count that
    /// has to be quoted against the limit to mean anything. A caller reading
    /// this needs the sentence, not the struct.
    ///
    /// It is an error rather than a silent clamp on purpose. Rounding a
    /// one-second window up to something workable would archive over boundaries
    /// the caller did not choose, and the caller cannot see that it happened.
    #[error("archive window {window:?} is unusable: {reason}")]
    ArchiveWindow {
        window: std::time::Duration,
        reason: String,
    },

    /// [`crate::graph::TraversalBuilder::as_of_recorded`] named an instant the
    /// hot log can no longer answer for (0.13.2, W7.1, D-174).
    ///
    /// A transaction-time traversal folds `transaction_log`, and
    /// [`crate::Database::archive`] removes superseded rows from it. Once
    /// anything has been archived, an instant below the cutoff is not *before
    /// history*, it is *history that is in the other file* — and a traversal
    /// takes a connection, not an archive path, so it cannot go and get it.
    ///
    /// **Conservative by one bit, deliberately.** The test is
    /// `hot_log_is_intact`: whether anything was *ever* removed. It cannot ask
    /// whether this particular instant is above the archive cutoff, because the
    /// cutoff is not recorded in the hot log — that is exactly what the hot-side
    /// marker D-132 refused would have carried. So an archived database
    /// refuses every `as_of_recorded`, including instants it could in principle
    /// have answered. The alternative is answering some of them from a partial
    /// fold, which returns *nearly* the right topology, and on a ledger that is
    /// the worst failure available.
    ///
    /// [`crate::temporal::reconstruct`] takes the archive path and answers the
    /// same question, which is why the message names it.
    #[error(
        "transaction-time instant {ts} cannot be answered from the hot log: rows \
         have been archived out of it and a traversal has no archive path. Use \
         macrame::temporal::reconstruct(conn, ts, archive_path, snapshots_dir), \
         which does"
    )]
    RecordedInstantUnreachable { ts: String },

    /// A timestamp that is not in canonical form (§4.1, D-029).
    ///
    /// **Distinct from [`Self::ReplayCorrupt`], which is what this used to be
    /// (Wave 4.5).** `timestamp::normalize` and `timestamp::parse` reported bad
    /// *caller input* as `ReplayCorrupt { seq: 0 }` — a claim that the ledger is
    /// damaged, carrying a sequence number that cannot exist because
    /// `AUTOINCREMENT` starts at 1. The same mistake as defect J: an error that
    /// names the wrong subject sends a caller to fix the wrong thing.
    ///
    /// The value is reported rather than the provenance, because one function
    /// serves both directions — a caller passing `2026-01-01T00:00:00Z` and a
    /// stored `recorded_at` that will not parse produce the same complaint about
    /// the same string. `SystemClock::new` is where the second case is
    /// interpreted, and it already logs and floors to the wall clock (D-027).
    #[error("timestamp {value:?} is not canonical: {reason}")]
    InvalidTimestamp { value: String, reason: String },

    /// An identifier the crate's own encodings cannot represent (D-061).
    ///
    /// Distinct from [`Self::NotFound`], and the distinction is defect J: this
    /// id was refused, not looked up. `validate_id` used to return `NotFound`
    /// here, which tells a caller the thing is missing and invites them to
    /// create it — with the same id, which will be refused again.
    #[error("invalid identifier {id:?}: {reason}")]
    InvalidId { id: String, reason: String },

    /// Two valid-time intervals for one relationship claim the same instant.
    ///
    /// Distinct from [`Self::SingleOpenViolation`], which is the storage layer's
    /// guard and covers only the *open* sentinel. This is the general case, and
    /// it is refused at the API rather than by a trigger (D-060): raw SQL against
    /// the same file can still write an overlap, and §4.2 says so.
    ///
    /// The consequence of allowing one is not an error later but a wrong answer:
    /// `query_as_of_edges` at an instant inside both returns the relationship
    /// twice, and every weighted algorithm downstream double-counts that edge.
    ///
    /// **Boxed, and it is the only variant that is (D-075).** Seven `String`s is
    /// 168 bytes, which made `DbError` — and therefore every `Result` in the
    /// crate, on the `Ok` path too — larger than `clippy::result_large_err`'s
    /// threshold the moment D-060 added it. The other variants are well under.
    /// Boxing the rarest one keeps the whole error small rather than trimming
    /// what a caller is told; `matches!(err, OverlappingInterval { .. })` is
    /// unaffected, which is how every call site uses it.
    #[error(
        "edge {} -> {} ({}): the asserted [{}, {}) overlaps [{}, {}), which {}",
        .overlap.source_id, .overlap.target_id, .overlap.edge_type,
        .overlap.valid_from, .overlap.valid_to,
        .overlap.existing_from, .overlap.existing_to,
        .overlap.provenance()
    )]
    OverlappingInterval { overlap: Box<Overlap> },

    #[error("links_current drift detected: {n} intervals diverge")]
    CurrentDrift { n: usize },

    #[error("rebuild verification failed: {n} intervals still diverge")]
    RebuildFailed { n: usize },
    /// A chunked shadow rebuild was abandoned rather than committed (T1.2, D-082).
    ///
    /// Distinct from [`Self::RebuildFailed`], and the distinction is the whole
    /// point: `RebuildFailed` means the repair ran and did not repair, which is
    /// a reason to distrust the ledger. This means the repair **did not run** —
    /// something invalidated the work in progress and it was discarded before it
    /// could be swapped in. `links_current` is untouched and whatever was true
    /// of it before is still true. The action is to retry.
    #[error("chunked rebuild abandoned: {reason}")]
    RebuildInterrupted { reason: String },

    // -- 0.4.5: writer-actor containment --
    #[error("write actor is not running (reopen the Database)")]
    WriterUnavailable,

    #[error("write actor dropped the response channel mid-request")]
    WriterDroppedResponder,

    /// The actor's task did not join cleanly at [`crate::Database::close`].
    ///
    /// Distinct from [`Self::WriterUnavailable`], which means the channel is
    /// gone while the handle is still in use. This is the shutdown path telling
    /// a caller that the write actor panicked — which `close()` used to swallow,
    /// so a database whose write path had died closed "successfully" (Wave 4.2).
    #[error("write actor did not shut down cleanly: {0}")]
    WriterStopped(String),

    // -- 0.5.0: concept integrity --
    #[error("recorded_at must advance on concept update (got {got}, had {had})")]
    RecordedAtRegression { got: String, had: String },

    /// The stored transaction-time floor is in the future (0.13.5, W7.4, §3.4).
    ///
    /// The clock is raised to `MAX(recorded_at)` at open so that stamps stay
    /// strictly increasing across restarts. That makes a single row from the
    /// future — a skewed host, a bad import, a fixture that escaped — this
    /// process's floor, and every stamp it issues lands at or after it. Those
    /// rows are then written, so the next open reads the same floor back: the
    /// damage is permanent, and it spreads.
    ///
    /// Refused at open rather than absorbed, which is where the crate can still
    /// tell the difference between a stamp it wrote and one it did not.
    /// `macrame::FutureStampPolicy` widens or waives the bound; waiving it
    /// opens the file to be *read*, and does not repair it.
    // The message names the *knob* rather than the Rust spelling of it,
    // because it crosses to Python verbatim and a caller there cannot write a
    // `Tuning` literal. `future_stamps` and `allow` are the two words that mean
    // the same thing on both surfaces.
    #[error(
        "the newest recorded_at in this database is {stamp}, past the limit \
         {limit}. The clock floor is taken from it, so opening would stamp \
         every later write at or after it — permanently, since the next open \
         reads those rows back. Set the future_stamps policy to allow to open \
         it and inspect it; that inherits the floor rather than repairing it"
    )]
    FutureRecordedAt { stamp: String, limit: String },

    /// A chunked bulk write stopped because its caller asked it to (0.13.8,
    /// W7.6, [D-181]).
    ///
    /// Not a failure of the ledger, and the only [`DbError`] a caller can
    /// *cause on purpose*. Nothing is rolled back: the chunks that committed
    /// before the token was seen are committed, which is the same per-chunk
    /// boundary [`crate::Database::bulk_import`] already documents. How many
    /// rows those were is on [`BulkInterrupted::written`], the error this
    /// arrives inside.
    ///
    /// It carries no count of its own precisely so that there is one place to
    /// read the count from, whether the stop was a cancellation or a
    /// constraint.
    ///
    /// [D-181]: ../../docs/architecture/s13-decision-register.md#d-181
    #[error("the bulk write was cancelled between chunks")]
    BulkCancelled,
}

pub type Result<T> = std::result::Result<T, DbError>;

/// A chunked bulk write that stopped partway, and how much of it landed
/// (0.13.8, W7.6, [D-181]).
///
/// The four chunked paths — [`crate::Database::bulk_import`],
/// [`write_concepts`], [`upsert_embeddings`] and
/// [`write_analytics_annotations`] — are atomic per chunk and not overall, so a
/// failure at row 19,000 of 20,000 leaves the first 18,000-odd rows committed.
/// Until 0.13.8 they returned a bare [`DbError`] and the caller was told only
/// that it failed: the count was computed, used to size the next chunk, and
/// dropped on the floor at the `?`. A caller who then retried the whole batch
/// re-wrote everything that had already landed, and one who skipped it lost the
/// tail.
///
/// This is why those four return `Result<usize, BulkInterrupted>` rather than
/// [`Result`]. `From<BulkInterrupted> for DbError` exists so `?` still works in
/// a function returning [`Result`] — that conversion is how a caller says the
/// count does not interest them, and it says so at the call site instead of
/// silently.
///
/// [`write_concepts`]: crate::Database::write_concepts
/// [`upsert_embeddings`]: crate::Database::upsert_embeddings
/// [`write_analytics_annotations`]: crate::Database::write_analytics_annotations
/// [D-181]: ../../docs/architecture/s13-decision-register.md#d-181
#[derive(Debug)]
pub struct BulkInterrupted {
    /// Rows the chunks that finished before the stop committed, and which are
    /// still committed. Zero is an ordinary value: the first chunk can fail.
    pub written: usize,
    /// Why it stopped. [`DbError::BulkCancelled`] if the caller asked;
    /// otherwise whatever the failing chunk raised, unchanged — this is not a
    /// new error, it is the same one with the count attached.
    pub cause: DbError,
}

impl std::fmt::Display for BulkInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} row(s) committed before the stop, and still committed)",
            self.cause, self.written
        )
    }
}

impl std::error::Error for BulkInterrupted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

impl From<BulkInterrupted> for DbError {
    /// Discards `written`. That is the point: a caller writing `?` into a
    /// function returning [`Result`] has decided the partial count is not
    /// something they will act on, and this puts that decision at the place it
    /// is taken rather than inside the crate.
    fn from(e: BulkInterrupted) -> Self {
        e.cause
    }
}

impl BulkInterrupted {
    /// Whether the stop was the caller's own cancellation rather than a fault.
    pub fn was_cancelled(&self) -> bool {
        matches!(self.cause, DbError::BulkCancelled)
    }
}

/// What the four chunked bulk paths return (0.13.8, W7.6).
pub type BulkResult<T> = std::result::Result<T, BulkInterrupted>;

/// A guard abort recognised by its message (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    SingleOpenInterval,
    RecordedAtRegression,
    DeleteOutsideArchive,
    /// Not one of our guards — an ordinary engine error.
    NotAGuard,
}

/// Recognise a schema guard's `RAISE(ABORT, …)` by its message.
///
/// **The only place in the crate that matches on engine error text.** SQLite
/// reports a `RAISE(ABORT)` as a generic constraint failure carrying the
/// message, so the message is the only thing distinguishing "you violated the
/// single-open-interval rule" from "the disk is full" — but matching on it
/// scattered across call sites means an upstream wording change degrades an
/// unknown number of typed errors into opaque ones, silently. Concentrated here,
/// a change breaks one function and the tests that cover it.
///
/// The needles are the [`crate::schema::ddl`] constants spliced into the
/// triggers themselves, so guard and classifier cannot drift.
pub fn abort_kind(err: &libsql::Error) -> AbortKind {
    use crate::schema::ddl::{ABORT_DELETE_GUARD, ABORT_MONOTONIC_RA, ABORT_SINGLE_OPEN};

    let text = err.to_string();
    if text.contains(ABORT_SINGLE_OPEN) {
        AbortKind::SingleOpenInterval
    } else if text.contains(ABORT_MONOTONIC_RA) {
        AbortKind::RecordedAtRegression
    } else if text.contains(ABORT_DELETE_GUARD) {
        AbortKind::DeleteOutsideArchive
    } else {
        AbortKind::NotAGuard
    }
}

/// What a failing statement was trying to do, so a guard abort can name it.
pub enum WriteOp<'a> {
    Edge {
        source_id: &'a str,
        target_id: &'a str,
        edge_type: &'a str,
    },
    Concept {
        id: &'a str,
        recorded_at: &'a str,
    },
    Delete {
        table: &'a str,
    },
    /// A derived annotation (0.13.3, W7.2, [`crate::Annotation`]).
    ///
    /// The only [`WriteOp`] whose failure is not a `RAISE(ABORT)`.
    /// `analytics_annotations` carries no triggers at all — that is why it is
    /// the cheapest bulk table and why its chunk ceiling is the largest
    /// (D-058) — so the guard vocabulary [`abort_kind`] speaks has nothing to
    /// say about it. What it does carry is a foreign key onto `concepts`, and
    /// that is the failure a caller can actually cause.
    Annotation {
        concept_id: &'a str,
    },
}

/// `SQLITE_CONSTRAINT_FOREIGNKEY` — `SQLITE_CONSTRAINT | (3 << 8)`.
///
/// libSQL reports statement failures through
/// `libsql::Error::SqliteFailure(extended_error_code(…), …)`, so this is the
/// *extended* code and discriminates a foreign-key failure from the CHECK,
/// PRIMARY KEY and NOT NULL failures that share primary code 19. Matching the
/// primary code would classify a malformed `computed_at` — a different bug with
/// a different fix — as a missing concept.
const SQLITE_CONSTRAINT_FOREIGNKEY: std::ffi::c_int = 787;

/// Recognise a foreign-key failure by its result code, not by its message.
///
/// The deliberate counterpart to [`abort_kind`]. That function matches text
/// because it has no alternative: SQLite flattens every `RAISE(ABORT)` into one
/// generic constraint failure and the message is the only thing left. A foreign
/// key is enforced by the engine itself and carries a code of its own, so
/// nothing here depends on wording — an upstream message change cannot degrade
/// this classification, which is exactly the failure mode `abort_kind`'s
/// rustdoc warns about and cannot escape.
fn is_foreign_key_violation(err: &libsql::Error) -> bool {
    matches!(err, libsql::Error::SqliteFailure(code, _) if *code == SQLITE_CONSTRAINT_FOREIGNKEY)
}

/// Turn an engine error into the typed error §7 specifies, where one applies.
///
/// Takes a connection because `RecordedAtRegression` reports the value it
/// clashed with, and the trigger does not put it in the message. One extra query
/// on an error path buys an error a caller can act on instead of one they have
/// to reproduce by hand.
pub async fn classify(conn: &libsql::Connection, err: libsql::Error, op: WriteOp<'_>) -> DbError {
    match (abort_kind(&err), op) {
        (
            AbortKind::SingleOpenInterval,
            WriteOp::Edge {
                source_id,
                target_id,
                edge_type,
            },
        ) => DbError::SingleOpenViolation {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            edge_type: edge_type.to_string(),
        },
        (AbortKind::RecordedAtRegression, WriteOp::Concept { id, recorded_at }) => {
            let had = current_recorded_at(conn, id).await.unwrap_or_default();
            DbError::RecordedAtRegression {
                got: recorded_at.to_string(),
                had,
            }
        }
        (AbortKind::DeleteOutsideArchive, WriteOp::Delete { table }) => DbError::ArchiveViolation {
            table: table.to_string(),
        },
        // An annotation naming a concept that is not there. The engine says
        // "FOREIGN KEY constraint failed" and no more — not which row, and a
        // rejected chunk may hold up to `chunk_rows::ANNOTATIONS` of them. The
        // typed error names the concept, which is the fact the database
        // actually knows and the one the caller has to act on.
        (_, WriteOp::Annotation { concept_id }) if is_foreign_key_violation(&err) => {
            DbError::NotFound(concept_id.to_string())
        }
        // A guard fired for an operation it does not describe. Reporting the raw
        // error is honest; inventing a typed one from the wrong context is not.
        _ => DbError::Engine(err),
    }
}

async fn current_recorded_at(conn: &libsql::Connection, id: &str) -> Option<String> {
    conn.query(
        "SELECT recorded_at FROM concepts WHERE id = ?1",
        libsql::params![id],
    )
    .await
    .ok()?
    .next()
    .await
    .ok()??
    .get(0)
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DbError` stays under `clippy::result_large_err`'s 128-byte threshold.
    ///
    /// Every fallible function in the crate returns `Result<T, DbError>`, so the
    /// enum's size is paid on the `Ok` path too. D-060 pushed it to 168 bytes with
    /// one seven-`String` variant and nobody noticed until D-075 read the lint
    /// output; boxing that variant brought it back. This is the tripwire, because
    /// the failure mode is a warning in a build log rather than a broken test —
    /// the kind this cycle has spent its whole length finding.
    #[test]
    fn the_error_enum_stays_small_enough_to_return_by_value() {
        let size = std::mem::size_of::<DbError>();
        assert!(
            size <= 128,
            "DbError is {size} bytes. Some variant has grown past what a Result \n             should carry — box it, as OverlappingInterval is boxed (D-075)."
        );
    }

    fn sample_overlap(within_batch: bool) -> DbError {
        DbError::OverlappingInterval {
            overlap: Box::new(Overlap {
                source_id: "a".into(),
                target_id: "b".into(),
                edge_type: "KNOWS".into(),
                valid_from: "2026-03-01T00:00:00.000000Z".into(),
                valid_to: "2026-09-01T00:00:00.000000Z".into(),
                existing_from: "2026-01-01T00:00:00.000000Z".into(),
                existing_to: "2026-06-01T00:00:00.000000Z".into(),
                within_batch,
            }),
        }
    }

    /// The boxed variant still reports both intervals.
    #[test]
    fn an_overlap_names_the_asserted_interval_and_the_other_one() {
        let msg = sample_overlap(false).to_string();
        assert!(msg.contains("a -> b (KNOWS)"), "{msg}");
        assert!(msg.contains("asserted [2026-03-01"), "{msg}");
        assert!(msg.contains("[2026-01-01"), "{msg}");
    }

    /// One error, two guards, and only one of them is talking about the
    /// database (0.13.7, D-180).
    ///
    /// `reject_overlaps_within` refuses the batch *before* the transaction
    /// opens, so the interval it names is not stored and will not become
    /// stored. Saying the edge "already holds" it sent a caller looking for a
    /// row that was never written.
    #[test]
    fn an_overlap_says_which_side_of_the_write_the_other_interval_is_on() {
        let stored = sample_overlap(false).to_string();
        assert!(stored.contains("is already recorded"), "{stored}");
        assert!(!stored.contains("batch"), "{stored}");

        let in_batch = sample_overlap(true).to_string();
        assert!(
            in_batch.contains("this same batch also asserts"),
            "{in_batch}"
        );
        assert!(!in_batch.contains("recorded"), "{in_batch}");
    }

    /// The count is the whole reason this type exists, so it has to be in the
    /// sentence a caller sees, not only in a field they have to know about
    /// (0.13.8, W7.6).
    #[test]
    fn a_partial_bulk_failure_says_how_much_landed() {
        let e = BulkInterrupted {
            written: 18_935,
            cause: DbError::NotFound("ghost".into()),
        };
        let text = e.to_string();
        assert!(text.contains("node ghost not found"), "{text}");
        assert!(text.contains("18935"), "{text}");
    }

    /// Cancellation is not a fault, and the type says which it was without the
    /// caller matching on a variant.
    #[test]
    fn cancellation_is_distinguishable_from_a_failure() {
        assert!(BulkInterrupted {
            written: 7,
            cause: DbError::BulkCancelled,
        }
        .was_cancelled());
        assert!(!BulkInterrupted {
            written: 7,
            cause: DbError::WriterUnavailable,
        }
        .was_cancelled());
    }

    /// `?` into a `Result<_, DbError>` keeps the cause and drops the count.
    /// Both halves of that are deliberate; this pins them.
    #[test]
    fn converting_to_a_db_error_keeps_the_cause_and_loses_the_count() {
        let e = BulkInterrupted {
            written: 400,
            cause: DbError::SingleOpenViolation {
                source_id: "a".into(),
                target_id: "b".into(),
                edge_type: "KNOWS".into(),
            },
        };
        let cause: DbError = e.into();
        assert!(matches!(cause, DbError::SingleOpenViolation { .. }));
        assert!(!cause.to_string().contains("400"));
    }

    /// The error chain reaches the cause, so `anyhow`-style reporters print
    /// both lines rather than only the wrapper's.
    #[test]
    fn the_cause_is_reachable_as_an_error_source() {
        use std::error::Error;
        let e = BulkInterrupted {
            written: 1,
            cause: DbError::BulkCancelled,
        };
        assert_eq!(
            e.source().map(ToString::to_string).as_deref(),
            Some("the bulk write was cancelled between chunks")
        );
    }
}
