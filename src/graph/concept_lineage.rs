//! One lineage's view of `concepts` (0.22.0, M4, [D-310]).
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
//! and drops out. The fold is a correlated seek on `idx_txlog_fold_partition (table_name, entity_id, branch_id, …)`,
//! paid only by concepts an ancestor touched after the fork — zero for Silk's
//! immutable turns.
//!
//! **The log arm reads by D-303's rule, not by seq** (0.22.0, plan §3.2). The
//! plain pick — `ORDER BY seq_id DESC LIMIT 1` — was the rule before 0.21.0,
//! and it disagrees with the rule exactly when a write lands out of valid
//! order: a backfill has the higher `seq_id` and the lower start. The one way
//! a write lands out of order is the skip (the projection declines, the log
//! row goes in anyway), and a skip marks its pair, so the fold dispatches on
//! the mark: an unmarked pair picks by seq — right *by induction*, the same
//! reasoning `state_rule_owed`'s missing-table branch documents — and a marked
//! pair picks by `valid_from DESC, seq_id DESC` over the same recorded bound
//! on the minting lineage. That is the D-303 correction pair for this reader,
//! gated per candidate rather than per statement: the probe is one indexed
//! lookup a candidate pays, and the rule seek only where it is owed. The
//! single-statement shape is deliberate (the plan pin below carries the
//! plan): a second, Rust-merged correction pass would have to reproduce
//! `cut_cv`'s effective-cutoff arithmetic outside SQL, which is exactly the
//! divergence this file exists to avoid.
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
//! [D-310]: ../../docs/architecture/s13-decision-register.md#d-310

//! # The overlay, since M6-b (0.22.0, [Amendment 2](../../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.3)
//!
//! D-214's law above ("every version of a concept lives on its minting
//! lineage") is no longer the whole story: M6-a built the overlay. A branch
//! writes *versions* of an inherited id on its own lineage and *cuts* for the
//! rejections it makes; the amendment's §5.3 replaces the one-candidate law
//! with a walk:
//!
//! - a read on lineage `L` walks the path nearest first, each step at its
//!   recorded cap (~[`Ancestor::cutoff`], composed with the read's instant);
//! - the first overlay row of an id *covering the instant* answers — a
//!   version wins over a cut where both cover — and a covering cut answers
//!   **absent**, stopping the walk outright;
//! - nothing overlay-relative decides for an id where the projection answers
//!   as it always did (D-310's fold, byte for byte, down to the marked-pair
//!   dispatch).
//!
//! A read that names no valid instant resolves at the far end of the axis:
//! "covering" there means the version is *open* (`valid_to` at the sentinel)
//! and every cut covers the present. So a windowed takeover (a version that
//! ends, with its cut standing) is absent at current belief — the
//! amendment's own sentence — and open within its window.
//!
//! [`overlay_engaged`] is the switch this milestone runs everything behind
//! (§5.4 cost 1; the byte-identical condition): a forked ledger with
//! *something* in the overlay resolves by the walk; anything else runs the
//! statements above, byte for byte, and the goldens keep pinning them.
//! [`rows_on`] is where the two laws meet: the walk's decisions first,
//! [D-310]'s fold for the ids the walk did not decide, and the walk says
//! **absent** (a cut) for an id, no reader revives it from the named tier —
//! the branch's law outranks shared vocabulary for ids the walk spoke on.

