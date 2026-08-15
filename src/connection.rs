use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::error::{classify, DbError, Result, WriteOp};
use crate::graph::edge::EdgeAssertion;
use crate::integrity::{rebuild_current, RebuildReport};
use crate::schema::migrations;
use crate::temporal::archive::{archive, rehydrate, ArchiveReport, RehydrateReport};
use crate::temporal::interval::Interval;
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
/// 90-edge chunk takes **9.06 ms** into an 8,000-edge table. So the bound is met
/// as measured here and *not* met on a populated database.
///
/// That gap was published as 47.7 ms until 0.10.0 and attributed to the schema
/// defect D-059 documents. The defect was fixed by the `v5 → v6` rung and the
/// figure was never updated. 9.08 ms is a 0.10.0 measurement, not D-059's 8.0 ms
/// carried forward: `chunk_budget` gained a seeded arm, because until it did,
/// nothing in the bench suite wrote a chunk into a populated table and this
/// number was unfalsifiable. It agrees with D-059 once the session is accounted
/// for — the empty arm read 2.69 and 2.65 ms beside it against the 2.39 ms
/// published above, so the *ratio* is 3.4× here and 3.35× there.
///
/// **The residual is attributed as of 0.11.0 (D-142).** It is not the missing
/// index, which shipped in 0.5.6; it is the `links_current` write. Dropping the
/// three `links` insert triggers one at a time puts effectively all of the
/// growth in `trg_links_current_sync` — the single-open guard contributes none,
/// the log trigger and the base insert ~0.35 ms of a 4.15 ms rise — and within
/// that trigger, 89% of the growth is maintenance of `idx_lc_traversal_cover`
/// and `idx_lc_open_interval` rather than the upsert itself, which costs 0.49 ms
/// run directly against the same table. Page-cache size, foreign keys and the
/// fixture's key distribution were each tested and are each not the cause.
///
/// Knowing the cause does not by itself change the constant: the expensive index
/// is D-042's covering index for the traversal, so narrowing it moves cost onto
/// the read path it exists to protect. Re-deriving these constants against the
/// D-088 fixture matrix is the named successor.
///
/// # These are ceilings as of 0.12.0, not sizes
///
/// D-143 re-derived all four against the D-088 matrix and the edge path came
/// back **20** against a shipped 90 — and 20 would have been wrong at 80,000
/// edges for the same reason 90 is wrong at 8,000, because per-row cost there
/// grows with `links_current`. The finding was that no row count can bound a
/// duration on such a path.
///
/// So the chunk loop stopped trying to pick one ahead of time. Each chunk is
/// timed by the actor and its measured hold chooses the next size; these
/// constants are the **largest** size that will ever be asked for, and every
/// derivation below still applies to them as such. A path may run well under its
/// constant on a populated database and at exactly it on an empty one, and both
/// are the bound being met rather than a size being missed.
pub mod chunk_rows {
    /// Edge assertions (`bulk_import`).
    ///
    /// Per-row cost on this path rises with the size of `links_current`, not
    /// with the chunk (D-059) — so cutting the chunk buys latency and costs
    /// throughput, ~11% for 1,000 edges. An earlier version of this comment
    /// claimed it was 3.3× *faster*; that came from multiplying eleven copies of
    /// a chunk measured into an empty database.
    ///
    /// **This size does not meet the 3 ms bound on a populated database.** 90
    /// edges into an 8,000-edge table take **9.06 ms** — measured, two sessions
    /// at 9.08 and 9.05, against an empty-table arm of 2.69 and 2.65 beside
    /// them (D-136).
    ///
    /// The reason given here until 0.10.0 — that `trg_links_single_open`'s
    /// `EXISTS` scans the whole out-degree, "a schema defect with a proven fix,
    /// recorded in D-059 and not applied here" — described 0.5.5. The fix *was*
    /// applied, as the `v5 → v6` rung, and took this from 47.7 ms to ~8 ms.
    /// What survives is the miss: the bound is still exceeded ~3×. Its cause is
    /// no longer unknown — D-142 attributes it to `trg_links_current_sync`, and
    /// within that to secondary-index maintenance on `links_current` — and the
    /// guard this comment used to blame contributes **no** growth at all.
    ///
    /// **The constant is unchanged, and that is now a measured decision**
    /// (D-143). Re-derived against all four D-088 shapes at 8,000 edges, they
    /// agree that the largest size meeting the bound is **20**. It stays at 90
    /// because 20 is the same miss at a larger population — per-row cost grows
    /// with `links_current`, so a constant fitted at 8,000 edges is wrong at
    /// 80,000 — while the throughput cost of turning eleven chunks into fifty
    /// is certain and immediate (D-058). The fix is not a row count: it is for
    /// the chunk loop to stop on elapsed time, **delivered in 0.12.0**. This
    /// number is now the ceiling that loop starts from and never exceeds; on a
    /// populated table it converges below it within a chunk or two.
    ///
    /// D-134 retired the growth claim on the neighbouring *single-assertion*
    /// path and did not measure this one; D-136 is why this line now carries a
    /// measurement rather than a figure quoted from 0.5.6.
    pub const EDGES: usize = 90;

    /// Concept upserts (`write_concepts`).
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

/// What one chunk transaction cost, reported by the actor to the caller-side
/// chunk loop (0.12.0, W1).
///
/// `held` is measured **inside** the actor, around its own transaction, and
/// therefore excludes the time the command spent queued. That exclusion is the
/// point: queue time is what strict preemption *does*, and a controller fed
/// `send + await` would shrink chunks as punishment for the actor correctly
/// serving an interactive write first.
///
/// Public only because [`LowPriCommand`] is. Nothing outside this crate
/// constructs one, and it is deliberately not re-exported at the crate root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOutcome {
    /// Rows the transaction actually wrote.
    pub rows: usize,
    /// How long the actor held the write lock for them.
    pub held: std::time::Duration,
}

/// Smallest chunk the adaptive loop will fall to (0.12.0, W2).
///
/// # A floor is a deliberate, measured violation of [`CHUNK_BUDGET`]
///
/// Feedback alone converges to whatever size meets the budget, and on a
/// populated `links` table that size keeps falling — per-row cost there grows
/// with the table (D-059, D-142), so there is no size at which the *fixed* cost
/// of a transaction stops dominating. Left unbounded the loop reaches chunks of
/// one or two rows, where nearly all the work is `BEGIN`/`COMMIT` and the import
/// no longer finishes.
///
/// 35 is measured, and **re-measured against the loop that uses it** — the
/// difference matters, because the figure this constant shipped with was an
/// extrapolation. `examples/chunk_matrix.rs -- converge` runs a 900-edge
/// `bulk_import` into each of the four D-088 shapes at 8,000 edges and reports
/// the actor's own per-transaction readings. A 35-row chunk costs **3.11–3.43 ms**
/// across the four shapes, two sessions, excluding the run-up. The floor misses
/// the 3 ms bound by 0.1–0.4 ms, not by the ~1.1 ms predicted from the sweep.
///
/// The miss is **steady state** — not a one-chunk transient on the way down —
/// and the defense is the argument [`CHUNK_BUDGET`] is answerable to rather than
/// the number itself: an interactive assertion arriving at the worst moment
/// waits ~3.2 ms for the chunk in flight and then runs its own ≤ 5 ms write, so
/// ~8.2 ms against a 16.7 ms frame.
///
/// What the same measurement says about the *size*: on this path at this
/// population the loop goes `[90, 35, 35, …]` on all four shapes and never picks
/// anything between. The proportional shrink from a 90-row chunk proposes ~31
/// rows, which clamps here — so on the edge path the floor is not a safety net
/// under the controller, it **is** the operating point, and this number is
/// carrying more weight than a backstop normally would. Re-measure it, not the
/// controller, when the edge path's per-row cost changes.
const CHUNK_FLOOR: usize = 35;

/// Size of the next chunk, from what the last one cost (0.12.0, W2).
///
/// Pure on purpose — no clock, no database, no actor — so the control law can be
/// tested for the properties that matter without a fixture. Three regimes:
///
/// | last hold | response | why |
/// |---|---|---|
/// | over `budget` | shrink to `current · budget / held`, × 0.9 | back off *fast* from a bound already being exceeded; the 0.9 undershoots so the correction does not have to be repeated |
/// | under `budget / 2` | grow by a quarter of `current`, at least one row | approach the bound *slowly*; the dead band above it stops a size that is merely comfortable from oscillating |
/// | otherwise | hold | in band, and moving costs more than it buys |
///
/// The asymmetry is the whole design. Proportional shrinking converges from
/// above in one or two steps, which matters because every step over budget is a
/// latency miss a caller can feel; additive growth cannot overshoot by more than
/// 25%, which matters because the ceiling is a throughput preference and not a
/// bound.
///
/// `ceiling` is the path's [`chunk_rows`] constant, which is why those constants
/// keep their values and their derivations: they are no longer the size, they
/// are the largest size this will ever ask for. `floor` is [`CHUNK_FLOOR`] —
/// see there for the budget it knowingly misses.
///
/// Never returns 0, at any input, including `held == 0` or `current == 0`.
fn next_chunk_size(
    current: usize,
    held: std::time::Duration,
    budget: std::time::Duration,
    floor: usize,
    ceiling: usize,
) -> usize {
    let held = held.as_nanos().max(1);
    let budget_ns = budget.as_nanos().max(1);
    let current = current.max(1);

    let next = if held > budget_ns {
        // Integer math, and the `max(1)` matters: a chunk 200× over budget
        // would otherwise propose 0 and the loop would stop making progress.
        let scaled = (current as u128) * budget_ns * 9 / (held * 10);
        (scaled as usize).max(1)
    } else if held * 2 < budget_ns {
        // Saturating because `current` is a `usize` and this is the one branch
        // that adds to it. Nothing sane reaches the boundary; the clamp below
        // makes the answer correct anyway rather than a debug panic.
        current.saturating_add((current / 4).max(1))
    } else {
        current
    };

    // Applied last and unconditionally, so a caller that passes a reversed pair
    // gets the floor rather than a panic — and `max(1)` last of all, because a
    // chunk of zero rows is the single answer no loop can make progress from.
    next.clamp(floor.min(ceiling), ceiling).max(1)
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
///
/// # Some operations are exempt, and the exemption is a contract, not an oversight
///
/// This was recorded in three separate rustdoc notes and nowhere near the bound
/// itself, which is where a reader looks for its scope (§8.6). Stated here, with
/// Wave 3's measurements:
///
/// | Path | Bound | Why it cannot be chunked |
/// |---|---|---|
/// | [`Database::write_bulk_atomic`] | none — caller-sized `Vec` | D-014: the batch is *one act* under one stamp. Splitting it is the thing the method exists not to do |
/// | [`Database::archive`] | measured **26.8 ms** for 2,000 archivable edges; see [`Database::archive_windowed`] | D-012: copy-then-delete must be atomic, or a crash between the phases duplicates or loses rows |
/// | `rebuild_current` | measured **24.6 / 104 / 318 ms** at 4K / 16K / 40K rows in `links` (was "~50 s per 10M edges", which nothing had measured) | D-023: the window between `DELETE` and `INSERT` is the whole of current belief; a reader landing in it sees a graph with no edges and no error |
/// | [`Database::rehydrate`] | unmeasured; a function of how many rows the caller named | D-012 backwards: the same copy-then-delete atomicity, in the other direction. **A row here since 0.12.9 only because it was previously invisible** — rehydration reported as `archive` and inherited its exemption without anyone deciding on it (W4.3, D-152) |
/// | [`Database::checkpoint`] | a function of the WAL's size, which is a function of how long since the last checkpoint — not of anything the caller passes | It is not a transaction at all. `PRAGMA wal_checkpoint` copies frames back into the main file and there is no unit smaller than the frame it is already working in; the caller asked for exactly this, and the alternative to a long checkpoint is a WAL that keeps growing (0.12.13, W5.2, D-156) |
///
/// The `archive` figure is end-to-end through this method, so it **includes**
/// the re-derivation `archive()` runs inside its transaction — but it does not
/// attribute it, and until D-077 more than half of that re-derivation was an
/// audit comparing `links_current` against the query that had just filled it.
/// Note also which variable that cost scales with: `rebuild_within` reprojects
/// **all of `links`**, so the archive's repair term grows with the *surviving*
/// table and not with the batch being archived. A budget stated per "100K closed
/// intervals" ([§9](../docs/architecture/s6-s10-flows-to-dependencies.md)) is
/// therefore parameterised on the wrong quantity.
///
/// The first four are atomic **by contract**, which is why "cap the batch" and
/// "add a third tier" were both considered and neither was taken: capping breaks the
/// guarantee the operation exists to provide, and a third tier changes which
/// caller waits without changing how long the lock is held. What was wrong was
/// never the exemption — it was that the bound was stated as though it had none.
///
/// A caller who needs the latency bound and not the atomicity has
/// [`Database::bulk_import`], which is the same write chunked at
/// [`chunk_rows::EDGES`] and explicitly *not* atomic overall (D-011).
///
/// # One of them is no longer unbounded (T1.1, D-080)
///
/// `archive` was the worst of them, because its hold is a function of *how long
/// since the last archive* rather than of anything the caller chose.
/// [`Database::archive_windowed`] runs the same work as N sessions, each
/// atomic, each its own actor turn. Measured on an 8,000-key fixture with four
/// generations of superseded history: the longest single hold falls from
/// **3.3 s to 0.77 s** at one-hour windows, for total wall time that is flat
/// within this cycle's noise.
///
/// The same measurement at 2,000 keys goes the other way — the hold falls
/// 260 ms → 117 ms while total time rises 260 ms → 671 ms — so windowing is a
/// trade and not a free improvement. It pays when the backlog is large, which
/// is when the unwindowed hold is a problem in the first place. `archive` is
/// kept, not deprecated, for exactly that reason.
pub const CHUNK_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);

/// Predicted hold above which [`Database::write_bulk_atomic`] warns (T1.3).
///
/// 250 ms is fifteen frames at 60 Hz: not a hitch, a visible freeze. It is well
/// above [`CHUNK_BUDGET`] on purpose — this path is exempt from that bound by
/// contract, so warning at 3 ms would fire on batches that are working exactly
/// as designed and train the reader to filter the message out.
pub const BULK_ATOMIC_WARN_HOLD: std::time::Duration = std::time::Duration::from_millis(250);

/// Roughly how long [`Database::write_bulk_atomic`] will hold the actor for
/// this batch (T1.3, D-081).
///
/// # Three terms, because the cost is neither linear nor a function of size
///
/// T1.3 asks for "rows × measured per-row cost". That model is wrong twice over,
/// and both corrections came out of measuring it.
///
/// First, the cost is not linear. `write_edges_atomic` opens with
/// `reject_overlaps_within`, which compares **every pair** in the batch before a
/// row is written. Second — and this is the one that matters — the quadratic
/// term's constant depends on the batch's *shape*, not its size. The pairwise
/// loop starts with an early `continue` on mismatched `(source, target,
/// edge_type)`; pairs that share all three fall through to `Interval::new` and
/// `overlaps`, which is **sixteen times** dearer per pair.
///
/// ```text
/// hold ≈ 73 µs · rows  +  5.5 ns · mismatched pairs  +  86 ns · matching pairs
/// ```
///
/// Two batches of 20,000 edges, measured on the same machine: one fanning out to
/// distinct targets holds the actor for **2.5 s**, and one asserting 20,000
/// corrections to a single relationship's history holds it for **18.6 s**. A
/// size-only model is off by 7× between those two, in the direction that
/// matters — it under-predicts the bad case. So this counts the matching pairs
/// rather than guessing, with one `HashMap` pass over the batch. That pass is
/// O(rows) against an operation about to spend milliseconds per row.
///
/// # What this is calibrated against, and where it will be wrong
///
/// libSQL 0.9.30, one machine, best of three, over 100–20,000 rows in both
/// shapes; within 5% across that range except below ~500 rows, where fixed costs
/// dominate and it over-predicts by 3× — harmless, since nothing that small can
/// approach [`BULK_ATOMIC_WARN_HOLD`].
///
/// It is machine-specific and says nothing about disk. It exists to turn
/// "uncapped" into an order of magnitude a caller can act on — the difference
/// between 30 ms and 18 s — and should not be read more precisely than that.
/// `examples/bulk_atomic_diag.rs` prints predicted against measured, so the
/// model's drift is visible rather than assumed.
pub fn estimated_bulk_hold(edges: &[EdgeAssertion]) -> std::time::Duration {
    let rows = edges.len() as u64;
    let all_pairs = rows.saturating_mul(rows.saturating_sub(1)) / 2;

    // Pairs sharing all three key columns, which is exactly the set that reaches
    // the guard's expensive path. Grouped rather than sorted: the batch is
    // borrowed, and sorting would either clone it or reorder the caller's data.
    let mut groups: std::collections::HashMap<(&str, &str, &str), u64> =
        std::collections::HashMap::new();
    for e in edges {
        *groups
            .entry((&e.source, &e.target, &e.edge_type))
            .or_insert(0) += 1;
    }
    let matching: u64 = groups.values().map(|&g| g * (g - 1) / 2).sum();
    let mismatched = all_pairs - matching;

    // Nanoseconds throughout, saturating: a caller who passes a batch large
    // enough to overflow this has a problem the arithmetic cannot express, and
    // saturating to ~584 years still crosses every threshold above.
    std::time::Duration::from_nanos(
        (73_000u64.saturating_mul(rows))
            .saturating_add(mismatched.saturating_mul(11) / 2)
            .saturating_add(matching.saturating_mul(86)),
    )
}

