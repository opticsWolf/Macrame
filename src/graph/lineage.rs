//! Reading one lineage's belief out of a ledger that holds several (§15.2,
//! §15.3, D-219, D-220, D-223).
//!
//! # Two shapes, and the measurement that forced them
//!
//! `links_current` is keyed `(source_id, target_id, edge_type, valid_from,
//! branch_id)` since v12, so a branch that corrects or retires an edge it
//! inherited writes its **own** row beside the ancestor's rather than over it.
//! Reading a lineage therefore means picking one row per edge key: the one
//! belonging to the **nearest** branch on the path from the reader to the root.
//!
//! The naive alternative — admit every row whose `branch_id` is anywhere on the
//! path — is not a resolution and `branch_traversal_probe` §4b measures what it
//! costs. A branch that retires an inherited edge by shadowing it gets the
//! *whole subtree back*: 1,111 nodes where the resolved read gives 1,000. The
//! union form does not merely report a stale weight, it discards retirement.
//!
//! Resolution is not free and does not become free when there is nothing to
//! resolve. Measured on a single-lineage database — every database this crate
//! has ever written — the resolved traversal is **3.0×** the unresolved one,
//! because a window function is opaque to the planner and it cannot see that
//! every partition holds exactly one row. So the read path picks a shape, the
//! way `temporal::replay::cold_lineage` picks one at the archive boundary
//! (D-216): [`LineageShape::Trunk`] emits today's SQL, [`LineageShape::Resolved`]
//! emits the ancestry join.
//!
//! # Why one lineage is a sufficient condition for the fast shape
//!
//! `branch_id` is `NOT NULL DEFAULT 'main' REFERENCES branches(branch_id)` on
//! every ledger table (D-215), and the key is real. So a `branches` table
//! holding one row is a database in which every ledger row reads `'main'` — not
//! by convention but because nothing else could have been stored. The ancestry
//! of `main` is `{main}`, every partition has one member, and the resolved form
//! and the plain form return the same rows by construction. The check is
//! therefore exact rather than a heuristic, which is what makes it safe to skip
//! the work rather than merely cheap.
//!
//! # The fork point is a visibility cutoff, and one filter cannot apply it
//!
//! 0.14.4 resolved *which lineage holds an edge* and never looked at
//! `branches.forked_at`, so a branch kept absorbing writes its parent made
//! after the fork. §15.3 says the opposite in as many words — rows written on
//! each ancestor **before the fork point on the path down from A** — and
//! `ddl`'s own comment calls `forked_at` "what §15.3's visibility cutoffs are
//! computed over". Nothing computed them. 0.14.6 does (D-223).
//!
//! **The finding is not that a filter was missing.** `links_current` answers
//! *current as of now* and structurally cannot answer *current as of t*: the
//! projection holds one belief per key per lineage, and
//! `trg_links_current_sync` is `ON CONFLICT … DO UPDATE … recorded_at =
//! excluded.recorded_at`. So the moment the trunk reweights or retires an edge
//! after the fork, the pre-fork version is **not in the table at all**, and its
//! only home is `transaction_log`. Adding `recorded_at <= cutoff` to the
//! existing read would therefore not show the branch its inherited edge — it
//! would make that edge *vanish*, which is wrong in a new and quieter way.
//! Every "just add the filter" instinct fails for that one reason.
//!
//! So the read is a **hybrid**: [`links_cut_cte`] takes the `links_current`
//! rows a lineage may still see directly, and folds the log for exactly the
//! keys where it may not. The fold arm's cost scales with **post-fork churn on
//! the ancestors**, not with history size, and the untouched keys — the common
//! case, because a fork exists to diverge from a trunk that mostly stands still
//! — stay on the projection.
//!
//! **The two arms are disjoint by construction, and that is why [`churned_cte`]
//! is defined over `links_current` rather than over the log.** One predicate on
//! one row decides which arm emits a given `(edge key, lineage)`:
//! `recorded_at <= cutoff` sends it to the projection arm, `>` sends it to the
//! fold arm. Deriving the churned set from `transaction_log` instead would have
//! been a cheaper seek — `idx_txlog_time` is a range index and the projection
//! has none on `recorded_at` — but disjointness would then be an argument about
//! what the archive does and does not remove, rather than a property of a
//! single comparison. Two arms emitting one key would put two rows at the same
//! `dist` into [`visible_cte`]'s partition, where the winner is whichever the
//! engine happened to order first.
//!
//! **Where this degrades, named rather than left to be found.** The fold arm
//! reads the *hot* log, and `LOG_ARCHIVABLE` archives any entry superseded by a
//! later one for the same entity. A pre-fork assertion superseded by a
//! post-fork correction is superseded, so once the retention horizon passes the
//! fork point that entry can be cold — and then the branch loses an edge its
//! ancestor churned. That is §3.2's already-carried `AtTime` degradation
//! reached from the branch side, not a new class of loss, and it is bounded to
//! churned keys: the projection arm never degrades, because `links_current` is
//! re-derived from surviving `links` rather than deleted from. It is
//! deliberately **not** guarded by `check_recorded_reach`, whose bit
//! (`hot_log_is_intact`) is coarse — any archive at all flips it, and refusing
//! every branched read on any archived database would cost far more than the
//! case it prevents. A cold arm for `links_cut` is the fix if it is ever
//! needed, and it belongs with §3.2 rather than with the cutoff.

