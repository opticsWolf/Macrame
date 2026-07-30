use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::error::{classify, DbError, Result, WriteOp};
use crate::graph::edge::EdgeAssertion;
use crate::integrity::{rebuild_current, RebuildReport};
use crate::schema::migrations;
use crate::temporal::archive::{archive, ArchiveReport};
use crate::temporal::snapshot::{self, SnapshotCadence};
use crate::util::clock::{Clock, SystemClock};
use crate::util::timestamp;
use crate::vector::ModelName;

/// Rows per chunk on the background write paths (§5.1.5, D-011, D-014, D-058).
///
/// The Write Actor holds the sole write connection, so a single large statement
/// blocks every other writer for its duration. Chunking bounds that stall; the
/// cost is that a bulk import is *not* atomic across chunks, which is why it is
/// a separate command from [`HighPriCommand::WriteBulkAtomic`] rather than a
/// tuning parameter on it.
///
/// # Why these are four constants and not one
///
/// Through 0.5.5 this was a single `CHUNK_ROWS = 1000` for all four bulk paths.
/// The golden rule it was meant to serve is a bound on *duration* — a background
/// chunk must commit fast enough that an interactive write queued behind it is
/// not made to wait — and one row count cannot express one duration across paths
/// whose measured per-row costs differ by 60× (D-058). At 1,000 rows the four
/// paths took 3.5 ms, 24 ms, 89 ms and 143 ms: the same constant, four answers,
/// three of them far outside the bound.
///
/// Each size below is derived from `benches/budgets.rs`'s `chunk_scaling`
/// sweep against [`CHUNK_BUDGET`], then verified by measuring that size directly.
/// They are *measurements of this machine*, not universal constants — D-055's
/// reasoning about reference hardware applies here too, and re-deriving them on
/// materially different storage is a `cargo bench` away.
///
/// # Sized for the tail, not the median
///
/// The first derivation solved `f + c·n = 3 ms` exactly and produced sizes whose
/// *median* commit was 2.93 ms and whose upper estimate was 2.96 — inside the
/// bound as reported and outside it for any chunk slower than typical. A latency
/// bound is a statement about the chunk an unlucky interactive write actually
/// queues behind, so these solve for ≈2.5 ms instead, leaving the remainder as
/// headroom for the tail. That costs a few percent of throughput on the two
/// linear paths and nothing on the two superlinear ones.
///
/// As measured by `chunk_budget`, each at its own size: edges **2.39 ms**,
/// concepts **2.35 ms**, annotations **2.36 ms**, embeddings **2.06 ms**, no
/// upper estimate above 2.42.
///
/// # Known limitation: these are empty-database figures
///
/// `chunk_budget` seeds concepts and starts with **no links and no vectors**,
/// and D-059 established that per-row cost on the edge and embedding paths grows
/// with the size of the structure being written, not with the chunk. The same
/// 90-edge chunk takes 47.7 ms against an 8,000-edge hub. So the bound is met as
/// measured and *not* met on a large database, most of that gap being the schema
/// defect D-059 documents. Re-deriving these against a realistic fixture needs a
/// decision about what "realistic" is, which is why it has not been done
/// silently.
pub mod chunk_rows {
    /// Edge assertions (`bulk_import`).
    ///
    /// Per-row cost on this path rises with the size of `links_current`, not
    /// with the chunk (D-059) — so cutting the chunk buys latency and costs
    /// throughput, ~11% for 1,000 edges. An earlier version of this comment
    /// claimed it was 3.3× *faster*; that came from multiplying eleven copies of
    /// a chunk measured into an empty database.
    ///
    /// **This size does not meet the 3 ms bound on a large database.** 90 edges
    /// into an 8,000-edge hub take 47.7 ms, because `trg_links_single_open`'s
    /// `EXISTS` is served by `idx_lc_traversal_cover` with only `source_id`
    /// bound and therefore scans the whole out-degree. That is a schema defect
    /// with a proven fix, recorded in D-059 and not applied here.
    pub const EDGES: usize = 90;

    /// Concept upserts (`write_annotations`).
    ///
    /// Linear at ~23 µs per row, so unlike [`EDGES`] this size *is* a genuine
    /// throughput sacrifice: 1,000-row chunks ran at 23.6 µs per row against
    /// ~35 µs here. Paid deliberately — a 1,000-row chunk takes 24 ms, eight
    /// times the bound.
    pub const CONCEPTS: usize = 70;

    /// Analytics annotations (`write_analytics_annotations`).
    ///
    /// The one path where the old constant was nearly right, and the only bulk
    /// table with no triggers at all: ~2.5 µs per row, linear, so the bound buys
    /// a large chunk. 1,000 rows would be 3.5 ms — over, but only just.
    pub const ANNOTATIONS: usize = 600;

    /// Embedding vectors (`upsert_embeddings`).
    ///
    /// The smallest by a wide margin, because DiskANN index maintenance makes an
    /// embedding the most expensive row in the system. That cost grows with the
    /// **corpus**, not the chunk (D-059): a fixed 30-vector chunk costs 49 µs per
    /// vector into an empty corpus and 224 µs into an 8,000-vector one. Graph
    /// insertion getting dearer as the graph grows is what DiskANN is, so unlike
    /// [`EDGES`] there is nothing here to fix — but it does mean this size buys
    /// latency at some throughput, not for free.
    pub const EMBEDDINGS: usize = 30;
}

