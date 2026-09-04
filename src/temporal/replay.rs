use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::{DbError, Result};
use crate::temporal::as_of::NodeAttributes;

/// One lineage's belief about one edge, at the instant a fold asked (§15.2).
///
/// # Why this is a struct and was a five-tuple until 0.14.5
///
/// The tuple had nowhere to put `branch_id`, and that was not a cosmetic
/// shortfall: [D-216](../../docs/architecture/s13-decision-register.md) widened
/// the four SQL folds to partition by `(table_name, entity_id, branch_id)` so
/// two lineages' beliefs about one edge would stay two rows, and then the
/// composition immediately downstream re-collapsed them, because `edge_key`
/// composed `source|target|type|valid_from` and the map it fed had one slot per
/// edge key. The widened partition was handing two rows to a container that
/// could not hold two. That is [D-221](../../docs/architecture/s13-decision-register.md#d-221),
/// and this type is its fix.
///
/// A struct rather than a six-tuple because the next field to arrive should be
/// additive, which is why it is also `#[non_exhaustive]` — the same call
/// [D-207](../../docs/architecture/s13-decision-register.md#d-207) made for
/// `DbError`, one release earlier, for the same reason. Construct these by
/// reading a [`MaterializedState`]; the crate is the only writer.
///
/// **Ordered by the tuple order of its fields**, so a `Vec<EdgeBelief>` sorts to
/// a canonical form and two reconstructions of the same instant are *equal*
/// rather than merely equivalent — a property the snapshot suite compares on.
///
/// # Constructing one
///
/// `#[non_exhaustive]` means no crate but this one may write the literal, and
/// [`save_snapshot`](crate::temporal::save_snapshot) is public and takes a
/// `MaterializedState` — so without a constructor the attribute would not make
/// the next field additive, it would make a public function uncallable. Use
/// [`EdgeBelief::new`], which takes the five fields that were the tuple and
/// defaults the sixth to the trunk, with [`EdgeBelief::on_branch`] for the
/// rest. That is [`EdgeAssertion::new`](crate::graph::EdgeAssertion::new)'s
/// shape, deliberately: the two are the same fact travelling in opposite
/// directions and should not need two idioms.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EdgeBelief {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    pub valid_from: String,
    pub valid_to: String,
    /// The lineage that holds this belief (0.14.5, D-221).
    ///
    /// `#[serde(default = "default_branch")]` so the field is additive at the
    /// bincode level. It is belt and braces — the snapshot container refuses any
    /// file whose format version is not this build's, and 0.14.5 bumps it
    /// precisely so a state written without this field gets a named refusal
    /// rather than a deserialisation error — but a default that is *right* costs
    /// nothing and `'main'` is what every pre-v12 row actually carried.
    #[serde(default = "default_branch")]
    pub branch_id: String,
}

fn default_branch() -> String {
    crate::schema::ddl::MAIN_BRANCH.to_string()
}

impl EdgeBelief {
    /// A belief held by the trunk. Use [`Self::on_branch`] for any other.
    ///
    /// Five arguments rather than six because `main` is what every belief
    /// written before 0.14.5 carried, so a caller porting a five-tuple wraps it
    /// and is correct rather than being asked a question the old shape could
    /// not have answered.
    pub fn new(
        source_id: impl Into<String>,
        target_id: impl Into<String>,
        edge_type: impl Into<String>,
        valid_from: impl Into<String>,
        valid_to: impl Into<String>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            target_id: target_id.into(),
            edge_type: edge_type.into(),
            valid_from: valid_from.into(),
            valid_to: valid_to.into(),
            branch_id: default_branch(),
        }
    }

    /// The lineage holding this belief.
    ///
    /// Unchecked against `branches`, because this type is a value and not a
    /// write: a `MaterializedState` naming a lineage the register has never
    /// heard of is a snapshot that will disagree with the log, which
    /// `verify_snapshot_chain` is there to report.
    pub fn on_branch(mut self, branch_id: impl Into<String>) -> Self {
        self.branch_id = branch_id.into();
        self
    }

    /// The log `entity_id` this belief was folded under.
    ///
    /// Must match `trg_links_log_insert`'s
    /// `source_id || '|' || target_id || '|' || edge_type || '|' || valid_from`
    /// exactly, or a delta row will fail to replace the snapshot row it
    /// supersedes. Safe because ULIDs are Crockford base32 and edge types are
    /// `[A-Z0-9]+`, so `|` cannot occur inside a component (§4.3).
    ///
    /// **This is not a unique key across lineages** and must not be used as one
    /// — see [`Self::belief_key`], which is.
    pub fn entity_id(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.source_id, self.target_id, self.edge_type, self.valid_from
        )
    }

    /// What identifies this belief: the edge key **and** the lineage holding it.
    pub fn belief_key(&self) -> String {
        format!("{}|{}", self.entity_id(), self.branch_id)
    }
}

/// Full materialized state reconstructed from transaction_log replay (§5.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedState {
    pub seq_anchor: i64,
    pub timestamp: String,
    pub concepts: HashMap<String, NodeAttributes>,
    /// Every lineage's belief, each labelled with the lineage holding it.
    ///
    /// **Not resolved to one lineage's view**, and deliberately: `reconstruct`
    /// asks a whole-ledger question — *what did the ledger hold at `ts`* — and
    /// the ledger held both. Resolving here would require an ancestry, which
    /// requires a connection this type does not have, and would silently answer
    /// a narrower question than the one asked. A caller wanting one lineage's
    /// view uses `graph::TraversalBuilder::on_branch` or
    /// `temporal::query_as_of_edges_on`, which resolve against the register
    /// ([D-220](../../docs/architecture/s13-decision-register.md#d-220)).
    pub edges: Vec<EdgeBelief>,
    /// **Nothing had been recorded yet at `timestamp`** (0.8.0, B5, D-121).
    ///
    /// An empty state has two meanings and a caller can act differently on
    /// them. *Everything was retired by then* is a fact about the data;
    /// *the ledger had not started* is a fact about the question. Both come
    /// back as zero concepts and zero edges, so the difference has to be
    /// carried rather than inferred.
    ///
    /// Set only when the log was verified **intact** — see
    /// `hot_log_reach`. If rows had been archived away, `ts` below the hot
    /// floor is not "before history", it is "the history is in the other file",
    /// and that path raises instead of answering.
    ///
    /// `#[serde(default)]` so the field is additive: a snapshot written without
    /// it deserialises with `false`, which is the right answer for any state
    /// that had rows to fold. Old snapshots cannot actually reach this code —
    /// the container carries `SCHEMA_VERSION` and v8 refused every v7 file
    /// (D-043) — but the tolerance costs nothing and the next field to arrive
    /// may not land in a release that bumps the schema.
    #[serde(default)]
    pub predates_recorded_history: bool,
}

impl MaterializedState {
    /// The state before any log row has been applied.
    fn empty(ts: &str) -> Self {
        Self {
            seq_anchor: 0,
            timestamp: ts.to_string(),
            concepts: HashMap::new(),
            edges: Vec::new(),
            predates_recorded_history: false,
        }
    }
}

/// The newest log payload shape this build writes and the highest it can read.
///
/// Kept beside the folds because they are the only readers, and bumped in step
/// with the `json_object('v', …)` literals in `schema::ddl` — a test asserts the
/// two agree, since nothing else would notice them drifting apart.
pub(crate) const PAYLOAD_VERSION: u8 = 2;

/// Every fold partitions on `(table_name, entity_id)`, never `entity_id` alone.
///
/// The two namespaces are not disjoint and nothing makes them so. A link's
/// `entity_id` is the synthetic `source|target|type|valid_from`; a concept's is
/// whatever the caller passed, unvalidated (defect AD). Partitioning on the id
/// alone therefore lets a concept and a link contend for one window, and
/// `ROW_NUMBER() = 1` hands the whole partition to whichever has the greater
/// `seq_id` — so the loser vanishes from the reconstruction while sitting
/// plainly in both `concepts` and `transaction_log`. Silent, and on the read
/// path the ledger exists to make trustworthy.
///
/// Validating identifiers would make the collision unreachable and is the
/// durable fix; this makes it harmless regardless, which is the property worth
/// having at the fold. `table_name` leads the partition because the log is
/// already indexed on `entity_id` and the discriminator is two values wide.
const HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#;

