use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::{DbError, Result};
use crate::temporal::as_of::NodeAttributes;

/// Full materialized state reconstructed from transaction_log replay (§5.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedState {
    pub seq_anchor: i64,
    pub timestamp: String,
    pub concepts: HashMap<String, NodeAttributes>,
    pub edges: Vec<(String, String, String, String, String)>,
}

impl MaterializedState {
    /// The state before any log row has been applied.
    fn empty(ts: &str) -> Self {
        Self {
            seq_anchor: 0,
            timestamp: ts.to_string(),
            concepts: HashMap::new(),
            edges: Vec::new(),
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
    SELECT seq_id, table_name, entity_id, operation, payload
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id ORDER BY seq_id DESC) as rn
        FROM transaction_log
        WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#;

/// Fold over hot and cold together (§5.5, D-026). Requires `cold` to be ATTACHed.
///
/// The hot entry wins for entities present in both files because its `seq_id` is
/// greater — the same last-writer-wins rule as snapshot composition.
const COLD_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id ORDER BY seq_id DESC) as rn
        FROM (
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at FROM main.transaction_log
            UNION ALL
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at FROM cold.transaction_log
        ) WHERE recorded_at <= ?1
    ) WHERE rn = 1
"#;

/// Fold over the hot log *above a snapshot anchor* (§5.5, D-049).
///
/// `seq_id > ?2` is an inequality, and deliberately so: `AUTOINCREMENT` leaves
/// gaps whenever a transaction rolls back, so successor arithmetic
/// (`seq_id = :anchor + 1`) would stop at the first gap and silently truncate
/// the delta. This is the first anchored fold in the crate, which makes it the
/// first code D-024's rule has ever bound — before this the rule was vacuous,
/// not satisfied.
const ANCHORED_HOT_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id ORDER BY seq_id DESC) as rn
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
const ANCHORED_COLD_FOLD: &str = r#"
    SELECT seq_id, table_name, entity_id, operation, payload
    FROM (
        SELECT seq_id, table_name, entity_id, operation, payload,
               ROW_NUMBER() OVER (PARTITION BY table_name, entity_id ORDER BY seq_id DESC) as rn
        FROM (
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at FROM main.transaction_log
            UNION ALL
            SELECT seq_id, table_name, entity_id, operation, payload, recorded_at FROM cold.transaction_log
        ) WHERE recorded_at <= ?1 AND seq_id > ?2
    ) WHERE rn = 1
"#;

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
    /// Keyed by `transaction_log.entity_id`: `source|target|type|valid_from`.
    edges: HashMap<String, (String, String, String, String, String)>,
    /// Concepts retired as of the fold's instant. See the type's note for why
    /// there is no edge equivalent.
    concepts_gone: HashSet<String>,
    max_seq: i64,
}