/// The latency bound [`chunk_rows`] is derived from (§5.1.5, D-058).
///
/// This is the golden rule's actual content. §9 has carried it as a row count
/// with a duration attached — "chunk commit, 500 rows ≤ 3 ms" — which reads as
/// two requirements and is one: the duration is the requirement, and the row
/// count is whatever satisfies it on a given path and machine.
///
/// 3 ms is §9's number, kept rather than renegotiated. What it buys, end to end:
/// an interactive assertion arriving at the worst possible moment waits for the
/// chunk in flight (≤ 3 ms, because the SQLite write lock is not preemptible —
/// see [`HighPriCommand`]) and then runs its own write (≤ 5 ms, §9), so ≤ 8 ms
/// worst case. That fits inside a 60 Hz frame with room, which is the standard
/// this bound is ultimately answerable to.
pub const CHUNK_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);

/// A concept assertion: the payload of an upsert.
#[derive(Debug, Clone, PartialEq)]
pub struct ConceptUpsert {
    pub id: String,
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
    pub valid_from: String,
    pub valid_to: String,
    pub retired: bool,
}

impl ConceptUpsert {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            content: String::new(),
            embedding_model: None,
            valid_from: String::new(),
            valid_to: timestamp::OPEN_SENTINEL.to_string(),
            retired: false,
        }
    }

    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    pub fn embedding_model(mut self, model: impl Into<String>) -> Self {
        self.embedding_model = Some(model.into());
        self
    }

    pub fn valid_from(mut self, ts: impl Into<String>) -> Self {
        self.valid_from = ts.into();
        self
    }

    pub fn valid_to(mut self, ts: impl Into<String>) -> Self {
        self.valid_to = ts.into();
        self
    }

    pub fn retired(mut self, retired: bool) -> Self {
        self.retired = retired;
        self
    }

    /// Put the timestamps in canonical form (D-029) before they cross the channel.
    pub fn normalized(mut self) -> Result<Self> {
        self.valid_from = timestamp::normalize(&self.valid_from)?;
        self.valid_to = timestamp::normalize(&self.valid_to)?;
        Ok(self)
    }
}

/// One derived analytics result for one concept (§5.4, D-041).
///
/// Not a `ConceptUpsert`. The distinction is the whole of D-041: a concept
/// upsert is a statement about the world and belongs in the ledger, while an
/// annotation is a function of an algorithm applied to a graph and belongs in
/// `analytics_annotations`, which carries no log trigger. Writing one as the
/// other overwrote the concept's `content` with the label and recorded every
/// analytics rerun as a fresh version of the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub concept_id: String,
    /// Namespaced by convention, e.g. `louvain.community`, `kcore.shell`.
    pub label: String,
    /// JSON-encoded payload. Opaque to this crate.
    pub value: String,
}

impl Annotation {
    pub fn new(
        concept_id: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            concept_id: concept_id.into(),
            label: label.into(),
            value: value.into(),
        }
    }
}

/// Commands sent to the Write Actor on the high-priority channel (UI-driven work).
pub enum HighPriCommand {
    AssertEdge {
        edge: EdgeAssertion,
        responder: oneshot::Sender<Result<()>>,
    },
    RetireEdge {
        source: String,
        target: String,
        edge_type: String,
        valid_from: String,
        valid_to: String,
        responder: oneshot::Sender<Result<()>>,
    },
    UpsertConcept {
        concept: ConceptUpsert,
        responder: oneshot::Sender<Result<()>>,
    },
    WriteBulkAtomic {
        edges: Vec<EdgeAssertion>,
        responder: oneshot::Sender<Result<usize>>,
    },
    RebuildCurrent {
        responder: oneshot::Sender<Result<RebuildReport>>,
    },
    /// Create a model's embedding table and its DiskANN index (D-037, D-048).
    ///
    /// High priority despite being setup work: it is one small transaction, and
    /// every embedding write for the model blocks on it, so queueing it behind a
    /// bulk job would stall the thing it gates.
    RegisterModel {
        model: ModelName,
        dim: usize,
        responder: oneshot::Sender<Result<()>>,
    },
    Shutdown {
        responder: oneshot::Sender<Result<()>>,
    },
}

/// Commands sent to the Write Actor on the low-priority channel (background work).
pub enum LowPriCommand {
    WriteAnnotationsChunk {
        chunk: Vec<ConceptUpsert>,
        responder: oneshot::Sender<Result<usize>>,
    },
    WriteAnalyticsChunk {
        chunk: Vec<Annotation>,
        responder: oneshot::Sender<Result<usize>>,
    },
    /// One chunk of vectors for one model (§5.9, D-048).
    ///
    /// Low priority: embedding is bulk derived work and must never preempt an
    /// interactive assertion.
    UpsertEmbeddingChunk {
        model: ModelName,
        chunk: Vec<(String, Vec<f32>)>,
        responder: oneshot::Sender<Result<usize>>,
    },
    BulkImportChunk {
        chunk: Vec<EdgeAssertion>,
        responder: oneshot::Sender<Result<usize>>,
    },
    Archive {
        cutoff: String,
        archive_path: PathBuf,
        responder: oneshot::Sender<Result<ArchiveReport>>,
    },
    /// Reconstruct the FTS index from `concepts` (§5.9, D-036, D-051).
    ///
    /// Low priority: it is maintenance on a derivative table, and a search index
    /// that is a few seconds stale is a smaller cost than an interactive write
    /// that waits behind a full reindex.
    RebuildFts {
        responder: oneshot::Sender<Result<()>>,
    },
}

