//! The read plan's lowering — **the one copy** of how a lineage read is spelled
//! (0.15.1, W13.1).
//!
//! Every branched read the crate emits is the same prelude in the same order:
//! the ancestry, the transaction-time fold if the read names a recorded
//! instant, the hybrid cut if it does not, and the nearest-lineage window over
//! whichever of those two the read resolves. Until this module existed that
//! prelude was assembled in three readers — `TraversalBuilder::walk_cte`,
//! `query_as_of_edges_on`, and `diff_sql` — and each of the three decided for
//! itself which relation its own query should join. They agreed, and
//! [D-227](../../docs/architecture/s13-decision-register.md#d-227) records
//! the four releases in which one of them did not: `query_as_of_edges_on`
//! spelled its own resolved form, missed the fork-point cutoff, and the tests
//! that would have caught it were written against the builder.
//!
//! What varies between the readers is exactly three things, and
//! [`Resolution`] is those three things: **which placeholder holds the
//! branch**, **which placeholder holds the recorded instant** when there is
//! one, and **the tag** that keeps two lineages apart in one `WITH` list. The
//! output, [`Lowered`], is the CTE list and the name of the relation the
//! reader's own query joins. The reader appends its query and binds its
//! parameters; it no longer knows that the resolution had arms, or how many.
//!
//! **The SQL this emits is byte-identical to what the three readers assembled
//! before it existed.** That is the release's whole acceptance: every plan pin
//! in `tests/index_plan_tests.rs`, every substring `tests/graph_tests.rs` and
//! `tests/bitemporal_plan_tests.rs` look for, and every golden string in
//! `builder.rs`'s unit tests passed unchanged rather than re-pinned. The
//! lowering is a move, not a rewrite, and a move whose output is checked
//! against the original is the only kind that can be trusted to have kept
//! D-227's repair intact.
//!
//! This is crate-private on purpose. The public `ReadPlan` road map §16 asks
//! for is a later release of the same wave; landing surface in the release
//! whose proof is that nothing observable moved would make the public-API gate
//! and the plan pins fail together, and neither failure would be diagnostic.

use crate::graph::lineage::{
    ancestry_values, churned_cte, links_cut_cte, visible_cte, Ancestor, KeySlots, LineageShape,
};

/// What a reader has decided about a lineage read, before any SQL exists.
///
/// `branch_slot` is read only under [`LineageShape::Resolved`]; a `Trunk`
/// read emits no `lineage` CTE and binds no branch, which is why each reader
/// still owns its placeholder layout — the lowering names slots, it does not
/// allocate them. `recorded_slot` is `Some` exactly when the read names a
/// transaction-time instant, and the fold it selects is the same one under
/// both shapes, bounded once more under `Resolved`.
///
/// `tag` is a suffix on every CTE name so that two resolutions can share one
/// `WITH` list — `diff_sql` lowers `_a` and `_b` and joins the two `visible`
/// relations. Every single-lineage reader passes `""` and gets the names it
/// always did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resolution<'a> {
    pub shape: LineageShape,
    pub branch_slot: usize,
    pub recorded_slot: Option<usize>,
    pub tag: &'a str,
    /// The edge key the reader already holds, when it holds one (0.15.8,
    /// W13.3, [D-250](../../docs/architecture/s13-decision-register.md#d-250)).
    ///
    /// `None` for every traversal: a walk discovers its edges and has no key
    /// to push down. `Some` for the write path, which has one before any SQL
    /// exists — see [`KeySlots`] for what it narrows and for the one column it
    /// adds.
    ///
    /// It narrows **current belief** only. A keyed transaction-time read would
    /// be correct and unnarrowed, because the fold's `entity_id` carries the
    /// `valid_from` this key does not fix and a prefix match is not a seek;
    /// there is no such reader today, and the day there is, that is the
    /// release that decides how.
    pub key: Option<KeySlots>,
    /// The reader's ancestry, resolved in Rust before any SQL exists.
    ///
    /// Read only under [`LineageShape::Resolved`], which is the only shape that
    /// emits a `lineage` relation at all. Empty for the other two, and the
    /// lowering does not look at it there.
    ///
    /// This is what replaced `ancestry_cte`'s `WITH RECURSIVE` in 0.15.17
    /// ([D-259]): the relation is the same three columns, computed by
    /// [`crate::graph::lineage::resolve`] and bound rather than recomputed by
    /// SQLite per query. See [`ancestry_values`] for why every value binds.
    ///
    /// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
    pub ancestry: &'a [Ancestor],
    /// Where the ancestry block starts, occupying `3 × ancestry.len()` slots.
    ///
    /// Every reader places it **after** its own fixed slots, so that adding the
    /// block moved no layout that already existed. Like [`Self::branch_slot`]
    /// this is named by the reader and not allocated here: the lowering spells
    /// slots, it does not hand them out (D-030, D-035).
    pub ancestry_slot: usize,
}