use std::collections::{HashMap, HashSet};

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
    ruled: bool,
) -> String {
    let lineage = ancestry_values(ancestry_len, first_slot, &format!("_cv{tag}"));
    // The effective cutoff. The reader's own row has none, and a recorded
    // instant gives it one; an ancestor keeps the earlier of its fork point
    // and the instant.
    let effective = match recorded_slot {
        None => "cutoff".to_string(),
        Some(p) => format!("CASE WHEN cutoff IS NULL OR cutoff > ?{p} THEN ?{p} ELSE cutoff END"),
    };
    // The log arm's own pick. Unmarked pairs take the last row written before
    // the cutoff; marked pairs take D-303's rule row — the latest start, and
    // within one start the row written last — over the same bound. The two
    // forms differ in their `ORDER BY` only; the `WHERE` is copied verbatim
    // between them so neither can drift from the other's window. The shape 5
    // lesson keeps the probe inside this expression rather than a join: as
    // `concepts_at_instant_sql_marked` records, a bare name in a subquery
    // binds to the innermost scope first, so the probe names `d` and both
    // arms name their `t` fully.
    let pick = if ruled {
        r#"CASE WHEN EXISTS (
                 SELECT 1 FROM concept_disorder d WHERE d.entity_id = c.id
               )
               THEN (SELECT json_set(t.payload, '$.__recorded_at', t.recorded_at)
                       FROM transaction_log t
                      WHERE t.table_name = 'concepts' AND t.entity_id = c.id
                        AND t.branch_id = c.branch_id AND t.recorded_at <= k.cutoff
                      ORDER BY json_extract(t.payload, '$.valid_from') DESC, t.seq_id DESC
                      LIMIT 1)
               ELSE (SELECT json_set(t.payload, '$.__recorded_at', t.recorded_at)
                       FROM transaction_log t
                      WHERE t.table_name = 'concepts' AND t.entity_id = c.id
                        AND t.branch_id = c.branch_id AND t.recorded_at <= k.cutoff
                      ORDER BY t.seq_id DESC
                      LIMIT 1)
           END"#
    } else {
        r#"(SELECT json_set(t.payload, '$.__recorded_at', t.recorded_at)
              FROM transaction_log t
             WHERE t.table_name = 'concepts' AND t.entity_id = c.id
               AND t.branch_id = c.branch_id AND t.recorded_at <= k.cutoff
             ORDER BY t.seq_id DESC LIMIT 1)"#
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
                    ELSE {pick}
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
    /// Whether this answer is an overlay version row (`(id, branch_id,
    /// valid_from)`), as opposed to the fold's answer (the mint lineage's
    /// row, whatever lineage currently holds it). Search needs the
    /// distinction to name the drop: a trunk-index hit for an id whose
    /// resolution is a version, and a version-index hit whose resolution
    /// is another version, contradict the answer the reader gives.
    pub(crate) versioned: bool,
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
    valid: Option<&str>,
) -> Result<(HashMap<String, ConceptRow>, HashSet<String>)> {
    let mut out: HashMap<String, ConceptRow> = HashMap::with_capacity(ids.len());
    let mut absent: HashSet<String> = HashSet::new();
    if ids.is_empty() {
        return Ok((out, absent));
    }
    if ancestry.is_empty() {
        // Every caller passes `scope_for`'s answer, which is never an empty
        // ancestry; an empty one would lower to `VALUES ()`, which SQLite
        // refuses, so it is caught here with a sentence instead.
        return Err(DbError::UnknownBranch(String::new()));
    }
    // M6-b: the resolved ids first ([Amendment 2]
    // (../../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
    // §5.3). Ids the walk did not decide fall through to the fold, byte for
    // byte; ids the walk answered with a cut are absent and are named back so
    // no caller revives them from the shared vocabulary.
    let engaged = overlay_engaged(conn).await?;
    let todo: Vec<String> = if engaged {
        for (id, decision) in overlay_scan(conn, ids, ancestry, recorded, valid).await? {
            match decision {
                OverlayDecision::Row(r) => {
                    out.insert(id, r);
                }
                OverlayDecision::Absent => {
                    absent.insert(id);
                }
            }
        }
        let remaining: Vec<String> = ids
            .iter()
            .filter(|i| !out.contains_key(*i) && !absent.contains(*i))
            .cloned()
            .collect();
        remaining
    } else {
        ids.to_vec()
    };

    if !todo.is_empty() {
        let held = legacy_rows_on(conn, &todo, ancestry, recorded, valid).await?;
        out.extend(held);
    }
    Ok((out, absent))
}