enum LoopCtl {
    Continue,
    Break,
}

/// Primary database handle for Macrame bitemporal ledger.
pub struct Database {
    db: libsql::Database,
    read_conn: libsql::Connection,
    highpri_tx: mpsc::Sender<HighPriCommand>,
    lowpri_tx: mpsc::Sender<LowPriCommand>,
    clock: Arc<dyn Clock>,
    archive_path: PathBuf,
    snapshots_dir: PathBuf,
    schema_version: u32,
    writer: Option<tokio::task::JoinHandle<Result<()>>>,
    /// Stops the snapshot cadence. Dropping it stops the task too, which is what
    /// keeps a `Database` that is dropped rather than closed from leaving a task
    /// running against a connection whose database is going away.
    cadence_stop: Option<tokio::sync::watch::Sender<bool>>,
    cadence: Option<tokio::task::JoinHandle<()>>,
}

impl Database {
    /// Open a database file at `path`, configuring pragmas, running migrations, and spawning the Write Actor.
    ///
    /// The snapshot cadence runs with [`SnapshotCadence::default`]. Use
    /// [`Database::open_with_cadence`] to tune or disable it.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_cadence(path, Some(SnapshotCadence::default())).await
    }

    /// Open with an explicit snapshot cadence, or `None` to run without one
    /// (§5.5, D-053).
    ///
    /// `None` restores the pre-0.5.5 behaviour, where `close()` is the only
    /// thing that ever writes an anchor. That is the right setting for a
    /// short-lived process that will not accumulate a delta worth bounding, and
    /// for tests that assert on the contents of the snapshot directory.
    pub async fn open_with_cadence(
        path: impl AsRef<Path>,
        cadence: Option<SnapshotCadence>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let db = libsql::Builder::new_local(path).build().await?;
        let write_conn = configure(db.connect()?).await?;
        let read_conn = configure(db.connect()?).await?;

        // PRAGMA query_only = ON on reader connection (§5.1.2)
        read_conn.execute("PRAGMA query_only = ON", ()).await?;

        migrations::run(&write_conn).await?;

        let (highpri_tx, highpri_rx) = mpsc::channel(256);
        let (lowpri_tx, lowpri_rx) = mpsc::channel(64);

        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new(&read_conn).await?);
        let writer = tokio::spawn(run_writer_actor(
            write_conn,
            Arc::clone(&clock),
            highpri_rx,
            lowpri_rx,
        ));

        let archive_path = derive_archive_path(path);
        let snapshots_dir = derive_snapshots_dir(path);

        // The task shares `read_conn` rather than opening a third connection:
        // `libsql::Connection` is an Arc-backed handle, and R15 makes every
        // additional local connection in one process a cost worth not paying
        // for nothing.
        let (cadence_stop, cadence) = match cadence {
            Some(cadence) => {
                let (tx, rx) = tokio::sync::watch::channel(false);
                let handle = tokio::spawn(snapshot::run_cadence(
                    read_conn.clone(),
                    snapshots_dir.clone(),
                    archive_path.clone(),
                    cadence,
                    rx,
                ));
                (Some(tx), Some(handle))
            }
            None => (None, None),
        };

        Ok(Self {
            db,
            read_conn,
            highpri_tx,
            lowpri_tx,
            clock,
            archive_path,
            snapshots_dir,
            schema_version: migrations::current_version(),
            writer: Some(writer),
            cadence_stop,
            cadence,
        })
    }

    /// Read connection handle for queries, traversals, and folds.
    pub fn read_conn(&self) -> &libsql::Connection {
        &self.read_conn
    }

    /// The clock every write is stamped with (§5.1.1).
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Schema version this handle opened against.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Cold database path, derived by convention from the main file.
    pub fn archive_path(&self) -> &Path {
        &self.archive_path
    }

    /// Snapshot directory, derived by convention from the main file.
    pub fn snapshots_dir(&self) -> &Path {
        &self.snapshots_dir
    }

    /// The underlying libSQL database, for callers that need their own connection.
    pub fn raw(&self) -> &libsql::Database {
        &self.db
    }

    // -- write surface (§5.1, Appendix A) --
    //
    // Every method here validates and canonicalises before the value crosses the
    // channel, so a bad edge type or a second-precision timestamp is a typed
    // error at the call site rather than an engine `CHECK` failure surfacing
    // from the far side of an actor with no context attached.
    //
    // NOTE (§5.1.8, D-028): awaiting one of these waits on a Rust channel, not
    // in SQLite, so `busy_timeout` does not bound it. During an in-flight
    // `rebuild_current` or `archive` the caller stalls for that transaction's
    // duration. Wrap in `tokio::time::timeout` if you need a bound — but a
    // timeout is not a cancellation: the command stays queued and commits when
    // the actor reaches it.

    /// Assert an edge (Doctrine III: a new row, never an update).
    pub async fn assert_edge(&self, edge: EdgeAssertion) -> Result<()> {
        let edge = edge.normalized()?;
        self.high(|responder| HighPriCommand::AssertEdge { edge, responder })
            .await
    }

    /// Close an open interval by asserting its replacement (Doctrine III).
    pub async fn retire_edge(
        &self,
        source: impl Into<String>,
        target: impl Into<String>,
        edge_type: impl Into<String>,
        valid_from: &str,
        valid_to: &str,
    ) -> Result<()> {
        let edge_type = edge_type.into();
        crate::graph::edge::validate_edge_type(&edge_type)?;
        let valid_from = timestamp::normalize(valid_from)?;
        let valid_to = timestamp::normalize(valid_to)?;
        let (source, target) = (source.into(), target.into());

        self.high(|responder| HighPriCommand::RetireEdge {
            source,
            target,
            edge_type,
            valid_from,
            valid_to,
            responder,
        })
        .await
    }

    /// Insert or update a concept.
    pub async fn upsert_concept(&self, concept: ConceptUpsert) -> Result<()> {
        let concept = concept.normalized()?;
        self.high(|responder| HighPriCommand::UpsertConcept { concept, responder })
            .await
    }

    /// Assert many edges in one transaction under one stamp (D-014).
    pub async fn write_bulk_atomic(&self, edges: Vec<EdgeAssertion>) -> Result<usize> {
        let edges = normalize_all(edges)?;
        self.high(|responder| HighPriCommand::WriteBulkAtomic { edges, responder })
            .await
    }

    /// Rebuild `links_current` from `links` and verify zero drift (§5.8).
    pub async fn rebuild_current(&self) -> Result<RebuildReport> {
        self.high(|responder| HighPriCommand::RebuildCurrent { responder })
            .await
    }

    /// Import edges on the background channel, chunked (D-011).
    ///
    /// Atomic *per chunk*, not overall: a failure partway leaves earlier chunks
    /// committed. That is the tradeoff [`chunk_rows`] documents — use
    /// [`Database::write_bulk_atomic`] when the batch must be all-or-nothing.
    ///
    /// Chunked at [`chunk_rows::EDGES`], which is also faster in total than the
    /// larger chunks this used through 0.5.5 (D-058).
    pub async fn bulk_import(&self, edges: Vec<EdgeAssertion>) -> Result<usize> {
        let edges = normalize_all(edges)?;
        let mut written = 0;
        for chunk in edges.chunks(chunk_rows::EDGES) {
            let chunk = chunk.to_vec();
            written += self
                .low(|responder| LowPriCommand::BulkImportChunk { chunk, responder })
                .await?;
        }
        Ok(written)
    }

    /// Upsert many **concepts** on the background channel, chunked (D-011).
    ///
    /// This is the bulk concept path, and every row it writes is a ledger write:
    /// it versions the concept and lands in `transaction_log`. Derived analytics
    /// output does not belong here — see
    /// [`Database::write_analytics_annotations`] and D-041. The name is a
    /// holdover from when the two were the same call and is due to change.
    pub async fn write_annotations(&self, concepts: Vec<ConceptUpsert>) -> Result<usize> {
        let concepts: Vec<ConceptUpsert> = concepts
            .into_iter()
            .map(ConceptUpsert::normalized)
            .collect::<Result<_>>()?;
        let mut written = 0;
        for chunk in concepts.chunks(chunk_rows::CONCEPTS) {
            let chunk = chunk.to_vec();
            written += self
                .low(|responder| LowPriCommand::WriteAnnotationsChunk { chunk, responder })
                .await?;
        }
        Ok(written)
    }

    /// State as believed at `ts` (§5.5, D-026, D-049).
    ///
    /// A read: it runs on `read_conn` and never touches the Write Actor, so a
    /// reconstruction and a full-speed write-back do not slow each other.
    ///
    /// Prefer this to calling [`crate::temporal::reconstruct`] directly. The
    /// free function takes the archive path and the snapshot directory as
    /// arguments, and a caller who passes `None` for the second gets a correct
    /// answer that folds the whole log every time — the composition is opt-in
    /// at that layer and easy to leave off by accident. Here both come from the
    /// handle, so the fast path is the default one.
    pub async fn reconstruct(&self, ts: &str) -> Result<crate::temporal::MaterializedState> {
        let ts = timestamp::normalize(ts)?;
        crate::temporal::reconstruct(
            &self.read_conn,
            &ts,
            Some(&self.archive_path),
            Some(&self.snapshots_dir),
        )
        .await
    }

    /// Create a model's embedding table and DiskANN index (§5.9, D-048).
    ///
    /// Idempotent: registering a model that already exists at the same
    /// dimension succeeds, and at a different dimension fails with
    /// [`DbError::DimMismatch`] naming both, rather than no-opping through
    /// `IF NOT EXISTS` and leaving the caller believing the dimension they
    /// asked for is the one in force.
    ///
    /// This issues DDL, which everywhere else in the crate is the migration
    /// runner's exclusive business (D-032). The exception is bounded and
    /// deliberate: a model's table is created once, by an explicit call, and
    /// the alternative — a caller-supplied write connection — is the very thing
    /// the Write Actor exists to make impossible.
    ///
    /// # Latency
    ///
    /// One small transaction, but it queues like any other write: see §5.1.8.
    pub async fn register_model(&self, model: &ModelName, dim: usize) -> Result<()> {
        let model = model.clone();
        self.high(|responder| HighPriCommand::RegisterModel {
            model,
            dim,
            responder,
        })
        .await
    }

    /// Store or replace vectors for `model`, chunked (§5.9, D-011, D-048).
    ///
    /// The write path for embeddings. Before 0.5.4 there was none:
    /// [`crate::vector::upsert_embedding`] takes a raw connection, `read_conn`
    /// is `query_only`, and the write connection lives inside the actor — so an
    /// application could search vectors it had no way to store.
    ///
    /// Low priority and chunked at [`chunk_rows::EMBEDDINGS`], because embedding
    /// is bulk derived work: a 50,000-vector backfill must yield to an
    /// interactive assertion at every chunk boundary. That constant is the
    /// smallest of the four by a wide margin — DiskANN index maintenance makes an
    /// embedding the most expensive row in the system (D-058). Atomic per chunk, not overall, which
    /// is the same trade [`Database::bulk_import`] makes and is safer here than
    /// there — an embedding is derived (Doctrine VII), so a partially written
    /// batch is recoverable by re-embedding.
    ///
    /// Fails with [`DbError::ModelNotRegistered`] if `model` has no table, and
    /// [`DbError::DimMismatch`] if a vector's length is not the declared
    /// dimension. The dimension is read from the schema once per chunk (D-037):
    /// the crate keeps no registry of its own to fall out of date.
    pub async fn upsert_embeddings(
        &self,
        model: &ModelName,
        rows: Vec<(String, Vec<f32>)>,
    ) -> Result<usize> {
        let mut written = 0;
        for chunk in rows.chunks(chunk_rows::EMBEDDINGS) {
            let chunk = chunk.to_vec();
            let model = model.clone();
            written += self
                .low(|responder| LowPriCommand::UpsertEmbeddingChunk {
                    model,
                    chunk,
                    responder,
                })
                .await?;
        }
        Ok(written)
    }

    /// Reconstruct the concept-text search index from the ledger (§5.9, D-036).
    ///
    /// The FTS index is derivative: D-036 promises every derivative table can be
    /// rebuilt from the ledger tables, and this is that promise made callable
    /// for `concepts_fts`. Needed after a restore that skipped the shadow
    /// tables, or if the index is ever suspected of drifting from the text —
    /// and, as a matter of policy, cheaper to run than to reason about.
    ///
    /// The work is `INSERT INTO concepts_fts(concepts_fts) VALUES('rebuild')`,
    /// which is FTS5's own operation over the content table, so this is not a
    /// second implementation of the sync triggers that could disagree with them.
    pub async fn rebuild_fts(&self) -> Result<()> {
        self.low(|responder| LowPriCommand::RebuildFts { responder })
            .await
    }

    /// Write derived analytics results on the background channel, chunked
    /// (§5.4, D-041).
    ///
    /// Rows go to `analytics_annotations`, which has no log trigger, so nothing
    /// written here reaches `transaction_log` and nothing here versions a
    /// concept. Rerunning an algorithm replaces the previous pass rather than
    /// recording that the world changed.
    ///
    /// Low priority and chunked at [`chunk_rows::ANNOTATIONS`] — the largest of
    /// the four, because this is the only bulk table carrying no triggers at all
    /// and its rows are correspondingly cheap (D-058) — so a 50,000-label Louvain
    /// save yields to interactive writes at every chunk boundary and carries the
    /// per-chunk fidelity boundary of §5.1.6 — a partially written pass is
    /// recoverable by rerunning, which is the property that makes derived state
    /// safe to write this way and assertions not.
    pub async fn write_analytics_annotations(
        &self,
        annotations: Vec<Annotation>,
    ) -> Result<usize> {
        let mut written = 0;
        for chunk in annotations.chunks(chunk_rows::ANNOTATIONS) {
            let chunk = chunk.to_vec();
            written += self
                .low(|responder| LowPriCommand::WriteAnalyticsChunk { chunk, responder })
                .await?;
        }
        Ok(written)
    }

    /// Move closed intervals and superseded log rows older than `cutoff` to the
    /// cold database (§5.7, D-012).
    pub async fn archive(&self, cutoff: &str) -> Result<ArchiveReport> {
        let cutoff = timestamp::normalize(cutoff)?;
        let archive_path = self.archive_path.clone();
        self.low(|responder| LowPriCommand::Archive {
            cutoff,
            archive_path,
            responder,
        })
        .await
    }

    /// Send a high-priority command and wait for its answer.
    ///
    /// The two error mappings here are the whole reason this helper exists.
    /// `send` failing means the actor is gone — `WriterUnavailable`. The
    /// responder being dropped without an answer means the actor took the
    /// command and never replied — `WriterDroppedResponder`, which is a bug in
    /// the actor rather than a condition the caller can retry. Both variants
    /// existed in `error.rs` from 0.4.5 and neither was ever constructed, so a
    /// dead actor and a hung one were both just a caller waiting forever.
    async fn high<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T>>) -> HighPriCommand,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.highpri_tx
            .send(make(tx))
            .await
            .map_err(|_| DbError::WriterUnavailable)?;
        rx.await.map_err(|_| DbError::WriterDroppedResponder)?
    }

    async fn low<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T>>) -> LowPriCommand,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.lowpri_tx
            .send(make(tx))
            .await
            .map_err(|_| DbError::WriterUnavailable)?;
        rx.await.map_err(|_| DbError::WriterDroppedResponder)?
    }

    /// Clean shutdown: stop the Write Actor, then write the final snapshot (§5.1.7).
    ///
    /// Order matters. The snapshot is taken *after* the actor has stopped and
    /// been joined, so no write can land between the fold and the file — the
    /// anchor it records is the last thing that happened, not the last thing
    /// that happened to be visible.
    ///
    /// A failed snapshot is reported rather than swallowed. It is not a
    /// durability loss — the ledger is in the WAL and the log replays without
    /// it — but it means the next open starts from an older anchor, and a caller
    /// that never hears about it cannot know why startup got slower.
    ///
    /// **The cadence stops first (§5.5, D-053).** Both it and `write_final` end
    /// by running retention over the snapshot directory, and retention deletes
    /// files. Letting them overlap would mean one pass enumerating the directory
    /// while the other removes from it — not a correctness problem for the
    /// ledger, which is why the ordering is stated rather than locked, but a
    /// source of spurious warnings and of a final anchor that could be deleted
    /// by a cleanup that started before it existed. Stopping the cadence, then
    /// the actor, then taking the snapshot leaves exactly one writer at each
    /// step.
    pub async fn close(mut self) -> Result<()> {
        if let Some(stop) = self.cadence_stop.take() {
            let _ = stop.send(true);
        }
        if let Some(handle) = self.cadence.take() {
            let _ = handle.await;
        }

        let (tx, rx) = oneshot::channel();
        let _ = self
            .highpri_tx
            .send(HighPriCommand::Shutdown { responder: tx })
            .await;
        let _ = rx.await;

        if let Some(handle) = self.writer.take() {
            let _ = handle.await;
        }

        let ts = self.clock.now();
        let archive = self
            .archive_path
            .exists()
            .then_some(self.archive_path.as_path());
        snapshot::write_final(&self.read_conn, &self.snapshots_dir, &ts, archive).await?;

        Ok(())
    }
}