use crate::error::{DbError, Result};
use crate::schema::ddl;

/// Which form of the read to emit. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineageShape {
    /// One lineage exists, so there is nothing to resolve and the plain SQL is
    /// exact. Not an optimisation with a correctness caveat — see the module
    /// docs for why the condition is sufficient.
    Trunk,
    /// More than one lineage exists. The read resolves along the ancestry.
    Resolved,
}

/// Decide the shape, and refuse a lineage that was never registered.
///
/// One query, on a table that holds one row per branch and is never large. The
/// same round trip answers both questions because they are asked together and
/// neither can change under us mid-read.
///
/// # Refusing an unregistered lineage rather than answering for the trunk
///
/// A traversal naming a branch that is not in `branches` has asked a question
/// about something that does not exist. Answering it with the trunk's view
/// would be the [D-069] failure — a right-looking answer to a question that was
/// not asked — and it is the answer a caller is *least* able to detect, because
/// on a database that has never forked the trunk's view is what they expected
/// to see anyway.
///
/// [D-069]: ../../docs/architecture/s13-decision-register.md
pub(crate) async fn lineage_shape(
    conn: &libsql::Connection,
    branch: Option<&str>,
) -> Result<LineageShape> {
    let named = branch.unwrap_or(ddl::MAIN_BRANCH);
    let row = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM branches), \
                    (SELECT COUNT(*) FROM branches WHERE branch_id = ?1)",
            libsql::params![named],
        )
        .await?
        .next()
        .await?;
    let (total, found): (i64, i64) = match row {
        Some(r) => (r.get(0)?, r.get(1)?),
        // No row from a two-aggregate SELECT is not a state SQLite produces.
        // Treated as the trunk rather than raising, because the alternative is
        // an error class no caller can act on.
        None => return Ok(LineageShape::Trunk),
    };
    if found == 0 {
        return Err(DbError::NotFound(named.to_string()));
    }
    Ok(if total <= 1 {
        LineageShape::Trunk
    } else {
        LineageShape::Resolved
    })
}