/// Fold over hot and cold together (§5.5, D-026). Requires `cold` to be ATTACHed.
///
/// The hot entry wins for entities present in both files because its `seq_id` is
/// greater — the same last-writer-wins rule as snapshot composition.
fn cold_fold(cold_lineage: ColdLineage) -> String {
    format!(
        r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM (
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id FROM main.transaction_log
            UNION ALL
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, {cold} FROM cold.transaction_log
        ) WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#,
        cold = cold_lineage.projection()
    )
}

/// Fold over the hot log *above a snapshot anchor* (§5.5, D-049).
///
/// `seq_id > ?2` is an inequality, and deliberately so: `AUTOINCREMENT` leaves
/// gaps whenever a transaction rolls back, so successor arithmetic
/// (`seq_id = :anchor + 1`) would stop at the first gap and silently truncate
/// the delta. This is the first anchored fold in the crate, which makes it the
/// first code D-024's rule has ever bound — before this the rule was vacuous,
/// not satisfied.
const ANCHORED_HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1 AND seq_id > ?2
    ) WHERE rn = 1
"#;

/// Fold over hot **and cold** above a snapshot anchor (§5.5, 0.5.5).
///
/// The union is what lets composition survive an archive. Rows keep their
/// `seq_id` when they move to cold — the cold schema declares a plain `INTEGER
/// PRIMARY KEY` precisely so history is not renumbered — so `seq_id > ?2`
/// partitions the two files consistently and last-writer-wins across them by the
/// same rule the unanchored folds use.
fn anchored_cold_fold(cold_lineage: ColdLineage) -> String {
    format!(
        r#"
    SELECT seq_id, table_name, entity_id, operation, payload, branch_id
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload, branch_id,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id, branch_id ORDER BY seq_id DESC) as rn
        FROM (
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, branch_id FROM main.transaction_log
            UNION ALL
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at, {cold} FROM cold.transaction_log
        ) WHERE recorded_at <= ?1 AND seq_id > ?2
    ) WHERE rn = 1
"#,
        cold = cold_lineage.projection()
    )
}

/// Whether an attached cold file predates the lineage column (§15.2, v12).
///
/// Cold files are **read-only media as far as the read path is concerned**.
/// They get moved (D-026), they can sit on a share, and a fold that upgraded
/// one in order to read it would be a write on a path callers have every reason
/// to believe is a read. So the shape is detected and tolerated, never
/// corrected: the archive *writer* upgrades, and only inside its own
/// transaction.
///
/// Detection is column presence rather than a version stamp, because a cold
/// file carries no version anyone can trust — it is a file that has been moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColdLineage {
    /// v12 or later: the file stamps its own rows.
    Stamped,
    /// Pre-v12: every row in it was written when only the trunk existed.
    PreV12,
}

impl ColdLineage {
    fn projection(self) -> &'static str {
        match self {
            ColdLineage::Stamped => "branch_id",
            // A literal, not a default: rows written before lineage existed
            // *were* trunk rows, and saying so is a fact about them rather than
            // a fallback.
            ColdLineage::PreV12 => "'main' AS branch_id",
        }
    }
}

/// Ask the attached cold file whether it carries `transaction_log.branch_id`.
///
/// Returns [`ColdLineage::PreV12`] when the pragma cannot be read at all. That
/// is the conservative direction: a fold that guesses "stamped" against a v11
/// file fails with `no such column`, while a fold that guesses "pre-v12"
/// against a v12 file reads rows it can still fold — it would mislabel a
/// branch's rows as trunk, which is why the guess is never made when the pragma
/// answers.
async fn cold_lineage(conn: &libsql::Connection) -> ColdLineage {
    let Ok(mut rows) = conn
        .query("PRAGMA cold.table_info(transaction_log)", ())
        .await
    else {
        return ColdLineage::PreV12;
    };
    while let Ok(Some(row)) = rows.next().await {
        if row.get::<String>(1).is_ok_and(|name| name == "branch_id") {
            return ColdLineage::Stamped;
        }
    }
    ColdLineage::PreV12
}

/// The winning log rows for one fold, before they are applied to a base state.
///
/// Absence and disappearance are different facts, and a merge is where the
/// difference starts to matter. A full fold from nothing can treat "this entity
/// went away" and "there is no row for it" identically — both end as absence.
/// Composed onto a snapshot they are opposites: a disappearance must *remove*
/// the entity the snapshot carries, and skipping it leaves the snapshot's stale
/// row standing as though nothing had happened. So they are collected rather
/// than dropped, and the full fold applies them to an empty base, which keeps
/// one code path for both cases (D-049).
///
/// **There is one such set, not two (D-072).** It used to carry `edges_gone`
/// beside `concepts_gone`, and both were populated only from the `'D'` branch of
/// [`fold_delta`] — so when that branch became an error, `edges_gone` was left
/// reachable by nothing. Closing one unreachable path by opening another is not
/// a fix, so it went too.
///
/// The asymmetry is real and worth stating, because "concepts can vanish and
/// edges cannot" looks like an oversight until you follow it:
///
/// * A **concept** disappears by being *retired*, which writes a `'U'` row whose
///   payload has `retired = 1`. That is a genuine removal from a composed state
///   and `concepts_gone` carries it.
/// * An **edge** never disappears. It is retired by asserting a successor over
///   the same interval key — same `source|target|type|valid_from`, later
///   `recorded_at` — so the log row is an `'I'` under the *same* `entity_id`, and
///   last-writer-wins in [`Self::apply_to`] replaces the tuple in place. There is
///   nothing to remove because nothing left; the interval simply closed.
///
/// That is Doctrine III showing through: an edge assertion is immutable and
/// superseded, never deleted.
#[derive(Default)]
struct Delta {
    concepts: HashMap<String, NodeAttributes>,
    /// Keyed by `entity_id` **and** `branch_id` — see [`EdgeBelief::belief_key`].
    ///
    /// `entity_id` alone was the collapse [D-221](../../docs/architecture/s13-decision-register.md#d-221)
    /// records: it is the edge key, shared across lineages by design, so an
    /// ancestor's assertion and a descendant's correction landed in one slot.
    edges: HashMap<String, EdgeBelief>,
    /// Concepts retired as of the fold's instant. See the type's note for why
    /// there is no edge equivalent.
    concepts_gone: HashSet<String>,
    max_seq: i64,
}

/// Release a `cold` handle left attached by an earlier call (§5.5, D-044).
///
/// Both ATTACH sites pair with an unconditional DETACH on the way out, so in
/// the normal course this finds nothing and the statement fails harmlessly with
/// "no such database: cold". It exists for the case the pairing cannot cover: a
/// panic unwinding between the two, which skips the DETACH no matter which exit
/// path the `Result` would have taken.
///
/// A `Drop` guard is the reflex here and does not work — `execute` is `async`,
/// and a `Drop` impl cannot await, so it would build a future, discard it, and
/// leave the handle attached while looking like it had cleaned up. Recovering
/// on the way *in* needs no destructor, works regardless of how the handle
/// leaked, and turns permanent poisoning of the connection into one failed
/// statement nobody sees.
pub(crate) async fn detach_stale_cold(conn: &libsql::Connection) {
    let _ = conn.execute("DETACH DATABASE cold", ()).await;
}