/// [D-310]'s fold, on the ids the overlay walk left undecided (0.22.0, M4,
/// valid arm [D-316]).
///
/// The body that ran for every id before the overlay existed; on a store with
/// nothing in the overlay it still runs for *every* id, byte for byte at the
/// present tense, which is the whole of the switch's reason. At the present
/// tense (`valid` unset) the window law is yesterday's: the fold answers one
/// current row per id and the caller applies `shows_at` after — no read
/// changed, byte for byte. At a named instant the fold serves the window when
/// the current row cannot: the projection's row is tried first (the fast path
/// it always was), and a row that is retired, or whose window excludes the
/// instant, pastes [`VALID_FOLD`]'s own answer for the id — one current belief
/// per valid start, the same selection a whole-ledger valid read holds, read
/// through the id (and lineage spelling) the projection's row carries. That
/// is the one-row-per-id blind the retirement review proved on the trunk
/// ([D-316]): a branch read decided its past from the overlay while the
/// trunk's tombstone (or a plain supersede's newer row) answered nothing at
/// every instant before the row's own start, and the same method was giving
/// two answers for one past date. The D-310 fold's own docstring gave the
/// reason once — "valid instants are not passed: today's callers apply their
/// windows after the fold" — and that sentence is the defect the instant
/// arm answers.
async fn legacy_rows_on(
    conn: &libsql::Connection,
    ids: &[String],
    ancestry: &[Ancestor],
    recorded: Option<&str>,
    valid: Option<&str>,
) -> Result<HashMap<String, ConceptRow>> {
    let mut out = HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    // The state rule reaches this fold once per call (D-303; 0.22.0 plan
    // §3.2 for the lineage read): one probe, and the fold dispatches on marks
    // per candidate row. The probe is cheap and per *call* on purpose,
    // exactly as `fold_concepts_at` reasons it — the chunking is an IO shape,
    // and what is owed does not change inside one read.
    let ruled = crate::temporal::replay::state_rule_owed(conn).await?;
    for chunk in ids.chunks(HYDRATE_CHUNK) {
        let n = chunk.len();
        let recorded_slot = recorded.map(|_| n + 1);
        let first = n + 1 + usize::from(recorded.is_some());
        let sql = format!(
            "WITH {} SELECT id, title, content, embedding_model, extra, retired, \
             valid_from, valid_to, branch_id, recorded_at FROM concept_versions \
             WHERE id IN ({})",
            versions_cte(ancestry.len(), first, recorded_slot, "", ruled),
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
            let current = ConceptRow {
                id: id.clone(),
                // Not an overlay row: this is the fold's answer, the
                // mint lineage's row (or the live one on the reading
                // lineage) - the law the overlay walk falls through to.
                versioned: false,
                title: row.get::<Option<String>>(1)?.unwrap_or_default(),
                content: row.get::<Option<String>>(2)?.unwrap_or_default(),
                embedding_model: row.get::<Option<String>>(3)?,
                extra: row.get::<Option<String>>(4)?.unwrap_or_else(|| "{}".into()),
                retired: row.get::<Option<i64>>(5)?.unwrap_or(0) != 0,
                valid_from: row.get::<Option<String>>(6)?,
                valid_to: row.get::<Option<String>>(7)?,
                branch_id: row.get(8)?,
                recorded_at: row.get::<Option<String>>(9)?.unwrap_or_default(),
            };
            // [D-316]'s paste: the current row serves the present-tense law
            // and any instant its window names; a retired row serves nothing,
            // and a non-retired row that was superseded (or cut, or written
            // at a later start) does not serve the instants before its own
            // start. There the log's own valid fold answers — one current
            // belief per valid start at the instant, the same selection a
            // whole-ledger valid read returns for this id — and only when it
            // itself holds a belief does the paste land. An empty fold is
            // the honest answer: the ledger remembered nothing, and this
            // read does not invent a belief to cover that.
            let serves = !current.retired
                && current
                    .valid_from
                    .as_deref()
                    .is_none_or(|f| f <= valid.unwrap_or(""))
                && valid.is_none_or(|v| current.valid_to.as_deref().is_none_or(|t| v < t));
            match valid {
                Some(v) if !serves => {
                    // The paste carries the fold's own two caps, earlier
                    // wins ([D-316], amended): the read's recorded instant
                    // when one is named, and the ancestry cutoff of the
                    // spelling the paste reads - the visibility rule the
                    // fold's own CTE bounds every ancestor's rows with, so
                    // on an inherited row the ancestor's post-fork writes
                    // stay invisible below the fork even where a backfill's
                    // window covers. The membership is the fold's own join
                    // (the CTE answered this row through it), so a spelling
                    // without an ancestry entry cannot reach here; the
                    // trunk's own paste has no cutoff above it and stays
                    // uncut. Stamps are Zulu, so text order is instant
                    // order - the same comparison `lineage::resolve` makes.
                    let cut = ancestry
                        .iter()
                        .find(|a| a.branch_id == current.branch_id)
                        .and_then(|a| a.cutoff.as_deref());
                    let bound = match (recorded, cut) {
                        (Some(r), Some(c)) => Some(if r <= c { r } else { c }),
                        (r, c) => r.or(c),
                    };
                    if let Some(pasted) = paste_fold_row(conn, &current, v, bound).await? {
                        out.insert(id, pasted);
                    }
                }
                _ => {
                    out.insert(id, current);
                }
            }
        }
    }
    Ok(out)
}

/// The `recorded_at` of one log row, read by its `seq_id` (the paste's
/// stamp-arm: a raw log payload carries the payload's fields, not the row's
/// own stamps, and the read's `recorded_at` answer names the belief's row,
/// not the fold's).
///
/// One indexed lookup, and only for a payload the fold's own arm would have
/// stamped — the log trigger composes the stamp into the `json_set` arm's
/// payload, but a paste meeting a pre-stamp shape (the trigger's literal
/// row) reads the column here.
async fn log_recorded_at(conn: &libsql::Connection, seq_id: i64) -> Result<String> {
    let mut rows = conn
        .query(
            "SELECT recorded_at FROM transaction_log WHERE seq_id = ?1",
            libsql::params![seq_id],
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .map(|r| r.get::<String>(0).unwrap_or_default())
        .unwrap_or_default())
}

