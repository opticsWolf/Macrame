//! One lineage's view of `concepts` (0.20.0, M4, [D-294]).
//!
//! # What was wrong
//!
//! Edges have been read per lineage since 0.14.4 and cut at fork points since
//! 0.14.6 (D-223). Concepts were not: every concept read — traversal
//! projection, attribute hydration, the subgraph loader, search — joined the
//! live `concepts` row. Two consequences, both measured against 0.19.1:
//!
//! 1. **A branch saw its ancestor's post-fork concept edits.** `concepts` is a
//!    current-state projection updated in place (`ON CONFLICT(id) DO UPDATE`),
//!    so once the trunk re-upserts a concept after a fork, the version the
//!    branch inherited is not in the table any more. Its only home is
//!    `transaction_log` — exactly the situation `links_current` is in, and for
//!    exactly the reason the edge read became a hybrid.
//! 2. **Every lineage saw every other lineage's concepts.** A concept minted
//!    on a sibling resolved by id from the trunk, and the keyword index found
//!    it from anywhere.
//!
//! # The rule
//!
//! For a reader `R` with ancestry `[(R, 0, None), (P1, 1, c1), …]`:
//!
//! - a concept minted on `R` is visible at its latest version;
//! - a concept minted on ancestor `Pi` is visible if it was minted at or
//!   before `ci`, at its latest version at or before `ci`;
//! - a concept minted anywhere else is not visible.
//!
//! A branch cannot restate an inherited concept (`trg_concepts_cross_lineage`,
//! D-214), so every version of a concept lives on its minting lineage and the
//! rule needs no nearest-ancestor window: each concept has exactly one
//! candidate lineage, and the only question is which version of it the cutoff
//! admits.
//!
//! # The hybrid, again
//!
//! [`versions_cte`] reads the projection where it can and the log where it
//! must, the same split `links_cut_cte` makes for edges. One comparison on one
//! row decides the arm: `recorded_at <= cutoff` (or no cutoff) serves the live
//! row; `recorded_at > cutoff` folds the log for the newest entry at or before
//! the cutoff, and a concept with no such entry was minted after the cutoff
//! and drops out. The fold is a correlated `ORDER BY seq_id DESC LIMIT 1`
//! seek on `idx_txlog_fold_partition (table_name, entity_id, branch_id, …)`,
//! paid only by concepts an ancestor touched after the fork — zero for Silk's
//! immutable turns.
//!
//! A recorded instant composes with the cutoff rather than replacing it: the
//! effective cutoff per ancestor is `min(cutoff, ts)`, and the reader's own
//! row (no cutoff) takes `ts`. That is the whole of "what did this branch
//! believe at `ts`".
//!
//! # What it does not cover
//!
//! The keyword and vector indexes hold one row per concept — the latest text
//! and the latest embedding. A churned ancestor concept is therefore *found*
//! by its latest text and *returned* with the version the lineage sees.
//! Searching superseded text would need versioned index rows, which is the
//! overlay design D-214 deferred; for immutable concepts the two coincide.
//!
//! [D-294]: ../../docs/architecture/s13-decision-register.md#d-294

use std::collections::HashMap;

use crate::error::{DbError, Result};
use crate::graph::lineage::{ancestry_params, ancestry_values, Ancestor, Lineages};
use crate::schema::ddl;
use crate::util::limits::HYDRATE_CHUNK;

/// The ancestry a concept read resolves against, or `None` when the database
/// holds one lineage and the live table is already exact.
///
/// Unlike [`crate::graph::lineage::resolve_for`] this returns an ancestry for
/// the **root** on a forked database too — `[(main, 0, None)]` — because the
/// root's concept view is not the whole table once a second lineage can mint:
/// that is consequence 2 in the module docs. Refuses an unregistered lineage
/// by name, as every lineage read does.
pub(crate) async fn scope_for(
    conn: &libsql::Connection,
    branch: Option<&str>,
) -> Result<Option<Vec<Ancestor>>> {
    let named = branch.unwrap_or(ddl::MAIN_BRANCH);
    let lineages = Lineages::load(conn).await?;
    // `shape` is the refusal: it raises `UnknownBranch` for a name that is not
    // registered, which is the answer whether or not the database has forked.
    lineages.shape(named)?;
    if lineages.rows.len() <= 1 {
        return Ok(None);
    }
    Ok(Some(lineages.ancestry(named)))
}

