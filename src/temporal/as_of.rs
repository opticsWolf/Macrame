use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{DbError, Result};
use crate::graph::builder::AttributeMode;
use crate::temporal::replay::PAYLOAD_VERSION;

/// The instant pair a temporal read is taken at (0.13.2, W7.1, D-174).
///
/// One field per axis, because [§3.1](../../docs/architecture/s0-s3-foundations.md)
/// is what happens when there is one field for both. `None` on either axis means
/// *the present* on that axis, and the two are independent: a read may fix valid
/// time and float transaction time, or the reverse, or fix both — which is the
/// cell Jensen and Snodgrass's BCDM defines a bitemporal database as answering,
/// and which no surface in this crate could express before W7.1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsOf {
    /// *What was true.* Bounds a row against its own `valid_from`/`valid_to`.
    pub valid: Option<String>,
    /// *What we believed.* Bounds `transaction_log.recorded_at`.
    pub recorded: Option<String>,
}

impl AsOf {
    /// Both axes at the present: live rows, current belief.
    pub fn now() -> Self {
        Self::default()
    }

    /// Fix valid time at `ts`, leaving belief at the present.
    pub fn valid_at(ts: impl Into<String>) -> Self {
        Self {
            valid: Some(ts.into()),
            recorded: None,
        }
    }

    /// Fix belief at `ts`, leaving valid time at the present.
    pub fn recorded_at(ts: impl Into<String>) -> Self {
        Self {
            valid: None,
            recorded: Some(ts.into()),
        }
    }

    /// Fix both — the bitemporal cell.
    pub fn bitemporal(valid: impl Into<String>, recorded: impl Into<String>) -> Self {
        Self {
            valid: Some(valid.into()),
            recorded: Some(recorded.into()),
        }
    }
}

/// Node attribute payload hydrated from concepts table or transaction_log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAttributes {
    pub id: String,
    pub title: String,
    pub content: String,
    pub embedding_model: Option<String>,
}

use crate::util::limits::HYDRATE_CHUNK;

/// Query valid-time graph edges under current belief as of `ts` (§5.2).
///
/// Reads the **trunk**, which is what this function has always meant and what
/// every database without a fork holds. On a forked ledger that is now a
/// resolution rather than an unfiltered scan: before 0.14.4 it returned every
/// lineage's rows at once, which is the failure `TraversalBuilder::build_sql`'s
/// own note describes — extra edges that look entirely ordinary.
///
/// Use [`query_as_of_edges_on`] to read another lineage. The signature here is
/// unchanged rather than gaining a parameter, because a breaking change to the
/// most-called reader in the crate is not what fixing its default is worth; the
/// two share one implementation, so neither can drift from the other.
pub async fn query_as_of_edges(
    conn: &libsql::Connection,
    ts: &str,
) -> Result<Vec<(String, String, String, String, String)>> {
    query_as_of_edges_on(conn, ts, None).await
}

/// [`query_as_of_edges`] on a named lineage (§15.3, D-220; the cutoff 0.14.10,
/// [D-227]).
///
/// # The repair this function was left out of
///
/// 0.14.4 gave three read paths the same resolution — this one, the traversal,
/// and `load_subgraph_with` — and 0.14.6 bounded that resolution by the fork
/// point ([D-223]). **The bound reached two of the three.** The traversal and
/// the subgraph loader share [`TraversalBuilder`](crate::graph::TraversalBuilder),
/// which carries the lineage and picks its own source relation, so a repair
/// written there arrived at both. This function takes the branch as a bare
/// parameter and spells its own SQL, so it kept 0.14.4's `visible` over
/// `links_current` and went on absorbing an ancestor's post-fork writes for
/// four releases.
///
/// It was wrong in both directions D-223 names, and the second is the silent
/// one: a branch was handed a trunk edge recorded after it forked, **and** lost
/// an inherited edge the moment the trunk retired it — because the retirement
/// overwrote the projection row the branch was reading through. The reader
/// returned four edges where the traversal on the same lineage reached five
/// nodes, and nothing in either answer said they disagreed.
///
/// So the resolved form is now the hybrid the traversal emits, produced by
/// the one lowering in `graph::plan` (since 0.15.1; before that, assembled
/// from the same functions in `graph::lineage`) rather than a second copy of
/// it: `links_cut` for what each ancestor may still show, and `visible` to pick
/// the nearest lineage holding each key. **The trunk's answer is unchanged** —
/// `main` has no ancestors and no cutoff, so `churned` is empty and `links_cut`
/// is `links_current` — and an unforked database never reaches this arm at all.
///
/// # Errors
///
/// [`DbError::UnknownBranch`](crate::DbError::UnknownBranch), naming it, when it
/// is not registered — refused rather than answered for the trunk, for the
/// reason `graph::lineage::lineage_shape` gives.
///
/// [D-223]: ../../docs/architecture/s13-decision-register.md#d-223
/// [D-227]: ../../docs/architecture/s13-decision-register.md#d-227
pub async fn query_as_of_edges_on(
    conn: &libsql::Connection,
    ts: &str,
    branch: Option<&str>,
) -> Result<Vec<(String, String, String, String, String)>> {
    Ok(crate::plan::edges_at(conn, ts, None, branch)
        .await?
        .into_iter()
        .map(|e| {
            (
                e.source_id,
                e.target_id,
                e.edge_type,
                e.valid_from,
                e.valid_to,
            )
        })
        .collect())
}

