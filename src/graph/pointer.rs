//! Open targets of a `(source, kind)` in one lineage's view (0.22.0, M3,
//! [D-309]).
//!
//! The one read the pointer machinery needs, on both sides of the crate: the
//! write actor asks it before moving a pointer and before admitting an edge of
//! a functional kind, and [`crate::Database::pointer`] asks it for callers.
//!
//! It is a reader of the lowered lineage plan like `plan::edges_at`, with its
//! own placeholder layout — `?1` source (or unbound when listing every
//! source), `?2` kind, then the branch when the shape binds one, then the
//! valid instant when there is one, then the recorded instant when there is
//! one, then the ancestry block — so every shape and both clocks are the
//! lowering's, not a fifth spelling of them.
//!
//! A read with no valid instant asks "what is open": `valid_to` is the open
//! sentinel, and that is still the only question the write path asks. A read
//! with one asks "where did `(source, kind)` hold at that instant", resolved
//! the way [`crate::plan::edges_at`] resolves validity — `valid_from <= ts /
//! <`, `ts < valid_to` — on the same lineage view, so a workspace read at a
//! past turn answers with the status that turn saw (0.22.0, plan §4.3).
//!
//! [D-309]: ../../docs/architecture/s13-decision-register.md#d-309

use crate::error::Result;
use crate::graph::lineage::{ancestry_params, KeySlots, LineageShape, Lineages};
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
    valid: Option<&str>,
    recorded: Option<&str>,
) -> Result<Vec<OpenEdge>> {
    let shape = lineages.shape(branch)?;
    let ancestry = match shape {
        LineageShape::Resolved => lineages.ancestry(branch),
        _ => Vec::new(),
    };
    // The two-step resolved read answers the open question under the write
    // path's own shape. A read naming a valid instant asks a different
    // question — containment, not the sentinel — and the open-sentinel CTEs
    // the two steps are built from cannot answer it; it takes the one
    // lowered statement below, the way `edges_at` resolves a listing
    // (0.22.0, plan §4.3).
    if shape == LineageShape::Resolved {
        if let (Some(src), None, None) = (source, valid, recorded) {
            return open_edges_resolved(conn, branch, &ancestry, src, kind).await;
        }
    }
    let (sql, params) = open_statement(shape, &ancestry, branch, source, kind, valid, recorded);

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

/// The lowered pointer statement and its parameter vector — the one spelling
/// of the reader outside the two-step resolved fast path, so the plan pin can
/// explain exactly what executes and a later edit cannot arrive at a plan
/// nothing pinned.
pub(crate) fn open_statement(
    shape: LineageShape,
    ancestry: &[crate::graph::lineage::Ancestor],
    branch: &str,
    source: Option<&str>,
    kind: &str,
    valid: Option<&str>,
    recorded: Option<&str>,
) -> (String, Vec<libsql::Value>) {
    const BRANCH_SLOT: usize = 3;
    let valid_slot = BRANCH_SLOT + usize::from(shape.binds_branch());
    let recorded_slot = valid_slot + usize::from(valid.is_some());
    let ancestry_slot = recorded_slot + usize::from(recorded.is_some());
    // A single-source read pushes `(source, kind)` into the base scans as a
    // prefix key, so a pointer read seeks `idx_lc_open_interval` on the
    // source instead of resolving the lineage's whole projection. The listing
    // has no source to push and resolves everything. The fold (a recorded
    // instant) takes no key, as `Resolution::key` documents; a valid instant
    // folds nothing, so it keeps the key, and the reader's own WHERE
    // below filters the key's rows by the instant.
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
        ancestry,
        ancestry_slot,
    });
    // Open when the read asks the write path's question: a pointer is the
    // latest belief, and `valid_to` the open sentinel. The `edges_at`
    // containment when it asks about an instant, the instant bound where this
    // function computed the slot to be — between the branch and the recorded
    // one, so a caller composing both clocks binds them in order.
    let validity = match valid {
        Some(_) => {
            format!(" AND l.valid_from <= ?{valid_slot} AND ?{valid_slot} < l.valid_to")
        }
        None => format!(" AND l.valid_to = '{OPEN_SENTINEL}'"),
    };
    // `?1 IS NULL` lets one statement serve both the single-source lookup and
    // the listing; the planner folds the disjunction for a bound value.
    let sql = format!(
        "{}SELECT l.source_id, l.target_id, l.valid_from, l.branch_id \
         FROM {} l \
         WHERE (?1 IS NULL OR l.source_id = ?1) AND l.edge_type = ?2{validity}{} \
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
    if let Some(ts) = valid {
        params.push(ts.into());
    }
    if let Some(ts) = recorded {
        params.push(ts.into());
    }
    params.extend(ancestry_params(ancestry));
    (sql, params)
}