fn normalize_all(edges: Vec<EdgeAssertion>) -> Result<Vec<EdgeAssertion>> {
    edges.into_iter().map(EdgeAssertion::normalized).collect()
}

/// Identical pragma configuration on every connection.
async fn configure(conn: libsql::Connection) -> Result<libsql::Connection> {
    // NOTE: `journal_mode` and `busy_timeout` return their resulting value as a
    // row, and libsql's `execute()` rejects any statement that yields rows
    // ("Execute returned rows"). They must be issued through `query()`.
    let _ = conn.query("PRAGMA journal_mode = WAL", ()).await?;
    let _ = conn.query("PRAGMA busy_timeout = 5000", ()).await?;
    conn.execute("PRAGMA synchronous = NORMAL", ()).await?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    conn.execute("PRAGMA recursive_triggers = OFF", ()).await?;
    Ok(conn)
}

/// Helper to derive the snapshot directory by convention: foo.db -> foo_snapshots/
fn derive_snapshots_dir(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("macrame");
    dir.set_file_name(format!("{stem}_snapshots"));
    dir
}

/// Helper to derive archive database path by convention: foo.db -> foo_archive.db
fn derive_archive_path(path: &Path) -> PathBuf {
    let mut archive = path.to_path_buf();
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("db");
        archive.set_file_name(format!("{stem}_archive.{ext}"));
    } else {
        archive.set_extension("archive.db");
    }
    archive
}