/// The log's own window for one id, pasted where the fold's current row cannot
/// serve the instant ([D-316]).
///
/// The query is [D-292]'s valid-fold selection narrowed to one id and one
/// lineage spelling: one current belief per valid start (`ROW_NUMBER` per
/// `(table_name, entity_id, branch_id, valid_from)`, newest `seq_id` wins -
/// the D-291 partition law's read shape), `concepts` rows only (the version
/// and cut rows are the overlay's own and the already-decided ids never get
/// here). The bound is the fold it stands in for already carries, twice
/// over: the read's own recorded instant when one is named (the bitemporal
/// cell's cap - the whole-ledger [`VALID_FOLD`] runs unbounded because it
/// names no lineage and asserts nothing about what a reader believed; a
/// per-lineage read of this id does), and the ancestry cutoff of the
/// lineage the paste reads - the visibility rule the fold's own CTE
/// bounds every ancestor's rows with, so on an inherited row an ancestor's
/// post-fork writes stay invisible below the fork even where a backfill's
/// window covers (D-315 already named the shape: "ancestor rows it could
/// ever fall through to are fixed at the fork"). The caller passes the
/// earlier of the two; `None` is the trunk's own paste, uncapped in
/// either axis.
///
/// (An earlier draft said "no recorded bound on purpose"; the decision
/// Register's D-316 amendment carries that history.)
///
///
/// The id and spelling come from the current row the fold already answered
/// (the id the caller asked for; the lineage the projection carries — the
/// mint lineage on an inherited read, the reading lineage's own spelling on
/// its own). The answer turns back into a [`ConceptRow`] the same shape the
/// CTE returns, `versioned: false` — it is the log's answer, not an overlay
/// row — and `concept_visible_at` applies the same half-open window and
/// retirement law the recorded folds apply, so a retired start's belief reads
/// absent exactly as the projection doc reads `retired`.
async fn paste_fold_row(
    conn: &libsql::Connection,
    current: &ConceptRow,
    valid: &str,
    recorded: Option<&str>,
) -> Result<Option<ConceptRow>> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT seq_id, table_name, entity_id, operation, payload, branch_id \
                 FROM ( \
                    SELECT seq_id, table_name, entity_id, operation, payload, branch_id, \
                           ROW_NUMBER() OVER ( \
                               PARTITION BY table_name, entity_id, branch_id, json_extract(payload, '$.valid_from') \
                               ORDER BY seq_id DESC \
                           ) as rn \
                    FROM transaction_log \
                    WHERE table_name = 'concepts' \
                      AND entity_id = ?1 \
                      AND branch_id = ?2 \
                      {cap} \
                ) WHERE rn = 1",
                cap = match recorded {
                    Some(_) => "AND recorded_at <= ?3",
                    None => "",
                }
            ),
            libsql::params![
                current.id.as_str(),
                current.branch_id.as_str(),
                match recorded {
                    Some(r) => libsql::Value::Text(r.to_string()),
                    None => libsql::Value::Null,
                },
            ],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let seq_id: i64 = row.get(0)?;
        let table_name: String = row.get(1)?;
        let entity_id: String = row.get(2)?;
        let op: String = row.get(3)?;
        let payload_str: String = row.get(4)?;
        let branch_id: String = row.get(5)?;
        let mut payload =
            crate::temporal::paste_log_row(seq_id, &table_name, &entity_id, &op, &payload_str)?;
        if payload.get("__recorded_at").is_none() {
            let stamp = log_recorded_at(conn, seq_id).await?;
            payload["__recorded_at"] = serde_json::Value::String(stamp);
        }
        if !crate::temporal::as_of::concept_visible_at(&payload, Some(valid))? {
            // Retired at the instant, or outside its own window: the old
            // shape's law, not believed at this instant.
            continue;
        }
        let title = payload
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let content = payload
            .get("content")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let embedding_model = payload
            .get("embedding_model")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        let extra = payload
            .get("extra")
            .and_then(|t| t.as_str())
            .unwrap_or("{}")
            .to_string();
        let valid_from = payload
            .get("valid_from")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        let valid_to = payload
            .get("valid_to")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        return Ok(Some(ConceptRow {
            id: entity_id,
            versioned: false,
            title,
            content,
            embedding_model,
            extra,
            retired: false,
            valid_from,
            valid_to,
            branch_id,
            recorded_at: payload
                .get("__recorded_at")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
        }));
    }
    Ok(None)
}