/// Reconstruct database state as believed at past instant `ts` using window-function log fold (§5.5, D-026).
///
/// When `ts` predates the hot log's horizon the cold database is ATTACHed for
/// exactly one fold and DETACHed unconditionally on the way out, error paths
/// included. ATTACH is not transactional and survives ROLLBACK, so a handle
/// leaked by an early return would make every later `reconstruct` *and* every
/// later `archive` fail with "database cold is already in use" — one corrupt
/// payload would permanently poison the connection. This is the same failure
/// mode `archive()` carries a note about, and the two now share a shape.
/// Snapshot composition (§5.5, D-049) applies when `snapshots_dir` holds a
/// snapshot at or before `ts` and no archive database exists — see
/// `snapshot_anchor` for why archiving disables it. Otherwise the fold runs
/// from genesis, which is correct and costs what the whole log costs.
pub async fn reconstruct(
    conn: &libsql::Connection,
    ts: &str,
    archive_path: Option<&Path>,
    snapshots_dir: Option<&Path>,
) -> Result<MaterializedState> {
    match hot_log_reach(conn, ts, archive_path).await? {
        HotLogReach::Covers => {
            if let Some(base) = snapshot_anchor(snapshots_dir, ts).await {
                let anchor = base.seq_anchor;
                let delta =
                    fold_delta(conn, ANCHORED_HOT_FOLD, libsql::params![ts, anchor]).await?;
                return Ok(delta.apply_to(base, ts));
            }
            return fold(conn, ts, HOT_FOLD).await;
        }
        HotLogReach::PredatesRecordedHistory => {
            // Nothing had been recorded by `ts`, and nothing has been removed
            // from the log, so there is no history anywhere to go looking for.
            // The empty state is the answer, flagged so a caller can tell it
            // from a state that is empty because everything was retired.
            let mut state = MaterializedState::empty(ts);
            state.predates_recorded_history = true;
            return Ok(state);
        }
        HotLogReach::NeedsArchive => {}
    }

    // The delta lives in the cold archive database. Both ways of failing to
    // reach it carry `archive_hint`, which is the message the rejected hot-side
    // marker was wanted for — see that function for why no marker is needed.
    //
    // **Computed inside the error arms, not before them.** `NeedsArchive` is the
    // ordinary path to a cold fold and usually succeeds; an eager hint would put
    // an extra query on it for a string almost every caller discards. An
    // injection probe caught this — `a_failed_cold_reconstruct_still_detaches`
    // reached the hint on a run that raised nothing from here.
    let archive = match archive_path {
        Some(p) => p,
        None => {
            return Err(DbError::ReplayCorrupt {
                seq: 0,
                reason: format!(
                    "state at {ts} predates the hot log and no archive path was given; {}",
                    archive_hint(conn).await
                ),
            })
        }
    };
    if !archive.exists() {
        return Err(DbError::ReplayCorrupt {
            seq: 0,
            reason: format!(
                "archive database file {archive:?} does not exist; {}",
                archive_hint(conn).await
            ),
        });
    }

    detach_stale_cold(conn).await;

    // Bound, not interpolated: a path is caller data, and hand-rolled quote
    // doubling is a worse version of what the driver already does correctly.
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![archive.to_string_lossy().as_ref()],
    )
    .await?;

    // Asked once, after the ATTACH and before either fold, because both arms
    // need it and the answer cannot change while we hold the handle.
    let cold_shape = cold_lineage(conn).await;

    // Composition works across the archive boundary because the anchored fold
    // unions both files; before 0.5.5 it was refused here rather than made to
    // work, and the refusal was the only thing keeping the answer right.
    let result = match snapshot_anchor(snapshots_dir, ts).await {
        Some(base) => {
            let anchor = base.seq_anchor;
            fold_delta(
                conn,
                &anchored_cold_fold(cold_shape),
                libsql::params![ts, anchor],
            )
            .await
            .map(|delta| delta.apply_to(base, ts))
        }
        None => fold(conn, ts, &cold_fold(cold_shape)).await,
    };

    // Unconditional: see the ATTACH note above.
    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("reconstruct: failed to DETACH cold database: {e}");
    }

    result
}

/// Fold from genesis and compare against the composed answer (§5.5, T5.3,
/// D-092).
///
/// # The problem this exists for
///
/// [`crate::temporal::save_snapshot`] is written by `write_final`, which calls
/// [`reconstruct`] — and `reconstruct` composes onto the *previous* snapshot
/// whenever one is usable. So snapshot *n* is derived from snapshot *n−1*, and
/// there is no periodic full fold anywhere in the chain. An error introduced at
/// any link is copied forward indefinitely, and every subsequent read agrees
/// with it, because they are all reading the same descendant.
///
/// The project's own open item names the difficulty honestly: a full fold is
/// exactly the cost snapshots exist to avoid, so this cannot run on every read.
/// It is a **scheduling** problem, and this function is the thing to schedule.
///
/// # It reports; it does not repair
///
/// Deliberate, and not merely conservative. Under [Doctrine VI] a snapshot is
/// derivative and disposable, so the repair is *delete the snapshots* — one
/// line, available to the caller, and correct without this function's help.
/// What the caller cannot get for themselves is the knowledge that the chain
/// diverged, and silently rewriting the file would destroy the only evidence of
/// a bug in composition. A divergence here is not a corrupt database; it is a
/// wrong **cache**, and it means composition has a defect worth finding.
///
/// # Cost
///
/// One fold from genesis over the whole log, plus one composed reconstruction.
/// That is the expensive path by construction — see [`crate::Database::
/// verify_snapshot_chain`] for the handle-level entry point and the note on
/// when to run it.
///
/// [Doctrine VI]: ../../../docs/architecture/s0-s3-foundations.md#doctrine-vi
pub async fn verify_snapshot_chain(
    conn: &libsql::Connection,
    ts: &str,
    archive_path: Option<&Path>,
    snapshots_dir: &Path,
) -> Result<ChainCheck> {
    // The composed answer: what every reader gets today.
    let composed = reconstruct(conn, ts, archive_path, Some(snapshots_dir)).await?;
    // The authority: the same instant, with the snapshot directory withheld, so
    // `snapshot_anchor` finds nothing and the fold runs from genesis. Passing
    // `None` is what makes this an independent computation rather than a second
    // call to the thing under test.
    let folded = reconstruct(conn, ts, archive_path, None).await?;
    Ok(ChainCheck::compare(ts, &composed, &folded))
}

/// The result of a [`verify_snapshot_chain`] cross-check.
///
/// Carries the disagreements rather than a bool, because "the chain diverged" is
/// not actionable and "these three concepts differ, and this edge is present in
/// one and not the other" is. Bounded — see [`ChainCheck::SAMPLE_LIMIT`] — since
/// a chain that went wrong early can disagree about every row, and a report that
/// is the size of the database is one nobody reads.
#[derive(Debug, Clone)]
pub struct ChainCheck {
    pub timestamp: String,
    /// `seq_anchor` of the composed answer and of the genesis fold. These
    /// **may legitimately differ**: the composed answer anchors at the snapshot
    /// it started from plus its delta, and the fold anchors at the newest row it
    /// saw. Reported for diagnosis, never compared.
    pub composed_anchor: i64,
    pub folded_anchor: i64,
    pub composed_concepts: usize,
    pub folded_concepts: usize,
    pub composed_edges: usize,
    pub folded_edges: usize,
    /// Concept ids present in one and not the other, or whose attributes differ.
    pub concept_disagreements: Vec<String>,
    /// Edge keys present in one and not the other.
    pub edge_disagreements: Vec<String>,
    /// True when either list was truncated at [`ChainCheck::SAMPLE_LIMIT`].
    pub truncated: bool,
}

impl ChainCheck {
    /// How many disagreements of each kind to carry.
    pub const SAMPLE_LIMIT: usize = 32;

    pub fn diverged(&self) -> bool {
        !self.concept_disagreements.is_empty() || !self.edge_disagreements.is_empty()
    }

    fn compare(ts: &str, composed: &MaterializedState, folded: &MaterializedState) -> Self {
        let mut concept_disagreements = Vec::new();
        let mut truncated = false;

        let mut ids: Vec<&String> = composed.concepts.keys().collect();
        ids.extend(folded.concepts.keys());
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let a = composed.concepts.get(id);
            let b = folded.concepts.get(id);
            let same = match (a, b) {
                (Some(a), Some(b)) => {
                    a.title == b.title
                        && a.content == b.content
                        && a.embedding_model == b.embedding_model
                }
                (None, None) => true,
                _ => false,
            };
            if !same {
                if concept_disagreements.len() < Self::SAMPLE_LIMIT {
                    concept_disagreements.push(id.clone());
                } else {
                    truncated = true;
                }
            }
        }