/// The lowering's output: the prelude, and the relation the reader joins.
///
/// `ctes` is in the order a `WITH` list must hold them — `churned` reads
/// `lineage`, `links_cut` reads `churned`, `visible` reads whichever source it
/// resolves, and SQLite resolves the list as written. It is a `Vec` rather
/// than a string because the three readers glue it three different ways (the
/// walk wants a trailing comma before its own recursive CTE; the other two
/// want a newline before their `SELECT`), and a lowering that chose one
/// gluing would have moved bytes it had no reason to move.
///
/// `source` is what the reader's `JOIN … l` names. Under `Resolved` it is
/// always `visible{tag}`, which holds one row per edge key from the nearest
/// lineage that has one *and was entitled to be seen*; the reader does not
/// need to know which relation `visible` reduced, nor that the reduction had
/// two arms (D-223). Under `Trunk` and `TrunkOnForked` it is that relation
/// directly.
///
/// `filter` is the predicate the reader appends to its own `WHERE`, on the
/// alias `l`, when the source alone does not narrow to the lineage — ` AND
/// l.branch_id = ?n` under [`LineageShape::TrunkOnForked`]'s current-belief
/// read, and empty everywhere else: `visible` is already one lineage's
/// view, `links_current` under `Trunk` holds one lineage, and the folded
/// `TrunkOnForked` read narrows inside the fold (see [`links_at_tx_cte`]).
/// It carries its own leading space, like the edge-type filter it sits
/// beside, so a reader whose shape has none emits the text it always did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lowered {
    pub ctes: Vec<String>,
    pub source: String,
    pub filter: String,
}

impl Lowered {
    /// The prelude as the walk splices it: each CTE followed by `,\n`, so the
    /// reader's own CTE can follow without knowing whether anything preceded
    /// it. Empty when there is nothing to resolve.
    pub(crate) fn prelude(&self) -> String {
        self.ctes.iter().map(|cte| format!("{cte},\n")).collect()
    }

    /// The prelude as a `WITH` list body: CTEs joined by `,\n`, no trailing
    /// separator. What `query_as_of_edges_on` and `diff_sql` put after
    /// `WITH RECURSIVE`.
    pub(crate) fn with_list(&self) -> String {
        self.ctes.join(",\n")
    }

    /// The whole `WITH RECURSIVE … ` clause with its trailing newline, or
    /// nothing when there is no prelude — for a reader whose query is one
    /// `format!` across shapes and must not emit an empty `WITH`.
    pub(crate) fn with_clause(&self) -> String {
        if self.ctes.is_empty() {
            String::new()
        } else {
            format!("WITH RECURSIVE {}\n", self.with_list())
        }
    }
}

