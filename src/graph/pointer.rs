//! Open targets of a `(source, kind)` in one lineage's view (0.20.0, M3,
//! [D-293]).
//!
//! The one read the pointer machinery needs, on both sides of the crate: the
//! write actor asks it before moving a pointer and before admitting an edge of
//! a functional kind, and [`crate::Database::pointer`] asks it for callers.
//!
//! It is a reader of the lowered lineage plan like `plan::edges_at`, with its
//! own placeholder layout — `?1` source (or unbound when listing every
//! source), `?2` kind, then the branch when the shape binds one, then the
//! recorded instant when there is one, then the ancestry block — so every
//! shape and both clocks are the lowering's, not a fourth spelling of them.
//!
//! "Open" means `valid_to` is the open sentinel: a pointer is the latest
//! belief about where `(source, kind)` points, not a valid-time question.
//!
//! [D-293]: ../../docs/architecture/s13-decision-register.md#d-293

use crate::error::Result;
use crate::graph::lineage::{ancestry_params, LineageShape, Lineages};
use crate::graph::plan::{lower, Resolution};
use crate::util::timestamp::OPEN_SENTINEL;

/// One open edge of a `(source, kind)` on a lineage.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenEdge {
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) valid_from: String,
    /// The lineage holding the row — the reader's own, or an ancestor's.
    pub(crate) branch_id: String,
}

/// Open edges of `kind` on `branch`, for one `source` or for all of them.
///
/// Ordered by source, then newest `valid_from` first, then target — so
/// [`crate::Database::pointer`] can take the first row for a source and be
/// deterministic even on a kind that is not functional.
pub(crate) async fn open_edges(
    conn: &libsql::Connection,
    lineages: &Lineages,
    branch: &str,
    source: Option<&str>,
    kind: &str,
    recorded: Option<&str>,
) -> Result<Vec<OpenEdge>> {
    let shape = lineages.shape(branch)?;
    let ancestry = match shape {
        LineageShape::Resolved => lineages.ancestry(branch),
        _ => Vec::new(),
    };
    if let (LineageShape::Resolved, Some(src), None) = (shape, source, recorded) {
        return open_edges_resolved(conn, branch, &ancestry, src, kind).await;
    }
    const BRANCH_SLOT: usize = 3;
    let recorded_slot = BRANCH_SLOT + usize::from(shape.binds_branch());
    let ancestry_slot = recorded_slot + usize::from(recorded.is_some());
    // A single-source read pushes `(source, kind)` into the base scans as a
    // prefix key, so a pointer read seeks `idx_lc_open_interval` on the
    // source instead of resolving the lineage's whole projection. The listing
    // has no source to push and resolves everything. The fold (a recorded
    // instant) takes no key, as `Resolution::key` documents.
    let key = match (source, recorded) {
        (Some(_), None) => Some(crate::graph::lineage::KeySlots {
            source: 1,
            target: None,
            edge_type: 2,
        }),
        _ => None,
    };
    let lowered = lower(&Resolution {
        shape,
        branch_slot: BRANCH_SLOT,
        recorded_slot: recorded.map(|_| recorded_slot),
        tag: "",
        key,
        ancestry: &ancestry,
        ancestry_slot,
    });
    // `?1 IS NULL` lets one statement serve both the single-source lookup and
    // the listing; the planner folds the disjunction for a bound value.
    let sql = format!(
        "{}SELECT l.source_id, l.target_id, l.valid_from, l.branch_id \
         FROM {} l \
         WHERE (?1 IS NULL OR l.source_id = ?1) AND l.edge_type = ?2 \
           AND l.valid_to = '{OPEN_SENTINEL}'{} \
         ORDER BY l.source_id, l.valid_from DESC, l.target_id",
        lowered.with_clause(),
        lowered.source,
        lowered.filter,
    );
    let mut params: Vec<libsql::Value> = vec![
        match source {
            Some(s) => libsql::Value::Text(s.to_string()),
            None => libsql::Value::Null,
        },
        kind.into(),
    ];
    if shape.binds_branch() {
        params.push(branch.into());
    }
    if let Some(ts) = recorded {
        params.push(ts.into());
    }
    params.extend(ancestry_params(&ancestry));

    let mut rows = conn.query(&sql, params).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(OpenEdge {
            source: row.get(0)?,
            target: row.get(1)?,
            valid_from: row.get(2)?,
            branch_id: row.get(3)?,
        });
    }
    Ok(out)
}

