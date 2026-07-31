use thiserror::Error;

/// The two intervals of a [`DbError::OverlappingInterval`], boxed out of the
/// error enum (D-075).
///
/// Both are reported because neither alone identifies the conflict: the caller
/// knows what they asserted and not what it collided with, and a message naming
/// only the stored interval reads as though the assertion were the innocent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    /// The interval the caller asserted.
    pub valid_from: String,
    pub valid_to: String,
    /// The interval already stored that it collides with.
    pub existing_from: String,
    pub existing_to: String,
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
    #[error("{source_id} -> {target_id} ({edge_type}) already has an open interval; retire it first")]
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

    /// A traversal asked about the past without saying which text it wanted
    /// (T3.2, D-085).
    ///
    /// `TraversalBuilder::as_of(ts)` fixes the *topology* at `ts`. Node
    /// attributes are a second, independent question, and the default answer —
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
    #[error(
        "traversal as_of({as_of}) did not state an attribute mode: topology at \
         {as_of} would be returned with attributes as they are *now*. Call \
         .attribute_mode(AttributeMode::AtTime) for attributes as believed at \
         {as_of}, or .attribute_mode(AttributeMode::Current) to confirm live \
         attributes are intended"
    )]
    AttributeModeUnstated { as_of: String },
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
        "edge {} -> {} ({}) already holds [{}, {}), which overlaps the asserted [{}, {})",
        .overlap.source_id, .overlap.target_id, .overlap.edge_type,
        .overlap.existing_from, .overlap.existing_to,
        .overlap.valid_from, .overlap.valid_to
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
}

pub type Result<T> = std::result::Result<T, DbError>;

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

    /// The boxed variant still reports both intervals.
    #[test]
    fn an_overlap_names_the_asserted_interval_and_the_stored_one() {
        let err = DbError::OverlappingInterval {
            overlap: Box::new(Overlap {
                source_id: "a".into(),
                target_id: "b".into(),
                edge_type: "KNOWS".into(),
                valid_from: "2026-03-01T00:00:00.000000Z".into(),
                valid_to: "2026-09-01T00:00:00.000000Z".into(),
                existing_from: "2026-01-01T00:00:00.000000Z".into(),
                existing_to: "2026-06-01T00:00:00.000000Z".into(),
            }),
        };
        let msg = err.to_string();
        assert!(msg.contains("a -> b (KNOWS)"), "{msg}");
        assert!(msg.contains("already holds [2026-01-01"), "{msg}");
        assert!(msg.contains("asserted [2026-03-01"), "{msg}");
    }
}