        // Edges are a `Vec` of tuples with no declared order, so the comparison
        // is on the set. Comparing the vectors directly would report a
        // divergence for a reordering, which is not one — and that false
        // positive is worse than useless here, because the whole point of this
        // check is that a report means "go and find the bug".
        // `valid_to` is in the key as well as the identity, because a
        // divergence in *what* the two paths believe is exactly what this
        // reports — two rows agreeing on the edge and the lineage and
        // disagreeing on the interval are a disagreement, not one row.
        let key = |e: &EdgeBelief| format!("{}|{}", e.belief_key(), e.valid_to);
        let ca: HashSet<String> = composed.edges.iter().map(key).collect();
        let fa: HashSet<String> = folded.edges.iter().map(key).collect();
        let mut edge_disagreements: Vec<String> = ca.symmetric_difference(&fa).cloned().collect();
        edge_disagreements.sort_unstable();
        if edge_disagreements.len() > Self::SAMPLE_LIMIT {
            edge_disagreements.truncate(Self::SAMPLE_LIMIT);
            truncated = true;
        }

        Self {
            timestamp: ts.to_string(),
            composed_anchor: composed.seq_anchor,
            folded_anchor: folded.seq_anchor,
            composed_concepts: composed.concepts.len(),
            folded_concepts: folded.concepts.len(),
            composed_edges: ca.len(),
            folded_edges: fa.len(),
            concept_disagreements,
            edge_disagreements,
            truncated,
        }
    }
}

impl std::fmt::Display for ChainCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.diverged() {
            return write!(
                f,
                "snapshot chain agrees with a genesis fold at {}: {} concepts, {} edges",
                self.timestamp, self.folded_concepts, self.folded_edges
            );
        }
        write!(
            f,
            "snapshot chain DIVERGED at {}: composed {} concepts / {} edges, \
             genesis fold {} concepts / {} edges; {} concept and {} edge \
             disagreements{}. The snapshots are a wrong cache, not a corrupt \
             ledger — deleting the snapshot directory restores correctness and \
             loses only speed (Doctrine VI). concepts: {:?} edges: {:?}",
            self.timestamp,
            self.composed_concepts,
            self.composed_edges,
            self.folded_concepts,
            self.folded_edges,
            self.concept_disagreements.len(),
            self.edge_disagreements.len(),
            if self.truncated { " (truncated)" } else { "" },
            self.concept_disagreements,
            self.edge_disagreements,
        )
    }
}

/// The newest usable snapshot at or before `ts`, or `None` to fold from genesis.
///
/// **Composition used to be disabled once an archive database existed, and as of
/// 0.5.5 it is not.** The reason for the refusal was real: `LOG_ARCHIVABLE`
/// (§5.7) removes superseded rows scattered through the sequence, so a row above
/// the anchor and at or before `ts` could be in cold while a newer row for the
/// same entity — recorded *after* `ts`, invisible to the fold — kept it out of
/// the hot log. The delta missed it and the snapshot answered with a stale
/// value. The fix is the one that note named: the cold log is now in the delta,
/// via [`ANCHORED_COLD_FOLD`], so the archived row is visible again and there is
/// nothing left to refuse.
///
/// Selection loads candidates newest-first and stops at the first whose
/// timestamp is at or before `ts`, so the common case — `reconstruct(now)` —
/// reads exactly one file. A snapshot this build cannot read
/// ([`DbError::SnapshotIncompatible`], D-043) is skipped, not raised: an
/// incompatible snapshot is an ordinary consequence of upgrading, and the whole
/// point of distinguishing it from corruption is that the answer is to carry on
/// without it.
///
/// # It runs on a blocking thread, and a lost one costs speed only (0.13.11, W8.1, D-184)
///
/// The scan is a directory listing plus one or more full
/// [`load_snapshot`](super::snapshot::load_snapshot) calls — decompression and
/// bincode over the whole state, on a worker that has other tasks waiting. The
/// *whole scan* is offloaded rather than each file, because the loop is
/// sequential by construction (it stops at the first usable file) and a hop per
/// candidate would add scheduling to a path whose common case reads exactly one.
///
/// A [`tokio::task::JoinError`] means the loader panicked, and the answer is the
/// same one this function already gives for every other kind of unusable file:
/// `None`, and fold from genesis. That is not leniency, it is what a snapshot
/// *is* — derivative and disposable under [Doctrine VI], so the cost of ignoring
/// one is a slower reconstruction and never a wrong one. It is also a real
/// improvement over the previous arrangement: inline, a panic in the loader
/// unwound through [`reconstruct`] and took the caller's task with it, which
/// meant a single corrupt file could stop a process that had a correct answer
/// available the whole time. W8.4 fuzzes for exactly those panics; this is what
/// happens to the ones it has not found yet.
///
/// [Doctrine VI]: ../../../docs/architecture/s0-s3-foundations.md#doctrine-vi
async fn snapshot_anchor(snapshots_dir: Option<&Path>, ts: &str) -> Option<MaterializedState> {
    let dir = snapshots_dir?.to_path_buf();
    let ts = ts.to_string();
    match tokio::task::spawn_blocking(move || newest_usable_snapshot(&dir, &ts)).await {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!("the snapshot scan did not finish ({e}); folding from genesis");
            None
        }
    }
}

/// The blocking half of [`snapshot_anchor`]: read the directory, load
/// newest-first, stop at the first snapshot at or before `ts`.
fn newest_usable_snapshot(dir: &Path, ts: &str) -> Option<MaterializedState> {
    let mut candidates: Vec<(i64, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| super::snapshot::seq_from_filename(&p).map(|s| (s, p)))
        .collect();
    candidates.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));

    for (_, path) in candidates {
        match super::snapshot::load_snapshot(&path) {
            // Sound as a string comparison because every timestamp is the
            // canonical fixed width (D-029).
            Ok(state) if state.timestamp.as_str() <= ts => return Some(state),
            Ok(_) => continue,
            Err(DbError::SnapshotIncompatible { reason, .. }) => {
                tracing::warn!("skipping snapshot {path:?}: {reason}");
                continue;
            }
            Err(e) => {
                tracing::warn!("skipping unreadable snapshot {path:?}: {e}");
                continue;
            }
        }
    }
    None
}

/// Where the answer for `ts` lives.
///
/// Three cases, not two (0.8.0, B5, D-121). This used to be a `bool`, and the
/// missing third case is the whole of B5: *below the log's floor* was folded in
/// with *the delta is elsewhere*, so a question about a time before the ledger
/// started came back as [`DbError::ReplayCorrupt`] — the class meaning the
/// ledger is damaged — naming an archive file the caller had never created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotLogReach {
    /// The hot log holds everything needed at `ts`. Fold it.
    Covers,
    /// Nothing had been recorded by `ts`, and nothing has ever been removed
    /// from the log, so no other file could hold it either. The empty state is
    /// the correct answer, not a failure to find one.
    PredatesRecordedHistory,
    /// The delta is in the cold archive. If it cannot be reached, that is an
    /// error and stays one.
    NeedsArchive,
}

/// Whether the hot log alone can answer for `ts` — a *completeness* test.
///
/// **This replaces a reach test that was not one (0.5.5).** The previous version
/// asked `MIN(recorded_at) <= ts`: whether the hot log stretches back far enough
/// to contain `ts`. That is a different question from whether it still contains
/// everything needed to answer at `ts`, and `LOG_ARCHIVABLE` (§5.7) is exactly
/// what pulls the two apart — it removes *superseded* rows, scattered through
/// the sequence rather than forming a prefix. One entity archived and another
/// not is enough: the unarchived one keeps `MIN` pointing before the cutoff
/// while the archived one's winning row is gone, and the fold silently returns a
/// state missing an entity. Measured, not theorised — see
/// `reconstructing_before_the_archive_cutoff_keeps_every_entity`.
///
/// The sound test rests on the one guarantee the archive does make: **the newest
/// row per entity is never archivable**, because archivability requires a later
/// row to exist. So if `ts` is at or after the newest hot stamp, every entity's
/// winning row at `ts` is its newest row overall, and every such row is hot.
/// That covers `reconstruct(now)` — the common case, and the case §5.7 designed
/// `LOG_ARCHIVABLE` around — and nothing else.
///
/// Anything earlier goes to the cold file. That is more ATTACHes than the old
/// rule performed, and the trade is not close: the old rule was cheaper because
/// it was answering a question nobody asked.
///
/// With no archive database in play the reach test *is* the completeness test —
/// nothing has been removed, so the hot log is the whole log — and it is kept,
/// because it is also what distinguishes "before recorded history" from "the
/// cold file is missing" (D-026).
async fn hot_log_reach(
    conn: &libsql::Connection,
    ts: &str,
    archive_path: Option<&Path>,
) -> Result<HotLogReach> {
    if archive_path.is_some_and(|p| p.exists()) {
        // An archive file beside the log is direct evidence that rows may have
        // gone, and it is *stronger* evidence than [`hot_log_is_intact`] on one
        // case: an empty hot log passes the seq_id test vacuously, so the
        // fully-archived database would otherwise report its own emptiness as
        // history. It cannot arise from `archive()` itself — the newest row per
        // entity always stays — but answering "covered" there would make such a
        // file reconstruct to the empty state with no error at all.
        return reach_with_rows_removed(conn, ts).await;
    }

    hot_log_reach_within(conn, ts).await
}