/// Most sessions [`Database::archive_windowed`] will run for one call (T1.1).
///
/// A limit exists because the session count is a function of *transaction-time
/// span divided by window*, and both come from the caller — a one-second window
/// over a decade of history is ten million actor turns, each opening a
/// transaction and writing a horizon row. That is not a slow archive, it is a
/// caller who meant something else.
///
/// 4,096 is chosen against the operation it bounds rather than against a clock:
/// at the measured 26.8 ms for a session with work in it, a full run of this
/// many is about two minutes of background writing, and the whole point of
/// windowing is that those two minutes are interruptible. It is a refusal
/// rather than a clamp — see [`DbError::ArchiveWindow`] for why.
pub const MAX_ARCHIVE_SESSIONS: usize = 4_096;

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
        crate::util::ids::validate_id(&self.id)?;
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
    /// Move WAL frames back into the main database file (§4.5, F-30, D-156).
    ///
    /// High priority, and for once the reason is not latency: a caller asking
    /// for a checkpoint is asking for it *now*, usually at the end of a bulk
    /// load or before taking a copy of the file, and queueing it behind the
    /// background work it was meant to follow inverts the intent. It is also
    /// the only command here that is not a transaction.
    Checkpoint {
        responder: oneshot::Sender<Result<CheckpointReport>>,
    },
    Shutdown {
        responder: oneshot::Sender<Result<()>>,
    },
}

/// What `PRAGMA wal_checkpoint` returned (0.12.13, W5.2, D-156).
///
/// The three columns SQLite gives back, named, rather than `()` — a checkpoint
/// that did nothing and a checkpoint that reclaimed a 400 MB WAL are the same
/// `Ok(())`, and the difference is the entire reason a caller asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointReport {
    /// `true` when SQLite could not complete the requested mode because a
    /// reader or writer was in the way.
    ///
    /// **This is not an error, and it is not ignorable.** `TRUNCATE` waits for
    /// readers only as long as `busy_timeout` allows; past that it gives up and
    /// says so, having possibly still copied frames. A caller checkpointing
    /// before copying the file away must read this, because a busy checkpoint
    /// means the main file is not self-contained yet.
    pub busy: bool,
    /// Frames left in the WAL at the end. `0` when the checkpoint completed,
    /// since the mode run is `TRUNCATE`.
    pub log_frames: u64,
    /// Frames moved back into the database file.
    ///
    /// Read from a `FULL` pass rather than from the `TRUNCATE` — see
    /// `run_checkpoint` for why a truncating checkpoint cannot report this
    /// number itself.
    pub checkpointed_frames: u64,
}

impl CheckpointReport {
    /// The WAL was fully reclaimed: nothing blocked, and nothing is left.
    pub fn is_complete(&self) -> bool {
        !self.busy && self.log_frames == 0
    }
}

/// Commands sent to the Write Actor on the low-priority channel (background work).
pub enum LowPriCommand {
    /// One chunk of **concepts** — a ledger write, logged and versioned.
    WriteConceptsChunk {
        chunk: Vec<ConceptUpsert>,
        responder: oneshot::Sender<Result<ChunkOutcome>>,
    },
    /// One chunk of **derived annotations** — off-ledger, no log trigger (D-041).
    ///
    /// The pair is named apart deliberately: this variant was `WriteAnalyticsChunk`
    /// beside a `WriteAnnotationsChunk` that carried concepts, which is the
    /// crossing D-075 undid.
    WriteAnalyticsChunk {
        chunk: Vec<Annotation>,
        responder: oneshot::Sender<Result<ChunkOutcome>>,
    },
    /// One chunk of vectors for one model (§5.9, D-048).
    ///
    /// Low priority: embedding is bulk derived work and must never preempt an
    /// interactive assertion.
    UpsertEmbeddingChunk {
        model: ModelName,
        chunk: Vec<(String, Vec<f32>)>,
        responder: oneshot::Sender<Result<ChunkOutcome>>,
    },
    BulkImportChunk {
        chunk: Vec<EdgeAssertion>,
        responder: oneshot::Sender<Result<ChunkOutcome>>,
    },
    Archive {
        cutoff: String,
        archive_path: PathBuf,
        responder: oneshot::Sender<Result<ArchiveReport>>,
    },
    /// Move named concepts back out of the cold file (0.9.0, C3).
    ///
    /// Low priority for the same reason `Archive` is: it is bulk physical
    /// movement with no latency bound, and it holds the write lock for its whole
    /// transaction.
    Rehydrate {
        ids: Vec<String>,
        archive_path: PathBuf,
        responder: oneshot::Sender<Result<RehydrateReport>>,
    },
    /// Reconstruct the FTS index from `concepts` (§5.9, D-036, D-051).
    ///
    /// Low priority: it is maintenance on a derivative table, and a search index
    /// that is a few seconds stale is a smaller cost than an interactive write
    /// that waits behind a full reindex.
    RebuildFts {
        responder: oneshot::Sender<Result<()>>,
    },
    /// Refresh or top up the query planner's statistics (0.12.4, D-149).
    ///
    /// Low priority, and not a close call: statistics being a few seconds stale
    /// costs a plan that was already the plan a moment ago, where preempting an
    /// interactive assertion costs a caller their latency bound. It is a write —
    /// it writes `sqlite_stat1` — so it takes the write lock like anything else,
    /// and `PRAGMA analysis_limit` in `configure` is what keeps the hold a
    /// function of the index count instead of the table size.
    Analyze {
        /// `true` runs `PRAGMA optimize`, which re-analyses only what SQLite
        /// believes has gone stale; `false` runs `ANALYZE` unconditionally.
        incremental: bool,
        responder: oneshot::Sender<Result<()>>,
    },
    /// One step of a chunked shadow rebuild (§5.8, T1.2, D-082).
    ///
    /// Low priority, and one command per step rather than one per rebuild: the
    /// whole value of building beside the live table is that the actor returns
    /// here between chunks. See [`Database::rebuild_current_chunked`].
    ShadowRebuild {
        step: crate::integrity::ShadowStep,
        responder: oneshot::Sender<Result<crate::integrity::ShadowOutcome>>,
    },
}

enum LoopCtl {
    Continue,
    Break,
}

/// Primary database handle for Macrame bitemporal ledger.
pub struct Database {
    db: libsql::Database,
    /// The file this handle opened, kept so [`Database::diagnostic_conn`] can
    /// open it again under different flags (T5.1, D-091). `archive_path` and
    /// `snapshots_dir` are derived from it and were previously the only trace
    /// of it on the struct.
    path: PathBuf,
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
    /// Set by [`Database::close`]. Read only by [`Drop`], which warns when it is
    /// still false — see that impl for why the omission is worth a warning.
    closed: bool,
    /// Shared with the actor (T1.4, T1.2). Held here rather than behind
    /// `#[cfg(feature = "metrics")]` so `open_inner` has one shape; with the
    /// feature off the metrics half is a zero-sized type and only
    /// [`Database::metrics`] is gated — which is also why the field is unread in
    /// the default build: the actor holds the other `Arc` and does the writing.
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    shared: Arc<ActorShared>,
}

/// What the snapshot cadence should do, for [`Tuning::cadence`].
///
/// # Why this is not `Option<SnapshotCadence>`
///
/// [`Database::open_with_cadence`] takes `Option<SnapshotCadence>`, where `None`
/// means *no cadence at all*. Carrying that field into [`Tuning`] unchanged
/// would have made it the one field in the struct whose `None` is a request to
/// change the behaviour rather than a request to leave it alone — and since
/// `Tuning` derives `Default`, `open_tuned(path, Tuning::default())` would then
/// have silently disabled snapshots, while `open(path)` runs them. Two calls
/// that read as synonyms, one of which stops writing anchors.
///
/// So the tri-state is written out. `Default` is the default cadence, matching
/// [`Database::open`]; `Disabled` is `open_with_cadence(path, None)`, and has to
/// be asked for by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CadencePolicy {
    /// [`SnapshotCadence::default`], as [`Database::open`] uses.
    #[default]
    Default,
    /// No cadence task. `close()` is then the only thing that writes an anchor
    /// (§5.5, D-053).
    Disabled,
    /// An explicit cadence.
    Every(SnapshotCadence),
}

impl CadencePolicy {
    /// Collapse to the `Option` the open path has always taken.
    fn resolve(self) -> Option<SnapshotCadence> {
        match self {
            Self::Default => Some(SnapshotCadence::default()),
            Self::Disabled => None,
            Self::Every(cadence) => Some(cadence),
        }
    }
}

/// When SQLite should checkpoint the WAL on its own, for
/// [`Tuning::wal_autocheckpoint`] (0.12.14, W5.3, D-157).
///
/// # Why this is not `Option<u32>`
///
/// The same reason [`CadencePolicy`] is not `Option<SnapshotCadence>`, and the
/// plan for this wave specified `Option<u32>` here too. In a struct that derives
/// `Default`, a field whose `None` means *turn the mechanism off* is a field
/// that turns the mechanism off for everyone who did not mention it. Absence
/// means "leave it alone" everywhere in [`Tuning`], and disabling the automatic
/// checkpointer — which is not safe without an explicit
/// [`Database::checkpoint`] to replace it — has to be asked for by name.
///
/// **The default does not change.** 1,000 pages is SQLite's default and stays
/// SQLite's default; F-30 is a control-loop perturbation, not a correctness bug,
/// and changing a default is a behaviour change for every existing caller.
///
/// # What disabling it actually buys, measured (0.12.14, W5.3, D-157)
///
/// F-30 says the automatic checkpointer is an unbudgeted hold *inside* 0.12.0's
/// adaptive chunk controller: a checkpoint firing during a chunk transaction is
/// charged to that chunk, and since D-146 made the measured hold the input to
/// `next_chunk_size`, the controller shrinks in response to work the chunk did
/// not do. Three rounds, 6,000 concepts of 1 KB each through `write_concepts`,
/// release build:
///
/// | | longest chunk hold | mean | chunks | over budget | wall |
/// |---|---|---|---|---|---|
/// | autocheckpoint on (default) | **9.3–10.3 ms** | 2.40–2.44 ms | 125–130 | 24–28 | 304–321 ms |
/// | autocheckpoint off | **4.50 ms** | 2.08–2.20 ms | 142–153 | 18–27 | 298–339 ms |
///
/// **The tail is the finding, and it is real and reproducible.** The longest
/// hold roughly halves, and the >10 ms histogram bucket is populated only with
/// the checkpointer on — that bucket is the checkpoint, landing inside somebody
/// else's transaction and being charged to it. Every round agrees.
///
/// **What it does not buy is a calmer controller.** `over_budget` overlaps
/// between the arms, and total wall time is the same within noise. The
/// controller works near the budget boundary either way, because
/// [D-090](../docs/architecture/s13-decision-register.md)'s ~0.8 ms
/// per-transaction floor and the convergence cost do not go anywhere. So the
/// honest statement is that disabling autocheckpoint removes an outlier, not an
/// oscillation.
///
/// **And the cost is deferred, not removed.** The explicit
/// [`Database::checkpoint`] at the end of the same fixture moved **8,400–9,100
/// frames in 41–45 ms** with the checkpointer off, against **~860 frames in
/// 5.5–6.2 ms** with it on. That is the whole trade in one line: the same work,
/// moved out of the latency-bounded path and into one hold the caller chose the
/// moment for. It is a good trade for a bulk importer and a bad one for an
/// interactive process, which is why this is a knob and not a new default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WalCheckpointPolicy {
    /// SQLite's own default: checkpoint once the WAL passes 1,000 pages.
    #[default]
    Default,
    /// No automatic checkpointing.
    ///
    /// **Only correct if you call [`Database::checkpoint`] yourself.** Without
    /// one, the WAL grows for the life of the process and the database file is
    /// never brought up to date.
    Disabled,
    /// Checkpoint once the WAL passes this many pages.
    ///
    /// `0` is not special-cased to [`Self::Disabled`] even though SQLite treats
    /// it that way, because a caller who computed a threshold and got zero has
    /// a bug, and inheriting SQLite's overload would turn it into a silently
    /// unbounded WAL.
    EveryPages(u32),
}

impl WalCheckpointPolicy {
    /// The pragma to run, or `None` to leave the connection at SQLite's
    /// default.
    fn pragma(self) -> Option<String> {
        match self {
            Self::Default => None,
            Self::Disabled => Some("PRAGMA wal_autocheckpoint = 0".to_string()),
            Self::EveryPages(pages) => Some(format!("PRAGMA wal_autocheckpoint = {pages}")),
        }
    }
}