/// The branched single-source read, in two steps (0.20.0, M3, D-293).
///
/// A pointer's `(source, kind)` accumulates one edge key per target it ever
/// held — a session's `HEAD` has one per turn — so resolving every key to find
/// the one or two open ones paid a nearest-lineage window over the whole
/// history: measured **4.3 ms** for a 1,000-turn session read from a fork,
/// against **0.1 ms** on the trunk (`examples/conversation_probe.rs`).
///
/// Shadowing only ever *closes*: a nearer lineage's row can hide a farther
/// lineage's open row, never open a closed one. So the keys open in the
/// reader's view are a subset of the keys with an open row in *some*
/// lineage's entitled version — `links_cut` filtered on the open sentinel,
/// which the planner pushes into both arms. Step one finds those candidate
/// targets; step two resolves each candidate under its **full** key, the
/// write path's keyed resolution, so the window sees one key's rows at a time.
/// Exact, not a heuristic: a key with no open candidate row cannot be open
/// after resolution.
async fn open_edges_resolved(
    conn: &libsql::Connection,
    branch: &str,
    ancestry: &[crate::graph::lineage::Ancestor],
    source: &str,
    kind: &str,
) -> Result<Vec<OpenEdge>> {
    use crate::graph::lineage::KeySlots;
    const BRANCH_SLOT: usize = 3;
    let ancestry_slot = BRANCH_SLOT + 1;
    let base_params = || -> Vec<libsql::Value> {
        let mut p: Vec<libsql::Value> = vec![source.into(), kind.into(), branch.into()];
        p.extend(ancestry_params(ancestry));
        p
    };

    // Step one: candidate targets with an open entitled row anywhere on the
    // ancestry.
    let prefix = lower(&Resolution {
        shape: LineageShape::Resolved,
        branch_slot: BRANCH_SLOT,
        recorded_slot: None,
        tag: "",
        key: Some(KeySlots {
            source: 1,
            target: None,
            edge_type: 2,
        }),
        ancestry,
        ancestry_slot,
    });
    let sql = format!(
        "{}SELECT DISTINCT l.target_id FROM links_cut l WHERE l.valid_to = '{OPEN_SENTINEL}'",
        prefix.with_clause()
    );
    let mut rows = conn.query(&sql, base_params()).await?;
    let mut candidates: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(row.get(0)?);
    }
    drop(rows);

    // Step two: each candidate under its full key.
    let target_slot = ancestry_slot + 3 * ancestry.len();
    let keyed = lower(&Resolution {
        shape: LineageShape::Resolved,
        branch_slot: BRANCH_SLOT,
        recorded_slot: None,
        tag: "",
        key: Some(KeySlots {
            source: 1,
            target: Some(target_slot),
            edge_type: 2,
        }),
        ancestry,
        ancestry_slot,
    });
    let sql = format!(
        "{}SELECT l.valid_from, l.branch_id FROM {} l WHERE l.valid_to = '{OPEN_SENTINEL}'",
        keyed.with_clause(),
        keyed.source
    );
    let mut out = Vec::new();
    for target in candidates {
        let mut params = base_params();
        params.push(target.clone().into());
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push(OpenEdge {
                source: source.to_string(),
                target: target.clone(),
                valid_from: row.get(0)?,
                branch_id: row.get(1)?,
            });
        }
    }
    out.sort_by(|a, b| {
        b.valid_from
            .cmp(&a.valid_from)
            .then_with(|| a.target.cmp(&b.target))
    });
    Ok(out)
}