/// The ancestry of the reading branch, nearest first, each with its cutoff.
///
/// `dist` is what makes this a resolution rather than a union: it is the
/// distance from the reader to each ancestor, and the row a lineage sees is the
/// one belonging to the smallest `dist` holding that edge key.
///
/// The recursion terminates on `parent_id IS NULL`, which the `branches` CHECK
/// pairs with `forked_at IS NULL` so that exactly one row — the root — can end
/// it. A cycle is not representable: `parent_id` is a foreign key into a table
/// whose rows are append-only and whose parent must already exist when the child
/// is written, so the graph is a forest by construction and the walk is finite
/// without a depth bound.
/// `slot` is where the reading branch binds, and it is passed rather than
/// spelled: the placeholder layout belongs to
/// [`TraversalBuilder`](crate::graph::TraversalBuilder), which computes it once
/// so that no two call sites can agree on it separately (D-030, D-035).
///
/// # `cutoff`, and why it is the child's `forked_at` rather than the row's
///
/// §15.3: a read on B sees rows written on each ancestor A *before the fork
/// point on the path down from A*. The fork point on the path down from A is
/// where **A's child on that path** diverged — so stepping from a branch to its
/// parent, the parent's cutoff is the stepping branch's own `forked_at`. The
/// reader itself has no cutoff at all, which is `NULL` in the anchor row and
/// the only `NULL` the column ever holds.
///
/// It is a running minimum rather than a plain assignment. `b` forks from `a`
/// at *t₂* and `a` from `main` at *t₁*, so `b` sees `main` as of *t₁* and not
/// as of *t₂* — inheritance composes, and each step can only narrow the window.
/// With `forked_at` monotone down a chain the `min` never fires; it is here
/// because the schema does not enforce that ordering and a read should not be
/// the thing that discovers it isn't.
///
/// # The equivalence that makes the fold arm safe
///
/// Under these cutoffs, **nearest-ancestor-wins and latest-`recorded_at`-wins
/// coincide**: each ancestor's visible window ends where its descendant's
/// begins, so the deepest lineage member holding a pre-cutoff row for a key is
/// also the one holding the newest such row. That is why
/// [`links_cut_cte`]'s fold arm may bound per lineage with a plain
/// `ROW_NUMBER() … PARTITION BY entity_id, branch_id` and hand the cross-lineage
/// question to [`visible_cte`], with no tiebreak between ancestors anywhere.
///
/// Stated as a consequence of the write path, not as a theorem about the
/// schema: it holds because a branch's own writes follow its fork, which
/// `fork()` and branch-scoped writes guarantee and no CHECK does. The
/// resolution still orders by `dist`, which is the definition; the equivalence
/// is what says the fold cannot disagree with it.
pub(crate) fn ancestry_cte(slot: usize) -> String {
    format!(
        r#"lineage(branch_id, dist, cutoff) AS (
    SELECT ?{slot}, 0, NULL
    UNION ALL
    SELECT b.parent_id, g.dist + 1,
           CASE WHEN g.cutoff IS NULL OR b.forked_at < g.cutoff
                THEN b.forked_at ELSE g.cutoff END
    FROM branches b JOIN lineage g ON b.branch_id = g.branch_id
    WHERE b.parent_id IS NOT NULL
)"#
    )
}

/// The `(edge key, lineage)` pairs whose projected row is *younger* than the
/// cutoff, and therefore cannot be shown to the reader.
///
/// One row here means: this ancestor holds a belief about this edge, and it
/// wrote that belief after the reader's line diverged. `links_current` has
/// overwritten whatever it believed before — the sync trigger's `DO UPDATE`
/// carries `recorded_at` forward — so the pre-fork version has to come from the
/// log. [`links_cut_cte`] is what goes and gets it.
///
/// **`entity_id` is composed here in the same order the log triggers compose
/// it**, `source|target|type|valid_from`, because that is the key the fold
/// joins on. It is a second spelling of a format the schema owns, and the thing
/// that keeps it honest is that a mismatch cannot be quiet: the join would
/// match nothing, every churned key would drop out of both arms, and
/// `branch_read_tests`' retire and reweight cases would go red together.
///
/// The `cutoff IS NOT NULL` clause is what makes this empty for a read on the
/// root — `main` has no ancestors and no cutoff, so a forked database still
/// pays nothing here to read its own trunk.
pub(crate) fn churned_cte() -> &'static str {
    r#"churned(entity_id, branch_id, cutoff) AS (
    SELECT lc.source_id || '|' || lc.target_id || '|' || lc.edge_type || '|' || lc.valid_from,
           lc.branch_id, g.cutoff
    FROM links_current lc
    JOIN lineage g ON g.branch_id = lc.branch_id
    WHERE g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff
)"#
}