/// Everything [`Database::open_tuned`] can be told, in one growable struct
/// (0.12.12, W5.1, D-155).
///
/// # Why a struct rather than a fourth constructor
///
/// There were three — [`Database::open`], [`Database::open_with_cadence`],
/// [`Database::open_with_clock`] — and each new knob added one more, with the
/// combinatorics of the ones before it. 0.13.0 alone wanted three knobs
/// (`wal_autocheckpoint`, and a page cache each for the writer and the
/// readers), which is the point at which the naming stops being possible.
///
/// **`Default` plus functional update is the whole design.** They make a new
/// knob an additive change: callers construct with `..Default::default()` and
/// keep compiling, and the fields that arrive after them are the ones they did
/// not ask about. That is not a hypothetical — W5.1 ships this struct with two
/// fields, and W5.3/W5.4 add the three tuning knobs to it without touching a
/// caller.
///
/// # Why this is *not* `#[non_exhaustive]`
///
/// The plan for this wave specified `#[non_exhaustive]` alongside `Default`,
/// on the usual reasoning that the attribute is what makes a struct growable.
/// It does not compile: a `#[non_exhaustive]` **struct** cannot be built with
/// literal syntax outside its own crate *at all*, and the functional-update
/// form is literal syntax, so `Tuning { cadence, ..Default::default() }` is
/// `E0639` for every external caller — the exact expression the attribute was
/// added to protect. (The rule differs from `#[non_exhaustive]` on an enum,
/// which only forces a wildcard arm; [`CadencePolicy`] keeps it for that
/// reason.) The two ways to have both are a builder with setters, or plain
/// `Default` — and `Default` is chosen because the field-literal form is the
/// legible one, and because the growth this needs to survive is *additive*
/// fields, which `..Default::default()` already absorbs.
///
/// The cost is real and worth stating: a caller who writes an exhaustive
/// literal, with no `..Default::default()`, breaks when a field is added. That
/// is a compile error at the call site with an obvious fix, not a silent
/// behaviour change, and it is the price of the readable form.
///
/// # The three constructors stay
///
/// They delegate here and are not deprecated. `open(path)` is the right call for
/// most callers and should not acquire a warning for being the common case; the
/// consolidation is about where the *next* knob goes, not about moving anyone.
///
/// ```no_run
/// # use macrame::prelude::*;
/// # async fn f() -> macrame::Result<()> {
/// let db = Database::open_tuned(
///     "graph.db",
///     Tuning {
///         cadence: CadencePolicy::Disabled,
///         ..Default::default()
///     },
/// )
/// .await?;
/// # Ok(()) }
/// ```
#[derive(Clone, Default)]
pub struct Tuning {
    /// What the snapshot cadence should do. Defaults to
    /// [`SnapshotCadence::default`], as [`Database::open`] does.
    pub cadence: CadencePolicy,
    /// A clock to stamp `recorded_at` with, for tests (§5.1.2, D-062). `None`
    /// is [`SystemClock`]. Floored against the database exactly as
    /// [`Database::open_with_clock`] describes — read that before injecting
    /// one against a non-empty file.
    pub clock: Option<Arc<dyn Clock>>,
    /// When SQLite checkpoints the WAL on its own (0.12.14, W5.3, F-30).
    ///
    /// Applied to the **write connection**, which is the only connection in
    /// this crate that commits, and therefore the only one whose autocheckpoint
    /// setting can ever fire. Pair [`WalCheckpointPolicy::Disabled`] with an
    /// explicit [`Database::checkpoint`] or the WAL grows without bound.
    pub wal_autocheckpoint: WalCheckpointPolicy,
    /// Page cache for the **write** connection, as SQLite's `cache_size`
    /// (0.12.15, W5.4).
    ///
    /// `None` leaves SQLite's default of −2000, which is −2000 *kibibytes*, or
    /// 2 MB. **Negative values are KiB and positive values are pages** — that
    /// is SQLite's convention and it is preserved rather than smoothed over,
    /// because a caller who knows the pragma should not have to discover that
    /// this crate redefined it. `Some(-64_000)` is 64 MB; `Some(64_000)` is
    /// 64,000 pages, which at the 4 KiB page size this crate gets is 256 MB.
    ///
    /// The writer wants a large cache: it is one connection, it holds the write
    /// lock while it works, and every page it has to re-read from disk is time
    /// no other writer can use.
    ///
    /// # Unlike the two above, `None` here is not a policy enum
    ///
    /// Because SQLite's default is a *value* rather than a mechanism. Absence
    /// still means "leave it alone" — it just happens that leaving this alone
    /// is expressible as not running a pragma, where leaving the automatic
    /// checkpointer alone required saying which of two things "alone" meant.
    pub writer_cache_size: Option<i32>,
    /// Page cache for every **read-only** connection: the shared
    /// [`Database::read_conn`], the snapshot cadence's own connection, and
    /// (since W5.5) each [`Database::diagnostic_conn`] (0.12.15, W5.4).
    ///
    /// Same units as [`Self::writer_cache_size`], and the same `None`.
    ///
    /// Split from the writer's because the profiles are opposite and one number
    /// cannot serve both. There is exactly one writer and it is long-lived, so
    /// its cache is a fixed cost paid once. Read-only connections are plural —
    /// `diagnostic_conn` mints a new one per call — so a large value here is
    /// multiplied by however many a caller opens, and the R15 hazard that
    /// method documents is about concurrent opens. A single shared number
    /// therefore has to be small enough for the multiplied case, which is the
    /// wrong size for the one connection that holds the write lock.
    pub reader_cache_size: Option<i32>,
}

// `Clock` is not `Debug` — it is a behavioural trait with two methods and
// requiring `Debug` of every implementor to print a handle here would be the
// tail wagging the dog. So the field is reported as present-or-absent, which is
// the only part of it a reader of a `Tuning` dump can act on.
impl Tuning {
    /// The `Option<SnapshotCadence>` the three older constructors take, mapped
    /// onto the tri-state. `None` there means *disabled*, which is why
    /// [`CadencePolicy`] exists — see its docs.
    fn from_legacy(cadence: Option<SnapshotCadence>, clock: Option<Arc<dyn Clock>>) -> Self {
        Self {
            cadence: match cadence {
                Some(cadence) => CadencePolicy::Every(cadence),
                None => CadencePolicy::Disabled,
            },
            clock,
            wal_autocheckpoint: WalCheckpointPolicy::default(),
            writer_cache_size: None,
            reader_cache_size: None,
        }
    }
}