/// `?n, ?n+1, …` for `count` ids starting at `first`.
///
/// The ids are caller data and are bound, never interpolated. Only the
/// *placeholders* are built by hand, which is the one part of the statement that
/// carries no caller input at all.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Hydrate attributes for a list of node IDs based on the specified AttributeMode (§5.2).
///
/// **Retirement is uniform across the three readers as of Wave 1 (defect AB).**
/// The rule is the one `AttributeMode::Current` and
/// [`crate::temporal::reconstruct`]
/// already followed and `AtTime` did not: *a concept retired as of the instant
/// being asked about is not returned*. Retirement is the application axis (§4.1)
/// and a temporal read shows what was visible, so the three modes now disagree
/// about which text they return and agree about which concepts exist. Before
/// this, `AtTime` read the payload and never looked at `retired`, so it returned
/// concepts retired long before `ts` — the one reader that consulted the ledger
/// most faithfully was also the one that answered the visibility question wrong.
///
/// Note what "as of `ts`" means for each: `Current` asks whether the concept is
/// retired *now* and `AtTime` asks whether it was retired *then*. That is not an
/// inconsistency, it is the two clocks — and it is why `Current` on a historical
/// query is worth objecting to. **That objection is no longer made here**
/// (T3.2, D-085): this function receives the mode as a parameter and has no way
/// to tell a historical query from a live one, so it does what it is told.
/// [`crate::graph::TraversalBuilder`] is the layer that knows, and it raises
/// [`DbError::AttributeModeUnstated`].
///
/// Both modes issue **one query per chunk of [`HYDRATE_CHUNK`] ids**, not one
/// per node (defect AE). Results come back in `node_ids` order regardless of the
/// order the rows arrive in, because a graph read that permuted its own output
/// between runs would break the property suite's equality comparisons for a
/// reason that has nothing to do with the property under test.
///
/// # `ts: &str` became `as_of: &AsOf` in 0.13.2 (W7.1, D-174)
///
/// The old parameter was one instant read on whichever clock the mode happened
/// to use — `Current` ignored it, `AtTime` compared it to `recorded_at`, and
/// neither ever compared it to a concept's own valid interval. So `AtTime`
/// returned concepts whose validity had ended before the instant asked about,
/// which is the smaller half of what [§3.1](../../docs/architecture/s0-s3-foundations.md)
/// names and was recorded in `TraversalBuilder::as_of`'s rustdoc in 0.12.17.
///
/// [`AttributeMode::AtTime`] now dispatches on which axes are fixed:
///
/// | `as_of` | reads |
/// |---|---|
/// | neither | live `concepts`, retired filtered — identical to `Current` |
/// | `valid` | live `concepts`, bounded by the row's own valid interval |
/// | `recorded` | the payload believed at that instant |
/// | both | the payload believed then, bounded by the validity it recorded |
///
/// [`AttributeMode::Current`] ignores both axes by definition — it is the
/// *stated* choice to read live text under a historical topology, which
/// `TraversalBuilder` makes the caller make rather than fall into (D-085).
///
/// # Errors
///
/// [`DbError::RecordedInstantUnreachable`] when `as_of.recorded` is set under
/// [`AttributeMode::AtTime`] and rows have been archived out of the hot log
/// (0.13.16, W9.1, [D-189](../../docs/architecture/s13-decision-register.md#d-189)).
/// Only the `recorded` row of the table above can raise it; the other three
/// read live `concepts` and never the log, so an archive cannot shorten them.
pub async fn hydrate_attributes(
    conn: &libsql::Connection,
    node_ids: &[String],
    as_of: &AsOf,
    mode: AttributeMode,
) -> Result<Vec<NodeAttributes>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let found: HashMap<String, NodeAttributes> = match mode {
        AttributeMode::Omit => return Ok(Vec::new()),
        // No warning here any more (T3.2, D-085). This function takes the mode
        // as a parameter and cannot tell a historical query from a live one, so
        // the warning fired on *every* `Current` hydrate, which is overwhelmingly
        // the ordinary live case where it is exactly right. Loud where it did not
        // matter and, being a log line, silent where it did. The decision now
        // lives in `TraversalBuilder`, which knows whether an instant was set,
        // and is a typed error.
        AttributeMode::Current => hydrate_current(conn, node_ids, None).await?,
        AttributeMode::AtTime => match as_of.recorded.as_deref() {
            None => hydrate_current(conn, node_ids, as_of.valid.as_deref()).await?,
            Some(recorded) => {
                hydrate_at_time(conn, node_ids, recorded, as_of.valid.as_deref()).await?
            }
        },
    };

    // Caller order, and absences simply dropped — the signature returns a Vec
    // rather than a per-id Option, so a node with no visible concept is reported
    // by being missing. That is what both modes did before.
    let mut out = Vec::with_capacity(found.len());
    for id in node_ids {
        if let Some(attrs) = found.get(id) {
            out.push(attrs.clone());
        }
    }
    Ok(out)
}