/// `open_edges_resolved`' first statement: candidate targets with an open
/// entitled row on any lineage of the ancestry. Two key slots — the design
/// lives in `target` being unbound ([D-309]'s read) — and the Open
/// sentinel filter for the candidates themselves.
fn candidates_sql(ancestry: &[crate::graph::lineage::Ancestor]) -> String {
    const BRANCH_SLOT: usize = 3;
    let ancestry_slot = BRANCH_SLOT + 1;
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
    format!(
        "{}SELECT DISTINCT l.target_id FROM links_cut l WHERE l.valid_to = '{OPEN_SENTINEL}'",
        prefix.with_clause()
    )
}

/// `open_edges_resolved`' second statement: one candidate under its full key —
/// the write path's whole-key resolution, whose seek the plan pin asserts.
fn keyed_sql(ancestry: &[crate::graph::lineage::Ancestor], ancestry_slot: usize) -> String {
    use crate::graph::lineage::KeySlots;
    const BRANCH_SLOT: usize = 3;
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
    format!(
        "{}SELECT l.valid_from, l.branch_id FROM {} l WHERE l.valid_to = '{OPEN_SENTINEL}'",
        keyed.with_clause(),
        keyed.source
    )
}

/// The branched single-source read, in two steps (0.22.0, M3, D-309).
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
    const BRANCH_SLOT: usize = 3;
    let ancestry_slot = BRANCH_SLOT + 1;
    let base_params = || -> Vec<libsql::Value> {
        let mut p: Vec<libsql::Value> = vec![source.into(), kind.into(), branch.into()];
        p.extend(ancestry_params(ancestry));
        p
    };

    // Step one: candidate targets with an open entitled row anywhere on the
    // ancestry.
    let sql = candidates_sql(ancestry);
    let mut rows = conn.query(&sql, base_params()).await?;
    let mut candidates: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(row.get(0)?);
    }
    drop(rows);

    // Step two: each candidate under its full key.
    let sql = keyed_sql(ancestry, ancestry_slot);
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
#[cfg(test)]
mod plan_pins {
    //! The two-step read's plan, pinned from the first commit it exists
    //! (0.22.0, plan §3.4): the draft's figures (4.3 ms → 1.3–1.9 ms) were
    //! taken on a shared sandbox against libSQL 0.9.30, and the join order
    //! a measured plan rests on has a shelf life only while nobody touches
    //! the statement — the 0.21.0 cross-join lesson the §3.2 plan pin
    //! already applied to the concept folds.
    //!
    //! The builders themselves are the pin's inputs (`candidates_sql`,
    //! `keyed_sql`), so a refactor cannot satisfy the test against a
    //! hand-copied reproduction of the statements.
    use super::*;

