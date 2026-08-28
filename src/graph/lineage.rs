//! Reading one lineage's belief out of a ledger that holds several (§15.2,
//! §15.3, D-219, D-220).
//!
//! # Two shapes, and the measurement that forced them
//!
//! `links_current` is keyed `(source_id, target_id, edge_type, valid_from,
//! branch_id)` since v12, so a branch correcting or retiring an edge it
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

/// The ancestry of the reading branch, nearest first.
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
pub(crate) fn ancestry_cte(slot: usize) -> String {
    format!(
        r#"lineage(branch_id, dist) AS (
    SELECT ?{slot}, 0
    UNION ALL
    SELECT b.parent_id, g.dist + 1
    FROM branches b JOIN lineage g ON b.branch_id = g.branch_id
    WHERE b.parent_id IS NOT NULL
)"#
    )
}

/// One row per edge key, from the nearest lineage that holds it.
///
/// `source` is the relation to resolve — `links_current` under current belief,
/// or the `links_at_tx` fold when the traversal names a transaction-time
/// instant. Both expose the same columns under the same names, which is what
/// lets this be written once.
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