/// Dedicated Write Actor event loop prioritizing high-priority UI requests over low-priority background work.
async fn run_writer_actor(
    conn: libsql::Connection,
    clock: Arc<dyn Clock>,
    mut highpri_rx: mpsc::Receiver<HighPriCommand>,
    mut lowpri_rx: mpsc::Receiver<LowPriCommand>,
) -> Result<()> {
    loop {
        let ctl = tokio::select! {
            biased;
            Some(cmd) = highpri_rx.recv() => cmd.execute(&conn, &*clock).await,
            Some(cmd) = lowpri_rx.recv()  => cmd.execute(&conn, &*clock).await,
            else => LoopCtl::Break,
        };
        if matches!(ctl, LoopCtl::Break) {
            break;
        }
    }
    Ok(())
}

const INSERT_LINK: &str = "INSERT INTO links \
     (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

/// Shared by the single-concept write and the chunked one, so the two paths
/// cannot drift into upserting different column sets — and so the chunk has a
/// statement text it can prepare once (D-056).
const UPSERT_CONCEPT: &str = "INSERT INTO concepts \
     (id, title, content, embedding_model, valid_from, valid_to, recorded_at, retired) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
     ON CONFLICT(id) DO UPDATE SET \
         title = excluded.title, \
         content = excluded.content, \
         embedding_model = excluded.embedding_model, \
         valid_from = excluded.valid_from, \
         valid_to = excluded.valid_to, \
         recorded_at = excluded.recorded_at, \
         retired = excluded.retired";