/// Keep the ids the lineage shows, in the order given (0.22.0, M4).
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
    valid: Option<&str>,
) -> Result<Vec<String>> {
    let mut ask = ids.clone();
    if !ask.iter().any(|i| i == start) {
        ask.push(start.to_string());
    }
    let (held, absent) = rows_on(conn, &ask, ancestry, recorded, valid).await?;
    if !held.contains_key(start) {
        return Ok(Vec::new());
    }
    let named: Vec<String> = ids
        .iter()
        .filter(|i| !held.contains_key(*i) && !absent.contains(*i))
        .cloned()
        .collect();
    let named_live = live_ids(conn, &named, recorded).await?;
    Ok(ids
        .into_iter()
        .filter(|id| match held.get(id) {
            Some(r) => !r.retired,
            None => !absent.contains(id) && named_live.contains(id),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// The overlay resolution (0.22.0, M6-b, [Amendment 2](../../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
// §5.3's walk, SQL-side)
// ---------------------------------------------------------------------------

/// Whether the overlay resolution runs for this store (§5.4 cost 1's switch):
/// more than one lineage, **and** the overlay has said something. Anything
/// else — single-lineage, or forked with an untouched overlay — runs the
/// readers exactly as they ran before this milestone, which is what the
/// byte-identical goldens pin and what "the schema is additive" keeps honest.
///
/// One query, two EXISTS probes on the way in; readonly connections see only
/// committed rows, so a write that lands between the probe and the read is
/// not a shape this read has to answer (the actor serializes them; the
/// decision is made once, on committed state).
pub(crate) async fn overlay_engaged(conn: &libsql::Connection) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM branches) > 1 \
             AND (EXISTS (SELECT 1 FROM concept_versions) \
             OR EXISTS (SELECT 1 FROM concept_cuts))",
            (),
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .map(|r| r.get::<i64>(0).unwrap_or(0) == 1)
        .unwrap_or(false))
}

/// The overlay's answer for one id: the resolved row (the nearest overlay
/// lineage's version that covers the instant), or an explicit refusal — a cut
/// the walk stopped on.
#[derive(Debug)]
// The variants differ by one `ConceptRow`'s size and the answers are few per
// read (one per id in the chunk); boxing the row would buy nothing.
#[allow(clippy::large_enum_variant)]
pub(crate) enum OverlayDecision {
    Row(ConceptRow),
    Absent,
}

/// The walk per id at a chunk of ids, at the named caps.
///
/// `valid` names the instant, or `None` for the axis's far end — current
/// belief. The distance law is a `row_number` over the candidates, ordered
/// nearest first; the version-before-cut rule rides the same `ORDER BY`, so
/// the winner's `kind` decides: a version is the read's text, a cut is
/// absent. It mirrors `m6_resolution_tests`' `RESOLVE_SQL` at the walk's core
/// and extends it per chunk with the two things a content reader carries that
/// the probe did not: the full row and the two hybrid channels (the
/// projection and the version log, per the same table-else-log rule D-310
/// spells for the mint lineage).
pub(crate) async fn overlay_scan(
    conn: &libsql::Connection,
    ids: &[String],
    ancestry: &[Ancestor],
    recorded: Option<&str>,
    valid: Option<&str>,
) -> Result<HashMap<String, OverlayDecision>> {
    let mut out: HashMap<String, OverlayDecision> = HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    // The valid slot is always bound: the instant when the read names one,
    // the axis's far end when it does not. One window text for both modes,
    // the mode's difference carried by the comparison the bound makes.
    let open_ended = valid.is_none();
    for chunk in ids.chunks(HYDRATE_CHUNK) {
        let n = chunk.len();
        let recorded_slot = recorded.map(|_| n + 1);
        let valid_slot = n + 1 + usize::from(recorded.is_some());
        let first = valid_slot + 1;
        let sql = scan_sql(
            n,
            ancestry.len(),
            first,
            recorded_slot,
            valid_slot,
            open_ended,
        );
        let mut params: Vec<libsql::Value> = chunk
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();
        if let Some(ts) = recorded {
            params.push(ts.into());
        }
        params.push(match valid {
            Some(v) => libsql::Value::Text(v.to_string()),
            None => libsql::Value::Text(crate::util::timestamp::OPEN_SENTINEL.to_string()),
        });
        params.extend(ancestry_params(ancestry));

        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            let kind: String = row.get(0)?;
            let id: String = row.get(1)?;
            if kind == "k" {
                out.insert(id, OverlayDecision::Absent);
                continue;
            }
            let bid: String = row.get(2)?;
            out.insert(
                id.clone(),
                OverlayDecision::Row(ConceptRow {
                    id,
                    // An overlay row by definition - the walk answered
                    // with concept_versions' own row.
                    versioned: true,
                    title: row.get::<Option<String>>(3)?.unwrap_or_default(),
                    content: row.get::<Option<String>>(4)?.unwrap_or_default(),
                    embedding_model: row.get::<Option<String>>(5)?,
                    extra: row.get::<Option<String>>(6)?.unwrap_or_else(|| "{}".into()),
                    // A version channel row is never the retired row: the
                    // retire is this lineage's cut or its own window's end.
                    retired: false,
                    valid_from: row.get(7)?,
                    valid_to: row.get(8)?,
                    branch_id: bid,
                    recorded_at: row.get::<Option<String>>(9)?.unwrap_or_default(),
                }),
            );
        }
    }
    Ok(out)
}