/// The verdict the **hot file alone** supports (0.15.4, W14.2, review C-2).
///
/// This is the whole of the reach question minus the one thing an archive path
/// adds, and it is a separate function because two callers need exactly it:
/// [`hot_log_reach`] when no archive file is present, and
/// [`hot_log_answers_for`] on behalf of readers that never had a path to offer.
/// Those two used to answer differently — the first on `MIN(recorded_at)`, the
/// second on intactness alone — and neither answer was the right one.
///
/// # Two cases, and the split is intactness rather than the timestamp
///
/// **Nothing was ever removed.** The hot log is the whole log, so it answers at
/// every instant. Above its floor the fold runs; below it, *nothing had been
/// recorded yet* is not a failure to find the answer, it is the answer
/// ([`HotLogReach::PredatesRecordedHistory`], D-121).
///
/// **Rows were removed.** Only [`reach_with_rows_removed`]'s rule holds, and it
/// is a bound on `ts` from above rather than below. This is the case the old
/// `MIN(recorded_at) <= ts` arm got wrong: it asked whether the hot log
/// *stretches back* far enough, which is the question 0.5.5 already established
/// is not the same as whether it is still *complete*. With no archive path to
/// fall through to, a `reconstruct` on an archived database whose cold file was
/// not passed folded whatever was left and returned it as history — the silent
/// short answer D-189 refused at the two connection-only readers, reachable at
/// the one reader that takes a path and was handed `None`. Pinned by
/// `reconstructing_without_the_archive_path_refuses_rather_than_folding_a_gap`.
async fn hot_log_reach_within(conn: &libsql::Connection, ts: &str) -> Result<HotLogReach> {
    // **The cheap arm first, and it is sound before the case split rather than
    // inside one of its branches** (0.15.5, W14.4, [D-247]). `MAX <= ts` covers
    // under *both* rules: on a log rows were removed from it is
    // [`reach_with_rows_removed`]'s argument, and on an intact one
    // `MIN <= MAX <= ts` gives the same verdict a step later. So the question
    // "were rows removed" — the only expensive one here — does not have to be
    // asked at all when the instant is at or after the newest surviving stamp.
    //
    // Which is where the readers actually ask. `as_of_recorded(now)`,
    // `reconstruct(now)` and every read at a recent instant land here, and pay
    // one index seek against `idx_txlog_time` instead of a covering scan whose
    // cost is the whole hot log. Measured at 500,000 log rows: **3.4 µs against
    // 24.2 ms**.
    if newest_stamp_covers(conn, ts).await? {
        return Ok(HotLogReach::Covers);
    }

    // Below the newest surviving stamp, and now it matters. An intact log
    // answers at every instant; a log rows were taken out of answers at none
    // below that stamp, and there is no cheaper exact test than counting —
    // `LOG_ARCHIVABLE` removes rows scattered through the sequence, so a gap
    // can be anywhere and only `COUNT(*)` finds it (see [`hot_log_is_intact`]).
    // This arm is *not* made cheaper by the reordering and pays one extra seek
    // for the arm that is: 3.4 µs on top of a scan that starts at 96 µs.
    if !hot_log_is_intact(conn).await? {
        return Ok(HotLogReach::NeedsArchive);
    }

    Ok(match oldest_hot_stamp(conn).await? {
        // Sound as a string comparison because every recorded_at is the
        // canonical fixed width (D-029).
        Some(min_ts) if min_ts.as_str() <= ts => HotLogReach::Covers,
        // Below the floor of a complete log, or no log at all: either way
        // nothing had been recorded by `ts` and the empty state is correct.
        _ => HotLogReach::PredatesRecordedHistory,
    })
}

/// The one rule that survives archiving, in the one place both callers read it.
///
/// `LOG_ARCHIVABLE` requires a later row at the same entity, so **the newest row
/// per entity is never archivable**. If `ts` is at or after the newest stamp
/// still in the hot log, then every entity's winning row at `ts` is its newest
/// row overall, and every such row is hot — the fold is complete without
/// knowing anything about what left. That covers `reconstruct(now)`, the common
/// case and the one §5.7 designed `LOG_ARCHIVABLE` around, and nothing earlier.
///
/// Which is also why the two halves of the question have opposite senses. On an
/// intact log the test is `MIN <= ts`: *does the log reach back to `ts`*. Once
/// rows have gone it is `MAX <= ts`: *is `ts` late enough that nothing missing
/// could matter*. Reading the second as a weaker form of the first is the
/// mistake 0.5.5 corrected once and W14.2 corrected again in the arm 0.5.5 did
/// not reach.
async fn reach_with_rows_removed(conn: &libsql::Connection, ts: &str) -> Result<HotLogReach> {
    Ok(if newest_stamp_covers(conn, ts).await? {
        HotLogReach::Covers
    } else {
        HotLogReach::NeedsArchive
    })
}

/// The rule itself, as a predicate, because two callers now read it and one of
/// them ([`hot_log_reach_within`]'s first arm) is not deciding between the same
/// two verdicts.
///
/// An empty log covers nothing, which is the arm that keeps a fully-archived
/// database from reporting its own emptiness as history.
async fn newest_stamp_covers(conn: &libsql::Connection, ts: &str) -> Result<bool> {
    Ok(newest_hot_stamp(conn)
        .await?
        .is_some_and(|max_ts| max_ts.as_str() <= ts))
}

/// The oldest `recorded_at` still in the hot log, or `None` if it is empty.
async fn oldest_hot_stamp(conn: &libsql::Connection) -> Result<Option<String>> {
    hot_stamp(conn, "MIN").await
}

/// The newest `recorded_at` still in the hot log, or `None` if it is empty.
async fn newest_hot_stamp(conn: &libsql::Connection) -> Result<Option<String>> {
    hot_stamp(conn, "MAX").await
}

/// One aggregate over `transaction_log.recorded_at`, which `idx_txlog_time`
/// serves as an index scan of one row at either end.
async fn hot_stamp(conn: &libsql::Connection, agg: &str) -> Result<Option<String>> {
    let row = conn
        .query(
            &format!("SELECT {agg}(recorded_at) FROM transaction_log"),
            (),
        )
        .await?
        .next()
        .await?;
    Ok(row.and_then(|r| r.get(0).ok()))
}