/// Lower a [`Resolution`] to its prelude and source.
///
/// # The two sources, and why they are not the same pair (D-223)
///
/// Under [`LineageShape::Trunk`] the reader joins `links_current` under
/// current belief and the `links_at_tx` fold otherwise. On a database with one
/// lineage there is nothing to resolve and nothing to bound: the only cutoff a
/// read could apply is a fork point, and a one-row `branches` has none.
///
/// Under [`LineageShape::Resolved`] the reader joins `visible`, and what
/// `visible` reduces is `links_at_tx` under a transaction-time read and
/// `links_cut` under current belief — **not** `links_current`, and the
/// difference is the whole of D-223. A transaction-time read already folds
/// the log, so `links_at_tx` takes the ancestry's cutoffs as one more bound on
/// rows it was going to read anyway. A current-belief read cannot: the
/// projection holds one row per key per lineage and the sync trigger
/// overwrites it, so a lineage's pre-fork belief about a churned edge is not
/// in `links_current` to be filtered — it is in the log, and
/// [`links_cut_cte`] is the hybrid that goes and gets exactly those.
///
/// All four expose the columns `links_current` does, which is what lets each
/// reader's own query — and [`visible_cte`] — be written once.
///
/// # The third shape, and why it is a filter rather than a prelude (D-244)
///
/// Under [`LineageShape::TrunkOnForked`] the reader is a root: no ancestry,
/// no cutoff, no churned set, so the resolved read is *its own rows* and the
/// lowering says so with one predicate instead of four CTEs. Under current
/// belief that predicate is `l.branch_id = ?{branch_slot}` on the projection,
/// handed back as `filter`; at a recorded instant it goes **inside** the
/// fold, so the window ranks only the trunk's own log entries, and `filter`
/// is empty because the source already is one lineage.
///
/// # Where a key goes, and why that is two places (0.15.8, W13.3, D-250)
///
/// `Resolution::key` is `Some` for the write path and `None` for every
/// traversal. Where the reader joins a CTE chain — `Resolved` under current
/// belief — the key is pushed into the base scans of [`churned_cte`] and
/// [`links_cut_cte`], because that is the only place it can turn a scan of
/// `links_current` into a seek on `idx_lc_open_interval`. Where the reader
/// joins `links_current` itself there is no chain to push into, so the three
/// equalities come back in `filter` beside the branch predicate, and the
/// planner sees them in the reader's own `WHERE` where it would have seen
/// them anyway.
///
/// The consequence worth stating: the **root's guard is no longer the
/// resolved statement**. Until 0.15.8 `TrunkOnForked` prepared the four-CTE
/// resolved form and got the right answer out of it, because a root's
/// ancestry is itself; it now prepares a two-predicate lookup. That is what
/// makes [`crate::connection`]'s choice of shape per batch load-bearing
/// rather than cosmetic, which [D-248] said this release would do.
///
/// [D-248]: ../../docs/architecture/s13-decision-register.md#d-248
pub(crate) fn lower(r: &Resolution<'_>) -> Lowered {
    let tag = r.tag;
    let folded = r.recorded_slot.is_some();
    // A key narrows whatever the reader ends up joining. Where that is a CTE
    // chain it goes into the base scans, which is the only place it can become
    // a seek; where it is `links_current` itself there is no chain to push it
    // into, so it joins the reader's own `WHERE` beside the branch predicate.
    let key_filter = |k: KeySlots| format!(" AND {}", k.equalities("l"));

    let mut ctes: Vec<String> = Vec::new();
    if r.shape == LineageShape::Resolved {
        ctes.push(ancestry_values(r.ancestry.len(), r.ancestry_slot, tag));
    }
    if let Some(slot) = r.recorded_slot {
        ctes.push(links_at_tx_cte(r.shape, slot, r.branch_slot, tag));
    }
    let mut filter = String::new();
    // Both trunk shapes name the fold `links_at_tx{tag}` rather than
    // `links_at_tx`: the tag is empty for both readers that fold today, so
    // this is the same bytes, and a tagged folded trunk would otherwise join
    // a relation `links_at_tx_cte` did not emit.
    let source = match r.shape {
        LineageShape::Trunk => {
            if folded {
                format!("links_at_tx{tag}")
            } else {
                if let Some(k) = r.key {
                    filter = key_filter(k);
                }
                "links_current".to_string()
            }
        }
        LineageShape::TrunkOnForked => {
            if folded {
                format!("links_at_tx{tag}")
            } else {
                filter = format!(" AND +l.branch_id = ?{}", r.branch_slot);
                if let Some(k) = r.key {
                    filter.push_str(&key_filter(k));
                }
                "links_current".to_string()
            }
        }
        LineageShape::Resolved => {
            // The hybrid is for *current* belief only; the folded path applies
            // its cutoffs in place, for the reason the doc above gives.
            let resolved = if folded {
                format!("links_at_tx{tag}")
            } else {
                ctes.push(churned_cte(tag, r.key));
                ctes.push(links_cut_cte(tag, r.key));
                format!("links_cut{tag}")
            };
            ctes.push(visible_cte(&resolved, tag, r.key));
            format!("visible{tag}")
        }
    };
    Lowered {
        ctes,
        source,
        filter,
    }
}