/// The `WITH` list entries that define `concept_versions{tag}` for one
/// lineage: the bound ancestry, the effective cutoffs, and the versions.
///
/// `first_slot` is where the ancestry block starts (`3 × ancestry_len`
/// placeholders). `recorded_slot` is the instant's placeholder when the read
/// names one. Callers put both **after** their own fixed slots, the rule every
/// lineage reader in this crate follows.
///
/// The relation's columns are the live table's, plus the version's own
/// `recorded_at` and the minting `branch_id`:
/// `(id, rowid_pk, title, content, embedding_model, extra, retired,
/// valid_from, valid_to, branch_id, recorded_at)`.
pub(crate) fn versions_cte(
    ancestry_len: usize,
    first_slot: usize,
    recorded_slot: Option<usize>,
    tag: &str,
) -> String {
    let lineage = ancestry_values(ancestry_len, first_slot, &format!("_cv{tag}"));
    // The effective cutoff. The reader's own row has none, and a recorded
    // instant gives it one; an ancestor keeps the earlier of its fork point
    // and the instant.
    let effective = match recorded_slot {
        None => "cutoff".to_string(),
        Some(p) => format!("CASE WHEN cutoff IS NULL OR cutoff > ?{p} THEN ?{p} ELSE cutoff END"),
    };
    format!(
        r#"{lineage},
cut_cv{tag}(branch_id, cutoff) AS (SELECT branch_id, {effective} FROM lineage_cv{tag}),
concept_versions{tag}(id, rowid_pk, title, content, embedding_model, extra, retired, valid_from, valid_to, branch_id, recorded_at) AS (
    SELECT id, rowid_pk,
           CASE WHEN fold IS NULL THEN title ELSE json_extract(fold, '$.title') END,
           CASE WHEN fold IS NULL THEN content ELSE json_extract(fold, '$.content') END,
           CASE WHEN fold IS NULL THEN embedding_model ELSE json_extract(fold, '$.embedding_model') END,
           CASE WHEN fold IS NULL THEN extra ELSE COALESCE(json_extract(fold, '$.extra'), '{{}}') END,
           CASE WHEN fold IS NULL THEN retired ELSE COALESCE(json_extract(fold, '$.retired'), 0) END,
           CASE WHEN fold IS NULL THEN valid_from ELSE json_extract(fold, '$.valid_from') END,
           CASE WHEN fold IS NULL THEN valid_to ELSE json_extract(fold, '$.valid_to') END,
           branch_id,
           CASE WHEN fold IS NULL THEN recorded_at ELSE json_extract(fold, '$.__recorded_at') END
    FROM (
        SELECT c.id, c.rowid_pk, c.title, c.content, c.embedding_model, c.extra, c.retired,
               c.valid_from, c.valid_to, c.branch_id, c.recorded_at, k.cutoff AS cut,
               CASE WHEN k.cutoff IS NULL OR c.recorded_at <= k.cutoff THEN NULL
                    ELSE (SELECT json_set(t.payload, '$.__recorded_at', t.recorded_at)
                            FROM transaction_log t
                           WHERE t.table_name = 'concepts' AND t.entity_id = c.id
                             AND t.branch_id = c.branch_id AND t.recorded_at <= k.cutoff
                           ORDER BY t.seq_id DESC LIMIT 1)
               END AS fold
        FROM concepts c JOIN cut_cv{tag} k ON k.branch_id = c.branch_id
    )
    WHERE cut IS NULL OR recorded_at <= cut OR fold IS NOT NULL
)"#
    )
}

/// One concept as one lineage sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConceptRow {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) content: String,
    pub(crate) embedding_model: Option<String>,
    pub(crate) extra: String,
    pub(crate) retired: bool,
    pub(crate) valid_from: Option<String>,
    pub(crate) valid_to: Option<String>,
    pub(crate) branch_id: String,
    pub(crate) recorded_at: String,
}

impl ConceptRow {
    /// Whether the version is live and, when `valid` is given, valid then —
    /// the same half-open bound every concept read in the crate applies.
    pub(crate) fn shows_at(&self, valid: Option<&str>) -> bool {
        if self.retired {
            return false;
        }
        match valid {
            None => true,
            Some(v) => {
                self.valid_from.as_deref().is_none_or(|f| f <= v)
                    && self.valid_to.as_deref().is_none_or(|t| v < t)
            }
        }
    }
}