/// The log's `entity_id` for a link, rebuilt from a materialised edge tuple.
///
/// Must match `trg_links_log_i`'s
/// `source_id || '|' || target_id || '|' || edge_type || '|' || valid_from`
/// exactly, or a delta row will fail to replace the snapshot row it supersedes.
/// Safe because ULIDs are Crockford base32 and edge types are `[A-Z0-9]+`, so
/// `|` cannot occur inside a component (§4.3).
fn edge_key(e: &(String, String, String, String, String)) -> String {
    format!("{}|{}|{}|{}", e.0, e.1, e.2, e.3)
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
/// [`snapshot_anchor`] for why archiving disables it. Otherwise the fold runs
/// from genesis, which is correct and costs what the whole log costs.
pub async fn reconstruct(
    conn: &libsql::Connection,
    ts: &str,
    archive_path: Option<&Path>,
    snapshots_dir: Option<&Path>,
) -> Result<MaterializedState> {
    if hot_log_is_complete(conn, ts, archive_path).await? {
        if let Some(base) = snapshot_anchor(snapshots_dir, ts) {
            let anchor = base.seq_anchor;
            let delta =
                fold_delta(conn, ANCHORED_HOT_FOLD, libsql::params![ts, anchor]).await?;
            return Ok(delta.apply_to(base, ts));
        }
        return fold(conn, ts, HOT_FOLD).await;
    }

    // The delta lives in the cold archive database.
    let archive = archive_path.ok_or_else(|| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("state at {ts} predates the hot log and no archive path was given"),
    })?;
    if !archive.exists() {
        return Err(DbError::ReplayCorrupt {
            seq: 0,
            reason: format!("archive database file {archive:?} does not exist"),
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

    // Composition works across the archive boundary because the anchored fold
    // unions both files; before 0.5.5 it was refused here rather than made to
    // work, and the refusal was the only thing keeping the answer right.
    let result = match snapshot_anchor(snapshots_dir, ts) {
        Some(base) => {
            let anchor = base.seq_anchor;
            fold_delta(conn, ANCHORED_COLD_FOLD, libsql::params![ts, anchor])
                .await
                .map(|delta| delta.apply_to(base, ts))
        }
        None => fold(conn, ts, COLD_FOLD).await,
    };

    // Unconditional: see the ATTACH note above.
    if let Err(e) = conn.execute("DETACH DATABASE cold", ()).await {
        tracing::warn!("reconstruct: failed to DETACH cold database: {e}");
    }

    result
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
fn snapshot_anchor(snapshots_dir: Option<&Path>, ts: &str) -> Option<MaterializedState> {
    let dir = snapshots_dir?;

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
async fn hot_log_is_complete(
    conn: &libsql::Connection,
    ts: &str,
    archive_path: Option<&Path>,
) -> Result<bool> {
    let row = conn
        .query(
            "SELECT MIN(recorded_at), MAX(recorded_at) FROM transaction_log",
            (),
        )
        .await?
        .next()
        .await?;
    let (min_recorded_at, max_recorded_at): (Option<String>, Option<String>) = match row {
        Some(r) => (r.get(0).ok(), r.get(1).ok()),
        None => (None, None),
    };

    // Sound as string comparisons because every recorded_at is the canonical
    // fixed width (D-029).
    if archive_path.is_some_and(|p| p.exists()) {
        // An empty hot log beside an archive is the fully-archived case and
        // covers nothing. It cannot arise from `archive()` itself — the newest
        // row per entity always stays — but answering "covered" here would make
        // such a file reconstruct to the empty state with no error at all.
        return Ok(max_recorded_at.is_some_and(|max_ts| max_ts.as_str() <= ts));
    }

    Ok(match min_recorded_at {
        Some(min_ts) => min_ts.as_str() <= ts,
        // No archive and no log: a genuinely empty database, which the empty
        // state answers correctly.
        None => true,
    })
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

        let payload: serde_json::Value = serde_json::from_str(&payload_str).map_err(|e| DbError::ReplayCorrupt {
            seq: seq_id,
            reason: format!("Failed to parse payload JSON: {e}"),
        })?;

        // v1 and v2 differ by one added field, so v1 folds by reading it as
        // absent — which is what `Option` already means here. A future shape
        // that *removes* or *retypes* a field would not be able to share this
        // path, and would want a match on `v` rather than a ceiling.
        let v = payload.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
        if v > PAYLOAD_VERSION as u64 {
            return Err(DbError::PayloadVersion { got: v as u8, max: PAYLOAD_VERSION });
        }

        if table_name == "concepts" {
            let id = _entity_id;
            let retired = payload.get("retired").and_then(|r| r.as_i64()).unwrap_or(0);
            if retired == 0 {
                let title = payload.get("title").and_then(|s| s.as_str()).unwrap_or("").to_string();
                let content = payload.get("content").and_then(|s| s.as_str()).unwrap_or("").to_string();
                let embedding_model = payload.get("embedding_model").and_then(|s| s.as_str()).map(|s| s.to_string());
                concepts.insert(id.clone(), NodeAttributes { id, title, content, embedding_model });
            } else {
                // Retirement is the application axis (§4.1), and a reconstruction
                // shows what was visible. Onto a snapshot that means removing
                // the concept, not declining to add it.
                d.concepts_gone.insert(id);
            }
        } else if table_name == "links" {
            let src = payload.get("source_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let tgt = payload.get("target_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let edge_type = payload.get("edge_type").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let vf = payload.get("valid_from").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let vt = payload.get("valid_to").and_then(|s| s.as_str()).unwrap_or("").to_string();
            edges.insert(_entity_id, (src, tgt, edge_type, vf, vt));
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
        let mut edges: HashMap<String, (String, String, String, String, String)> =
            base.edges.into_iter().map(|e| (edge_key(&e), e)).collect();

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
        }
    }
}