/// Live attributes under current belief, filtered by retirement *now* and — when
/// `valid` is given — by the row's own valid interval (W7.1).
///
/// The valid-time bound is what `AttributeMode::Current` never had and could not
/// have: `concepts` carries `valid_from`/`valid_to` and nothing read them, so a
/// concept whose validity had ended still hydrated into a historical traversal.
/// Passing `None` is the live read, unchanged, and is what `Current` still does —
/// that mode's whole meaning is *today's text regardless of the instant*.
async fn hydrate_current(
    conn: &libsql::Connection,
    node_ids: &[String],
    valid: Option<&str>,
) -> Result<HashMap<String, NodeAttributes>> {
    let mut found = HashMap::new();

    for chunk in node_ids.chunks(HYDRATE_CHUNK) {
        // The ids bind from `?1` when there is no instant and from `?2` when
        // there is, so the instant can lead and the variadic part can trail.
        let (first, valid_filter) = match valid {
            Some(_) => (2, " AND valid_from <= ?1 AND ?1 < valid_to"),
            None => (1, ""),
        };
        let sql = format!(
            "SELECT id, title, content, embedding_model FROM concepts \
             WHERE retired = 0{valid_filter} AND id IN ({})",
            placeholders(first, chunk.len())
        );
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        if let Some(v) = valid {
            params.push(libsql::Value::Text(v.to_string()));
        }
        params.extend(chunk.iter().map(|id| libsql::Value::Text(id.clone())));

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            found.insert(
                id.clone(),
                NodeAttributes {
                    id,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    embedding_model: row.get(3).ok(),
                },
            );
        }
    }

    Ok(found)
}