/// The walk's statement: the bound ancestry, the per-lineage recorded caps,
/// the two hybrid channels against those caps, the cuts as their own rows,
/// and a distance pick. Fixed slots first, ancestry last — ids `1..n`, the
/// recorded instant when named, the valid instant (the sentinel at the far
/// end), then the `3 × rows` ancestry tuples [`ancestry_params`] fills.
///
/// The window's comparison is the mode's whole difference: at an instant the
/// half-open bound every read in the crate applies (`valid_from <= v AND
/// v < valid_to`); at the far end a version covers while it is *open*
/// (`valid_to >= v`, which reaches the sentinel and nothing else), because a
/// belief whose window ended is not what the lineage believes now — the
/// amendment's own sentence, put in SQL.
///
/// The log arms pair with the table arms the way [`versions_cte`]'s does: a
/// log row counts only when the table's own row for the same pair is not yet
/// at the cap (its `recorded_at` past the read), and a pair's newest log row
/// is its state there. The versions channel takes by `(id, valid_from)`
/// partitions, as the write law keeps the same start's rewrites in place;
/// the cuts channel takes by `(id, cut_at)`.
///
/// **The cap and the pick are the two rules the log arms keep, and the two
/// the review of M6-b refined.** First, a log row counts only on a *named*
/// cap: under an unbounded read the table row is always `at the cap`, and a
/// stale log payload of that pair (the pre-close insert, whose window's only
/// record is the log) would sit beside the current row and be picked over it.
/// Second, a pair's newest row is picked **before** coverage is applied: a
/// `WHERE` that filtered on the window first would let an older open row
/// survive a pick that must answer with the newer closed one (covered
/// after, then the cut's window), which is exactly the state as of the cap
/// between a close and a later touch of the same pair. So coverage is a
/// predicate on the pair-picked rows, not a `WHERE` on the log arms — which
/// is also why both arms can share the normalized columns: coverage reads
/// `valid_from`/`valid_to` of whatever the pair-pick left standing.
fn scan_sql(
    n: usize,
    rows: usize,
    first: usize,
    recorded_slot: Option<usize>,
    valid_slot: usize,
    open_ended: bool,
) -> String {
    let p = valid_slot;
    // The window's sources per arm — the table's columns and the log row's
    // payload — and the mode's one difference, the comparison on `valid_to`:
    // an instant takes the half-open bound, the axis's far end takes "open",
    // which reaches exactly the sentinel row.
    // `{t}` is the compared expression whole — a column when the candidate
    // row is a table row, its parked payload's field when it is a log row —
    // and `{op}` is the direction the mode picks: the axis's far end is
    // covered while the interval is open, an instant by the same half-open
    // bound every read in this crate applies.
    let to_cmp = if open_ended {
        "{t} >= ?{p}"
    } else {
        "{t} > ?{p}"
    };
    // The staged strings carry `{p}` because the outer `format!` cannot
    // interpolate a value's braces; the slot is finished here, once.
    let p_str = p.to_string();
    let finish = |staged: String| staged.replace("{p}", &p_str);
    let vtw = finish(format!(
        "v.valid_from <= ?{p} AND {}",
        to_cmp.replace("{t}", "v.valid_to")
    ));
    // The pick's coverage predicate over the normalized columns (the table
    // and log arms share one shape after the union, so one predicate serves
    // both; cuts guard their named-end arm off with `kind = 'k'`).
    let vtw_plain = finish(to_cmp.replace("{t}", "valid_to"));
    let ktw = finish("k.cut_at <= ?{p}".to_string());

    let cap = match recorded_slot {
        Some(q) => format!("CASE WHEN cutoff IS NULL OR cutoff > ?{q} THEN ?{q} ELSE cutoff END"),
        None => "cutoff".to_string(),
    };
    let lineage = ancestry_values(rows, first, "_ov");
    let ids = (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"WITH {lineage},
ov_step(bid, dist, cap) AS (
    SELECT branch_id, dist, {cap}
    FROM lineage_ov
),
ov_rows(id, bid, dist, kind, valid_from, valid_to, title, content, extra, embedding_model, recorded_at, seq) AS (
    SELECT v.id, s.bid, s.dist, 'v', v.valid_from, v.valid_to, v.title, v.content, v.extra, v.embedding_model, v.recorded_at, 0
    FROM ov_step s JOIN concept_versions v ON v.branch_id = s.bid
    WHERE v.id IN ({ids}) AND (s.cap IS NULL OR v.recorded_at <= s.cap) AND {vtw}
    UNION ALL
    SELECT j.entity_id, s.bid, s.dist, 'v',
           json_extract(j.payload, '$.valid_from'), json_extract(j.payload, '$.valid_to'),
           json_extract(j.payload, '$.title'), json_extract(j.payload, '$.content'),
           COALESCE(json_extract(j.payload, '$.extra'), '{{}}'), json_extract(j.payload, '$.embedding_model'),
           j.recorded_at, j.seq_id
    FROM ov_step s JOIN transaction_log j ON j.branch_id = s.bid
    WHERE j.table_name = 'concept_versions' AND j.entity_id IN ({ids})
      AND s.cap IS NOT NULL
      AND j.recorded_at <= s.cap
      AND NOT EXISTS (
          SELECT 1 FROM concept_versions w
          WHERE w.id = j.entity_id AND w.branch_id = s.bid
            AND w.valid_from = json_extract(j.payload, '$.valid_from')
            AND w.recorded_at <= s.cap
      )
    UNION ALL
    SELECT k.id, s.bid, s.dist, 'k', k.cut_at, NULL, NULL, NULL, NULL, NULL, k.recorded_at, 0
    FROM ov_step s JOIN concept_cuts k ON k.branch_id = s.bid
    WHERE k.id IN ({ids}) AND (s.cap IS NULL OR k.recorded_at <= s.cap) AND {ktw}
    UNION ALL
    SELECT j.entity_id, s.bid, s.dist, 'k',
           json_extract(j.payload, '$.cut_at'), NULL, NULL, NULL, NULL, NULL, j.recorded_at, j.seq_id
    FROM ov_step s JOIN transaction_log j ON j.branch_id = s.bid
    WHERE j.table_name = 'concept_cuts' AND j.entity_id IN ({ids})
      AND s.cap IS NOT NULL
      AND j.recorded_at <= s.cap
      AND NOT EXISTS (
          SELECT 1 FROM concept_cuts w
          WHERE w.id = j.entity_id AND w.branch_id = s.bid
            AND w.cut_at = json_extract(j.payload, '$.cut_at')
            AND w.recorded_at <= s.cap
      )
),
ov_pair AS (
    -- A pair's newest row is its state at the cap: one pick per
    -- (id, lineage, kind, start) — the tables' own keys — before coverage
    -- says anything. The table arm and the log arm never mix for one pair
    -- (the NOT EXISTS pairs them), so the seq order is between log rows
    -- alone; a table row is its pair's whole arm.
    SELECT id, bid, dist, kind, valid_from, valid_to, title, content, extra, embedding_model, recorded_at, seq,
           ROW_NUMBER() OVER (
               PARTITION BY id, bid, kind, valid_from
               ORDER BY seq DESC
           ) AS rn
    FROM ov_rows
),
ov_pick AS (
    -- The distance pick over the pair-picked rows, coverage applied as a
    -- filter here and not sooner: at an instant the half-open bound, at the
    -- far end a version covers while it is open (a cut's `valid_from` is its
    -- `cut_at`, and NULL `valid_to` never passes the version predicate).
    SELECT id, bid, valid_from, valid_to, kind, title, content, extra, embedding_model, recorded_at,
           ROW_NUMBER() OVER (
               PARTITION BY id
               ORDER BY dist ASC,
                        CASE kind WHEN 'k' THEN 1 ELSE 0 END ASC,
                        valid_from DESC, seq DESC
           ) AS rn
    FROM ov_pair
    WHERE rn = 1
      AND valid_from <= ?{p}
      AND (kind = 'k' OR {vtw_plain})
)
SELECT kind, id, bid, title, content, extra, embedding_model, valid_from, valid_to, recorded_at
FROM ov_pick WHERE rn = 1"#
    )
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

#[cfg(test)]
mod plan_pins {
    use super::*;

    pub(super) async fn fresh() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn
    }

    async fn plan(conn: &libsql::Connection, ruled: bool) -> String {
        let sql = format!(
            "WITH {with} SELECT id FROM concept_versions WHERE id = 'x'",
            with = versions_cte(2, 1, None, "", ruled)
        );
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            plan.push(r.get::<String>(3).unwrap());
        }
        plan.join(" | ")
    }

    /// The log arm's plan, pinned from the first commit it exists (0.22.0,
    /// plan §3.2). The plan §3.2 of the 0.22.0 plan says it plainly: a new
    /// statement shape gets an `EXPLAIN QUERY PLAN` pin, because the
    /// 0.21.0 cross-join lesson measured a plan that had a shelf life only
    /// while nobody touched the statement.
    ///
    /// Four lines are load-bearing:
    ///
    /// 1. **the fold seek binds the whole key** —
    ///    `idx_txlog_fold_partition (table_name=? AND entity_id=? AND
    ///    branch_id=?)` — one concept's history per seek, wherever it runs
    ///    (the projection join and the seq arm);
    /// 2. **the mark probe is the covering PK probe, entity alone** —
    ///    `concept_disorder_1 (entity_id=?)` — the bound `concepts_at_instant_sql_marked`
    ///    documents; the branch adds nothing, because a concept's marks live
    ///    only on its minting lineage anyway;
    /// 3. **the rule arm rides `idx_txlog_concept_starts`** — the v24 rung
    ///   ([D-303]) rebuilt that partial index with `valid_from DESC` exactly
    ///    so the correction's winner could be read in index order; the plan
    ///    confirms the two releases agree;
    /// 4. **no temp b-tree anywhere** — the seek order serves the ORDER BY
    ///    under LIMIT 1, so the rule's cost is a bounded walk that stops at
    ///    the first row.
    #[tokio::test]
    async fn the_folds_seek_the_partition_and_the_rule_rides_the_starts_index() {
        let conn = fresh().await;

        let ruled = plan(&conn, true).await;
        assert!(
            ruled.contains(
                "SEARCH t USING INDEX idx_txlog_fold_partition \
                 (table_name=? AND entity_id=? AND branch_id=?)"
            ),
            "the seq arm (and the projection join) must seek the fold \
             partition on the whole key: {ruled}"
        );
        assert!(
            ruled.contains("concept_disorder_1 (entity_id=?)"),
            "the mark probe binds entity alone, as \
             `concepts_at_instant_sql_marked` documents: {ruled}"
        );
        assert!(
            ruled.contains("idx_txlog_concept_starts"),
            "the rule arm rides the v24 starts index in its own \
             (valid_from DESC) order: {ruled}"
        );
        assert!(
            !ruled.contains("USE TEMP B-TREE FOR ORDER BY"),
            "the ORDER BY under LIMIT 1 must not sort: {ruled}"
        );

        // The unruled half is its own assertion, so the pair's asymmetry is
        // pinned and not implied: no marks reference at all, one seek shape.
        let plain = plan(&conn, false).await;
        assert!(
            !plain.contains("concept_disorder"),
            "the plain fold must not touch the marks table at all: {plain}"
        );
        assert!(
            plain.contains(
                "SEARCH t USING INDEX idx_txlog_fold_partition \
                 (table_name=? AND entity_id=? AND branch_id=?)"
            ),
            "the plain fold's seek is the same shape: {plain}"
        );
    }
}