/// The lineage's version of each id it can see, retired ones included.
///
/// Ids the lineage cannot see are absent. One query per [`HYDRATE_CHUNK`] ids
/// (defect AE's rule). `recorded`, when given, must already have passed the
/// caller's hot-log reach check.
pub(crate) async fn rows_on(
    conn: &libsql::Connection,
    ids: &[String],
    ancestry: &[Ancestor],
    recorded: Option<&str>,
) -> Result<HashMap<String, ConceptRow>> {
    let mut out = HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    if ancestry.is_empty() {
        // Every caller passes `scope_for`'s answer, which is never an empty
        // ancestry; an empty one would lower to `VALUES ()`, which SQLite
        // refuses, so it is caught here with a sentence instead.
        return Err(DbError::UnknownBranch(String::new()));
    }
    for chunk in ids.chunks(HYDRATE_CHUNK) {
        let n = chunk.len();
        let recorded_slot = recorded.map(|_| n + 1);
        let first = n + 1 + usize::from(recorded.is_some());
        let sql = format!(
            "WITH {} SELECT id, title, content, embedding_model, extra, retired, \
             valid_from, valid_to, branch_id, recorded_at FROM concept_versions \
             WHERE id IN ({})",
            versions_cte(ancestry.len(), first, recorded_slot, ""),
            (1..=n)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut params: Vec<libsql::Value> = chunk
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();
        if let Some(ts) = recorded {
            params.push(ts.into());
        }
        params.extend(ancestry_params(ancestry));

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let id: String = row.get(0)?;
            out.insert(
                id.clone(),
                ConceptRow {
                    id,
                    title: row.get::<Option<String>>(1)?.unwrap_or_default(),
                    content: row.get::<Option<String>>(2)?.unwrap_or_default(),
                    embedding_model: row.get::<Option<String>>(3)?,
                    extra: row.get::<Option<String>>(4)?.unwrap_or_else(|| "{}".into()),
                    retired: row.get::<Option<i64>>(5)?.unwrap_or(0) != 0,
                    valid_from: row.get::<Option<String>>(6)?,
                    valid_to: row.get::<Option<String>>(7)?,
                    branch_id: row.get(8)?,
                    recorded_at: row.get::<Option<String>>(9)?.unwrap_or_default(),
                },
            );
        }
    }
    Ok(out)
}

/// Keep the ids the lineage shows, in the order given (0.20.0, M4).
///
/// The traversal walked the lineage's *edges*; this decides which of the
/// nodes it reached the lineage shows, in two tiers:
///
/// - **Held** — minted on the lineage or inherited from an ancestor at or
///   before its cutoff. Shown iff the lineage's own version is live, so a
///   concept the trunk retired after the fork is still shown on the branch.
/// - **Named** — any other concept a visible edge reached. D-214 made concept
///   identity shared vocabulary and edges the belief: a lineage that asserted
///   an edge to a sibling's concept believes that concept is relevant, and
///   `cross_branch_tests` pins that it reads through it. Shown iff live by
///   the reading the single-lineage path applies (the live row now, the fold
///   at a recorded instant), because the lineage holds no version of it.
///
/// The start node has no edge behind it, so it must be **held**: a traversal
/// from a concept the lineage neither minted nor inherited returns nothing,
/// as a traversal from an unknown id always has.
pub(crate) async fn filter_visible(
    conn: &libsql::Connection,
    start: &str,
    ids: Vec<String>,
    ancestry: &[Ancestor],
    recorded: Option<&str>,
) -> Result<Vec<String>> {
    let mut ask = ids.clone();
    if !ask.iter().any(|i| i == start) {
        ask.push(start.to_string());
    }
    let held = rows_on(conn, &ask, ancestry, recorded).await?;
    if !held.contains_key(start) {
        return Ok(Vec::new());
    }
    let named: Vec<String> = ids
        .iter()
        .filter(|i| !held.contains_key(*i))
        .cloned()
        .collect();
    let named_live = live_ids(conn, &named, recorded).await?;
    Ok(ids
        .into_iter()
        .filter(|id| match held.get(id) {
            Some(r) => !r.retired,
            None => named_live.contains(id),
        })
        .collect())
}

/// Which of `ids` are live by the single-lineage reading: the live row now,
/// or the log fold at `recorded`.
async fn live_ids(
    conn: &libsql::Connection,
    ids: &[String],
    recorded: Option<&str>,
) -> Result<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    if ids.is_empty() {
        return Ok(out);
    }
    match recorded {
        Some(ts) => {
            let folded = crate::temporal::as_of::fold_concepts_at(conn, ids, ts).await?;
            for (id, payload) in folded {
                if crate::temporal::as_of::concept_visible_at(&payload, None)? {
                    out.insert(id);
                }
            }
        }
        None => {
            for chunk in ids.chunks(HYDRATE_CHUNK) {
                let sql = format!(
                    "SELECT id FROM concepts WHERE retired = 0 AND id IN ({})",
                    (1..=chunk.len())
                        .map(|i| format!("?{i}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let params: Vec<libsql::Value> = chunk
                    .iter()
                    .map(|i| libsql::Value::Text(i.clone()))
                    .collect();
                let mut rows = conn.query(&sql, params).await?;
                while let Some(row) = rows.next().await? {
                    out.insert(row.get(0)?);
                }
            }
        }
    }
    Ok(out)
}