impl std::fmt::Debug for Tuning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tuning")
            .field("cadence", &self.cadence)
            .field("clock", &self.clock.as_ref().map(|_| "<injected>"))
            .field("wal_autocheckpoint", &self.wal_autocheckpoint)
            .field("writer_cache_size", &self.writer_cache_size)
            .field("reader_cache_size", &self.reader_cache_size)
            .finish()
    }
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
        Self::open_inner(path.as_ref(), Tuning::from_legacy(cadence, None)).await
    }

    /// Open with an injected clock (§5.1.2, **defect K**, D-062).
    ///
    /// The reason this exists is testing: `recorded_at` is the transaction-time
    /// axis, and until now every test that wanted to assert on one had to either
    /// avoid it or drive a raw connection, because `open()` hardcoded
    /// [`SystemClock`]. `FakeClock` has been public and constructed in the test
    /// harness since 0.5.2 with nothing to inject it into — the compiler warned
    /// about the dead field on every build for three releases.
    ///
    /// **The clock is floored against the database before the actor starts.**
    /// [`Clock::raise_floor`] is called with the newest `recorded_at` in the
    /// ledger, so an injected clock cannot issue a stamp below what is already
    /// stored — which would abort the next concept write on
    /// `trg_concepts_monotonic_ra` rather than merely being odd. This is the
    /// step whose absence kept the defect open: the obvious implementation
    /// (take an `Arc<dyn Clock>`, use it) produces a `Database` that fails on
    /// its first write against any non-empty file.
    ///
    /// On a fresh database there is no floor, so an injected `FakeClock` issues
    /// exactly the stamps it was given.
    pub async fn open_with_clock(
        path: impl AsRef<Path>,
        cadence: Option<SnapshotCadence>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::open_inner(path.as_ref(), Tuning::from_legacy(cadence, Some(clock))).await
    }

    /// Open with an explicit [`Tuning`] (0.12.12, W5.1, D-155).
    ///
    /// The consolidated form of the three constructors above, and the one that
    /// grows: every knob 0.13.0 adds arrives as a field here rather than as a
    /// fourth `open_*`. See [`Tuning`] for why the struct is
    /// `#[non_exhaustive]` and why that makes the growth additive.
    pub async fn open_tuned(path: impl AsRef<Path>, tuning: Tuning) -> Result<Self> {
        Self::open_inner(path.as_ref(), tuning).await
    }

    async fn open_inner(path: &Path, tuning: Tuning) -> Result<Self> {
        let Tuning {
            cadence,
            clock: injected,
            wal_autocheckpoint,
            writer_cache_size,
            reader_cache_size,
        } = tuning;
        let cadence = cadence.resolve();
        let db = libsql::Builder::new_local(path).build().await?;
        let write_conn = configure(db.connect()?, writer_cache_size).await?;
        // The writer is the only connection that commits, so it is the only one
        // whose `wal_autocheckpoint` can ever fire. Setting it on the readers
        // would be a pragma with no path to running (0.12.14, W5.3, D-157).
        if let Some(pragma) = wal_autocheckpoint.pragma() {
            let _ = write_conn.query(&pragma, ()).await?;
        }
        let read_conn = configure(db.connect()?, reader_cache_size).await?;

        // PRAGMA query_only = ON on reader connection (§5.1.2)
        read_conn.execute("PRAGMA query_only = ON", ()).await?;

        let migration = migrations::run(&write_conn).await?;

        let (highpri_tx, highpri_rx) = mpsc::channel(256);
        let (lowpri_tx, lowpri_rx) = mpsc::channel(64);

        // Floored after `migrations::run`, so the tables the floor is read from
        // are guaranteed to exist.
        let clock: Arc<dyn Clock> = match injected {
            Some(clock) => {
                if let Some(floor) = crate::util::clock::recorded_at_floor(&read_conn).await? {
                    clock.raise_floor(floor);
                }
                clock
            }
            None => Arc::new(SystemClock::new(&read_conn).await?),
        };
        let shared = Arc::new(ActorShared::default());
        let writer = tokio::spawn(run_writer_actor(
            write_conn,
            Arc::clone(&clock),
            highpri_rx,
            lowpri_rx,
            Arc::clone(&shared),
        ));

        let archive_path = derive_archive_path(path);
        let snapshots_dir = derive_snapshots_dir(path);

        // **The cadence gets its own connection (Wave 4.1).** It used to share
        // `read_conn`, on the reasoning that `libsql::Connection` is an
        // Arc-backed handle and R15 makes every extra local connection a cost worth
        // not paying for nothing. The cost it was not paying for turned out to be
        // real: `reconstruct` brackets a fold with `ATTACH cold … DETACH cold`,
        // that region is per-connection state, and it is not synchronised. Two
        // folds on one connection can therefore interleave so that one DETACHes
        // the handle the other is mid-fold on.
        //
        // Recorded in §8.5 as a hazard rather than a defect because it **did not
        // reproduce**: 200 concurrent reconstructions against a 1 ms cadence with
        // an archive present produced zero errors, since the cadence anchors at
        // `MAX(recorded_at)` and so almost always takes the hot path. Narrow, and
        // real — a write landing between `log_head` and the fold opens it.
        //
        // Separate connections remove the interleaving rather than ordering it,
        // which is why this is preferred to a mutex around the region: there is
        // no shared state left to race on, and nothing to remember to hold. The
        // R15 objection does not apply — that fault is about *concurrent* opens,
        // and this is one more sequential open during `open()`.
        let (cadence_stop, cadence) = match cadence {
            Some(cadence) => {
                let cadence_conn = configure(db.connect()?, reader_cache_size).await?;
                cadence_conn.execute("PRAGMA query_only = ON", ()).await?;
                let (tx, rx) = tokio::sync::watch::channel(false);
                let handle = tokio::spawn(snapshot::run_cadence(
                    cadence_conn,
                    snapshots_dir.clone(),
                    archive_path.clone(),
                    cadence,
                    rx,
                ));
                (Some(tx), Some(handle))
            }
            None => (None, None),
        };

        let handle = Self {
            db,
            path: path.to_path_buf(),
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
            closed: false,
            shared,
        };

        // **Re-anchor after a migration (Wave 4.4).**
        //
        // D-043 makes a `SCHEMA_VERSION` bump invalidate every snapshot on disk,
        // which is correct — a snapshot is a serialised `MaterializedState` and a
        // schema change can change what that means. What was missing is the other
        // half: nothing wrote a replacement, so the first `reconstruct` after an
        // upgrade skipped every file as incompatible and folded from genesis. On
        // a database with a large log that is the difference between reading one
        // snapshot and folding the whole history, and the only trace was a
        // `warn!` per skipped file.
        //
        // Written here rather than left to the cadence because the cadence fires
        // on log *growth* (D-053): an upgraded database that is then read but not
        // written would never re-anchor at all.
        //
        // Failure is logged, not returned. A missing anchor costs time and no
        // information — snapshots are derivative under Doctrine VI — so refusing
        // to open a database because its optimisation could not be rebuilt would
        // trade a real capability for a performance one.
        //
        // Gated on the cadence being enabled, as well as on an actual upgrade:
        // `open_with_cadence(None)` means *this handle writes no snapshots except
        // at close()*, and a one-off write at open would contradict that for a
        // caller who asked for the quiet mode precisely to control when files
        // appear. They still get an anchor from `close()`.
        if migration.upgraded() && handle.cadence.is_some() {
            let ts = handle.clock.now();
            let archive = handle
                .archive_path
                .exists()
                .then_some(handle.archive_path.as_path());
            match snapshot::write_final(&handle.read_conn, &handle.snapshots_dir, &ts, archive)
                .await
            {
                Ok(path) => tracing::info!(
                    "schema moved v{} -> v{}; re-anchored snapshots at {:?}",
                    migration.from,
                    migration.to,
                    path
                ),
                Err(e) => tracing::warn!(
                    "schema moved v{} -> v{} but the re-anchor failed: {e}. \
                     Reconstruction stays correct and folds from genesis until the \
                     cadence writes one.",
                    migration.from,
                    migration.to
                ),
            }
        }

        Ok(handle)
    }

    /// Read connection handle for queries, traversals, and folds.
    pub fn read_conn(&self) -> &libsql::Connection {
        &self.read_conn
    }

    /// The file this handle opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A **new, independently owned, OS-level read-only** connection to this
    /// database, for diagnostics (§4.7, T5.1, D-091).
    ///
    /// # Why this exists when `read_conn()` already does
    ///
    /// Two different things, and the difference is the point:
    ///
    /// * `read_conn()` returns a shared `&Connection` carrying
    ///   `PRAGMA query_only = ON`. That pragma is **per-connection and
    ///   reversible by its holder in one statement**, so it is a guardrail
    ///   against accident, not a capability boundary. And because the reference
    ///   is shared, a caller who runs a long reporting query on it is competing
    ///   with every traversal and fold in the process.
    /// * This returns a connection opened with `SQLITE_OPEN_READ_ONLY`, which is
    ///   enforced by the engine below the pragma layer, and it is the caller's
    ///   own.
    ///
    /// **Measured on libSQL 0.9.30 rather than assumed**
    /// (`examples/readonly_open_probe.rs`), against a live WAL database with the
    /// write actor running:
    ///
    /// | | `read_conn()` | `diagnostic_conn()` |
    /// |---|---|---|
    /// | `SELECT`, `EXPLAIN QUERY PLAN` | allowed | allowed |
    /// | `INSERT` | refused | refused |
    /// | `PRAGMA query_only = OFF` | **allowed** | allowed |
    /// | `INSERT` after that | **allowed** | **refused** |
    /// | `ATTACH` an existing file | allowed | allowed |
    /// | `INSERT` into the attachment | refused¹ | **refused** |
    /// | `ATTACH` a path that does not exist | — | refused (`SQLITE_CANTOPEN`) |
    ///
    /// The third and fourth rows are the whole difference: turning the pragma
    /// off restores writes on `read_conn()` and does not here. That is what
    /// "boundary rather than guardrail" means, and it is now a number rather
    /// than a claim.
    ///
    /// ¹ On `read_conn()` that refusal is `query_only` — the same reversible
    /// thing as row 2. On `diagnostic_conn()` it is the open flags, and the
    /// probe runs it *after* `query_only = OFF` so that the pragma cannot be
    /// what is doing the work.
    ///
    /// # `ATTACH` is permitted, and does not widen the write boundary
    ///
    /// Checked because `diagnostic_query` (Python) is the only arbitrary-SQL
    /// surface this crate exposes, and an attachment is a second `open` whose
    /// flags it does not obviously inherit. It does inherit them: the
    /// attachment is read-only, and a nonexistent path is `SQLITE_CANTOPEN`
    /// rather than a new file, because `SQLITE_OPEN_CREATE` is dropped for the
    /// attachment as it is for `main`. So `SQLITE_OPEN_READ_ONLY` bounds the
    /// **connection**, not just the one file it names (0.10.0, W4.3).
    ///
    /// What it does widen is *reading*: an `ATTACH` can name any file the
    /// process can open, so this connection is a read surface over the
    /// filesystem, not over this database. That is a property of arbitrary SQL
    /// rather than of the flags, and it is unchanged by them.
    ///
    /// # One way this is *more* permissive, which is worth knowing
    ///
    /// `CREATE TEMP TABLE` **succeeds** here and is refused by `read_conn()`.
    /// Temp tables live in a separate temporary database that is writable
    /// regardless of how the main one was opened, whereas `query_only` refuses
    /// them outright — which is the mechanism [D-050] measured when it removed
    /// `TwoPhaseTempTable` for returning `SQLITE_READONLY (8)` on the read
    /// connection. So the stronger boundary is not uniformly stronger, and a
    /// strategy that needs a temp table has a connection it could run on. That
    /// is recorded, not acted on: D-050 removed the strategy for two reasons and
    /// this addresses one of them.
    ///
    /// # Calling this concurrently is R15's shape
    ///
    /// **This is the one method on `Database` that opens the file.** Everything
    /// else runs on connections established once, at `open`. Each call here is
    /// a fresh `libsql::Builder::…build()`, so *N* threads calling it at once
    /// are *N* concurrent opens — which is exactly the pattern behind
    /// [R15](https://github.com/opticsWolf/Macrame#known-risks), the upstream
    /// libSQL access violation (`0xC0000005`) that `examples/r15_soak.rs`
    /// reproduces and `RUST_TEST_THREADS=1` exists to avoid in the suite.
    ///
    /// **This is measured, not inferred.** 48 threads sharing one handle and
    /// calling only this method: 7 bad runs in 18 — two access violations and
    /// five *returned* SQLite errors (`database is locked`, `bad parameter or
    /// other API misuse`). With the calls serialised, 0 in 18
    /// (`tests_py/probes/r15_diagnostic_path.py`). The returned-error mode is
    /// the one to watch for: it looks like a fact about the database, on the
    /// method a caller reaches for when they already doubt the typed answer.
    ///
    /// **Bound this yourself if you call it from more than one thread.** One
    /// outstanding open at a time is enough; a mutex around the call costs
    /// nothing on a diagnostic path. This method does not do it for you on
    /// purpose: serialising behind a lock the caller cannot see would
    /// contradict the thing above it — that the connection is *the caller's
    /// own* — and it would put a hidden queue in front of the one surface whose
    /// job is to answer questions when the typed path is already suspect. The
    /// Python binding does bound it, because it wraps this in a method a caller
    /// cannot see into (`PyDatabase::diagnostic_rows`); a Rust caller can.
    ///
    /// # Errors
    ///
    /// The file must already exist. `SQLITE_OPEN_READ_ONLY` drops
    /// `SQLITE_OPEN_CREATE` with it, so a missing file is `SQLITE_CANTOPEN`
    /// rather than a fresh empty database — which is the right failure, and is
    /// surfaced as a typed error rather than as libSQL's error 14.
    pub async fn diagnostic_conn(&self) -> Result<libsql::Connection> {
        let fail = |reason: String| DbError::DiagnosticConn {
            path: self.path.display().to_string(),
            reason,
        };
        if !self.path.exists() {
            return Err(fail(
                "the file does not exist, and a read-only open cannot create it".to_string(),
            ));
        }
        let db = libsql::Builder::new_local(&self.path)
            .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .build()
            .await
            .map_err(|e| fail(e.to_string()))?;
        db.connect().map_err(|e| fail(e.to_string()))
    }

    /// Cross-check the snapshot chain against a fold from genesis (§5.5, T5.3,
    /// D-092).
    ///
    /// `write_final` composes onto the previous snapshot, so snapshot *n* is
    /// derived from snapshot *n−1* and nothing in the chain ever folds the whole
    /// log. An error at any link propagates forward forever and every read
    /// agrees with it, because every read descends from it. This is the check
    /// that would notice.
    ///
    /// # When to run it
    ///
    /// **Not on a schedule this crate chooses.** A genesis fold is precisely the
    /// cost snapshots exist to avoid, so running it periodically by default
    /// would give every application the bill snapshots were bought to remove —
    /// on a database whose log is large enough for snapshots to matter, which is
    /// the only kind where this is worth doing. The plan calls it a scheduling
    /// problem and it is the caller's schedule: an idle period, a nightly job,
    /// or once per *N* anchors, chosen against a log size this crate cannot see.
    ///
    /// The cadence is deliberately left alone for the same reason — it runs on a
    /// connection shared with nothing and a fold there would compete with
    /// interactive reads at a moment nobody chose.
    ///
    /// # It reports; it does not repair
    ///
    /// A divergence means the snapshots are a wrong **cache**, not that the
    /// ledger is corrupt: [Doctrine VI] makes them disposable, so deleting
    /// [`Self::snapshots_dir`] restores correctness and costs only speed.
    /// Rewriting the file here would destroy the evidence that composition has a
    /// defect, which is the only thing this can tell you that you did not
    /// already know.
    ///
    /// Pair it with the actor counters ([`Self::metrics`], D-079) so a
    /// divergence found by a scheduled run is visible beside the write latency
    /// of the period that produced it.
    ///
    /// [Doctrine VI]: ../../docs/architecture/s0-s3-foundations.md#doctrine-vi
    pub async fn verify_snapshot_chain(&self, ts: &str) -> Result<crate::temporal::ChainCheck> {
        let archive = self
            .archive_path
            .exists()
            .then_some(self.archive_path.as_path());
        crate::temporal::verify_snapshot_chain(&self.read_conn, ts, archive, &self.snapshots_dir)
            .await
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

    /// What the write actor has done since this handle was opened (T1.4, D-079).
    ///
    /// Requires the `metrics` feature. The counters are per-handle and start at
    /// zero on `open()` — they are not read from the database, because the thing
    /// being measured is *this process's* actor and merging two processes'
    /// histograms would produce a number about neither.
    ///
    /// The intended first question is [`crate::metrics::MetricsSnapshot::budget_violations`]:
    ///
    /// ```no_run
    /// # async fn f(db: &macrame::Database) {
    /// # #[cfg(feature = "metrics")] {
    /// for k in db.metrics().budget_violations() {
    ///     eprintln!("{} broke the 3 ms bound {} times", k.kind, k.over_budget);
    /// }
    /// # }
    /// # }
    /// ```
    ///
    /// Reading this does not stop the actor — see
    /// [`crate::metrics::ActorMetrics::snapshot`] for what that costs in
    /// consistency, and why the trade goes that way.
    #[cfg(feature = "metrics")]
    pub fn metrics(&self) -> crate::metrics::MetricsSnapshot {
        self.shared.metrics.snapshot()
    }

    /// The underlying libSQL database, for callers that need their own connection.
    ///
    /// # Actor containment is a convention above this line, not a guarantee
    ///
    /// **Kept public, and the honest statement of what that costs (Wave 4.3).**
    /// §5.1 says the write actor is the sole writer, and two mechanisms make that
    /// true of the handle: every write method goes through a channel, and
    /// [`Self::read_conn`] carries `PRAGMA query_only = ON`. **Nothing protects a
    /// connection obtained from here.** A caller can open one, write to `links`
    /// directly, and the actor will not know — the triggers still fire and the
    /// ledger stays internally consistent, but the single-writer property that
    /// [`crate::CHUNK_BUDGET`]'s latency argument rests on is gone, and so is the
    /// serialisation the overlap guard (D-060) relies on.
    ///
    /// This is the same shape as the limit stated in §4.2 for that guard, and it
    /// is one fact rather than two: **the storage layer permits what this API
    /// refuses.** Making it private would not change that — the database file is
    /// reachable by any SQLite client on the machine — it would only remove the
    /// supported way to do the thing, which is how escape hatches become
    /// `unsafe`-adjacent folklore.
    ///
    /// The free functions [`crate::register_model`] and
    /// [`crate::upsert_embedding`] take a bare connection for the same reason and
    /// carry the same caveat; prefer [`Self::register_model`] and
    /// [`Self::upsert_embeddings`], which go through the actor.
    ///
    /// # The legitimate-use list is now one item long (T5.1, D-091)
    ///
    /// It used to read: `EXPLAIN QUERY PLAN` and other diagnostics, read-only
    /// reporting queries wanting their own connection rather than sharing the
    /// reader, and provoking a guard in a test. The first two are exactly what
    /// [`Self::diagnostic_conn`] now does, and it does them behind an OS-level
    /// read-only open rather than on a handle that can write. **Use that.**
    ///
    /// What is left is the one use that genuinely requires write access through
    /// a connection the actor does not own: *provoking a guard* — writing the
    /// state §4.7 says the storage layer permits and this API refuses, so a test
    /// can assert the gap is still where the document says it is. That is the
    /// only thing this crate's own suite uses it for.
    ///
    /// # Why `#[doc(hidden)]` and not a `raw-access` feature
    ///
    /// T5.1 offers either. The feature is the stronger declaration — it shows up
    /// in the consumer's `Cargo.toml`, where a reviewer sees it — and it was
    /// **not** taken, for a reason specific to what uses this:
    ///
    /// Cargo features are additive and cannot be *required* by a test target
    /// except through `required-features`, which makes a plain `cargo test`
    /// **skip** that binary silently. The binaries that call this are
    /// `storage_boundary_tests` and `wave1_regression_tests` — the §4.7
    /// tripwires, whose entire job is to fail when a documented gap moves. Gating
    /// them behind a feature would mean the ordinary `cargo test` stopped running
    /// the tests that enforce the section this item is about, to make a
    /// declaration about a hatch. That trade is the wrong way round, and it is
    /// the same failure the project already names: a suite that quietly does less
    /// than it appears to.
    ///
    /// So the hatch stays reachable and stops being *discoverable*: it is absent
    /// from the docs, and the documented path for every non-write use is
    /// [`Self::diagnostic_conn`]. [D-068] is unchanged — removing it would buy
    /// the appearance of a guarantee, since the file is reachable by any SQLite
    /// client on the machine.
    ///
    /// [D-068]: ../../docs/architecture/s13-decision-register.md#d-068
    // convention (D-068/D-091): `raw()` is #[doc(hidden)] and is NOT exposed by
    // any binding. Everything above this line is invisible on docs.rs and
    // invisible to a contributor reading the Python surface list, which is where
    // the decision to expose it would actually be taken — hence this sentinel and
    // its twin in `bindings/python/src/lib.rs` (0.10.0, W4.10). The documented
    // path for every non-write use is `diagnostic_conn`.
    #[doc(hidden)]
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
    ///
    /// # One row costs a transaction, so N rows cost N transactions
    ///
    /// This is the correct method for a caller who genuinely has one edge, and
    /// it is the wrong one in a loop. Each call is its own transaction and pays
    /// the ~0.8 ms per-transaction floor (D-090) whole, so a thousand edges
    /// asserted one at a time spend roughly **0.8 s in transaction overhead
    /// alone** — before any of the work — and mint a thousand distinct
    /// `recorded_at` stamps for what the caller probably means as one act.
    ///
    /// There are two bulk forms and the difference between them is the one to
    /// get right:
    ///
    /// - [`Self::bulk_import`] is **chunked** against [`CHUNK_BUDGET`] and
    ///   atomic per chunk. It amortises the transaction floor across the batch
    ///   while still yielding to interactive work at every chunk boundary. This
    ///   is the one a loop should almost always become.
    /// - [`Self::write_bulk_atomic`] is one transaction under one stamp and is
    ///   **the one write with no latency bound** — the hold is a function of
    ///   `edges.len()`, tabulated in its own docs, and is time every other
    ///   writer spends waiting. Reach for it when the batch is genuinely one
    ///   act that must not be observable half-applied, not for speed.
    ///
    /// The choice is the caller's and neither form is deprecated. Doctrine III
    /// makes "one act, one stamp" a semantic claim rather than a performance
    /// one, and only the caller knows whether their thousand edges are one act.
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
    ///
    /// # One row costs a transaction
    ///
    /// The same trade [`Self::assert_edge`] describes, for the same reason and
    /// with the same ~0.8 ms floor (D-090): correct for one concept, wrong in a
    /// loop. [`Self::write_concepts`] takes a `Vec` and commits it as one
    /// transaction under one stamp.
    ///
    /// There is no atomic-across-chunks concept path and none is needed to make
    /// the choice: `write_concepts` is chunked against [`CHUNK_BUDGET`] and
    /// atomic per chunk, so a large `Vec` is cooperative rather than a stall.
    /// The responsiveness argument for writing one row at a time therefore does
    /// not apply — the bulk form already yields at every chunk boundary.
    pub async fn upsert_concept(&self, concept: ConceptUpsert) -> Result<()> {
        let concept = concept.normalized()?;
        self.high(|responder| HighPriCommand::UpsertConcept { concept, responder })
            .await
    }

    /// Assert many edges in one transaction under one stamp (D-014).
    ///
    /// # This is the one write with no latency bound, and here is what it costs
    ///
    /// The batch is one act under one `recorded_at`, so it cannot be chunked —
    /// splitting it is the thing this method exists not to do. That makes the
    /// actor's hold a function of `edges.len()`, and until now the only
    /// statement of that anywhere was the prose "uncapped" in
    /// [`CHUNK_BUDGET`]'s table. A caller who stalls every other writer for
    /// eight seconds should have been able to predict it from the signature.
    ///
    /// Measured on libSQL 0.9.30 (T1.3, D-081), holding the actor for:
    ///
    /// | rows | hold |
    /// |---|---|
    /// | 500 | ~34 ms |
    /// | 2,000 | ~155 ms |
    /// | 10,000 | ~1.0 s |
    /// | 20,000 | ~2.6 s |
    ///
    /// [`estimated_bulk_hold`] is that curve as a function, and this method
    /// emits a `tracing::warn!` when it predicts more than
    /// [`BULK_ATOMIC_WARN_HOLD`]. **The estimate is a shape, not a promise** —
    /// see [`estimated_bulk_hold`] for what it is calibrated against and where
    /// it will be wrong.
    ///
    /// A caller who needs the latency bound and not the atomicity wants
    /// [`Self::bulk_import`], which is the same write chunked and explicitly not
    /// atomic overall (D-011).
    pub async fn write_bulk_atomic(&self, edges: Vec<EdgeAssertion>) -> Result<usize> {
        let estimate = estimated_bulk_hold(&edges);
        if estimate > BULK_ATOMIC_WARN_HOLD {
            // Warned here rather than in the actor, and before the send: this is
            // the caller's own task, so the log line lands with their span
            // attached and names the call site that chose the batch size. By the
            // time the actor has it, the only context left is "a large batch".
            tracing::warn!(
                rows = edges.len(),
                estimated_hold_ms = estimate.as_millis() as u64,
                "write_bulk_atomic will hold the write actor for roughly \
                 {estimate:?} — it is atomic by contract (D-014) and cannot be \
                 chunked. Every other writer waits that long. Use bulk_import \
                 if the batch does not need to be all-or-nothing."
            );
        }

        let edges = normalize_all(edges)?;
        self.high(|responder| HighPriCommand::WriteBulkAtomic { edges, responder })
            .await
    }

    /// Rebuild `links_current` from `links` and verify zero drift (§5.8).
    /// Move the WAL back into the main database file (§4.5, F-30, 0.12.13,
    /// W5.2, D-156).
    ///
    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)` on the write connection, as an
    /// actor turn, and returns what SQLite reported. **Read
    /// [`CheckpointReport::busy`]** — a checkpoint that could not run is an
    /// `Ok` whose WAL is still there.
    ///
    /// # When a caller needs this
    ///
    /// Three cases, and only three:
    ///
    /// - **Before copying the database file elsewhere.** In WAL mode the `.db`
    ///   file alone is not the database; recent commits live in the `-wal`. A
    ///   complete checkpoint is what makes the main file self-contained.
    /// - **At the end of a bulk load that turned the automatic checkpointer
    ///   off.** That is the pairing this method exists for — see
    ///   [`Tuning::wal_autocheckpoint`]. Disabling autocheckpoint without
    ///   calling this leaves a WAL that grows for the life of the process.
    /// - **Before a long idle period**, to give back the disk.
    ///
    /// Nobody else should call it on a timer. SQLite checkpoints automatically
    /// every 1,000 pages and that default is not changed by this method
    /// existing; a periodic explicit checkpoint on top of it buys nothing and
    /// takes the write lock to do so.
    ///
    /// # It takes the write lock, and it is budget-exempt
    ///
    /// The hold is a function of how many frames have accumulated, which is a
    /// function of how long since the last checkpoint — not of anything passed
    /// in. It is on [`CHUNK_BUDGET`]'s exemption table for that reason, and it
    /// is the one entry there that is not a transaction: there is no smaller
    /// unit to chunk into, because the operation *is* the copy.
    pub async fn checkpoint(&self) -> Result<CheckpointReport> {
        self.high(|responder| HighPriCommand::Checkpoint { responder })
            .await
    }

    pub async fn rebuild_current(&self) -> Result<RebuildReport> {
        self.high(|responder| HighPriCommand::RebuildCurrent { responder })
            .await
    }

    /// Rebuild `links_current` beside itself, in chunks (§5.8, T1.2, D-082).
    ///
    /// Same result as [`Self::rebuild_current`], different latency profile.
    /// `rebuild_current` is one transaction holding the write lock for its whole
    /// duration, because D-023 will not let the `DELETE` and the `INSERT` be
    /// split: a reader landing between them sees a graph with no edges and no
    /// error. This builds the replacement in a shadow table instead — the live
    /// table stays live and trigger-maintained throughout — and swaps it in at
    /// the end.
    ///
    /// Each step is its own actor turn, so an interactive assertion can jump the
    /// queue between chunks. That is the whole of the improvement, and it is why
    /// the loop is here rather than inside the actor's arm (the same reasoning
    /// as [`Self::archive_windowed`] and [`Self::bulk_import`]).
    ///
    /// # What the swap still costs
    ///
    /// Not microseconds. Index names are global and SQLite has no `ALTER INDEX
    /// … RENAME`, so the shadow cannot be built carrying `links_current`'s index
    /// names while `links_current` still holds them — and building it under
    /// other names would leave the table permanently indexed under names absent
    /// from [`CREATE_INDICES`](crate::schema::ddl::CREATE_INDICES), so the next
    /// migration would create a second copy of each.
    /// `DROP TABLE` frees the names, so the swap transaction is where
    /// the three indexes get built. What the chunking moves off the lock is the
    /// **projection** — the window function over all of `links` — which is the
    /// O(E log E) term.
    ///
    /// # When this returns an error rather than a repair
    ///
    /// [`DbError::RebuildInterrupted`] means an archive committed while the
    /// shadow was being built. Its deletions are invisible to a catch-up pass
    /// keyed on `recorded_at` — a deleted row has no `recorded_at` left to find
    /// it by — so the work is discarded rather than swapped in. `links_current`
    /// is untouched and the call can simply be retried.
    ///
    /// Use [`Self::rebuild_current`] when the repair must be one atomic act, or
    /// when nothing else is contending for the actor and the extra turns are
    /// pure overhead.
    pub async fn rebuild_current_chunked(&self) -> Result<RebuildReport> {
        use crate::integrity::{ShadowOutcome, ShadowStep};

        // Each `else` arm is unreachable: the actor maps each step to its own
        // outcome variant. Written as a refutable pattern rather than an
        // `unwrap` so that adding a step cannot turn a mismatch into a panic on
        // the write path — and `WriterDroppedResponder` is the honest name for
        // "the actor answered with something this cannot use".
        let ShadowOutcome::Started { build_start, epoch } =
            self.shadow_step(ShadowStep::Begin).await?
        else {
            return Err(DbError::WriterDroppedResponder);
        };

        let mut after: Option<String> = None;
        loop {
            let ShadowOutcome::Filled { last } = self
                .shadow_step(ShadowStep::Fill {
                    after: after.take(),
                })
                .await?
            else {
                return Err(DbError::WriterDroppedResponder);
            };
            match last {
                Some(last) => after = Some(last),
                None => break,
            }
        }

        let ShadowOutcome::Swapped { rows } = self
            .shadow_step(ShadowStep::Swap { build_start, epoch })
            .await?
        else {
            return Err(DbError::WriterDroppedResponder);
        };

        Ok(RebuildReport {
            rows_rebuilt: rows,
            // Not audited. The chunked path's whole argument is that the
            // expensive work happens off the lock, and `audit_current` is two
            // `EXCEPT` passes over the projection — the cost D-077 removed from
            // the archive for the same reason. A caller who wants the check has
            // `audit_current` on the read connection, where it costs nobody the
            // write lock.
            drift_after: 0,
        })
    }

    /// Run one step of a chunked rebuild, for a caller doing its own scheduling.
    ///
    /// [`Self::rebuild_current_chunked`] is this in a loop and is what almost
    /// everyone wants. This exists because that loop offers no seam: it drives
    /// `Begin`, then `Fill` to exhaustion, then `Swap`, and a caller who needs to
    /// do something *between* steps — pace them against a frame budget, abandon
    /// a rebuild that has run long enough, or provoke the archive interlock in a
    /// test — cannot get in.
    ///
    /// The obligation that comes with it: `epoch` from
    /// [`ShadowOutcome::Started`](crate::integrity::ShadowOutcome) must be handed
    /// back to [`ShadowStep::Swap`](crate::integrity::ShadowStep), or the
    /// archive interlock is defeated and a stale projection can be swapped in.
    /// The looping version cannot get that wrong; this one can.
    pub async fn shadow_step(
        &self,
        step: crate::integrity::ShadowStep,
    ) -> Result<crate::integrity::ShadowOutcome> {
        self.low(|responder| LowPriCommand::ShadowRebuild { step, responder })
            .await
    }

    /// Import edges on the background channel, chunked (D-011).
    ///
    /// Atomic *per chunk*, not overall: a failure partway leaves earlier chunks
    /// committed. That is the tradeoff [`chunk_rows`] documents — use
    /// [`Database::write_bulk_atomic`] when the batch must be all-or-nothing.
    ///
    /// Chunked adaptively, at most [`chunk_rows::EDGES`] rows at a time: that
    /// constant is where the loop starts and the largest chunk it will send, and
    /// each chunk's measured hold sizes the next against [`CHUNK_BUDGET`]. It is
    /// also faster in total than the larger chunks this used through 0.5.5
    /// (D-058).
    ///
    /// A consequence worth planning for: the chunk boundaries — and so the
    /// `recorded_at` stamps this import writes — depend on how fast the machine
    /// was, not only on how many edges were passed (§5.1.6).
    pub async fn bulk_import(&self, edges: Vec<EdgeAssertion>) -> Result<usize> {
        let edges = normalize_all(edges)?;
        self.low_chunked(edges, chunk_rows::EDGES, |chunk, responder| {
            LowPriCommand::BulkImportChunk { chunk, responder }
        })
        .await
    }

    /// Upsert many **concepts** on the background channel, chunked (D-011).
    ///
    /// This is the bulk concept path, and every row it writes is a ledger write:
    /// it versions the concept and lands in `transaction_log`. Derived analytics
    /// output does not belong here — see
    /// [`Database::write_analytics_annotations`] and D-041.
    ///
    /// Called `write_annotations` through 0.5.6, from when the two writes were
    /// one call. D-041 split them and the name stayed on the wrong one for three
    /// releases, so the crate had a `write_annotations` that wrote concepts
    /// sitting beside a `write_analytics_annotations` that wrote annotations
    /// (D-075).
    pub async fn write_concepts(&self, concepts: Vec<ConceptUpsert>) -> Result<usize> {
        let concepts: Vec<ConceptUpsert> = concepts
            .into_iter()
            .map(ConceptUpsert::normalized)
            .collect::<Result<_>>()?;
        self.low_chunked(concepts, chunk_rows::CONCEPTS, |chunk, responder| {
            LowPriCommand::WriteConceptsChunk { chunk, responder }
        })
        .await
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
        self.low_chunked(rows, chunk_rows::EMBEDDINGS, |chunk, responder| {
            LowPriCommand::UpsertEmbeddingChunk {
                model: model.clone(),
                chunk,
                responder,
            }
        })
        .await
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

    /// Refresh the query planner's statistics (0.12.4, [D-149]).
    ///
    /// Runs `ANALYZE`, which writes `sqlite_stat1`. **Before 0.12.4 nothing in
    /// this crate ever did**, so the planner costed every query against SQLite's
    /// built-in defaults — assume ~1M rows, assume each bound equality column
    /// divides by ten. That estimate is structural: it depends on how many
    /// columns a query binds, not on what the table contains.
    ///
    /// Which is this schema's own worst defect restated. D-042, D-059 and D-064
    /// are three occasions where *a covering index captured a query because it
    /// contained the columns, not because it discriminated*, and two of the four
    /// declared indices lead on the same column. Statistics are what let the
    /// planner tell them apart by measurement instead of by shape.
    ///
    /// # Cost, and why it is bounded
    ///
    /// This is a write and it takes the write lock. `PRAGMA analysis_limit`
    /// (set per connection, see [`ddl::ANALYSIS_LIMIT`]) caps the rows examined
    /// per index, so the hold scales with the number of indices — four — and not
    /// with the size of `links_current`. It is scheduled as low-priority work
    /// and will not preempt an interactive assertion.
    ///
    /// # When to call it
    ///
    /// After a bulk import, and after anything that changes a table's shape by
    /// an order of magnitude. Prefer [`optimize`] for routine upkeep: it does
    /// nothing when nothing has moved, and this does the work unconditionally.
    ///
    /// Statistics are derived state in the sense Doctrine VI means it — deleting
    /// `sqlite_stat1` costs plan quality and no information, and this call
    /// rebuilds it.
    ///
    /// [D-149]: ../docs/architecture/s13-decision-register.md#d-149
    /// [`ddl::ANALYSIS_LIMIT`]: crate::schema::ddl::ANALYSIS_LIMIT
    /// [`optimize`]: Database::optimize
    pub async fn analyze(&self) -> Result<()> {
        self.low(|responder| LowPriCommand::Analyze {
            incremental: false,
            responder,
        })
        .await
    }

    /// Re-analyse only what has gone stale (0.12.4, [D-149]).
    ///
    /// `PRAGMA optimize`. SQLite tracks how far each table has drifted since its
    /// last analysis and re-analyses only where it believes the statistics no
    /// longer hold — so this is a no-op on an idle database and the full cost of
    /// [`analyze`] on one that has changed completely.
    ///
    /// That property is the whole point: it is safe to call on a schedule, where
    /// [`analyze`] is not. `close()` runs it, so a process that opens, works and
    /// closes keeps its statistics current without anybody arranging it.
    ///
    /// [D-149]: ../docs/architecture/s13-decision-register.md#d-149
    /// [`analyze`]: Database::analyze
    pub async fn optimize(&self) -> Result<()> {
        self.low(|responder| LowPriCommand::Analyze {
            incremental: true,
            responder,
        })
        .await
    }

    // **There is deliberately no `verify_fts()` (§5.9, D-071).**
    //
    // `rebuild_fts` is the repair with no way to ask whether it is needed, and
    // Wave 5 set out to add the missing half. FTS5 offers `'integrity-check'`,
    // which looked like exactly the engine-provided answer this crate prefers.
    // It is not: on libSQL 0.9.30 it verifies the index's *internal* consistency
    // and not its agreement with the content table. Measured — after
    // `'delete-all'` the index matches nothing where it matched ten rows, and
    // both `'integrity-check'` and `'integrity-check', 0` still report success.
    //
    // A `verify_fts()` on that footing would answer "healthy" for an empty
    // index, which is worse than having no method at all: it is the shape of
    // defect AC, a function that looks like it checks something and does not.
    // `an_emptied_fts_index_still_passes_integrity_check` pins the limitation so
    // that if a later libSQL fixes it, the test fails and says so.

    /// Write derived analytics results on the background channel, chunked
    /// (§5.4, D-041).
    ///
    /// Rows go to `analytics_annotations`, which has no log trigger, so nothing
    /// written here reaches `transaction_log` and nothing here versions a
    /// concept. Rerunning an algorithm replaces the previous pass rather than
    /// recording that the world changed.
    ///
    /// Low priority and chunked at up to [`chunk_rows::ANNOTATIONS`] — the
    /// largest ceiling of the four, because this is the only bulk table carrying
    /// no triggers at all
    /// and its rows are correspondingly cheap (D-058) — so a 50,000-label Louvain
    /// save yields to interactive writes at every chunk boundary and carries the
    /// per-chunk fidelity boundary of §5.1.6 — a partially written pass is
    /// recoverable by rerunning, which is the property that makes derived state
    /// safe to write this way and assertions not.
    pub async fn write_analytics_annotations(&self, annotations: Vec<Annotation>) -> Result<usize> {
        self.low_chunked(annotations, chunk_rows::ANNOTATIONS, |chunk, responder| {
            LowPriCommand::WriteAnalyticsChunk { chunk, responder }
        })
        .await
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

    /// Move the named concepts back from the cold database into the hot tables
    /// (§2.3, C3).
    ///
    /// Rehydration is a **physical move back, not a write**: it mints no
    /// transaction-time facts and is invisible to both clocks. An id that is not
    /// in the cold file is skipped rather than being an error — the caller
    /// generally has a list from a cold-side query, and a partially-stale list is
    /// the normal case rather than a mistake. The report says how many actually
    /// moved.
    ///
    /// See [`RehydrateReport::rowids_reassigned`] for the one way a rehydrated
    /// row can differ from the row that was archived.
    pub async fn rehydrate(&self, ids: &[&str]) -> Result<RehydrateReport> {
        let ids: Vec<String> = ids.iter().map(|s| (*s).to_string()).collect();
        let archive_path = self.archive_path.clone();
        self.low(|responder| LowPriCommand::Rehydrate {
            ids,
            archive_path,
            responder,
        })
        .await
    }

    /// Archive up to `cutoff` as a sequence of sessions, each covering at most
    /// `window` of **transaction** time (T1.1, D-080).
    ///
    /// `archive(cutoff)` is one transaction whose size is set by how long it has
    /// been since the last one, which makes it the least bounded of the three
    /// operations exempt from [`CHUNK_BUDGET`] — its hold is a function of
    /// operational history rather than of anything a caller chose. This runs the
    /// same work as *N* complete sessions, each with its own marker, horizon row
    /// and rebuild, and returns one [`ArchiveReport`] per session in order.
    ///
    /// # D-012 is satisfied per session, and that is what it requires
    ///
    /// The atomicity D-012 demands is that copy-then-delete never be split — a
    /// crash between the phases duplicates or loses rows. *N* small sessions
    /// satisfy that exactly as one large one does. The obligation windowing adds
    /// is that a partial run leave a coherent intermediate state, which it does:
    /// each session commits a valid horizon, so a failure at window *k* leaves a
    /// database archived up to boundary *k−1* and nothing in between. **The
    /// sequence is not atomic and does not claim to be** — on error, the reports
    /// for the sessions that did commit are lost with it, but their effect is
    /// not, and re-running with the same `cutoff` completes the job.
    ///
    /// # Each session is its own actor turn, and that is the entire point
    ///
    /// This loop lives here, on the handle, rather than inside the actor's
    /// `Archive` arm. Putting it there would have produced *N* small
    /// transactions inside **one** hold, which shrinks the transaction and
    /// changes the latency not at all: the actor is single-threaded, so nothing
    /// else writes until its turn returns regardless of how many `COMMIT`s the
    /// turn contains. Sending *N* commands returns the actor to its `select!`
    /// between sessions, which is where an interactive assertion gets to jump
    /// the queue — and it is high-priority, so it does.
    ///
    /// The same reasoning is why [`Self::bulk_import`] chunks here and not
    /// there, and it is the trap T1.2 names for `CREATE TABLE … AS SELECT`.
    ///
    /// # Choosing a window
    ///
    /// The bound is on *transaction* time, so the session count is set by how
    /// far back the hot file goes, not by how much it holds. A window is
    /// rejected rather than clamped if it would need more than
    /// [`MAX_ARCHIVE_SESSIONS`] sessions — see [`DbError::ArchiveWindow`].
    ///
    /// Windows containing nothing archivable are cheap but not free: each still
    /// opens a transaction and writes a horizon row. What they no longer do is
    /// re-project `links_current`, which `archive_session` now skips when its
    /// `DELETE` removed no rows — without that, windowing costs *more* in total
    /// than not windowing, because the repair term scales with the surviving
    /// table and not with the batch (D-077).
    pub async fn archive_windowed(
        &self,
        cutoff: &str,
        window: std::time::Duration,
    ) -> Result<Vec<ArchiveReport>> {
        let cutoff = timestamp::normalize(cutoff)?;
        let boundaries = self.archive_boundaries(&cutoff, window).await?;

        let mut reports = Vec::with_capacity(boundaries.len());
        for boundary in boundaries {
            let archive_path = self.archive_path.clone();
            reports.push(
                self.low(|responder| LowPriCommand::Archive {
                    cutoff: boundary,
                    archive_path,
                    responder,
                })
                .await?,
            );
        }
        Ok(reports)
    }

    /// The cutoffs [`Self::archive_windowed`] will run, ascending, ending at
    /// `cutoff` exactly.
    ///
    /// Read on `read_conn`, not on the actor: this is two `MIN`s and the actor
    /// has no reason to hold its lock for them.
    ///
    /// The lower end comes from the data rather than from the clock. Stepping
    /// from some fixed epoch would make the session count a function of the
    /// calendar — a database opened yesterday would still be asked to archive
    /// 1970 — whereas the oldest `recorded_at` actually present is the earliest
    /// boundary that can contain anything.
    async fn archive_boundaries(
        &self,
        cutoff: &str,
        window: std::time::Duration,
    ) -> Result<Vec<String>> {
        // A single session at `cutoff` is exactly `archive(cutoff)`, and it is
        // the right answer for an empty hot file: it still writes the horizon
        // row, so windowed and unwindowed runs leave the same observable state.
        let Some(oldest) = self.oldest_hot_stamp(cutoff).await? else {
            return Ok(vec![cutoff.to_string()]);
        };

        let start = timestamp::parse(&oldest)?;
        let end = timestamp::parse(cutoff)?;
        let Ok(span) = end.duration_since(start) else {
            // Everything in the hot file is at or after the cutoff, so there is
            // nothing in range to divide.
            return Ok(vec![cutoff.to_string()]);
        };

        if window.is_zero() {
            return Err(DbError::ArchiveWindow {
                window,
                reason: "a zero-length window never advances past the first boundary".into(),
            });
        }

        // `div_ceil` on nanos: a span of 90 minutes in 60-minute windows is two
        // sessions, not one. `as_nanos` is u128, so neither the division nor the
        // span can overflow for any timestamp this crate can store.
        let sessions = span.as_nanos().div_ceil(window.as_nanos());
        if sessions > MAX_ARCHIVE_SESSIONS as u128 {
            return Err(DbError::ArchiveWindow {
                window,
                reason: format!(
                    "a span of {span:?} would need {sessions} sessions (limit \
                     {MAX_ARCHIVE_SESSIONS}); widen the window"
                ),
            });
        }

        let mut boundaries = Vec::with_capacity(sessions as usize);
        for k in 1..sessions {
            boundaries.push(timestamp::format(start + window * k as u32));
        }
        // The last boundary is `cutoff` itself and not `start + n*window`, which
        // would overshoot and archive rows the caller excluded.
        boundaries.push(cutoff.to_string());
        Ok(boundaries)
    }

    /// Oldest `recorded_at` below `cutoff` in either hot table, or `None`.
    async fn oldest_hot_stamp(&self, cutoff: &str) -> Result<Option<String>> {
        let mut oldest: Option<String> = None;
        for table in ["links", "transaction_log"] {
            let found: Option<String> = self
                .read_conn
                .query(
                    &format!("SELECT MIN(recorded_at) FROM {table} WHERE recorded_at < ?1"),
                    libsql::params![cutoff],
                )
                .await?
                .next()
                .await?
                .and_then(|row| row.get(0).ok());
            if let Some(found) = found {
                if oldest.as_ref().is_none_or(|o| found < *o) {
                    oldest = Some(found);
                }
            }
        }
        Ok(oldest)
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

    /// Send each chunk in turn and sum the counts — the shape all four bulk
    /// paths share (T3.4, D-086).
    ///
    /// # This is sequential on purpose, and the purpose is a measurement
    ///
    /// T3.4 proposed pipelining: send *k* chunks ahead so the actor never finds
    /// an empty queue. The reasoning is that awaiting each chunk before building
    /// the next leaves the actor idle for a channel round trip every time, which
    /// on a 1M-edge import is ~11,000 idle gaps.
    ///
    /// Both halves of that are true and the conclusion does not follow. The gaps
    /// are real; they are also **four orders of magnitude smaller than the work
    /// they interrupt**. A tokio mpsc hop is sub-microsecond and a chunk takes
    /// 13–21 ms. Implemented and swept at depths 1, 2, 4, 8 and 16 over 20K and
    /// 100K edges: every cell landed within 1% of sequential, in both directions
    /// — see `examples/pipeline_diag.rs`, which is kept precisely so this is not
    /// re-proposed from the same reasoning.
    ///
    /// So the pipelining was removed and the deduplication kept. It was not free
    /// to hold: with chunks in flight, a failure at chunk `i` no longer leaves a
    /// **prefix** committed, because `i+1 ..= i+k-1` were already sent and commit
    /// anyway. D-011 promises "earlier chunks committed", and paying for that
    /// with a weaker recovery story in exchange for nothing measurable is the
    /// wrong trade.
    ///
    /// Sending stops at the first error, so what commits is exactly the prefix
    /// before the failure.
    /// # The size is now measured, not assumed (0.12.0, W3)
    ///
    /// Until 0.11.0 the caller pre-split into `chunks(chunk_rows::WHATEVER)` and
    /// this loop sent what it was given. That made the constant *the* size, and
    /// D-143 is the record of a constant fitted at one population being wrong at
    /// another: all four D-088 shapes agreed the largest in-budget edge chunk was
    /// **20** against a shipped 90, and 20 would itself have been wrong at 80,000
    /// edges, because per-row cost on that path grows with `links_current`.
    ///
    /// No row count can bound a duration on such a path, so the loop stopped
    /// trying to pick one ahead of time. `ceiling` — still the path's
    /// [`chunk_rows`] constant, with its derivation intact — is now the largest
    /// size this will ever ask for, and each chunk's measured hold chooses the
    /// next through `next_chunk_size`.
    ///
    /// **Feedback, not preemption.** The chunk in flight always commits in full;
    /// the SQLite write lock is not preemptible, so nothing here can shorten a
    /// transaction already running. A batch of one chunk gets no protection at
    /// all, and convergence costs one or two chunks — which is the price of the
    /// bound being a duration rather than a promise.
    ///
    /// The last chunk's outcome is discarded, there being no next chunk to size.
    async fn low_chunked<T>(
        &self,
        items: Vec<T>,
        ceiling: usize,
        make: impl Fn(Vec<T>, oneshot::Sender<Result<ChunkOutcome>>) -> LowPriCommand,
    ) -> Result<usize> {
        let mut items = items.into_iter();
        let mut size = ceiling.max(1);
        let mut written = 0usize;
        loop {
            let chunk: Vec<T> = items.by_ref().take(size).collect();
            if chunk.is_empty() {
                return Ok(written);
            }
            let (tx, rx) = oneshot::channel();
            self.lowpri_tx
                .send(make(chunk, tx))
                .await
                .map_err(|_| DbError::WriterUnavailable)?;
            let outcome = rx.await.map_err(|_| DbError::WriterDroppedResponder)??;
            written += outcome.rows;
            size = next_chunk_size(size, outcome.held, CHUNK_BUDGET, CHUNK_FLOOR, ceiling);
        }
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

        // Top up the planner's statistics while the actor is still alive to do
        // it (0.12.4, D-149). `PRAGMA optimize` re-analyses only what SQLite
        // believes has gone stale, so on a database that did nothing this costs
        // nothing, and on one that was just bulk-loaded it is the difference
        // between the next process planning on measurements and planning on
        // built-in guesses.
        //
        // **Deliberately not fatal.** A failure here costs plan quality on the
        // next open and nothing else — no ledger state depends on it — and
        // `close()` is where a caller learns whether their *writes* survived.
        // Turning a stale-statistics problem into a failed close would bury that
        // answer under a much less important one.
        if let Err(e) = self.optimize().await {
            tracing::warn!(
                "PRAGMA optimize failed during close(): {e}. Statistics may be \
                 stale for the next process; call analyze() to rebuild them. \
                 Nothing else is affected."
            );
        }

        let (tx, rx) = oneshot::channel();
        let _ = self
            .highpri_tx
            .send(HighPriCommand::Shutdown { responder: tx })
            .await;
        let _ = rx.await;

        // **The writer's `Result` is propagated, not discarded (Wave 4.2).**
        // It used to be `let _ = handle.await`, so an actor that had panicked or
        // returned an error closed "successfully" and the caller's last chance to
        // learn that the write path had died was spent silently. A `JoinError`
        // here means the actor panicked; the inner `Result` is whatever it
        // returned.
        //
        // Ordered before the final snapshot on purpose: a snapshot written after
        // a failed writer records a state the caller has no reason to trust, and
        // returning the writer's error while also having written that file is
        // worse than not writing it.
        if let Some(handle) = self.writer.take() {
            match handle.await {
                Ok(res) => res?,
                Err(e) => {
                    return Err(DbError::WriterStopped(format!(
                        "the write actor did not exit cleanly: {e}"
                    )))
                }
            }
        }

        let ts = self.clock.now();
        let archive = self
            .archive_path
            .exists()
            .then_some(self.archive_path.as_path());
        snapshot::write_final(&self.read_conn, &self.snapshots_dir, &ts, archive).await?;

        // Marks the handle closed so `Drop` knows not to complain.
        self.closed = true;
        Ok(())
    }
}

/// Notes a missed `close()` at `warn!`, and deliberately does **not** assert.
///
/// **§7.3 offered option B — document `close()` as mandatory and `debug_assert`
/// in `Drop` — and Wave 4.2 implemented it, measured the consequence, and
/// reduced it to a warning.** The assert fired on roughly thirty tests on its
/// first run. That is the signal it was built to produce, and the right reading
/// of it was not "thirty tests are wrong".
///
/// What dropping actually costs is one final snapshot. Nothing else: every
/// public write method awaits its responder, so by the time a caller *can* drop
/// the handle, every write it issued has already committed; and the cadence stops
/// on its own, because `cadence_stop` is a `watch::Sender` whose drop signals the
/// task. A snapshot is derivative state under Doctrine VI — disposable,
/// reconstructible, and never the only copy of anything. Losing one makes the
/// next `reconstruct` fold from an older anchor, which is **slower, not wrong**.
///
/// A `debug_assert` aborts a test run. Spending that on a performance loss, in a
/// project whose own notes say a suite that fails for reasons unrelated to the
/// code under test trains people to ignore red, is the wrong trade — and paying
/// it in thirty places would have made `close()` look mandatory by ceremony
/// rather than by consequence. `close()` remains the right thing to call, and
/// the two reasons to call it are now stated where they can be acted on: the
/// snapshot, and the writer's `Result`, which only `close()` can return.
///
/// Option A ("abort the actor and log") stays rejected, for the reason it was
/// rejected twice before: `Drop` cannot await, so it cannot drain, and cleanup
/// that cannot clean up is worse than none — it looks like cleanup.
impl Drop for Database {
    fn drop(&mut self) {
        if !self.closed {
            tracing::warn!(
                "Database dropped without close(): the final snapshot was not written, \
                 so the next reconstruct folds from an older anchor, and the write \
                 actor's exit status was not checked. Prefer close().await."
            );
        }
    }
}

fn normalize_all(edges: Vec<EdgeAssertion>) -> Result<Vec<EdgeAssertion>> {
    edges.into_iter().map(EdgeAssertion::normalized).collect()
}

/// One `FULL` checkpoint for the numbers, then a `TRUNCATE` for the file.
///
/// # Why `TRUNCATE` and not a mode parameter
///
/// The four SQLite modes are not four things a caller of *this* crate wants.
/// `PASSIVE` is what the automatic checkpointer already runs on its own, so an
/// explicit `PASSIVE` asks for something that was going to happen anyway;
/// `RESTART` and `FULL` differ from `TRUNCATE` only in whether the WAL file is
/// left at its high-water size. The reason W5.2 exists is
/// [`Tuning::wal_autocheckpoint`] — a bulk importer turns the automatic
/// checkpointer off and calls this once at the end — and what that caller wants
/// is the WAL *gone*, not smaller than it was. So the mode is fixed and decided
/// here rather than pushed to the caller as a choice they would have to read
/// SQLite's documentation to make. If a mode ever needs selecting, that is an
/// additive method, not a change to this one.
///
/// # Why it is two pragmas, which is not the obvious implementation
///
/// **A successful `TRUNCATE` reports `busy=0, log=0, checkpointed=0`** — the
/// counts describe the WAL *after* the operation, and after a truncation there
/// is no WAL to describe. Measured, not inferred: on a 387-frame WAL, `PASSIVE`
/// returns `0, 387, 387` and `TRUNCATE` on the same file returns `0, 0, 0`. So
/// the single-pragma implementation returns a [`CheckpointReport`] whose two
/// counts are structurally zero on success, which makes the whole struct a
/// less useful `bool`.
///
/// `FULL` copies every frame back and reports what it moved; the `TRUNCATE`
/// that follows finds nothing left to copy and resets the file. The second pass
/// is close to free for exactly that reason — it is a file operation, not a
/// second copy. `busy` is the **union**: a checkpoint that was blocked in
/// either phase did not fully happen, and a caller about to copy the database
/// file elsewhere needs the pessimistic answer.
///
/// # They return rows, so they go through `query()`
///
/// The same libsql constraint the pragmas in `configure` document: `execute()`
/// rejects any statement that yields rows, and these yield the row that is the
/// entire point.
async fn run_checkpoint(conn: &libsql::Connection) -> Result<CheckpointReport> {
    // The columns are `busy, log, checkpointed`. SQLite reports -1 for the two
    // counts when the checkpoint could not run; clamped to 0 rather than
    // surfaced as a signed count, because `busy` already carries "this did not
    // happen" and a negative frame count is not a quantity anyone can use.
    //
    // A database not in WAL mode returns no row at all. `configure` puts every
    // connection this crate opens into WAL, so that is unreachable here — but a
    // zeroed report is a better failure than a panic if it stops being.
    async fn one(conn: &libsql::Connection, sql: &str) -> Result<(bool, u64, u64)> {
        let mut rows = conn.query(sql, ()).await?;
        let Some(row) = rows.next().await? else {
            return Ok((false, 0, 0));
        };
        let field = |i: i32| -> u64 { row.get::<i64>(i).unwrap_or(0).max(0) as u64 };
        Ok((row.get::<i64>(0).unwrap_or(0) != 0, field(1), field(2)))
    }

    let (full_busy, _, moved) = one(conn, "PRAGMA wal_checkpoint(FULL)").await?;
    let (trunc_busy, log_frames, _) = one(conn, "PRAGMA wal_checkpoint(TRUNCATE)").await?;

    Ok(CheckpointReport {
        busy: full_busy || trunc_busy,
        log_frames,
        checkpointed_frames: moved,
    })
}

/// Identical pragma configuration on every connection.
async fn configure(
    conn: libsql::Connection,
    cache_size: Option<i32>,
) -> Result<libsql::Connection> {
    // NOTE: `journal_mode` and `busy_timeout` return their resulting value as a
    // row, and libsql's `execute()` rejects any statement that yields rows
    // ("Execute returned rows"). They must be issued through `query()`.
    let _ = conn.query("PRAGMA journal_mode = WAL", ()).await?;
    let _ = conn.query("PRAGMA busy_timeout = 5000", ()).await?;
    conn.execute("PRAGMA synchronous = NORMAL", ()).await?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    conn.execute("PRAGMA recursive_triggers = OFF", ()).await?;
    // Bounds every `ANALYZE` this connection will ever run, explicit or
    // triggered by `PRAGMA optimize` (D-149). Set here rather than around the
    // call sites so the scheduled path is bounded too — that is the half that
    // runs with nobody watching. Returns the previous limit as a row, so it goes
    // through `query()` for the reason the note above gives.
    let _ = conn.query(crate::schema::ddl::ANALYSIS_LIMIT, ()).await?;
    // Per-connection, and split writer from reader since 0.12.15 (W5.4,
    // D-158). `None` runs no pragma at all rather than restating SQLite's
    // default, so the default remains SQLite's to change.
    if let Some(pages) = cache_size {
        conn.execute(&format!("PRAGMA cache_size = {pages}"), ())
            .await?;
    }
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
///
/// # The turn is the unit, not the statement (T1.4)
///
/// One iteration of this loop is one *hold*: the actor is single-threaded and
/// the SQLite write lock is not preemptible, so from the moment a command starts
/// executing until it returns, nothing else writes. That is the quantity
/// [`CHUNK_BUDGET`] bounds, and so it is the quantity
/// [`crate::metrics::ActorMetrics`] measures — deliberately around the whole
/// `execute` call rather than inside it. Timing the SQL alone would have
/// reported a bound that held while callers waited.
///
/// Queue depth is sampled *before* the `select!`, so it is the backlog the turn
/// found on arrival rather than the one it left behind.
///
/// # `biased` has no floor, and since 0.12.10 that is measured (W4.4, D-153)
///
/// `biased` makes the arms poll in declaration order, so high-priority work is
/// taken whenever any is ready. Nothing bounds how long that can continue:
/// sustained interactive traffic can hold the low tier off indefinitely, and
/// through 0.12.9 nothing in the crate could say whether it ever did.
/// `record_priority_choice` counts the turns where the choice went against
/// queued low-priority work, and the longest unbroken run of them, which is the
/// half that distinguishes "prioritised" from "starved".
///
/// **No forced yield is added here.** Whether one is needed is the question the
/// counter answers, and adding a policy now would be fixing a bound nobody has
/// observed being hit — the same mistake D-124 was retracted for.
async fn run_writer_actor(
    conn: libsql::Connection,
    clock: Arc<dyn Clock>,
    mut highpri_rx: mpsc::Receiver<HighPriCommand>,
    mut lowpri_rx: mpsc::Receiver<LowPriCommand>,
    shared: Arc<ActorShared>,
) -> Result<()> {
    loop {
        // Read once and reused by both the depth sample and the starvation
        // counter, so the two cannot disagree about what was queued when this
        // turn went looking (W4.4, D-153).
        let low_queued = lowpri_rx.len();
        shared.metrics.record_turn(highpri_rx.len(), low_queued);

        let ctl = tokio::select! {
            biased;
            Some(cmd) = highpri_rx.recv() => {
                shared.metrics.record_priority_choice(true, low_queued);
                let turn = Turn::start(cmd.kind(), &shared);
                cmd.execute(&conn, &*clock, &turn).await
            }
            Some(cmd) = lowpri_rx.recv() => {
                shared.metrics.record_priority_choice(false, low_queued);
                let turn = Turn::start(cmd.kind(), &shared);
                cmd.execute(&conn, &*clock, &turn).await
            }
            else => LoopCtl::Break,
        };
        if matches!(ctl, LoopCtl::Break) {
            break;
        }
    }
    Ok(())
}

/// One command's hold: the timer, its label, and the counters it reports to.
///
/// # The hold is recorded *before* the caller is answered, and it has to be
///
/// The obvious placement — time the whole `execute` call from the loop — is
/// wrong in a way that only shows up under test. Every arm of `execute` ends by
/// sending on a `oneshot`, which wakes the waiting caller; the actor then
/// returns to the loop and records. Those are two tasks, so a caller that awaits
/// its own write and immediately reads [`Database::metrics`] can be scheduled
/// first and see a turn count that does not include the write it just did.
///
/// Not a correctness bug in the ledger, and it would never have been noticed in
/// production — a dashboard sampling every few seconds cannot see the window.
/// It makes every test and diagnostic of the counters flaky, which is worse: the
/// instrumentation would have been *believed* while being wrong exactly when
/// someone tried to check it. `examples/bulk_atomic_diag.rs` was the thing that
/// caught it, reporting a 20,000-row batch as a 0 ms hold.
///
/// So `answer` records and then sends, in that order, and the ordering is the
/// method's whole reason to exist. What it costs is that the `oneshot::send`
/// itself falls outside the measurement, which is a few nanoseconds against a
/// turn measured in microseconds at best.
struct Turn<'a> {
    kind: crate::metrics::CommandKind,
    timer: crate::metrics::HoldTimer,
    shared: &'a ActorShared,
}

/// State the actor owns and a `Turn` needs to reach.
///
/// `archive_epoch` is here rather than in [`crate::metrics::ActorMetrics`]
/// because it is **not** a metric: T1.2's shadow rebuild reads it to decide
/// whether its work is still valid, so it has to be present in every build, not
/// only under the `metrics` feature. Counting archives happens to be what both
/// want; only one of them is allowed to be compiled out.
#[derive(Default)]
struct ActorShared {
    metrics: crate::metrics::ActorMetrics,
    archive_epoch: std::sync::atomic::AtomicU64,
}

impl<'a> Turn<'a> {
    fn start(kind: crate::metrics::CommandKind, shared: &'a ActorShared) -> Self {
        Self {
            kind,
            timer: crate::metrics::HoldTimer::start(),
            shared,
        }
    }

    fn epoch(&self) -> u64 {
        self.shared
            .archive_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record that an archive session committed.
    ///
    /// Bumped on **success only**: a failed archive rolls back, so it deletes
    /// nothing and invalidates no shadow build.
    fn archive_committed(&self) {
        self.shared
            .archive_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Close the hold and hand the result back. Never the other way round.
    ///
    /// The `let _ =` on the send is deliberate and predates this: a caller that
    /// dropped its receiver — `tokio::time::timeout` around a write, which
    /// [`Database`]'s write surface explicitly documents — is not an actor
    /// error, and the command committed regardless.
    fn answer<T>(&self, responder: oneshot::Sender<Result<T>>, res: Result<T>) {
        self.shared
            .metrics
            .record_hold(self.kind, self.timer.elapsed());
        let _ = responder.send(res);
    }

    /// [`answer`](Self::answer) for a chunk: the same reading, handed back to the
    /// caller as well as recorded (0.12.0, W1).
    ///
    /// One `elapsed()` serves both, so the duration the chunk loop sizes against
    /// is *the same number* the histogram shows — a controller and a dashboard
    /// disagreeing about what a chunk cost would be a bad way to spend a
    /// debugging session.
    ///
    /// The record-then-send ordering documented on [`Turn`] is preserved, and
    /// matters here for the same reason: the send wakes the caller, which may be
    /// scheduled before this method returns.
    fn answer_chunk(&self, responder: oneshot::Sender<Result<ChunkOutcome>>, res: Result<usize>) {
        let held = self.timer.elapsed();
        self.shared.metrics.record_hold(self.kind, held);
        let _ = responder.send(res.map(|rows| ChunkOutcome { rows, held }));
    }
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
    /// The metrics label for this variant (T1.4).
    ///
    /// Exhaustive for the same reason `execute` is: a new variant that silently
    /// borrowed another's label would attribute its holds to the wrong command,
    /// and the one question the counters exist to answer is *which* command
    /// broke the budget.
    fn kind(&self) -> crate::metrics::CommandKind {
        use crate::metrics::CommandKind as K;
        match self {
            HighPriCommand::AssertEdge { .. } => K::AssertEdge,
            HighPriCommand::RetireEdge { .. } => K::RetireEdge,
            HighPriCommand::UpsertConcept { .. } => K::UpsertConcept,
            HighPriCommand::WriteBulkAtomic { .. } => K::WriteBulkAtomic,
            HighPriCommand::RebuildCurrent { .. } => K::RebuildCurrent,
            HighPriCommand::RegisterModel { .. } => K::RegisterModel,
            HighPriCommand::Checkpoint { .. } => K::Checkpoint,
            HighPriCommand::Shutdown { .. } => K::Shutdown,
        }
    }

    /// Run one command and answer its caller.
    ///
    /// Deliberately exhaustive — there is no `_` arm. The 0.4.5–0.5.4 actor
    /// matched `Shutdown` and `AssertEdge` and sent everything else to
    /// `_ => LoopCtl::Continue`, which **dropped the responder**: the caller's
    /// `rx.await` resolved to a `RecvError` that no code mapped, so four of six
    /// commands were indistinguishable from a hung database. An exhaustive match
    /// makes that failure a compile error instead of a runtime silence, which is
    /// why adding a variant should break this function.
    async fn execute(
        self,
        conn: &libsql::Connection,
        clock: &dyn Clock,
        turn: &Turn<'_>,
    ) -> LoopCtl {
        match self {
            HighPriCommand::Shutdown { responder } => {
                turn.answer(responder, Ok(()));
                return LoopCtl::Break;
            }
            HighPriCommand::Checkpoint { responder } => {
                let res = run_checkpoint(conn).await;
                turn.answer(responder, res);
            }
            HighPriCommand::AssertEdge { edge, responder } => {
                let stamp = clock.now();
                if let Err(e) = reject_overlapping_interval(conn, &edge).await {
                    turn.answer(responder, Err(e));
                    return LoopCtl::Continue;
                }
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
                turn.answer(responder, res);
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
                    conn,
                    &source,
                    &target,
                    &edge_type,
                    &valid_from,
                    &valid_to,
                    &stamp,
                )
                .await;
                turn.answer(responder, res);
            }
            HighPriCommand::UpsertConcept { concept, responder } => {
                let stamp = clock.now();
                let res = upsert_concept(conn, &concept, &stamp).await;
                turn.answer(responder, res);
            }
            HighPriCommand::WriteBulkAtomic { edges, responder } => {
                // One stamp for the whole batch (D-014): the rows were asserted
                // by one act, and giving them different transaction times would
                // invent an ordering the caller never expressed.
                let stamp = clock.now();
                let res = write_edges_atomic(conn, &edges, &stamp).await;
                turn.answer(responder, res);
            }
            HighPriCommand::RebuildCurrent { responder } => {
                turn.answer(responder, rebuild_current(conn).await);
            }
            HighPriCommand::RegisterModel {
                model,
                dim,
                responder,
            } => {
                turn.answer(
                    responder,
                    crate::vector::register_model(conn, &model, dim).await,
                );
            }
        }
        LoopCtl::Continue
    }
}

impl LowPriCommand {
    /// The metrics label for this variant (T1.4). See [`HighPriCommand::kind`].
    fn kind(&self) -> crate::metrics::CommandKind {
        use crate::metrics::CommandKind as K;
        match self {
            LowPriCommand::WriteConceptsChunk { .. } => K::WriteConceptsChunk,
            LowPriCommand::WriteAnalyticsChunk { .. } => K::WriteAnalyticsChunk,
            LowPriCommand::UpsertEmbeddingChunk { .. } => K::UpsertEmbeddingChunk,
            LowPriCommand::BulkImportChunk { .. } => K::BulkImportChunk,
            LowPriCommand::Archive { .. } => K::Archive,
            // Its own counter since 0.12.9 (W4.3, D-152). It reported as
            // `K::Archive` from 0.9.0 to 0.12.8 — the budget really is shared,
            // but attribution is not budget, and an operator reading a long
            // `archive` hold could not tell whether anything had been archived.
            // What kept it folded was that a `CommandKind` variant was a
            // breaking addition; `#[non_exhaustive]` (W4.2) removed that.
            LowPriCommand::Rehydrate { .. } => K::Rehydrate,
            LowPriCommand::RebuildFts { .. } => K::RebuildFts,
            LowPriCommand::Analyze { .. } => K::Analyze,
            LowPriCommand::ShadowRebuild { .. } => K::ShadowRebuild,
        }
    }

    /// Run one background command and answer its caller.
    ///
    /// Also exhaustive. The pre-0.5.4 version was a single `LoopCtl::Continue`
    /// for *every* variant — every background write silently discarded, its
    /// caller waiting forever.
    async fn execute(
        self,
        conn: &libsql::Connection,
        clock: &dyn Clock,
        turn: &Turn<'_>,
    ) -> LoopCtl {
        match self {
            LowPriCommand::BulkImportChunk { chunk, responder } => {
                // A stamp per chunk, not per batch: the chunks commit
                // separately, so a shared stamp would claim a simultaneity the
                // storage does not have.
                let stamp = clock.now();
                turn.answer_chunk(responder, write_edges_atomic(conn, &chunk, &stamp).await);
            }
            LowPriCommand::WriteConceptsChunk { chunk, responder } => {
                let stamp = clock.now();
                turn.answer_chunk(responder, write_concepts_atomic(conn, &chunk, &stamp).await);
            }
            LowPriCommand::WriteAnalyticsChunk { chunk, responder } => {
                let stamp = clock.now();
                turn.answer_chunk(
                    responder,
                    write_annotations_atomic(conn, &chunk, &stamp).await,
                );
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
                turn.answer_chunk(
                    responder,
                    crate::vector::search::upsert_embedding_chunk(conn, &model, &chunk).await,
                );
            }
            LowPriCommand::Archive {
                cutoff,
                archive_path,
                responder,
            } => {
                // The archive *time*, not the cutoff. `archive_horizon` records
                // both and they are different facts — see `archive()` (Wave 4.5).
                let archived_at = clock.now();
                let res = archive(conn, &cutoff, &archived_at, &archive_path).await;
                // Before the answer, so a shadow rebuild that reads the epoch on
                // its next turn cannot miss an archive that has already deleted
                // rows out from under it (T1.2).
                if res.is_ok() {
                    turn.archive_committed();
                }
                turn.answer(responder, res);
            }
            LowPriCommand::Rehydrate {
                ids,
                archive_path,
                responder,
            } => {
                let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                let res = rehydrate(conn, &refs, &archive_path).await;
                // Same reason as `Archive`: rehydration moves rows into `links`'
                // parent table, so a shadow rebuild in flight must see the epoch
                // move before the caller is answered (T1.2).
                if res.is_ok() {
                    turn.archive_committed();
                }
                turn.answer(responder, res);
            }
            LowPriCommand::ShadowRebuild { step, responder } => {
                use crate::integrity::{shadow, ShadowOutcome, ShadowStep};
                let res = match step {
                    ShadowStep::Begin => {
                        shadow::begin(conn)
                            .await
                            .map(|build_start| ShadowOutcome::Started {
                                build_start,
                                epoch: turn.epoch(),
                            })
                    }
                    ShadowStep::Fill { after } => shadow::fill_chunk(conn, after.as_deref())
                        .await
                        .map(|last| ShadowOutcome::Filled { last }),
                    ShadowStep::Swap { build_start, epoch } => {
                        shadow::swap(conn, &build_start, epoch, turn.epoch())
                            .await
                            .map(|rows| ShadowOutcome::Swapped { rows })
                    }
                };
                turn.answer(responder, res);
            }
            LowPriCommand::RebuildFts { responder } => {
                let res = conn
                    .execute(crate::schema::ddl::REBUILD_CONCEPTS_FTS, ())
                    .await
                    .map(|_| ())
                    .map_err(Into::into);
                turn.answer(responder, res);
            }
            LowPriCommand::Analyze {
                incremental,
                responder,
            } => {
                // Both go through `query()`, not `execute()`. `PRAGMA optimize`
                // yields rows, and libsql's `execute()` rejects any statement
                // that does ("Execute returned rows") — the same trap
                // `configure` documents. `ANALYZE` does not yield rows, but is
                // issued the same way so the two arms cannot drift into needing
                // different call shapes for no visible reason.
                let sql = if incremental {
                    crate::schema::ddl::OPTIMIZE
                } else {
                    crate::schema::ddl::ANALYZE
                };
                let res = conn.query(sql, ()).await.map(|_| ()).map_err(Into::into);
                turn.answer(responder, res);
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

/// Every recorded interval for one relationship key, for [`Interval::overlaps`]
/// to judge.
///
/// **Three equalities and nothing else, deliberately — and the "and nothing
/// else" was measured, not assumed.** The first version added
/// `AND valid_from < :new_valid_to`, a provably safe narrowing (overlap requires
/// `max(start) < min(end)`, so an interval starting at or after the new one's end
/// cannot overlap it). It cost **9.8 ms on a 90-edge chunk into a 2,000-edge
/// hub**, because it walked the planner straight into D-059's trap:
///
/// ```text
/// with the range:     SEARCH links_current USING COVERING INDEX
///                     idx_lc_traversal_cover (source_id=? AND valid_from<?)
/// without it:         SEARCH links_current USING COVERING INDEX
///                     idx_lc_open_interval (source_id=? AND target_id=? AND edge_type=?)
/// ```
///
/// `idx_lc_traversal_cover` leads on `(source_id, valid_from, …)` and contains
/// every column this query mentions, so with a `valid_from` range available it
/// wins as a covering index while binding **one** equality column — and the
/// guard scans the source's entire out-degree. That is the same shape as the
/// defect D-059 diagnosed in `trg_links_single_open`, reintroduced by an
/// optimisation, one wave after it was fixed.
///
/// Dropping the range makes the query a pure three-column point lookup that
/// `idx_lc_open_interval` serves exactly, and the rows it returns are the
/// intervals recorded for one `(source, target, edge_type)` — a version count,
/// not an out-degree. **A narrowing predicate is not free if it changes the
/// plan**, which is the general lesson and the reason this constant carries its
/// own `EXPLAIN` output.
const OVERLAP_CANDIDATES: &str = "SELECT valid_from, valid_to FROM links_current \
     WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3 \
       AND valid_from <> ?4";

/// Whether this pair is the storage layer's case rather than this guard's.
///
/// Two **open** intervals overlap — they share every instant from the later
/// start onwards — so a naive overlap check reports them, and reporting them
/// here would leave `DbError::SingleOpenViolation` constructible by nothing.
/// That variant is the more specific error, it is enforced by
/// `trg_links_single_open` rather than by this function, and its field names
/// were ratified in §1.2. Shadowing it with a general one would be defect Q's
/// shape reintroduced by a fix: a typed error that no code path can produce.
///
/// So the two guards partition the space rather than overlapping it. Both open
/// belongs to the trigger. Everything else — open against closed, closed against
/// closed — is unguarded at the storage layer and belongs here. That the split
/// is exactly the trigger's `WHEN` clause is not a coincidence; it is the
/// definition of what was missing.
fn defer_to_single_open(proposed: &Interval, existing: &Interval) -> bool {
    proposed.is_open() && existing.is_open()
}

/// Refuse an assertion whose valid-time interval overlaps one already recorded
/// for the same `(source, target, edge_type)` — **defect AA, D-060**.
///
/// `trg_links_single_open` fires only `WHEN NEW.valid_to = '9999-…'`, so it
/// guards the open sentinel and nothing else. Two *closed* intervals that
/// overlap were accepted without complaint, and `query_as_of_edges` at an
/// instant inside both returned one relationship as two edges.
///
/// **This runs in the write actor, which is what makes it sound.** The obvious
/// place is `EdgeAssertion::normalized`, and it cannot go there — `normalized`
/// is a pure function with no connection, and doing the read at the API boundary
/// instead would leave a check-then-write race between the read and the actor's
/// insert. Inside the actor there is one writer by construction (D-014), and for
/// the batch paths this runs inside the same transaction as the insert, so the
/// window does not exist rather than being small.
///
/// **What it does not cover, and §4.2 now says so:** raw SQL against the same
/// file. The storage layer permits what this API refuses, which is the honest
/// cost of not putting the check in a trigger. The alternative was a second
/// index probe inside `trg_links_single_open` on every insert — on the path
/// D-059 has just finished making fast — for a guarantee that only holds against
/// callers who were going through the actor anyway.
///
/// `valid_from <> ?4` excludes the row being re-asserted. Re-assertion at the
/// same `valid_from` is Doctrine III's ordinary case — a new belief about the
/// same interval — and is settled by the primary key and the single-open
/// trigger, not here.
/// The single-assertion path prepares one statement for one check, which is what
/// `AssertEdge` needs; the batch path prepares once and calls
/// [`check_prepared`] per row.
async fn reject_overlapping_interval(
    conn: &libsql::Connection,
    edge: &EdgeAssertion,
) -> Result<()> {
    let stmt = conn.prepare(OVERLAP_CANDIDATES).await?;
    check_prepared(&stmt, edge).await
}

/// The guard's body, against a statement the caller has already prepared.
///
/// **Split out because preparing per row was worth 10.4 ms on a 90-edge chunk**
/// (§8.8) — the same defect D-056 and D-057 diagnosed and fixed for
/// `INSERT_LINK`, reintroduced by the Wave 2 guard that was written beside it.
/// Measured with and without the guard, on a 2,000-edge hub: 8.65 ms → 19.25 ms,
/// and *identical* with and without `idx_lc_open_interval`, which is what
/// identified preparation rather than a scan as the cost. A guard that reads an
/// index correctly and prepares its statement 90 times is indistinguishable, at
/// the call site, from one that scans.
///
/// `reset()` between rows is not optional: libsql binds and steps without
/// resetting, so a reused statement must be returned to its initial state.
async fn check_prepared(stmt: &libsql::Statement, edge: &EdgeAssertion) -> Result<()> {
    let proposed = Interval::new(edge.valid_from.clone(), edge.valid_to.clone());

    stmt.reset();
    let mut rows = stmt
        .query(libsql::params![
            edge.source.as_str(),
            edge.target.as_str(),
            edge.edge_type.as_str(),
            edge.valid_from.as_str()
        ])
        .await?;

    while let Some(row) = rows.next().await? {
        let existing = Interval::new(row.get::<String>(0)?, row.get::<String>(1)?);
        if defer_to_single_open(&proposed, &existing) {
            continue;
        }
        if proposed.overlaps(&existing) {
            return Err(DbError::OverlappingInterval {
                overlap: Box::new(crate::error::Overlap {
                    source_id: edge.source.clone(),
                    target_id: edge.target.clone(),
                    edge_type: edge.edge_type.clone(),
                    valid_from: edge.valid_from.clone(),
                    valid_to: edge.valid_to.clone(),
                    existing_from: existing.valid_from,
                    existing_to: existing.valid_to,
                }),
            });
        }
    }

    Ok(())
}

/// The same guard applied *within* a batch, before any of it is written.
///
/// The database check cannot see rows that are not in the database yet, so a
/// batch carrying two overlapping intervals for one relationship would pass
/// every per-row check and commit the overlap in one transaction. Quadratic in
/// the batch, which is affordable because the chunk is bounded at
/// [`chunk_rows::EDGES`] = 90 and because the comparison is a pair of string
/// compares — and because grouping first means the inner loop only ever runs
/// over edges sharing a key, which is normally one.
fn reject_overlaps_within(edges: &[EdgeAssertion]) -> Result<()> {
    for (i, a) in edges.iter().enumerate() {
        let ia = Interval::new(a.valid_from.clone(), a.valid_to.clone());
        for b in &edges[i + 1..] {
            if a.source != b.source || a.target != b.target || a.edge_type != b.edge_type {
                continue;
            }
            // Identical valid_from is re-assertion within one batch: the last
            // writer wins by seq_id, as it does across batches. Not an overlap.
            if a.valid_from == b.valid_from {
                continue;
            }
            let ib = Interval::new(b.valid_from.clone(), b.valid_to.clone());
            // Both open is the trigger's case; it fires during the insert and
            // rolls the batch back with the more specific error.
            if defer_to_single_open(&ia, &ib) {
                continue;
            }
            if ia.overlaps(&ib) {
                return Err(DbError::OverlappingInterval {
                    overlap: Box::new(crate::error::Overlap {
                        source_id: a.source.clone(),
                        target_id: a.target.clone(),
                        edge_type: a.edge_type.clone(),
                        valid_from: a.valid_from.clone(),
                        valid_to: a.valid_to.clone(),
                        existing_from: ib.valid_from,
                        existing_to: ib.valid_to,
                    }),
                });
            }
        }
    }
    Ok(())
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

    // Before the transaction opens: a batch that contradicts itself is refused
    // without taking the write lock at all (D-060).
    reject_overlaps_within(edges)?;

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // Inside the transaction, so the rows this checks against cannot change
    // between the check and the insert.
    // One preparation for the whole chunk, not one per row — see
    // `check_prepared`, and D-056 for the same lesson learned on `INSERT_LINK`.
    let guard = tx.prepare(OVERLAP_CANDIDATES).await?;
    for edge in edges {
        if let Err(e) = check_prepared(&guard, edge).await {
            // Released before the rollback: a live statement on the connection
            // is what makes SQLite refuse to end a transaction.
            drop(guard);
            let _ = tx.rollback().await;
            return Err(e);
        }
    }
    drop(guard);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(target: &str, micros: usize) -> EdgeAssertion {
        EdgeAssertion::new("src", target, "LINKS")
            .valid_from(format!("2026-01-01T00:00:00.{micros:06}Z"))
            .valid_to(format!("2026-01-01T00:00:00.{:06}Z", micros + 1))
    }

    /// The estimate must depend on the batch's **shape**, not only its size.
    ///
    /// This is the correction T1.3's "rows × per-row cost" needed. Two batches
    /// of the same length whose measured holds differ by 7× must not be
    /// predicted identically, and the direction matters: a model that averages
    /// the two under-predicts the expensive shape, which is the only one anyone
    /// needs warning about.
    #[test]
    fn two_batches_of_one_size_are_not_predicted_alike() {
        const N: usize = 20_000;
        let fanout: Vec<_> = (0..N).map(|i| edge(&format!("t{i:07}"), i)).collect();
        let history: Vec<_> = (0..N).map(|i| edge("t0", i)).collect();

        let (a, b) = (estimated_bulk_hold(&fanout), estimated_bulk_hold(&history));
        assert!(
            b > a * 5,
            "the guard's expensive path is 16x dearer per pair and this batch \
             takes it on every pair, but the estimates are {a:?} and {b:?}"
        );
    }

    /// Measured on libSQL 0.9.30: 2.5 s and 18.6 s for those two batches. The
    /// estimator tracked both within 5%, and this pins that it still does — a
    /// coefficient edited without re-measuring fails here.
    #[test]
    fn the_estimate_matches_what_was_measured() {
        const N: usize = 20_000;
        let fanout: Vec<_> = (0..N).map(|i| edge(&format!("t{i:07}"), i)).collect();
        let history: Vec<_> = (0..N).map(|i| edge("t0", i)).collect();

        for (batch, measured_ms, label) in
            [(fanout, 2_618u128, "fanout"), (history, 18_057, "history")]
        {
            let predicted = estimated_bulk_hold(&batch).as_millis();
            let ratio = predicted as f64 / measured_ms as f64;
            assert!(
                (0.8..1.25).contains(&ratio),
                "{label}: predicted {predicted} ms against a measured \
                 {measured_ms} ms ({ratio:.2}x). Re-run \
                 examples/bulk_atomic_diag.rs before changing the coefficients."
            );
        }
    }

    /// An empty or single-edge batch has no pairs, and the arithmetic must not
    /// underflow computing it.
    #[test]
    fn a_batch_too_small_to_have_pairs_still_estimates() {
        assert_eq!(estimated_bulk_hold(&[]), std::time::Duration::ZERO);
        let one = [edge("t0", 0)];
        assert_eq!(
            estimated_bulk_hold(&one),
            std::time::Duration::from_nanos(73_000)
        );
    }

    /// The warning threshold sits well above the bound this path is exempt from.
    ///
    /// Warning at `CHUNK_BUDGET` would fire on batches working exactly as
    /// designed — the exemption is a contract (D-014), not a failure — and a
    /// warning that fires on correct behaviour gets filtered out, taking the
    /// 18-second case with it.
    #[test]
    fn the_warning_threshold_is_not_the_chunk_budget() {
        assert!(BULK_ATOMIC_WARN_HOLD > CHUNK_BUDGET * 10);
    }

    // -----------------------------------------------------------------------
    // next_chunk_size — the control law (0.12.0, W2)
    //
    // All of these run without a database, a clock or an actor, which is why
    // W2 comes before W3: the loop that will use this function can only be
    // tested against a real write, and the properties below cannot be observed
    // there without also observing the machine.
    // -----------------------------------------------------------------------

    use std::time::Duration;

    /// `next_chunk_size` with the shipped budget and floor.
    fn step_to(current: usize, held_ms: f64, ceiling: usize) -> usize {
        next_chunk_size(
            current,
            Duration::from_nanos((held_ms * 1_000_000.0) as u64),
            CHUNK_BUDGET,
            CHUNK_FLOOR,
            ceiling,
        )
    }

    /// The edge path, which is every test here that does not say otherwise.
    fn step(current: usize, held_ms: f64) -> usize {
        step_to(current, held_ms, chunk_rows::EDGES)
    }

    /// Iterate the law against a machine that costs `per_row_us` per row plus a
    /// fixed `overhead_ms` per transaction — the two-term model D-142 measured.
    fn converge(
        start: usize,
        per_row_us: f64,
        overhead_ms: f64,
        ceiling: usize,
        steps: usize,
    ) -> Vec<usize> {
        let mut size = start;
        (0..steps)
            .map(|_| {
                let held = overhead_ms + per_row_us * size as f64 / 1000.0;
                size = step_to(size, held, ceiling);
                size
            })
            .collect()
    }

    /// The reason the shrink is proportional rather than a halving: at 4× over
    /// budget, halving needs three steps and every one of them is a latency
    /// miss a caller can feel.
    ///
    /// Run on the annotations path, because it is the only one whose ceiling
    /// leaves room to start far above a size that is reachable — on the edge
    /// path a 4× miss lands under [`CHUNK_FLOOR`], which is a different test.
    #[test]
    fn a_chunk_far_over_budget_converges_from_above_in_at_most_two_steps() {
        const CEILING: usize = chunk_rows::ANNOTATIONS;
        let (per_row_us, overhead_ms) = (20.0, 0.05);
        let held = |n: usize| overhead_ms + per_row_us * n as f64 / 1000.0;
        assert!(
            held(CEILING) > 4.0 * 3.0,
            "the start is not far over budget"
        );

        let trace = converge(CEILING, per_row_us, overhead_ms, CEILING, 4);
        let first_in_budget = trace
            .iter()
            .position(|&n| held(n) <= 3.0)
            .expect("never reached the budget");
        assert!(
            first_in_budget <= 1,
            "took {} steps to get under budget: {trace:?}",
            first_in_budget + 1
        );
    }

    /// Growth is additive, so a size that is merely comfortable cannot leap the
    /// ceiling — and cannot overshoot the budget by more than a quarter.
    #[test]
    fn growth_is_slow_and_shrinking_is_fast() {
        let grown = step(40, 1.0);
        assert!(
            (41..=50).contains(&grown),
            "40 rows at 1 ms should grow by about a quarter, got {grown}"
        );
        let shrunk = step(90, 9.0);
        assert!(
            shrunk <= 40,
            "90 rows at 3x the budget should shrink proportionally, got {shrunk}"
        );
    }

    /// The dead band. Between `budget / 2` and `budget` the size is right and
    /// moving it only costs a re-measurement; without this the law oscillates
    /// across the bound forever.
    #[test]
    fn a_chunk_inside_the_band_is_left_alone() {
        for held_ms in [1.6, 2.0, 2.5, 2.9, 3.0] {
            assert_eq!(step(60, held_ms), 60, "moved at {held_ms} ms");
        }
        assert_ne!(step(60, 1.4), 60, "did not grow at well under half budget");
    }

    /// Both clamps, and the floor's violation stated as a test rather than only
    /// as a comment: a populated table drives this to `CHUNK_FLOOR` and holds it
    /// there **over budget**, which is [`CHUNK_FLOOR`]'s documented trade.
    #[test]
    fn the_floor_and_the_ceiling_both_hold() {
        // 118 µs/row + 0.03 ms fixed — the populated arm, where 35 rows is
        // ~4.1 ms and no size in range meets the bound.
        let trace = converge(chunk_rows::EDGES, 118.0, 0.03, chunk_rows::EDGES, 8);
        assert!(
            trace.iter().all(|&n| n >= CHUNK_FLOOR),
            "fell through the floor: {trace:?}"
        );
        assert_eq!(*trace.last().unwrap(), CHUNK_FLOOR, "settled off the floor");

        // A free machine cannot grow past the path's constant.
        let fast = converge(CHUNK_FLOOR, 1.0, 0.01, chunk_rows::EDGES, 40);
        assert_eq!(*fast.last().unwrap(), chunk_rows::EDGES);
        assert!(fast.iter().all(|&n| n <= chunk_rows::EDGES));
    }

    /// Zero is the one answer that cannot be recovered from: a loop asked for
    /// chunks of no rows makes no progress and never finishes. Degenerate
    /// inputs included, since `held` is a measurement and measurements arrive
    /// from a machine under load.
    #[test]
    fn the_law_never_returns_zero() {
        let cases = [
            (0usize, Duration::ZERO),
            (0, Duration::from_secs(60)),
            (1, Duration::from_secs(60)),
            (90, Duration::from_secs(3600)),
            (usize::MAX, Duration::from_nanos(1)),
            (1, Duration::ZERO),
        ];
        for (current, held) in cases {
            for (floor, ceiling) in [(35, 90), (1, 1), (0, 0), (90, 35)] {
                let n = next_chunk_size(current, held, CHUNK_BUDGET, floor, ceiling);
                assert!(
                    n > 0,
                    "returned 0 for current={current}, held={held:?}, \
                     floor={floor}, ceiling={ceiling}"
                );
            }
        }
    }

    /// A zero budget is not a configuration anyone should reach, but it is one
    /// division away from a panic, so it is pinned.
    #[test]
    fn a_zero_budget_shrinks_to_the_floor_rather_than_dividing_by_it() {
        assert_eq!(
            next_chunk_size(90, Duration::from_millis(1), Duration::ZERO, 35, 90),
            35
        );
    }
}