/// What the caller needs to know when the cold delta cannot be reached —
/// **assembled from the hot file alone** (0.9.0, C4).
///
/// # This is the message the hot-side marker was wanted for
///
/// [D-121](../../docs/architecture/s13-decision-register.md) rejected a hot-side
/// marker recording *archived at* and *horizon*, then left the door open: 0.9.0
/// was to adopt it "only if it wants the richer message". C4 asked for the
/// message and found the marker cannot supply it, because the proposed message —
/// *"this database was archived on X; pass the archive path"* — is **weaker**
/// than what the hot log already carries:
///
/// * *how many rows went* is `MAX(seq_id) - COUNT(*)`, exact for the reason
///   [`hot_log_is_intact`] gives;
/// * *how far back the hot file still reaches* is `MIN(seq_id)` and its
///   `recorded_at` — which is the fact that actually tells a caller whether the
///   archive is worth fetching, and which a marker's archive **timestamp** does
///   not give them;
/// * *that archiving happened at all* is the one bit [`hot_log_is_intact`]
///   already answers.
///
/// The only datum a marker would add is the wall-clock instant of the last
/// archive run, and no branch and no caller needs it. So the marker is refused
/// outright rather than deferred again: under
/// [D-036](../../docs/architecture/s13-decision-register.md) a hot-table addition
/// lands pre-1.0 or not at all, and a table whose whole content is a timestamp
/// used in one error string is not worth a rung.
///
/// # There is no "nothing was archived" case, and that was settled by injection
///
/// This first carried a branch for `removed == 0`, on the reasoning that the
/// `NeedsArchive` arm is reachable without any archiving. That reasoning was
/// **wrong about where the cost lands and right about the branch**, and only a
/// probe told the two apart: replacing the branch body with a panic showed it
/// firing from `a_failed_cold_reconstruct_still_detaches`, a test that raises
/// nothing from here — because the hint was being computed *before* the two
/// arms that use it, on every cold fold. Made lazy, the probe went quiet across
/// all 27 targets.
///
/// So the branch was dead at the use sites: both arms require
/// [`hot_log_is_intact`] to have returned false, or an archive file to have
/// existed when `hot_log_reach` looked and to have gone by the time this did.
/// Rows really were removed in every case that gets here, and the message may
/// say so without qualification. Deleted rather than kept as a defensive
/// fallback, for the reason `delete_guarded` records about
/// `classify_archive_violation`: unreachable code that looks reasonable is
/// harder to remove later than now.
///
/// Best-effort by construction: this runs on the error path, where a second
/// failure must not replace the diagnosis with its own. A query that does not
/// answer yields a hint that says so, and the caller still gets the error it came
/// for.
async fn archive_hint(conn: &libsql::Connection) -> String {
    // `COUNT(*)` always returns a row, so `None` here means the query itself
    // failed and there is nothing to say beyond that.
    let row = match conn
        .query(
            "SELECT COUNT(*), MIN(seq_id), MAX(seq_id), MIN(recorded_at) FROM transaction_log",
            (),
        )
        .await
    {
        Ok(mut rows) => rows.next().await.ok().flatten(),
        Err(_) => None,
    };

    let Some(row) = row else {
        return "the hot log could not be inspected for an archive horizon".into();
    };
    let count: i64 = row.get(0).unwrap_or(0);
    if count == 0 {
        return "the hot log is empty".into();
    }
    let min: i64 = row.get(1).unwrap_or(0);
    let max: i64 = row.get(2).unwrap_or(0);
    let floor: String = row.get(3).unwrap_or_default();
    let removed = max - count;

    format!(
        "{removed} log rows have been archived out of this database; the hot log \
         now begins at seq_id {min} ({floor})"
    )
}

/// Was any row ever removed from `transaction_log`? — answered exactly, from
/// the hot file alone (0.8.0, B5, D-121).
///
/// # Why this question needs answering at all
///
/// With `ts` below the hot log's floor and no archive file present, the state
/// on disk is consistent with two very different histories: **nothing was ever
/// archived**, in which case the hot log is the whole log and the answer to
/// *what was believed at `ts`* is "nothing yet"; or **rows were archived and
/// the cold file is gone**, in which case the answer is unknowable and saying
/// "nothing" would be inventing one. Before this, the two were conflated and
/// both raised — which made an ordinary question about a young database report
/// the ledger as damaged.
///
/// # It was a `COUNT(*)` until 0.15.7, and the count was the whole cost
///
/// The v15 form was `MIN(seq_id) = 1 AND COUNT(*) = MAX(seq_id)`, and the
/// argument for it was a proof rather than a heuristic. `transaction_log.seq_id`
/// is `INTEGER PRIMARY KEY AUTOINCREMENT`, so values are allocated 1, 2, 3, …
/// and **never reused**; a rolled-back transaction leaves no gap, which
/// [D-049](../../docs/architecture/s13-decision-register.md#d-049) established
/// by measurement after assuming the opposite; and `trg_txlog_guard_delete`
/// confines deletion to an archive session. So if nothing was removed the ids
/// are exactly `1..=MAX`, and conversely those two equalities force the `COUNT`
/// distinct ids inside `[1, MAX]` to be all of it. Exact in both directions.
///
/// It was also a scan. `MIN` and `MAX` on the rowid are index seeks and
/// `COUNT(*)` is not, so this cost the whole hot log — **0.134 ms at 2,000 rows
/// and 32.6 ms at 500,000** — on every recorded-time read below the newest
/// surviving stamp, in front of an id-bounded hydration that is flat at 0.14 ms
/// however long the log is (`examples/log_integrity_probe.rs`, review C-5,
/// [D-247], [D-249]). The one-row read is **0.033 ms and does not move with the
/// log**, which is the shape of the change rather than the factor: at 2,000
/// rows it is 4x, at half a million it is 930x, and the difference between
/// those two is the whole finding.
///
/// # So the storage writes it down at the moment it becomes true
///
/// `log_integrity.rows_removed`, maintained by `trg_txlog_mark_gap`, is the
/// same bit as a one-row read. A **trigger** rather than the archive code,
/// because there is no route to deleting a log row that avoids it — §4.2 admits
/// that raw SQL against the file can do what this API refuses, and a bit
/// maintained in Rust would be wrong after exactly that, in the direction that
/// folds a gap silently.
///
/// The proof above did not go away; it moved. It is what the v15 → v16 rung
/// runs, once, to seed a database that may already have been archived, and what
/// `the_bit_agrees_with_the_count_it_replaced` asserts it against.
///
/// # One state changed hands, and it was wrong before
///
/// An **empty** hot log used to answer *intact*, unconditionally: `count == 0`
/// returned `true`. That conflates a young database with a fully archived one,
/// and the second then reported its own emptiness as history — the caller was
/// told nothing had been recorded by `ts` when in truth everything had, and was
/// told it without an error. [`hot_log_reach`] catches that case when it has an
/// archive path to look at; [`hot_log_answers_for`] has none and could not.
/// The bit tells them apart on the log alone, which is what a young database
/// and an emptied one differ by.
///
/// # What it deliberately does not claim
///
/// Nothing about *when* the archiving happened or *what* went, which is what
/// the marker [D-132](../../docs/architecture/s13-decision-register.md#d-132)
/// refused would have carried, and [D-249] does not revisit that refusal — this
/// row answers the guard's own question and holds nothing a message would want.
///
/// [D-247]: ../../docs/architecture/s13-decision-register.md#d-247
/// [D-249]: ../../docs/architecture/s13-decision-register.md#d-249
async fn hot_log_is_intact(conn: &libsql::Connection) -> Result<bool> {
    let row = conn
        .query("SELECT rows_removed FROM log_integrity WHERE id = 1", ())
        .await?
        .next()
        .await?;
    // No row is not a state the ladder produces: the rung seeds it and the
    // baseline seeds it, and `verify` fails a database missing the table. A
    // database that reached here without one is damaged in a way this function
    // must not paper over with an optimistic answer.
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(row.get::<i64>(0)? == 0)
}

/// Whether a connection alone can fold `transaction_log` at `ts` (W7.1, D-174).
///
/// The completeness question [`hot_log_reach`] answers, minus the archive file
/// it does not have. Both callers take a `Connection`, so when the hot log is
/// short they have nowhere to go and must refuse rather than fold what is left:
/// [`crate::graph::TraversalBuilder::as_of_recorded`] folds for topology, and
/// [`crate::temporal::hydrate_attributes`] folds for the text (0.13.16, W9.1,
/// [D-189](../../docs/architecture/s13-decision-register.md#d-189)). The second
/// was folding without asking, which is what §3.2 was.
///
/// # It ignored `ts` until 0.15.4 (W14.2, review C-2)
///
/// The body was `hot_log_is_intact(conn)` and the parameter was `_ts`: one bit,
/// *was anything ever removed*, with the instant discarded. So the first archive
/// session a deployment ever ran took `AttributeMode::AtTime` and every
/// `as_of_recorded` traversal away from it permanently, for its whole history
/// rather than for the archived part of it — including `as_of_recorded(now)`,
/// which is the instant the archive is *guaranteed* to answer.
///
/// The old comment here justified that as conservative-by-one-bit on the ground
/// that the archive cutoff is not recorded hot-side (D-132's refused marker),
/// and the ground was sound. The conclusion did not follow: the cutoff is not
/// needed. [`reach_with_rows_removed`] decides the same question from the newest
/// surviving stamp, which is hot by construction, and [`hot_log_reach`] had been
/// computing exactly that verdict per timestamp since 0.5.5 two functions away.
/// Both readers now take the three-way verdict and refuse on one arm of it.
///
/// [`HotLogReach::PredatesRecordedHistory`] is an answer, not a refusal: the
/// fold returns the empty state, which is what was believed at an instant before
/// anything was recorded. That is also what the old bit did there, so the arm is
/// unchanged rather than newly permitted.
pub(crate) async fn hot_log_answers_for(conn: &libsql::Connection, ts: &str) -> Result<bool> {
    Ok(!matches!(
        hot_log_reach_within(conn, ts).await?,
        HotLogReach::NeedsArchive
    ))
}