#[cfg(test)]
mod scan_probe {
    use super::*;

    /// The scan statement parses and binds against the migrated schema —
    /// the placeholder layout's own rehearsal (the shape-5 lesson keeps a
    /// class of probes here rather than out in the tests).
    #[tokio::test]
    async fn the_overlay_scan_statement_parses_and_binds() {
        let conn = plan_pins::fresh().await;
        let ids = vec!["x".to_string(), "y".to_string()];
        for (ancestry, valid) in [(2usize, None), (2, Some("2026-01-01T00:00:00.000000Z"))] {
            let anc: Vec<Ancestor> = (0..ancestry)
                .map(|i| Ancestor {
                    branch_id: format!("b{i}"),
                    dist: i as i64,
                    cutoff: None,
                })
                .collect();
            let _ = overlay_scan(&conn, &ids, &anc, None, valid).await;
            println!(
                "SQL: {}",
                scan_sql(ids.len(), anc.len(), 3, None, 6, valid.is_none())
            );
        }
    }

    /// The pair-pick rehearsal: a pair opened at T5 and closed at AFTER,
    /// its table row pushed past the named cap, read inside the window at
    /// the cap. The cap arm must serve the pair's log rows, pick the close
    /// (the newest row for the pair), and cover with it.
    #[tokio::test]
    async fn the_pair_pick_reads_the_close_at_the_cap() {
        let conn = plan_pins::fresh().await;
        // branch identities
        conn.execute(
            "INSERT OR IGNORE INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('main', NULL, NULL, ?1)",
            libsql::params!["2026-01-01T00:00:00.000000Z"],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES ('b0', 'main', ?1, ?1)",
            libsql::params!["2026-01-02T00:00:00.000000Z"],
        )
        .await
        .unwrap();
        let (cap, poke) = ("2026-02-01T00:00:00.000000Z", "2027-01-01T00:00:00.000000Z");
        for row in [
            // (valid_from, valid_to, recorded)
            ("2026-01-10T00:00:00.000000Z", "2026-01-11T00:00:00.000000Z"),
        ] {
            conn.execute(
                "INSERT INTO concept_versions (id, branch_id, valid_from, valid_to, title, recorded_at) \
                 VALUES ('x', 'b0', ?1, ?2, 'v', ?3)",
                libsql::params![row.0, row.1, cap],
            )
            .await
            .unwrap();
        }
        // push the table row past the cap
        conn.execute(
            "UPDATE concept_versions SET recorded_at = ?1",
            libsql::params![poke],
        )
        .await
        .unwrap();
        // two log rows for the same pair: the open insert and the close
        for (i, (valid_to, rec)) in [
            ("9999-12-31T23:59:59.999999Z", "2026-01-10T00:00:00.000000Z"),
            ("2026-01-11T00:00:00.000000Z", "2026-01-11T00:00:00.000000Z"),
        ]
        .into_iter()
        .enumerate()
        {
            let payload = format!(
                r#"{{"valid_from":"2026-01-10T00:00:00.000000Z","valid_to":"{valid_to}","title":"v","v":1}}"#
            );
            conn.execute(
                "INSERT INTO transaction_log (table_name, entity_id, operation, payload, branch_id, recorded_at) \
                 VALUES ('concept_versions', 'x', 'I', ?1, 'b0', ?2)",
                libsql::params![payload, rec],
            )
            .await
            .unwrap();
            let _ = i;
        }
        conn.execute(
            "INSERT INTO concept_cuts (id, branch_id, cut_at, recorded_at) \
             VALUES ('x', 'b0', '2026-01-10T00:00:00.000000Z', '2026-01-10T00:00:00.000000Z')",
            (),
        )
        .await
        .unwrap();
        let anc = vec![
            Ancestor {
                branch_id: "b0".into(),
                dist: 0,
                cutoff: None,
            },
            Ancestor {
                branch_id: "main".into(),
                dist: 1,
                cutoff: Some("2026-01-02T00:00:00.000000Z".into()),
            },
        ];
        let rows = overlay_scan(
            &conn,
            &["x".to_string()],
            &anc,
            Some(cap),
            Some("2026-01-10T12:00:00.000000Z"),
        )
        .await
        .unwrap();
        for (id, decision) in &rows {
            match decision {
                OverlayDecision::Row(r) => println!(
                    "proved: {id} -> row [{:?} {:?}) on {}",
                    r.valid_from, r.valid_to, r.branch_id
                ),
                OverlayDecision::Absent => println!("proved: {id} -> absent"),
            }
        }
    }
}