/// Attributes as recorded at `ts`, filtered by retirement *at* `ts` and — when
/// `valid` is given — by the validity the payload itself recorded (W7.1).
///
/// **The `valid` arm is the bitemporal cell.** The fold picks the row the ledger
/// held at `ts`; the payload of that row carries the `valid_from`/`valid_to` the
/// concept had *at that point in the ledger's belief*, so bounding against those
/// answers *what did we believe at `recorded` about what was true at `valid`*.
/// Reading the concept's valid interval from the live `concepts` table instead
/// would answer something else entirely — today's belief about validity, wearing
/// the past's title — which is the exact conflation W7.1 exists to end.
///
/// **It reads the hot log only, and refuses what the hot log cannot answer**
/// (0.13.16, W9.1). See the guard at the top of the body: this is the same
/// refusal [`crate::graph::TraversalBuilder::as_of_recorded`] makes, at the
/// second surface that folds `transaction_log`.
///
/// The window partitions on `entity_id` alone and is sound doing so only because
/// `table_name = 'concepts'` is already in the `WHERE` — the discriminator is
/// applied by the filter instead of by the partition, so the concept/link
/// collision that defect W is about cannot arise here. Stated because the four
/// folds in `replay.rs` now carry the discriminator in the partition and the
/// difference should not read as an oversight.
async fn hydrate_at_time(
    conn: &libsql::Connection,
    node_ids: &[String],
    ts: &str,
    valid: Option<&str>,
) -> Result<HashMap<String, NodeAttributes>> {
    // §3.2, closed in 0.13.16 (W9.1, D-189). The fold below reads the *hot*
    // log, and `archive` physically moves superseded rows out of it -- which is
    // precisely the rows a past instant asks for. Without this the answer was a
    // shorter `Vec`, and a missing element here is indistinguishable from
    // retired and from never having existed.
    //
    // Applied where the read is rather than at whichever caller remembered.
    // `TraversalBuilder::execute_ids` already checks before its own fold, so
    // `execute` now pays for two, and that is the right way round: the two
    // folds are separately reachable, and a guard that lives at the caller is
    // one the next caller does not inherit.
    if !crate::temporal::replay::hot_log_answers_for(conn, ts).await? {
        return Err(DbError::RecordedInstantUnreachable { ts: ts.to_string() });
    }

    let mut found = HashMap::new();

    for chunk in node_ids.chunks(HYDRATE_CHUNK) {
        // **`entity_id` alone, and unlike the link folds that is correct here.**
        //
        // The sweep that widened the four folds in `replay.rs` to carry
        // `branch_id` (D-216) and the traversal's own fold at 0.14.4 (D-220)
        // both left this one alone, so the reason is written down rather than
        // left as an omission that happens to be safe.
        //
        // A link's `entity_id` is the edge key and is shared across lineages by
        // design — that is how a branch corrects an edge it inherited — so a
        // partition on it alone puts two lineages' beliefs in one group. A
        // *concept*'s `entity_id` is the concept id, and under Option A there is
        // exactly one concept row per id across the whole ledger: the guards
        // refuse a second lineage restating one at all, and `branch_id` on
        // `concepts` is provenance rather than identity. One row per id means
        // one `branch_id` per partition, so adding it would change nothing.
        //
        // `table_name = 'concepts'` is in the `WHERE` rather than the partition,
        // which is the same discriminator applied one step earlier.
        let sql = format!(
            r#"
            SELECT entity_id, seq_id, payload FROM (
                SELECT entity_id, seq_id, payload,
                       ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY seq_id DESC) as rn
                FROM transaction_log
                WHERE table_name = 'concepts'
                  AND recorded_at <= ?1
                  AND entity_id IN ({})
            ) WHERE rn = 1
            "#,
            placeholders(2, chunk.len())
        );

        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(libsql::Value::Text(ts.to_string()));
        params.extend(chunk.iter().map(|id| libsql::Value::Text(id.clone())));

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            let seq_id: i64 = row.get(1)?;
            let payload_str: String = row.get(2)?;

            // Raised rather than skipped. A payload that will not parse is the
            // ledger disagreeing with itself, and the previous version's
            // `if let Ok(..)` turned that into a node quietly missing from the
            // answer — the same shape of silence defect W was.
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).map_err(|e| DbError::ReplayCorrupt {
                    seq: seq_id,
                    reason: format!("Failed to parse payload JSON: {e}"),
                })?;

            let v = payload.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
            if v > PAYLOAD_VERSION as u64 {
                return Err(DbError::PayloadVersion {
                    got: v as u8,
                    max: PAYLOAD_VERSION,
                });
            }

            // Retired as of `ts`: not visible, and not an error either.
            if payload.get("retired").and_then(|r| r.as_i64()).unwrap_or(0) != 0 {
                continue;
            }

            // Outside its own valid interval at the instant asked about. Applied
            // in Rust rather than in the `WHERE` because the interval lives
            // inside the JSON payload and the fold has already narrowed to one
            // row per entity — a `json_extract` in the outer filter would read
            // the same bytes this arm already has in hand.
            //
            // A v1 payload carries no `valid_from`/`valid_to` (they arrived with
            // v2), and an absent bound is treated as unbounded on that side: the
            // row is from before the crate recorded validity in the log, and
            // excluding it would report a gap in the ledger that is really a gap
            // in the payload schema.
            if let Some(v) = valid {
                let from = payload.get("valid_from").and_then(|s| s.as_str());
                let to = payload.get("valid_to").and_then(|s| s.as_str());
                if from.is_some_and(|f| f > v) || to.is_some_and(|t| t <= v) {
                    continue;
                }
            }

            found.insert(
                id.clone(),
                NodeAttributes {
                    id,
                    title: payload
                        .get("title")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    content: payload
                        .get("content")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    // Absent in a v1 payload, which is indistinguishable here
                    // from present-and-null and correctly so: both mean the
                    // concept carries no model.
                    embedding_model: payload
                        .get("embedding_model")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                },
            );
        }
    }

    Ok(found)
}