/// Run one fold query from nothing — the unanchored path.
async fn fold(conn: &libsql::Connection, ts: &str, query: &str) -> Result<MaterializedState> {
    let delta = fold_delta(conn, query, libsql::params![ts]).await?;
    Ok(delta.apply_to(MaterializedState::empty(ts), ts))
}

/// Run one fold query and collect the winning rows, deletions included.
async fn fold_delta(
    conn: &libsql::Connection,
    query: &str,
    params: impl libsql::params::IntoParams,
) -> Result<Delta> {
    let mut rows = conn.query(query, params).await?;
    let mut d = Delta::default();
    let (concepts, edges, max_seq) = (&mut d.concepts, &mut d.edges, &mut d.max_seq);

    while let Some(row) = rows.next().await? {
        let seq_id: i64 = row.get(0)?;
        let table_name: String = row.get(1)?;
        let _entity_id: String = row.get(2)?;
        let op: String = row.get(3)?;
        let payload_str: String = row.get(4)?;
        // Projected by all four folds since 0.14.5. They have partitioned on it
        // since D-216; what was missing was carrying it out of the query, which
        // is why the correct partition produced a collapsed result anyway.
        let branch_id: String = row.get(5)?;

        if seq_id > *max_seq {
            *max_seq = seq_id;
        }

        // A `'D'` row is corruption, not a tombstone (D-072).
        //
        // Doctrine V permits no physical delete outside an archive session, and
        // the archive *moves* rows rather than logging their removal — so no
        // trigger in the schema writes a `'D'`, and no code path in the crate
        // can produce one. This arm used to treat it as a tombstone, which read
        // as a claim that deletions are recorded and reconstructible. They are
        // not. Refusing here makes the doctrine enforced at the fold rather than
        // assumed by it, and is the same call D-060 made for overlap: the layer
        // that can notice should.
        //
        // Retirement is unaffected and is the mechanism that actually removes a
        // concept from a composed state — see the `retired != 0` branch below,
        // which is where `concepts_gone` is populated in practice.
        if op == "D" {
            return Err(DbError::ReplayCorrupt {
                seq: seq_id,
                reason: format!(
                    "transaction_log carries a 'D' operation for {table_name} \
                     entity {_entity_id:?}; Doctrine V permits no physical delete \
                     outside an archive session, and the archive logs none. This \
                     row was not written by this crate."
                ),
            });
        }

        let payload: serde_json::Value =
            serde_json::from_str(&payload_str).map_err(|e| DbError::ReplayCorrupt {
                seq: seq_id,
                reason: format!("Failed to parse payload JSON: {e}"),
            })?;

        // v1 and v2 differ by one added field, so v1 folds by reading it as
        // absent — which is what `Option` already means here. A future shape
        // that *removes* or *retypes* a field would not be able to share this
        // path, and would want a match on `v` rather than a ceiling.
        let v = payload.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
        if v > PAYLOAD_VERSION as u64 {
            return Err(DbError::PayloadVersion {
                got: v as u8,
                max: PAYLOAD_VERSION,
            });
        }

        if table_name == "concepts" {
            let id = _entity_id;
            let retired = payload.get("retired").and_then(|r| r.as_i64()).unwrap_or(0);
            if retired == 0 {
                let title = payload
                    .get("title")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = payload
                    .get("content")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let embedding_model = payload
                    .get("embedding_model")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                concepts.insert(
                    id.clone(),
                    NodeAttributes {
                        id,
                        title,
                        content,
                        embedding_model,
                    },
                );
            } else {
                // Retirement is the application axis (§4.1), and a reconstruction
                // shows what was visible. Onto a snapshot that means removing
                // the concept, not declining to add it.
                d.concepts_gone.insert(id);
            }
        } else if table_name == "links" {
            let src = payload
                .get("source_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let tgt = payload
                .get("target_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let edge_type = payload
                .get("edge_type")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let vf = payload
                .get("valid_from")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let vt = payload
                .get("valid_to")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let belief = EdgeBelief {
                source_id: src,
                target_id: tgt,
                edge_type,
                valid_from: vf,
                valid_to: vt,
                branch_id,
            };
            edges.insert(belief.belief_key(), belief);
        }
    }

    Ok(d)
}

impl Delta {
    /// Compose onto `base` under last-writer-wins by `seq_id` (§5.5).
    ///
    /// The delta is by construction newer than the base — it is the fold of
    /// everything above the base's anchor — so every row it carries wins, and
    /// every retirement it carries removes. This is the same rule
    /// `trg_links_current_sync`'s upsert applies and the same rule the cold
    /// fold applies; that the three agree is asserted by test rather than by
    /// this comment (§8).
    fn apply_to(self, base: MaterializedState, ts: &str) -> MaterializedState {
        let mut concepts = base.concepts;
        let mut edges: HashMap<String, EdgeBelief> = base
            .edges
            .into_iter()
            .map(|e| (e.belief_key(), e))
            .collect();

        for id in self.concepts_gone {
            concepts.remove(&id);
        }
        // No edge equivalent: an edge is superseded in place under the same
        // `entity_id`, never removed — see [`Delta`] (D-072).
        concepts.extend(self.concepts);
        edges.extend(self.edges);

        // Sorted so the result is a function of the state and not of hash
        // iteration order — `reconstruct` is compared against itself by the
        // property suite, and two runs must be equal, not merely equivalent.
        let mut edges: Vec<_> = edges.into_values().collect();
        edges.sort();

        MaterializedState {
            seq_anchor: self.max_seq.max(base.seq_anchor),
            timestamp: ts.to_string(),
            concepts,
            edges,
            // A delta was applied, so there was history to fold. `reconstruct`
            // sets the flag on the one path that never gets here.
            predates_recorded_history: false,
        }
    }
}

#[cfg(test)]
mod reach_table {
    //! Every cell of the reach question, named (0.15.5, W14.4, [D-247]).
    //!
    //! [`hot_log_reach_within`] decides on two facts — whether rows were removed
    //! from the log, and where `ts` sits against the stamps that remain — and
    //! the order it establishes them in is a **cost** decision, not a
    //! correctness one. 0.15.5 reordered it so the cheap fact is enough on the
    //! arm the readers actually use. A reordering is exactly the kind of change
    //! that is obviously behaviour-preserving until it is not, and the argument
    //! for it ("`MAX <= ts` covers under both rules") is short enough to be
    //! believed without checking. This table is the checking.
    //!
    //! Enumerated rather than sampled, because the defects this area has
    //! actually produced were all boundary cells: `ts` exactly at the newest
    //! stamp (0.15.4), `ts` below the floor of an intact log (0.8.0, D-121),
    //! and an empty log that passes the intactness test vacuously (0.5.5).

    use super::*;

    const A: &str = "1970-01-01T01:00:00.000000Z";
    const B: &str = "1970-01-01T02:00:00.000000Z";
    const C: &str = "1970-01-01T03:00:00.000000Z";
    const BEFORE_A: &str = "1970-01-01T00:30:00.000000Z";
    const BETWEEN: &str = "1970-01-01T02:30:00.000000Z";
    const AFTER_C: &str = "1970-01-01T04:00:00.000000Z";

    /// A log holding one row at each of `A`, `B`, `C`, optionally with `B`'s
    /// removed the way an archive removes it — a hole in the middle of the
    /// `seq_id` run, leaving the floor and the ceiling where they were.
    ///
    /// That shape is the point. A gap at the end cannot happen (the newest row
    /// per entity is never archivable) and a gap at the front would move `MIN`
    /// and make the two rules agree by accident.
    async fn log(gapped: bool) -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        for (i, ts) in [A, B, C].iter().enumerate() {
            conn.execute(
                "INSERT INTO transaction_log \
                 (table_name, entity_id, operation, payload, recorded_at) \
                 VALUES ('concepts', ?1, 'upsert', '{}', ?2)",
                libsql::params![format!("c{i}").as_str(), *ts],
            )
            .await
            .unwrap();
        }
        if gapped {
            let marker = crate::schema::ddl::ARCHIVE_SESSION_MARKER;
            conn.execute(&format!("CREATE TABLE {marker} (x)"), ())
                .await
                .unwrap();
            conn.execute(
                "DELETE FROM transaction_log WHERE recorded_at = ?1",
                libsql::params![B],
            )
            .await
            .unwrap();
            conn.execute(&format!("DROP TABLE {marker}"), ())
                .await
                .unwrap();
        }
        conn
    }

    async fn empty_log() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn
    }

    /// An intact log is the whole log, so it answers at every instant: with the
    /// fold above its floor, and with the empty state below it.
    #[tokio::test]
    async fn an_intact_log_answers_everywhere() {
        let conn = log(false).await;
        for (ts, want) in [
            (BEFORE_A, HotLogReach::PredatesRecordedHistory),
            (A, HotLogReach::Covers),
            (B, HotLogReach::Covers),
            (BETWEEN, HotLogReach::Covers),
            (C, HotLogReach::Covers),
            (AFTER_C, HotLogReach::Covers),
        ] {
            assert_eq!(
                hot_log_reach_within(&conn, ts).await.unwrap(),
                want,
                "intact log at {ts}"
            );
            assert!(
                hot_log_answers_for(&conn, ts).await.unwrap(),
                "an intact log refuses nothing, and refused {ts}"
            );
        }
    }

    /// Once a row has gone, the boundary moves to the *newest* surviving stamp
    /// and the sense of the comparison inverts.
    ///
    /// `A` is the case that matters and the one the old rule got wrong: the log
    /// still reaches back to it — `MIN(recorded_at)` is `A` — and the answer at
    /// `A` is nonetheless in the other file, because the row that won at `A`
    /// for the entity whose `B` row went is no longer here to be found.
    #[tokio::test]
    async fn a_gapped_log_answers_only_from_its_newest_stamp() {
        let conn = log(true).await;
        for (ts, want) in [
            (BEFORE_A, HotLogReach::NeedsArchive),
            (A, HotLogReach::NeedsArchive),
            (BETWEEN, HotLogReach::NeedsArchive),
            (C, HotLogReach::Covers),
            (AFTER_C, HotLogReach::Covers),
        ] {
            assert_eq!(
                hot_log_reach_within(&conn, ts).await.unwrap(),
                want,
                "gapped log at {ts}"
            );
            assert_eq!(
                hot_log_answers_for(&conn, ts).await.unwrap(),
                want != HotLogReach::NeedsArchive,
                "the boolean guard must agree with the verdict at {ts}"
            );
        }
    }

    /// `C` is the newest surviving stamp and must be *answered*, not refused.
    ///
    /// Split out of the table above rather than left as one row in it, because
    /// it is the cell 0.15.4 was about and the cell a `<` instead of a `<=`
    /// takes. A boundary that is one row of six is a boundary nobody reads.
    #[tokio::test]
    async fn the_newest_surviving_stamp_is_answered_and_not_refused() {
        let conn = log(true).await;
        assert_eq!(
            hot_log_reach_within(&conn, C).await.unwrap(),
            HotLogReach::Covers,
            "at the newest surviving stamp every entity's winning row is its \
             newest row, and every one of those is still here"
        );
    }

    /// An empty log is intact vacuously — nothing was removed because nothing
    /// is there — and the empty state is the honest answer at every instant.
    ///
    /// This is the cell that keeps the archive-file check in [`hot_log_reach`]
    /// from being folded into intactness: the *same* database with a cold file
    /// beside it is fully archived rather than young, and must not answer here.
    #[tokio::test]
    async fn an_empty_log_predates_everything_rather_than_covering_it() {
        let conn = empty_log().await;
        for ts in [BEFORE_A, C, AFTER_C] {
            assert_eq!(
                hot_log_reach_within(&conn, ts).await.unwrap(),
                HotLogReach::PredatesRecordedHistory,
                "empty log at {ts}"
            );
        }
    }

    /// The reordering is a cost change, so the cheap arm must give the verdict
    /// the whole case split gives — on both sides of the split.
    ///
    /// Written as a comparison rather than as two expected values: it is the
    /// property the optimisation rests on, and asserting literals here would
    /// pass if the property were false and both sides were wrong together.
    #[tokio::test]
    async fn the_cheap_arm_agrees_with_the_rule_it_short_circuits() {
        for gapped in [false, true] {
            let conn = log(gapped).await;
            for ts in [BEFORE_A, A, BETWEEN, C, AFTER_C] {
                if newest_stamp_covers(&conn, ts).await.unwrap() {
                    assert_eq!(
                        hot_log_reach_within(&conn, ts).await.unwrap(),
                        HotLogReach::Covers,
                        "the cheap arm claimed {ts} on a gapped={gapped} log and \
                         the full rule disagrees"
                    );
                }
            }
        }
    }

    /// Empty the log the way a long-running archive does.
    async fn empty_the_log(conn: &libsql::Connection) {
        let marker = crate::schema::ddl::ARCHIVE_SESSION_MARKER;
        conn.execute(&format!("CREATE TABLE {marker} (x)"), ())
            .await
            .unwrap();
        conn.execute("DELETE FROM transaction_log", ())
            .await
            .unwrap();
        conn.execute(&format!("DROP TABLE {marker}"), ())
            .await
            .unwrap();
    }

    /// A log archived down to nothing is not a log nothing was written to
    /// (0.15.7, W14.5, [D-249]).
    ///
    /// This is the cell the module doc names as a defect from 0.5.5 and the one
    /// the table did not have: `count = 0` returned *intact* by a separate arm,
    /// so a fully archived database was told `PredatesRecordedHistory` —
    /// nothing had been recorded by `ts` — at every instant, with its whole
    /// history sitting in the archive and no error to say so. [`hot_log_reach`]
    /// catches it when it has an archive path to look at; this function has
    /// none, and the bit is what it has instead.
    #[tokio::test]
    async fn a_log_archived_down_to_nothing_asks_for_the_archive() {
        let conn = log(false).await;
        empty_the_log(&conn).await;

        for ts in [BEFORE_A, A, BETWEEN, C, AFTER_C] {
            assert_eq!(
                hot_log_reach_within(&conn, ts).await.unwrap(),
                HotLogReach::NeedsArchive,
                "an emptied log answered for {ts} out of its own emptiness"
            );
            assert!(
                !hot_log_answers_for(&conn, ts).await.unwrap(),
                "an emptied log claims to answer for {ts}"
            );
        }

        // And the state it must not be confused with, unchanged: a log nothing
        // was ever written to still predates history rather than refusing.
        let young = empty_log().await;
        assert_eq!(
            hot_log_reach_within(&young, BEFORE_A).await.unwrap(),
            HotLogReach::PredatesRecordedHistory,
            "a young log was made to refuse, which is the opposite over-correction"
        );
    }

    /// A database whose integrity row is gone is damaged, and damaged is not
    /// intact (0.15.7, W14.5, [D-249]).
    ///
    /// The ladder seeds the row and `verify` requires the table, so nothing the
    /// crate does produces this. Something outside the crate can — §4.2 says
    /// so — and the arm that handles it chooses to refuse rather than to assume
    /// the happy answer, because the happy answer here is *fold an incomplete
    /// log and return it as belief*.
    #[tokio::test]
    async fn a_log_without_its_integrity_row_is_not_assumed_intact() {
        let conn = log(false).await;
        conn.execute("DELETE FROM log_integrity", ()).await.unwrap();

        assert!(
            !hot_log_is_intact(&conn).await.unwrap(),
            "a missing integrity row was read as an intact log"
        );
        assert_eq!(
            hot_log_reach_within(&conn, BETWEEN).await.unwrap(),
            HotLogReach::NeedsArchive,
            "a damaged database was answered from rather than refused"
        );
    }
}