/// `links_current` as the ledger believed it at the recorded instant
/// (W7.1, D-174; lineage 0.14.4, D-220).
///
/// `links_current` is a *projection of current belief*: the sync trigger
/// upserts each corrected edge over its predecessor, so the row that was
/// there before a correction is not in the table any more. It is in
/// `transaction_log`, because links are strictly append-only — every
/// assertion and every correction is an `INSERT`, each logged `'I'` with
/// `entity_id = source|target|type|valid_from` — so the last log row per
/// entity at or before the instant *is* what `links_current` held then.
///
/// # The partition, and the third column it needed
///
/// `table_name` is not in the partition because it is in the `WHERE`, which
/// is the same discriminator applied one step earlier; that much has always
/// been sound and the four folds in `replay.rs` make the other choice
/// deliberately.
///
/// `branch_id` **was** missing, and that was a defect rather than a
/// difference of style. `entity_id` is the edge key and it is shared across
/// lineages by design — that is exactly how a branch corrects an edge it
/// inherited — so a partition on `entity_id` alone put an ancestor's row and
/// a descendant's row in one group and kept whichever carried the higher
/// `seq_id`. Two lineages' assertions collapsed to one, and which one
/// survived was decided by write order.
///
/// D-216 fixed this shape in `replay.rs`, the DDL's own log triggers were
/// written knowing it, and this fold was left behind because its rustdoc
/// argued the partition was sound and the argument it gave — about the
/// concept/link collision — was true and about something else. A correct
/// justification for the wrong claim reads exactly like a correct claim,
/// which is why the note now names what it does *not* cover.
///
/// `branch_id` is also **selected**, not only partitioned on: it is the
/// column [`visible_cte`] joins the ancestry against, so the fold has to
/// carry it out.
///
/// There is no `'D'` arm because there are no link deletes:
/// `trg_links_guard_delete` refuses them outside an archive session, and an
/// archive session removes the *log rows* rather than logging a removal.
///
/// # The second bound, which is per row rather than per query (D-223)
///
/// Under [`LineageShape::Resolved`] the fold is bounded twice: by the read's
/// own transaction instant, which is one value for the whole query, and by
/// each ancestor's visibility cutoff, which is a different value per lineage.
/// Joining `lineage` here rather than filtering after the window is not a
/// style choice — `ROW_NUMBER()` picks the last entry *per partition*, so a
/// post-cutoff row left in the input wins its partition and is then
/// discarded, taking the pre-cutoff row that should have won with it. The
/// bound has to be inside.
///
/// This is also why the transaction-time path needs no [`links_cut_cte`]: it
/// is already reading the log, so the cutoff is one more `WHERE` clause
/// rather than a second source.
///
/// The columns are qualified by table name and not by an alias because
/// `EXPLAIN QUERY PLAN` prints the `FROM` clause: a two-character alias
/// silently renames `transaction_log` in every plan assertion that mentions
/// it, on a path where the alias buys nothing (D-223).
///
/// Under [`LineageShape::TrunkOnForked`] the second bound is not a cutoff but
/// the lineage itself — `transaction_log.branch_id = ?{branch_slot}` — and it
/// is inside for the cost rather than for the answer: the partition is per
/// lineage already, so a filter after the window would rank the other
/// lineages' entries and then discard them. The unary `+` is planner
/// steering, not arithmetic: without it SQLite took the equality as an access
/// path, walked `idx_txlog_entity` for the window's order, and gave up the
/// `recorded_at` seek that `bitemporal_plan_tests` pinned. The predicate is a
/// filter and the `+` says so.
///
/// **That seek is gone as of 0.15.12 and the `+` stays**
/// ([D-254](../../docs/architecture/s13-decision-register.md#d-254)). This
/// fold now reaches `idx_txlog_fold_partition (table_name=?)`, whose remaining
/// columns are this window's partition and order once the leading equality is
/// bound, so what the plan pins is the *absence of a sort* rather than a range
/// seek. Removing the `+` was measured against that index and changes nothing
/// on the fixture the pin uses — `branch_id` is the index's third column and
/// cannot be an access-path prefix — which is an argument for leaving it
/// alone, not for taking it out: it costs nothing, it is what stops the
/// predicate being read as an access path on some other distribution, and the
/// index that made it necessary is still declared.
///
/// # `MATERIALIZED`, and the 180× it is worth (0.15.2, D-244)
///
/// The fold is referenced once, by the reader's join, and SQLite's default for
/// a single-reference CTE is a co-routine. Inside a **recursive step** that
/// means the entire fold — the `recorded_at` range, the window, the
/// `json_extract`s — is evaluated again for every row the walk produces.
/// Measured on 11,110 trunk edges at depth 4, transaction-time read: 10.6 s as
/// a co-routine, 59 ms materialised. It went unmeasured for eleven releases
/// because the resolved shape's `visible` window forces materialisation by
/// itself, so the branched read — the one every probe timed — never showed
/// it, and the trunk's folded read was assumed to be the cheap one. Pinned by
/// plan in `builder.rs`'s tests on all three shapes.
pub(crate) fn links_at_tx_cte(
    shape: LineageShape,
    slot: usize,
    branch_slot: usize,
    tag: &str,
) -> String {
    let (lineage_join, cutoff) = match shape {
        LineageShape::Resolved => (
            format!("\n        JOIN lineage{tag} g ON g.branch_id = transaction_log.branch_id"),
            "\n          AND (g.cutoff IS NULL OR transaction_log.recorded_at <= g.cutoff)"
                .to_string(),
        ),
        LineageShape::TrunkOnForked => (
            String::new(),
            format!("\n          AND +transaction_log.branch_id = ?{branch_slot}"),
        ),
        LineageShape::Trunk => (String::new(), String::new()),
    };
    format!(
        r#"links_at_tx{tag}(source_id, target_id, edge_type, valid_from, valid_to, weight, branch_id) AS MATERIALIZED (
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
        FROM transaction_log{lineage_join}
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= ?{slot}{cutoff}
    ) WHERE rn = 1
)"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A two-row ancestry: the reader, and one ancestor it inherits from.
    ///
    /// The smallest fixture that makes a `Resolved` lowering *mean* something.
    /// An empty ancestry lowers to `VALUES ()`, which SQLite refuses, so a
    /// golden-string test written against one would pin text no database would
    /// accept (0.15.17, [D-259]).
    ///
    /// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
    fn anc() -> Vec<Ancestor> {
        vec![
            Ancestor {
                branch_id: "b1".to_string(),
                dist: 0,
                cutoff: None,
            },
            Ancestor {
                branch_id: "main".to_string(),
                dist: 1,
                cutoff: Some("2026-01-01T00:00:00.000000Z".to_string()),
            },
        ]
    }

    use crate::graph::builder::TraversalBuilder;

    const TUE: &str = "2026-01-06T00:00:00.000000Z";

    fn names(l: &Lowered) -> Vec<String> {
        l.ctes
            .iter()
            .map(|cte| cte.split('(').next().unwrap().to_string())
            .collect()
    }

    /// A trunk read under current belief resolves nothing and joins the
    /// projection directly.
    #[test]
    fn a_trunk_read_lowers_to_nothing() {
        let l = lower(&Resolution {
            shape: LineageShape::Trunk,
            branch_slot: 5,
            recorded_slot: None,
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert!(l.ctes.is_empty());
        assert_eq!(l.source, "links_current");
        assert_eq!(l.prelude(), "");
        assert_eq!(l.with_list(), "");
    }

    /// A trunk read at a recorded instant is the fold alone, unbounded by any
    /// cutoff, because a one-row `branches` has none.
    #[test]
    fn a_trunk_read_at_a_recorded_instant_is_the_fold_alone() {
        let l = lower(&Resolution {
            shape: LineageShape::Trunk,
            branch_slot: 5,
            recorded_slot: Some(5),
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(names(&l), ["links_at_tx"]);
        assert_eq!(l.source, "links_at_tx");
        assert!(l.ctes[0].contains("recorded_at <= ?5"));
        assert!(!l.ctes[0].contains("lineage"), "no ancestry to bound by");
    }

    /// The resolved current-belief prelude is the four CTEs in dependency
    /// order, and the reader joins the window.
    #[test]
    fn a_resolved_read_lowers_to_the_hybrid_in_order() {
        let l = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 5,
            recorded_slot: None,
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(names(&l), ["lineage", "churned", "links_cut", "visible"]);
        assert_eq!(l.source, "visible");
        // The first CTE is the ancestry itself, bound rather than walked
        // (0.15.17, [D-259]): one tuple per ancestor, three placeholders each,
        // starting at the slot the reader chose. It used to be a recursive
        // anchor row reading the branch out of `?5`.
        assert!(l.ctes[0].starts_with("lineage(branch_id, dist, cutoff) AS (VALUES "));
        assert!(
            l.ctes[0].contains("(?9, ?10, ?11), (?12, ?13, ?14)"),
            "{}",
            l.ctes[0]
        );
        assert!(
            !l.ctes[0].contains("SELECT"),
            "nothing is walked: {}",
            l.ctes[0]
        );
        assert!(l.ctes[3].contains("FROM links_cut l"));
    }

    /// At a recorded instant the hybrid is not emitted: the fold takes the
    /// cutoffs in place and the window reads the fold (D-223).
    #[test]
    fn a_resolved_read_at_a_recorded_instant_folds_with_the_cutoff_inside() {
        let l = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 5,
            recorded_slot: Some(6),
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(names(&l), ["lineage", "links_at_tx", "visible"]);
        assert_eq!(l.source, "visible");
        let fold = &l.ctes[1];
        assert!(fold.contains("recorded_at <= ?6"));
        assert!(fold.contains("JOIN lineage g ON g.branch_id = transaction_log.branch_id"));
        assert!(fold.contains("g.cutoff IS NULL OR transaction_log.recorded_at <= g.cutoff"));
        assert!(l.ctes[2].contains("FROM links_at_tx l"));
    }

    /// A tag reaches every name, including the one `visible` reads, so two
    /// resolutions can share one `WITH` list.
    #[test]
    fn a_tag_reaches_every_name() {
        let l = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 1,
            recorded_slot: None,
            tag: "_a",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(
            names(&l),
            ["lineage_a", "churned_a", "links_cut_a", "visible_a"]
        );
        assert_eq!(l.source, "visible_a");
        assert!(l.ctes[3].contains("FROM links_cut_a l"));
        assert!(l.ctes[3].contains("JOIN lineage_a g"));
        assert!(
            !l.with_list().contains("lineage g"),
            "an untagged name leaked: {}",
            l.with_list()
        );

        let folded = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 1,
            recorded_slot: Some(3),
            tag: "_b",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(names(&folded), ["lineage_b", "links_at_tx_b", "visible_b"]);
        assert!(folded.ctes[1].contains("JOIN lineage_b g"));
        assert!(folded.ctes[2].contains("FROM links_at_tx_b l"));
    }

    /// The three readers emit the prelude this module emits — the walk
    /// verbatim, the as-of read and `diff` up to the branch slot and tag,
    /// which are the only things a reader is allowed to choose.
    #[test]
    fn the_three_readers_share_one_prelude() {
        use crate::graph::lineage::diff_sql;

        let walk = TraversalBuilder::new("a").walk_cte(LineageShape::Resolved, &anc());
        let ours = lower(&TraversalBuilder::new("a").resolution(LineageShape::Resolved, &anc()));
        assert!(
            walk.contains(&ours.prelude()),
            "the walk assembles its own prelude"
        );

        let folded = TraversalBuilder::new("a").as_of_recorded(TUE);
        let walk = folded.walk_cte(LineageShape::Resolved, &anc());
        assert!(walk.contains(&lower(&folded.resolution(LineageShape::Resolved, &anc())).prelude()));
        let walk = folded.walk_cte(LineageShape::Trunk, &anc());
        assert!(walk.contains(&lower(&folded.resolution(LineageShape::Trunk, &anc())).prelude()));

        // `diff_sql` binds the two lineages at `?1` and `?2`, then `a`'s
        // ancestry block, then `b`'s — so the two sides no longer lower to the
        // same text at the same slots, and the test has to say where each one
        // starts (0.15.17, [D-259]).
        let diff = diff_sql(&anc(), &anc());
        for (slot, tag, ancestry_slot) in [(1, "_a", 3), (2, "_b", 3 + anc().len() * 3)] {
            let side = lower(&Resolution {
                shape: LineageShape::Resolved,
                branch_slot: slot,
                recorded_slot: None,
                tag,
                key: None,
                ancestry: &anc(),
                ancestry_slot,
            });
            assert!(
                diff.contains(&side.with_list()),
                "diff spells lineage {tag} itself"
            );
        }
        // And the two sides differ from each other only by slot and tag: the
        // same text with the tag and slot substituted is the other side.
        let a = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 1,
            recorded_slot: None,
            tag: "_a",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        let b = lower(&Resolution {
            shape: LineageShape::Resolved,
            branch_slot: 2,
            recorded_slot: None,
            tag: "_b",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        let retagged = ["lineage", "churned", "links_cut", "visible"]
            .iter()
            .fold(a.with_list(), |s, name| {
                s.replace(&format!("{name}_a"), &format!("{name}_b"))
            })
            .replace("SELECT ?1, 0, NULL", "SELECT ?2, 0, NULL");
        assert_eq!(retagged, b.with_list());
    }

    /// The trunk on a forked ledger is its own rows: no prelude, one
    /// predicate, and the branch bound where the reader said it would be.
    #[test]
    fn the_trunk_on_a_forked_ledger_lowers_to_a_filter() {
        let l = lower(&Resolution {
            shape: LineageShape::TrunkOnForked,
            branch_slot: 5,
            recorded_slot: None,
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert!(l.ctes.is_empty(), "no ancestry to resolve: {:?}", names(&l));
        assert_eq!(l.source, "links_current");
        assert_eq!(l.filter, " AND +l.branch_id = ?5");
        assert_eq!(l.with_clause(), "");

        // At a recorded instant the predicate moves inside the fold, before
        // the window, and the reader has nothing left to add.
        let folded = lower(&Resolution {
            shape: LineageShape::TrunkOnForked,
            branch_slot: 5,
            recorded_slot: Some(6),
            tag: "",
            key: None,
            ancestry: &anc(),
            ancestry_slot: 9,
        });
        assert_eq!(names(&folded), ["links_at_tx"]);
        assert_eq!(folded.source, "links_at_tx");
        assert_eq!(folded.filter, "");
        let fold = &folded.ctes[0];
        assert!(fold.contains("recorded_at <= ?6"));
        assert!(fold.contains("AND +transaction_log.branch_id = ?5"));
        assert!(!fold.contains("lineage"), "a root has no ancestry to join");
        assert!(folded
            .with_clause()
            .starts_with("WITH RECURSIVE links_at_tx("));
    }

    /// The other shapes hand the reader no filter: their source is already
    /// one lineage's view, or the only lineage there is.
    #[test]
    fn only_the_forked_trunk_needs_a_reader_side_filter() {
        for (shape, recorded) in [
            (LineageShape::Trunk, None),
            (LineageShape::Trunk, Some(5)),
            (LineageShape::Resolved, None),
            (LineageShape::Resolved, Some(6)),
        ] {
            let l = lower(&Resolution {
                shape,
                branch_slot: 5,
                recorded_slot: recorded,
                tag: "",
                key: None,
                ancestry: &anc(),
                ancestry_slot: 9,
            });
            assert_eq!(l.filter, "", "{shape:?} at {recorded:?}");
        }
    }
}