/// The parameter row for [`UPSERT_CONCEPT`], in one place for the same reason.
fn concept_params<'a>(concept: &'a ConceptUpsert, stamp: &'a str) -> [libsql::Value; 8] {
    [
        concept.id.as_str().into(),
        concept.title.as_str().into(),
        concept.content.as_str().into(),
        concept
            .embedding_model
            .as_deref()
            .map_or(libsql::Value::Null, Into::into),
        concept.valid_from.as_str().into(),
        concept.valid_to.as_str().into(),
        stamp.into(),
        (concept.retired as i64).into(),
    ]
}

impl HighPriCommand {
    /// Run one command and answer its caller.
    ///
    /// Deliberately exhaustive — there is no `_` arm. The 0.4.5–0.5.4 actor
    /// matched `Shutdown` and `AssertEdge` and sent everything else to
    /// `_ => LoopCtl::Continue`, which **dropped the responder**: the caller's
    /// `rx.await` resolved to a `RecvError` that no code mapped, so four of six
    /// commands were indistinguishable from a hung database. An exhaustive match
    /// makes that failure a compile error instead of a runtime silence, which is
    /// why adding a variant should break this function.
    async fn execute(self, conn: &libsql::Connection, clock: &dyn Clock) -> LoopCtl {
        match self {
            HighPriCommand::Shutdown { responder } => {
                let _ = responder.send(Ok(()));
                return LoopCtl::Break;
            }
            HighPriCommand::AssertEdge { edge, responder } => {
                let stamp = clock.now();
                let res = match conn
                    .execute(
                        INSERT_LINK,
                        libsql::params![
                            edge.source.as_str(),
                            edge.target.as_str(),
                            edge.edge_type.as_str(),
                            edge.valid_from.as_str(),
                            edge.valid_to.as_str(),
                            edge.weight,
                            edge.properties.as_str(),
                            stamp.as_str()
                        ],
                    )
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(e) => Err(classify(
                        conn,
                        e,
                        WriteOp::Edge {
                            source_id: &edge.source,
                            target_id: &edge.target,
                            edge_type: &edge.edge_type,
                        },
                    )
                    .await),
                };
                let _ = responder.send(res);
            }
            HighPriCommand::RetireEdge {
                source,
                target,
                edge_type,
                valid_from,
                valid_to,
                responder,
            } => {
                let stamp = clock.now();
                let res = retire_edge(
                    conn, &source, &target, &edge_type, &valid_from, &valid_to, &stamp,
                )
                .await;
                let _ = responder.send(res);
            }
            HighPriCommand::UpsertConcept { concept, responder } => {
                let stamp = clock.now();
                let res = upsert_concept(conn, &concept, &stamp).await;
                let _ = responder.send(res);
            }
            HighPriCommand::WriteBulkAtomic { edges, responder } => {
                // One stamp for the whole batch (D-014): the rows were asserted
                // by one act, and giving them different transaction times would
                // invent an ordering the caller never expressed.
                let stamp = clock.now();
                let res = write_edges_atomic(conn, &edges, &stamp).await;
                let _ = responder.send(res);
            }
            HighPriCommand::RebuildCurrent { responder } => {
                let _ = responder.send(rebuild_current(conn).await);
            }
            HighPriCommand::RegisterModel {
                model,
                dim,
                responder,
            } => {
                let _ = responder.send(crate::vector::register_model(conn, &model, dim).await);
            }
        }
        LoopCtl::Continue
    }
}