    async fn fresh() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn
    }

    async fn plan(conn: &libsql::Connection, sql: &str) -> String {
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

    fn anc() -> Vec<crate::graph::lineage::Ancestor> {
        vec![
            crate::graph::lineage::Ancestor {
                branch_id: "exp".into(),
                dist: 0,
                cutoff: None,
            },
            crate::graph::lineage::Ancestor {
                branch_id: "main".into(),
                dist: 1,
                cutoff: Some("2026-01-06T00:00:00.000000Z".into()),
            },
        ]
    }

    /// Step one seeks on `source_id` **alone**: `idx_lc_open_interval` leads
    /// with `(source_id, target_id, edge_type, …)`, and `target_id` sits
    /// between the two slots the prefix key names, so `edge_type` cannot ride
    /// the seek — the base scans read a source's own pointer history and
    /// filter the kind, which is what `CREATE_INDICES`' own comment records
    /// and what the 1.3–1.9 ms figure was measured against. A future edit
    /// that "improves" the seek by reordering the index's columns
    /// re-measures that figure first; this red is its reminder.
    ///
    /// Step two is the write path's whole-key resolution: three slots seek
    /// `idx_lc_open_interval` in its own column order, one seek per
    /// candidate. Both statements order nothing in SQL — the read sorts in
    /// Rust — so a temp b-tree here is a plan nobody measured.
    #[tokio::test]
    async fn the_candidates_seek_the_prefix_key_and_the_resolution_seeks_the_whole_key() {
        let conn = fresh().await;

        // Re-pinned at 0.22.0 §4.3: with `idx_lc_pointer_kind` in the schema
        // the candidates read's own WHERE (valid_to open, `source_id`,
        // `edge_type` all bound) qualifies the partial index as well, and the
        // planner took it — same two equality columns the prefix key named,
        // so the seek *is* the prefix key, only on the index whose rows are
        // all open. Re-measured (conversation_probe, idle): fork pointer 1.41
        // → 1.31 ms, append-through-link_previous ~3.0 ms unchanged.
        let cand = plan(&conn, &candidates_sql(&anc())).await;
        assert!(
            cand.contains(
                "SEARCH lc USING INDEX idx_lc_pointer_kind (edge_type=? AND source_id=?)"
            ),
            r"the candidates arm must seek the two equality columns the prefix \
              key binds — via the partial index whose rows are all open, the \
              planner's pick on this schema; a plan that leaves it for \
              idx_lc_open_interval re-runs the conversation_probe first: {cand}"
        );
        assert!(
            !cand.contains("target_id=?"),
            r"no candidate arm binds the target — that is the prefix key's \
              whole point, and binding it here would read the old unkeyed read back: {cand}"
        );
        assert!(
            cand.contains("USE TEMP B-TREE FOR DISTINCT"),
            r"step one's dedupe is the DISTINCT b-tree, and it stays named —               the 1.3-1.9 ms measured claim includes it: {cand}"
        );

        let keyed = plan(&conn, &keyed_sql(&anc(), 4)).await;
        assert!(
            keyed
                .contains("SEARCH lc USING INDEX idx_lc_open_interval (source_id=? AND target_id=? AND edge_type=?)"),
            r"the resolution must seek the whole key in one go, the write path's \
              own shape: {keyed}"
        );
        assert!(
            keyed.contains("USE TEMP B-TREE FOR ORDER BY"),
            r"the compound's own ORDER BY (links_cut's D-272 determinism) must               stay where it is, inside the projection rather than on the read: {keyed}"
        );
    }

    /// The ready-set listing on the trunk shape, the statement the kind's
    /// partial index exists for (0.22.0, plan §4.3): a single-seek plan on
    /// `idx_lc_pointer_kind` (the partial index qualifying exactly because
    /// the reader's own WHERE carries its WHERE), and the sort the read pays
    /// for its workspace order — the 14.2 ms p50 the probe measured was
    /// measured *with* this sort in, so a plan without it is a different
    /// shape nobody measured and the pin is its reminder. Dropping the index
    /// to see the scan it replaced is the probe's work; the pin holds the
    /// shipped plan in the shipped schema.
    #[tokio::test]
    async fn the_ready_set_seeks_the_kind_index_and_sorts_the_workspace_order() {
        let conn = fresh().await;
        let (sql, _params) = open_statement(
            LineageShape::Trunk,
            &[],
            crate::schema::ddl::MAIN_BRANCH,
            None,
            "CURRENT",
            None,
            None,
        );
        let ready = plan(&conn, &sql).await;
        assert!(
            ready.contains("SEARCH l USING INDEX idx_lc_pointer_kind (edge_type=?)"),
            r"the ready-set read must seek the kind's partial index, one bound               column — a scan of every open pointer row is what the probe found               before the index, and a reorder of the index's columns must               re-measure the 1.70x first. (libsql's in-memory engine words the               plan `USING INDEX` where the file engine words it `USING PARTIAL               INDEX`; the index name is the load-bearing half either way.):               {ready}"
        );
        assert!(
            ready.contains("USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY"),
            r"the workspace order keeps its own sort inside the read — the               measured p50 includes it, and a plan that lost it is a shape               the fixture revised: {ready}"
        );
    }
}
