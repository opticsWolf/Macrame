use thiserror::Error;

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

    #[error("links_current drift detected: {n} intervals diverge")]
    CurrentDrift { n: usize },

    #[error("rebuild verification failed: {n} intervals still diverge")]
    RebuildFailed { n: usize },

    // -- 0.4.5: writer-actor containment --
    #[error("write actor is not running (reopen the Database)")]
    WriterUnavailable,

    #[error("write actor dropped the response channel mid-request")]
    WriterDroppedResponder,

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