/// `links_current` as each lineage on the ancestry was entitled to see it.
///
/// The hybrid §15.3 did not have a name for, entered there as option (4). Two
/// arms over one predicate:
///
/// * rows whose lineage may still show them directly — the reader's own
///   (`cutoff IS NULL`) and every ancestor row recorded at or before its cutoff;
/// * for the rest, the last log entry that lineage wrote at or before its
///   cutoff, which is what `links_current` held for that key at the fork.
///
/// A key the ancestor first asserted *after* the cutoff contributes nothing
/// from either arm, and that is correct rather than a gap: the branch's line
/// diverged before that edge existed, so there is no pre-fork row to resurrect.
/// The fold returning empty is the answer.
///
/// **`UNION ALL` is sound because the arms partition, not because duplicates
/// are harmless.** They split `links_current ⋈ lineage` on `recorded_at <=
/// cutoff`; see [`churned_cte`] for why the churned set is derived from the
/// projection and not from the log, which is the same point from the other
/// side.
///
/// The column list matches `links_current`'s and `links_at_tx`'s exactly, which
/// is what lets [`visible_cte`] reduce any of the three without knowing which.
pub(crate) fn links_cut_cte() -> &'static str {
    r#"links_cut(source_id, target_id, edge_type, valid_from, valid_to, weight, branch_id) AS (
    SELECT lc.source_id, lc.target_id, lc.edge_type, lc.valid_from, lc.valid_to,
           lc.weight, lc.branch_id
    FROM links_current lc
    JOIN lineage g ON g.branch_id = lc.branch_id
    WHERE g.cutoff IS NULL OR lc.recorded_at <= g.cutoff
    UNION ALL
    SELECT json_extract(payload, '$.source_id'),
           json_extract(payload, '$.target_id'),
           json_extract(payload, '$.edge_type'),
           json_extract(payload, '$.valid_from'),
           json_extract(payload, '$.valid_to'),
           json_extract(payload, '$.weight'),
           branch_id
    FROM (
        SELECT transaction_log.payload, transaction_log.branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY transaction_log.entity_id, transaction_log.branch_id
                   ORDER BY transaction_log.seq_id DESC
               ) AS rn
        FROM transaction_log
        JOIN churned k ON k.entity_id = transaction_log.entity_id
                      AND k.branch_id = transaction_log.branch_id
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= k.cutoff
    ) WHERE rn = 1
)"#
}

/// One row per edge key, from the nearest lineage that holds it.
///
/// `source` is the relation to resolve — [`links_cut_cte`] under current
/// belief, or the `links_at_tx` fold when the traversal names a transaction-time
/// instant. Both expose the same columns under the same names, which is what
/// lets this be written once, and both have already applied the ancestry's
/// cutoffs before this sees them.
///
/// **The partition is the edge key and not the edge.** Two lineages asserting
/// the same `(source, target, type)` at *different* `valid_from` are two
/// assertions in valid time and stay two rows; two lineages asserting at the
/// same `valid_from` are one edge believed twice and resolve to one. That is
/// also what makes shadow-retirement work: a branch writes its own row at the
/// ancestor's key with a closed interval, the resolution prefers it, and the
/// edge is gone from that lineage's view while the ancestor's row is untouched.
/// Closing the ancestor's own row is the parent corruption
/// [Doctrine III](../../docs/architecture/s0-s3-foundations.md#doctrine-iii)
/// forbids, and shadowing is the only retirement across lineages that does not
/// commit it.
///
/// **`ORDER BY g.dist` is a total order over the surviving rows**, because each
/// source contributes at most one row per `(edge key, lineage)` and each
/// lineage appears in `lineage` once. See [`ancestry_cte`] for why ordering by
/// `recorded_at` instead would pick the same row.
pub(crate) fn visible_cte(source: &str) -> String {
    format!(
        r#"visible(source_id, target_id, edge_type, valid_from, valid_to, weight) AS (
    SELECT source_id, target_id, edge_type, valid_from, valid_to, weight FROM (
        SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.weight,
               ROW_NUMBER() OVER (
                   PARTITION BY l.source_id, l.target_id, l.edge_type, l.valid_from
                   ORDER BY g.dist
               ) AS rn
        FROM {source} l
        JOIN lineage g ON g.branch_id = l.branch_id
    ) WHERE rn = 1
)"#
    )
}