impl LowPriCommand {
    /// Run one background command and answer its caller.
    ///
    /// Also exhaustive. The pre-0.5.4 version was a single `LoopCtl::Continue`
    /// for *every* variant — every background write silently discarded, its
    /// caller waiting forever.
    async fn execute(self, conn: &libsql::Connection, clock: &dyn Clock) -> LoopCtl {
        match self {
            LowPriCommand::BulkImportChunk { chunk, responder } => {
                // A stamp per chunk, not per batch: the chunks commit
                // separately, so a shared stamp would claim a simultaneity the
                // storage does not have.
                let stamp = clock.now();
                let _ = responder.send(write_edges_atomic(conn, &chunk, &stamp).await);
            }
            LowPriCommand::WriteAnnotationsChunk { chunk, responder } => {
                let stamp = clock.now();
                let _ = responder.send(write_concepts_atomic(conn, &chunk, &stamp).await);
            }
            LowPriCommand::WriteAnalyticsChunk { chunk, responder } => {
                let stamp = clock.now();
                let _ = responder.send(write_annotations_atomic(conn, &chunk, &stamp).await);
            }
            LowPriCommand::UpsertEmbeddingChunk {
                model,
                chunk,
                responder,
            } => {
                // No clock reading: an embedding carries no timestamp on either
                // axis. It is a derived artifact of a model applied to content
                // (Doctrine VII), and the ledger already records when the
                // content changed.
                let _ = responder
                    .send(crate::vector::search::upsert_embedding_chunk(conn, &model, &chunk).await);
            }
            LowPriCommand::Archive {
                cutoff,
                archive_path,
                responder,
            } => {
                let _ = responder.send(archive(conn, &cutoff, &archive_path).await);
            }
            LowPriCommand::RebuildFts { responder } => {
                let res = conn
                    .execute(crate::schema::ddl::REBUILD_CONCEPTS_FTS, ())
                    .await
                    .map(|_| ())
                    .map_err(Into::into);
                let _ = responder.send(res);
            }
        }
        LoopCtl::Continue
    }
}

/// Close an open interval by asserting its successor (Doctrine III).
///
/// Never an `UPDATE`. The replacement row copies weight and properties from
/// current belief and differs only in `valid_to` and `recorded_at`, so the
/// original assertion survives intact and `reconstruct` at an earlier instant
/// still sees the interval open — which is the entire point of a bitemporal
/// ledger.
async fn retire_edge(
    conn: &libsql::Connection,
    source: &str,
    target: &str,
    edge_type: &str,
    valid_from: &str,
    valid_to: &str,
    stamp: &str,
) -> Result<()> {
    let affected = conn
        .execute(
            "INSERT INTO links \
                 (source_id, target_id, edge_type, valid_from, valid_to, weight, properties, recorded_at) \
             SELECT source_id, target_id, edge_type, valid_from, ?5, weight, properties, ?6 \
             FROM links_current \
             WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 AND valid_from = ?4",
            libsql::params![source, target, edge_type, valid_from, valid_to, stamp],
        )
        .await
        .map_err(DbError::Engine)?;

    if affected == 0 {
        return Err(DbError::NotFound(format!(
            "{source} -> {target} ({edge_type}) at {valid_from}"
        )));
    }
    Ok(())
}

async fn upsert_concept(
    conn: &libsql::Connection,
    concept: &ConceptUpsert,
    stamp: &str,
) -> Result<()> {
    let res = conn
        .execute(UPSERT_CONCEPT, concept_params(concept, stamp))
        .await;

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(classify(
            conn,
            e,
            WriteOp::Concept {
                id: &concept.id,
                recorded_at: stamp,
            },
        )
        .await),
    }
}

/// Write every edge or none, under a single stamp.
///
/// **The statement is prepared once for the whole chunk (§9, D-056).** It used to
/// be `tx.execute(INSERT_LINK, …)` per row, which re-prepares on every call — and
/// `links` carries two triggers, so each preparation compiles their bodies along
/// with the insert.
///
/// Measured at 500 rows: **≈62 ms → ≈37 ms, a 41% saving.** Preparation was a
/// large cost and *not* the dominant one, which the first guess had it as. The
/// residual is the triggers themselves: the same 500 rows with
/// `trg_links_log_insert` and `trg_links_current_sync` dropped commit in **2.96
/// ms**, so trigger amplification is ~92% of what remains. There is no further
/// win available here without changing what the ledger records, and Doctrine IV
/// is what says it must be recorded. See D-056 for what that implies about §9's
/// ≤ 3 ms budget — briefly, 2.96 ms *is* the un-amplified figure, so the budget
/// appears to have been set without the amplification its own preamble says is
/// included.
///
/// `reset()` between rows is not optional: libsql's `execute` binds and steps
/// without resetting, so a reused statement must be returned to its initial state
/// or the second row steps a completed statement.
async fn write_edges_atomic(
    conn: &libsql::Connection,
    edges: &[EdgeAssertion],
    stamp: &str,
) -> Result<usize> {
    if edges.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    let stmt = tx.prepare(INSERT_LINK).await?;

    for edge in edges {
        stmt.reset();
        let res = stmt
            .execute(libsql::params![
                edge.source.as_str(),
                edge.target.as_str(),
                edge.edge_type.as_str(),
                edge.valid_from.as_str(),
                edge.valid_to.as_str(),
                edge.weight,
                edge.properties.as_str(),
                stamp
            ])
            .await;

        if let Err(e) = res {
            let typed = classify(
                &tx,
                e,
                WriteOp::Edge {
                    source_id: &edge.source,
                    target_id: &edge.target,
                    edge_type: &edge.edge_type,
                },
            )
            .await;
            // Released before the rollback: a live statement on the connection
            // is exactly what makes SQLite refuse to end a transaction.
            drop(stmt);
            let _ = tx.rollback().await;
            return Err(typed);
        }
    }

    drop(stmt);
    tx.commit().await?;
    Ok(edges.len())
}

/// Write every concept or none, under a single stamp.
/// Upsert one chunk of derived annotations in a single transaction (D-041).
///
/// `stamp` is the actor's clock reading, exactly as for every other chunk — but
/// it lands in `computed_at`, not in a `recorded_at`, and the difference is not
/// cosmetic. `recorded_at` is the transaction-time axis and is subject to
/// Doctrine II and the monotonicity guard; `computed_at` is a note about when a
/// derivation last ran, on a table the ledger does not see. Rerunning an
/// algorithm therefore replaces the row and advances the note, rather than
/// versioning a concept the world did not change.
async fn write_annotations_atomic(
    conn: &libsql::Connection,
    annotations: &[Annotation],
    stamp: &str,
) -> Result<usize> {
    if annotations.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    let stmt = tx
        .prepare(
            "INSERT INTO analytics_annotations (concept_id, label, value, computed_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(concept_id, label) DO UPDATE SET \
                 value = excluded.value, computed_at = excluded.computed_at",
        )
        .await?;

    for a in annotations {
        stmt.reset();
        let res = stmt
            .execute(libsql::params![
                a.concept_id.as_str(),
                a.label.as_str(),
                a.value.as_str(),
                stamp
            ])
            .await;
        if let Err(e) = res {
            drop(stmt);
            let _ = tx.rollback().await;
            return Err(DbError::Engine(e));
        }
    }

    drop(stmt);
    tx.commit().await?;
    Ok(annotations.len())
}

async fn write_concepts_atomic(
    conn: &libsql::Connection,
    concepts: &[ConceptUpsert],
    stamp: &str,
) -> Result<usize> {
    if concepts.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // Prepared once, like the edge chunk (D-056). This no longer routes through
    // [`upsert_concept`] — that function prepares per call by construction — but
    // it shares that function's statement text and parameter row, so the two
    // cannot upsert different columns.
    let stmt = tx.prepare(UPSERT_CONCEPT).await?;

    for concept in concepts {
        stmt.reset();
        let res = stmt.execute(concept_params(concept, stamp)).await;

        if let Err(e) = res {
            let typed = classify(
                &tx,
                e,
                WriteOp::Concept {
                    id: &concept.id,
                    recorded_at: stamp,
                },
            )
            .await;
            drop(stmt);
            let _ = tx.rollback().await;
            return Err(typed);
        }
    }

    drop(stmt);
    tx.commit().await?;
    Ok(concepts.len())
}
